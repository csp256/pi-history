import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { startBridge } from "./server.ts";

/** Optional Pi companion for the standalone pi-history viewer. No model tools. */
export default function (pi: ExtensionAPI) {
  let bridge: Awaited<ReturnType<typeof startBridge>> | undefined;
  let generation = 0;
  pi.on("session_start", async (_event, ctx) => {
    const ownGeneration = ++generation;
    const previous = bridge;
    bridge = undefined;
    await previous?.close();
    if (ownGeneration !== generation || (ctx.mode !== "tui" && ctx.mode !== "rpc")) return;
    if (!ctx.sessionManager.getSessionFile()) return; // Ephemeral sessions cannot be targeted.
    try {
      const created = await startBridge({
        getSession: () => {
          const sessionFile = ctx.sessionManager.getSessionFile();
          if (!sessionFile) throw new Error("No saved active session");
          return {
            sessionFile,
            sessionId: ctx.sessionManager.getSessionId(),
            name: ctx.sessionManager.getSessionName(),
            busy: !ctx.isIdle(),
          };
        },
        send: (message, delivery) => {
          // Like normal Enter: steer while busy, prompt while idle. Pi retains its
          // own steeringMode/followUpMode settings. Alt+Enter chooses followUp.
          // Always pass deliverAs to avoid an idle/busy race between inspection and dispatch.
          pi.sendUserMessage(message, { deliverAs: delivery, expandPromptTemplates: false });
        },
      });
      if (ownGeneration !== generation) await created.close();
      else bridge = created;
    } catch (error) {
      ctx.ui.notify(`pi-history bridge unavailable: ${error instanceof Error ? error.message : error}`, "warning");
    }
  });
  pi.on("session_shutdown", async () => {
    ++generation;
    const previous = bridge;
    bridge = undefined;
    await previous?.close();
  });
  pi.registerCommand("history-bridge", {
    description: "Show whether pi-history can send messages to this session",
    handler: async (_args, ctx) => {
      ctx.ui.notify(bridge ? "pi-history bridge active. Open this session in pi-history and press m to compose." : "pi-history bridge inactive (requires a saved TUI or RPC session).", bridge ? "info" : "warning");
    },
  });
}
