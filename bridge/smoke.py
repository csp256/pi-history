# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte"]
# ///
"""macOS/Linux end-to-end test: real Pi TUI + real viewer, local mock model only.
Run through CPU slots: uv run .../slots.py -n 2 -- uv run bridge/smoke.py target/debug/pi-history
No credentials, external models, global config, or user sessions are used.
"""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import pyte


class Terminal:
    def __init__(self, command, env, cwd):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 180, 0, 0))
        self.screen = pyte.Screen(180, 32)
        self.stream = pyte.Stream(self.screen)
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)

    @property
    def text(self):
        return "\n".join(self.screen.display)

    def send(self, text):
        os.write(self.master, text.encode() if isinstance(text, str) else text)

    def pump(self):
        if select.select([self.master], [], [], 0)[0]:
            try:
                chunk = os.read(self.master, 65536)
                if b"\x1b[6n" in chunk:
                    self.send(b"\x1b[1;1R")
                self.stream.feed(chunk.decode(errors="replace"))
            except OSError:
                pass

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        os.close(self.master)


def main():
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/pi-history").resolve())
    pi = shutil.which("pi")
    assert pi, "Pi must be installed"
    bridge = Path(__file__).resolve().with_name("index.ts")
    terminals = []
    holding, release = threading.Event(), threading.Event()

    def text_content(value):
        if isinstance(value, str):
            return value
        return "".join(block.get("text", "") for block in value if block.get("type") == "text")

    class MockModel(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            latest = next(text_content(message["content"]) for message in reversed(body["messages"]) if message["role"] == "user")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta, reason=None):
                payload = {"id": "local-test", "object": "chat.completion.chunk", "created": int(time.time()), "model": "test", "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
                self.wfile.flush()

            try:
                chunk({"role": "assistant", "content": "ECHO_" + latest})
                if latest.startswith("HOLD_"):
                    holding.set()
                    assert release.wait(20), "Test failed to release mock model"
                chunk({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = http.server.HTTPServer(("127.0.0.1", 0), MockModel)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with tempfile.TemporaryDirectory(prefix="pi-history-live-") as home:
            home = Path(home)
            agent = home / "agent"
            agent.mkdir()
            directory = home / "bridge"
            session = home / "session.jsonl"
            (agent / "settings.json").write_text(json.dumps({"defaultProvider": "bridge-test", "defaultModel": "test", "defaultThinkingLevel": "off", "quietStartup": True, "lastChangelogVersion": "1.5.0"}))
            (agent / "models.json").write_text(json.dumps({"providers": {"bridge-test": {"baseUrl": f"http://127.0.0.1:{server.server_port}/v1", "api": "openai-completions", "apiKey": "local-test-only", "models": [{"id": "test", "name": "Local bridge test", "reasoning": False, "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 200000, "maxTokens": 1000}]}}}))
            env = dict(os.environ, TERM="xterm-256color", HOME=str(home), PI_CODING_AGENT_DIR=str(agent), PI_HISTORY_BRIDGE_DIR=str(directory), PI_OFFLINE="1", RAYON_NUM_THREADS="1")
            host = Terminal([pi, "--offline", "--no-extensions", "--no-skills", "--no-prompt-templates", "-e", str(bridge), "--provider", "bridge-test", "--model", "test", "--session", str(session)], env, home)
            terminals.append(host)

            def wait(predicate, label, seconds=15):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    for terminal in terminals:
                        terminal.pump()
                    if predicate():
                        return
                    time.sleep(.025)
                raise AssertionError(label + "\n" + "\n---\n".join(terminal.text for terminal in terminals))

            def users():
                if not session.exists():
                    return []
                return [text_content(entry["message"]["content"]) for line in session.read_text().splitlines() if (entry := json.loads(line)).get("type") == "message" and entry["message"]["role"] == "user"]

            def assistant_has(marker):
                if not session.exists():
                    return False
                return any(entry.get("type") == "message" and entry["message"]["role"] == "assistant" and marker in text_content(entry["message"]["content"]) for line in session.read_text().splitlines() if (entry := json.loads(line)))

            wait(lambda: directory.exists() and any(directory.glob("*.json")), "Companion did not start for new, not-yet-persisted session")
            host.send("INITIAL\r")
            wait(lambda: assistant_has("ECHO_INITIAL"), "Local model did not complete initial prompt")
            viewer = Terminal([binary, str(session)], env, home)
            terminals.append(viewer)
            wait(lambda: "ECHO_INITIAL" in viewer.text, "Viewer did not open current session")

            def compose(message, key=b"\r", busy=False):
                viewer.send("m")
                wait(lambda: "Pi PID" in viewer.text and "Message running Pi" in viewer.text, "Composer did not connect")
                if busy:
                    assert "busy when checked" in viewer.text, viewer.text
                viewer.send("\x1b[200~" + message + "\x1b[201~")
                wait(lambda: message.splitlines()[0] in viewer.text, "Bracketed paste did not enter composer")
                viewer.send(key)
                wait(lambda: "Message running Pi" not in viewer.text and "Pi accepted" in viewer.text, "No accepted acknowledgement")

            idle = "IDLE_λ世界\nLINE_TWO"
            compose(idle)
            wait(lambda: users().count(idle) == 1 and assistant_has("ECHO_IDLE_"), "Idle message was not saved/responded to")
            wait(lambda: "ECHO_IDLE_" in host.text, "User message/response did not appear in original Pi terminal")

            for label, key in [("STEER", b"\r"), ("FOLLOW", b"\x1b\r")]:
                holding.clear()
                release.clear()
                host.send(f"HOLD_{label}\r")
                wait(holding.is_set, "Mock model never became busy")
                marker = "FROM_HISTORY_" + label
                compose(marker, key=key, busy=True)
                release.set()
                wait(lambda: users().count(marker) == 1 and assistant_has("ECHO_" + marker), f"{label} message was not consumed exactly once")
                wait(lambda: "ECHO_" + marker in host.text, f"{label} response missing from original terminal")

            host.send(b"\x04")
            wait(lambda: host.process.poll() is not None, "Pi did not shut down")
            wait(lambda: not list(directory.glob("*.json")), "Companion did not remove its endpoint on shutdown")
            viewer.send("m")
            wait(lambda: "No running Pi bridge" in viewer.text, "Stopped Pi was still considered live")
            viewer.send("\x1b[200~OFFLINE_DRAFT\x1b[201~")
            wait(lambda: "OFFLINE_DRAFT" in viewer.text, "Offline draft was discarded")
            viewer.send(b"\x1b")
            assert "OFFLINE_DRAFT" not in users()
            viewer.send("q")
            wait(lambda: viewer.process.poll() is not None, "Viewer did not shut down")
            print("PASS: real Pi TUI + viewer; new-session discovery; Unicode/multiline paste; idle dispatch; busy steering; Alt+Enter follow-up; original-terminal display; exact-once history; shutdown cleanup; offline draft safety.")
    finally:
        release.set()
        for terminal in reversed(terminals):
            terminal.close()
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)


if __name__ == "__main__":
    main()
