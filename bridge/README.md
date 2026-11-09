# Optional Pi message bridge

The viewer remains standalone. Sending is opt-in and requires this companion **inside the already-running Pi process** on the same machine and user account. It never starts a second agent or writes user messages into session JSONL itself.

## Install

With the current `@earendil-works/pi-coding-agent` API (tested with Pi 1.5.0):

```sh
pi install git:github.com/csp256/pi-history
```

Run `/reload` in existing Pi tabs, or restart them. `/history-bridge` reports whether the companion is active. For a temporary development run instead:

```sh
pi -e /absolute/path/to/pi-history/bridge/index.ts
```

Open that session in `pi-history` and press **m**:

- **Enter:** normal Pi submission — prompt while idle, steer while busy.
- **Alt+Enter** (or **Ctrl+Q**): follow-up while busy.
- **Ctrl+J** or **Shift+Enter:** newline. Arrow keys edit; bracketed paste supports multiline Unicode text.
- **Esc:** discard the draft and close. An in-flight request cannot be cancelled after Pi may have accepted it.

Pi's existing `steeringMode` and `followUpMode` settings still determine how it consumes queues. There is no separate viewer delivery preference. Pi receives a normal user message through `pi.sendUserMessage`, so its input hooks, agent execution, terminal, and session persistence remain authoritative. Queued messages appear in history when Pi consumes them, not necessarily at acknowledgement time. Sending targets the **active branch** of the selected session, not an older message/branch you happen to be reading.

The initial composer is text-only (maximum **32 KiB UTF-8**). No images, `@file` attachments, shell execution, or slash-command expansion: slash-prefixed text is sent as a user message. A success notice means **accepted for dispatch**, not that input hooks accepted it or that the model finished successfully; check the original Pi terminal for those outcomes. Pi's extension API does not return a completion receipt.

## Safety and lifecycle

- A loopback-only TCP listener uses a random 256-bit capability token. This is JSONL, not HTTP; browser requests are not valid bridge messages.
- Discovery records live in `~/.cache/pi-history/bridge/` (directory **0700**, records **0600** on Unix). They contain credentials: do not share, commit, or log them. The companion refuses insecure/non-owned/symlink directories; the viewer ignores insecure or symlink credential files. On Windows, keep this directory under your private user profile or apply equivalent user-only ACLs.
- To isolate installs/tests, set **`PI_HISTORY_BRIDGE_DIR` to the same private directory in both processes**. It is a trusted local-user boundary, not a remote-control service or protection against other programs running as your user.
- Every operation authenticates and rechecks the canonical session filename and Pi session ID. Multiple responsive Pi processes for the same file are ambiguous and sending is refused. The viewer only connects to `127.0.0.1`, never a host supplied by a registry file.
- New sessions can register their allocated filename before Pi first persists a user message. No history file is created by the bridge. Ephemeral (`--no-session`), print, and JSON-mode sessions do not expose a bridge.
- Session replacement, reload, and orderly shutdown close the listener and remove its record. Crashes may leave stale records; they are probed and ignored, never used to launch/resume Pi. If discovery reports too many stale records, close Pi processes before deleting stale `.json` records.
- Transport work runs off the viewer UI thread. Requests/responses, connections, and timeouts are bounded. A missing bridge leaves your draft intact; there is no automatic send/retry or fallback to a different process.
- A lost acknowledgement is **uncertain**, not “not sent.” Check Pi before retrying. An unchanged retry in the same composer uses the same request ID; the companion deduplicates it and rejects reuse with different text/delivery. Sending changed text/delivery or reopening the composer creates a new request. Receipt IDs are kept for the lifetime of that listener (up to 4096, then new sends are refused until reload); never assume deduplication survives a Pi restart or `/reload`.

## Verification

```sh
just bridge-test
just test
just build
# macOS/Linux only: real Pi TUI + viewer, isolated config and a local mock model.
# No API credentials, paid model calls, user sessions, or global config changes.
uv run bridge/smoke.py target/debug/pi-history
```

Use your environment's CPU-slot wrapper for builds/tests when required. Node unit tests require Node 22.18+ (native TypeScript stripping) and install no dependencies. The real-TUI smoke test uses the installed `pi` executable and `pyte` via its PEP 723 script metadata.
