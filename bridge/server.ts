import { createServer, type Socket } from "node:net";
import { randomBytes, randomUUID, createHash, timingSafeEqual } from "node:crypto";
import { mkdirSync, lstatSync, realpathSync, writeFileSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname, basename } from "node:path";

export const MAX_MESSAGE_BYTES = 32 * 1024;
const MAX_WIRE_BYTES = 64 * 1024;
const VERSION = 1;
export type SessionState = { sessionFile: string; sessionId: string; name?: string; busy: boolean };
type Delivery = "steer" | "followUp";
type Options = {
  directory?: string;
  getSession: () => SessionState;
  send: (message: string, delivery: Delivery) => void | Promise<void>;
};

export function bridgeDirectory(): string {
  return process.env.PI_HISTORY_BRIDGE_DIR ?? join(homedir(), ".cache", "pi-history", "bridge");
}

function canonicalSessionFile(path: string): string {
  try { return realpathSync.native(path); }
  catch (error) {
    // Pi allocates a new session filename at startup but creates it only on the
    // first user message. Resolve the existing parent without writing history.
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return join(realpathSync.native(dirname(path)), basename(path));
  }
}

function privateDirectory(directory: string) {
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const stat = lstatSync(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() ||
      (process.platform !== "win32" && ((stat.mode & 0o077) !== 0 || stat.uid !== process.getuid?.()))) {
    throw new Error("Bridge directory must be owned by you, not a symlink, and private (0700)");
  }
}

/** Loopback-only, capability-authenticated, one bounded JSONL request per connection. */
export async function startBridge(options: Options) {
  const directory = options.directory ?? bridgeDirectory();
  privateDirectory(directory);
  const initial = options.getSession();
  const sessionFile = canonicalSessionFile(initial.sessionFile);
  const sessionId = initial.sessionId;
  const token = randomBytes(32).toString("hex");
  const clients = new Set<Socket>();
  const receipts = new Map<string, { digest: string; result: Promise<object> }>();
  let closing = false;
  let registryFile: string | undefined;
  const current = () => {
    if (closing) throw new Error("Session bridge is closing");
    const state = options.getSession();
    if (state.sessionId !== sessionId || canonicalSessionFile(state.sessionFile) !== sessionFile) {
      throw new Error("This session is no longer active in this Pi instance");
    }
    return state;
  };
  const handle = async (raw: unknown): Promise<object> => {
    if (!raw || typeof raw !== "object") throw new Error("Invalid request");
    const request = raw as Record<string, unknown>;
    const supplied = typeof request.token === "string" ? Buffer.from(request.token) : Buffer.alloc(0);
    const expected = Buffer.from(token);
    if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) throw new Error("Unauthorized");
    if (request.version !== VERSION || request.sessionFile !== sessionFile || request.sessionId !== sessionId) {
      throw new Error("Protocol or session identity mismatch");
    }
    const state = current();
    if (request.type === "status") {
      return { version: VERSION, ok: true, pid: process.pid, sessionFile, sessionId, name: state.name, busy: state.busy };
    }
    if (request.type !== "send" || typeof request.message !== "string" || !request.message.trim() ||
        Buffer.byteLength(request.message) > MAX_MESSAGE_BYTES || request.message.includes("\0") ||
        typeof request.requestId !== "string" || !/^[a-zA-Z0-9_-]{1,128}$/.test(request.requestId) ||
        (request.delivery !== "steer" && request.delivery !== "followUp")) {
      throw new Error("Invalid message (nonempty text, at most 32 KiB, and valid request ID/delivery required)");
    }
    const message = request.message;
    const delivery = request.delivery as Delivery;
    const digest = createHash("sha256").update(JSON.stringify([message, delivery])).digest("hex");
    const previous = receipts.get(request.requestId);
    if (previous) {
      if (previous.digest !== digest) throw new Error("Request ID was already used for different content");
      return previous.result;
    }
    // Never evict IDs and accidentally deliver a retried request twice.
    if (receipts.size >= 4096) throw new Error("Bridge receipt cache is full; reload Pi before sending more messages");
    const result = Promise.resolve().then(async () => {
      const atSend = current();
      await options.send(message, delivery);
      // This acknowledges dispatch to Pi, not model completion or input-hook behavior.
      return { version: VERSION, ok: true, requestId: request.requestId, disposition: "accepted", delivery: atSend.busy ? delivery : "prompt" };
    });
    receipts.set(request.requestId, { digest, result });
    return result;
  };
  const server = createServer((socket) => {
    if (closing || clients.size >= 16) { socket.destroy(); return; }
    clients.add(socket);
    const deadline = setTimeout(() => socket.destroy(), 3000);
    deadline.unref();
    socket.on("close", () => { clearTimeout(deadline); clients.delete(socket); });
    socket.on("error", () => {});
    let buffer = Buffer.alloc(0);
    let processed = false;
    const reply = (value: object) => {
      if (!socket.destroyed) socket.end(JSON.stringify(value) + "\n");
    };
    socket.on("data", (chunk: Buffer) => {
      if (processed) return;
      if (buffer.length + chunk.length > MAX_WIRE_BYTES) {
        processed = true;
        reply({ version: VERSION, ok: false, error: "Request too large" });
        return;
      }
      buffer = Buffer.concat([buffer, chunk]);
      const newline = buffer.indexOf(10);
      if (newline < 0) return;
      processed = true;
      try {
        if (buffer.subarray(newline + 1).toString().trim()) throw new Error("Only one request per connection is allowed");
        const request = JSON.parse(buffer.subarray(0, newline).toString("utf8"));
        void handle(request).then(reply, (error) => reply({ version: VERSION, ok: false, error: String(error instanceof Error ? error.message : error) }));
      } catch (error) {
        reply({ version: VERSION, ok: false, error: String(error instanceof Error ? error.message : error) });
      }
    });
  });
  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => { server.off("error", reject); resolve(); });
    });
    server.on("error", () => {});
    server.unref();
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("No bridge address");
    registryFile = join(directory, `${process.pid}-${randomUUID()}.json`);
    writeFileSync(registryFile, JSON.stringify({ version: VERSION, pid: process.pid, port: address.port, token, sessionFile, sessionId }), { mode: 0o600, flag: "wx" });
    let closed: Promise<void> | undefined;
    return {
      registryFile,
      port: address.port,
      close(): Promise<void> {
        if (closed) return closed;
        closing = true;
        try { if (registryFile) unlinkSync(registryFile); } catch { /* already removed */ }
        for (const socket of clients) socket.destroy();
        closed = new Promise<void>((resolve) => server.close(() => resolve()));
        return closed;
      },
    };
  } catch (error) {
    closing = true;
    for (const socket of clients) socket.destroy();
    server.close();
    if (registryFile) { try { unlinkSync(registryFile); } catch {} }
    throw error;
  }
}
