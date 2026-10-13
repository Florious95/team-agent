"""Controlled Pi composer fixture; no provider/model and no synthetic delivery receipt.

The real injection must finish bracketed paste and deliver a separate Enter.
Only then does the existing fake-worker consume the original bytes and report.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import termios
import time
import tty

BEGIN = b"\x1b[200~"
END = b"\x1b[201~"


def main():
    binary, workspace, agent = sys.argv[1:]
    workspace = Path(workspace)
    events = workspace / ".role-config-launch" / f"{agent}.tty.jsonl"
    original = termios.tcgetattr(0)
    transcript = []
    draft = bytearray()
    pending = b""
    pasting = False

    def record(event, **fields):
        with events.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps({"event": event, **fields}) + "\n")

    def editor_text():
        # tmux paste-buffer's default separator is CR. The native editor
        # interprets CR/CRLF as newlines inside paste, not a submit key.
        return bytes(draft).decode("utf-8").replace("\r\n", "\n").replace("\r", "\n")

    def render():
        # Actual draft/transcript tails, not manufactured message tokens. Keep
        # the editor within Pi's measured max(5, floor(pane_rows * .3)) extent.
        width = max(8, min(os.get_terminal_size(1).columns, 72))
        border = "─" * width
        rows = transcript[-3:] + [f"TEAM_AGENT_FAKE_READY agent={agent}", border]
        rows += editor_text().split("\n")[-4:]
        rows += [border, "gpt-6-luna · max · 0%/272k · 0% · $0.00"]
        os.write(1, ("\x1b[2J\x1b[H" + "\r\n".join(rows)).encode("utf-8"))

    def submit(key):
        payload = editor_text()
        matches = re.findall(r"^\[team-agent-token:([^\]\n]+)\]$", payload, re.M)
        if len(matches) != 1:
            raise RuntimeError("controlled submit requires one standalone message token")
        message_id = matches[0]
        record("enter_received", key=key, message_id=message_id,
               input_sha256=hashlib.sha256(bytes(draft)).hexdigest())
        transcript.extend(payload.split("\n"))
        draft.clear()
        render()
        # The normal submit observer must arm this exact turn before the fake
        # worker can complete it; do not write state or bypass that observer.
        deadline = time.monotonic() + 5
        while True:
            state = json.loads((workspace / ".team/runtime/state.json").read_text())
            owner = os.environ.get("TEAM_AGENT_OWNER_TEAM_ID")
            scoped = state.get("teams", {}).get(owner, state)
            current = scoped.get("agents", {}).get(agent, {}).get("current_turn_message_id")
            if current == message_id:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("controlled submit observer did not arm this message")
            time.sleep(0.01)
        record("turn_observed", message_id=message_id)
        child = subprocess.run(
            [binary, "fake-worker", "--workspace", str(workspace), "--agent-id", agent],
            input=(payload + "\n").encode("utf-8"), capture_output=True, timeout=5,
        )
        record("fake_result", message_id=message_id, exit_code=child.returncode)
        if child.returncode != 0:
            raise RuntimeError("controlled fake-worker failed")

    try:
        tty.setraw(0)
        os.write(1, b"\x1b[?1049h\x1b[?2004h")
        render()
        record("ready")
        while True:
            chunk = os.read(0, 4096)
            if not chunk:
                return
            pending += chunk
            while pending:
                if pasting:
                    end = pending.find(END)
                    if end < 0:
                        # Retain the possible end-marker prefix across reads.
                        keep = min(len(pending), len(END) - 1)
                        draft.extend(pending[:-keep])
                        pending = pending[-keep:]
                        break
                    draft.extend(pending[:end])
                    pending = pending[end + len(END):]
                    pasting = False
                    record("paste_received", input_bytes=len(draft),
                           linefeeds=draft.count(b"\n"), carriage_returns=draft.count(b"\r"),
                           editor_lines=len(editor_text().split("\n")),
                           input_sha256=hashlib.sha256(bytes(draft)).hexdigest())
                    render()
                elif pending.startswith(BEGIN):
                    pending = pending[len(BEGIN):]
                    pasting = True
                elif BEGIN.startswith(pending):
                    break
                else:
                    key, pending = pending[0], pending[1:]
                    if key in (3, 4):
                        return
                    if key in (10, 13):
                        submit(key)
                    elif key == 27:
                        raise RuntimeError("unexpected controlled input escape")
                    else:
                        draft.append(key)
            if not pasting and draft:
                render()
    finally:
        os.write(1, b"\x1b[?2004l\x1b[?1049l")
        termios.tcsetattr(0, termios.TCSANOW, original)


if __name__ == "__main__":
    main()
