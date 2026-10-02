#!/usr/bin/env python3
"""No-LLM raw-PTY diagnostic for tmux submit key spelling; owns one socket/session."""
import json
import os
from pathlib import Path
import select
import shlex
import subprocess
import sys
import tempfile
import time
import tty


def record(path, ready, enhanced):
    tty.setraw(sys.stdin.fileno())
    if enhanced:
        os.write(sys.stdout.fileno(), b"\x1b[>4;2m")
    Path(ready).touch()
    while True:
        if select.select([sys.stdin], [], [], 20)[0]:
            data = os.read(sys.stdin.fileno(), 4096)
            if not data:
                return
            with open(path, "a", encoding="ascii") as output:
                output.write(data.hex() + "\n")
                output.flush()
        else:
            return


def probe():
    print(subprocess.check_output(["tmux", "-V"], text=True).strip(), flush=True)
    rows = []
    with tempfile.TemporaryDirectory(prefix="ta-submit-key-") as tmp:
        root = Path(tmp)
        socket = str(root / "socket")
        session = "submit-key-probe"

        def tmux(*args):
            return subprocess.check_output(["tmux", "-f", "/dev/null", "-S", socket, *args], text=True).strip()

        try:
            tmux("new-session", "-d", "-s", session, "-n", "bootstrap", "sleep 120")
            for mode, fmt in [("off", "csi-u"), ("on", "csi-u"), ("on", "xterm")]:
                tmux("set-option", "-s", "extended-keys", mode)
                tmux("set-option", "-s", "extended-keys-format", fmt)
                capture = root / f"{mode}-{fmt}.hex"
                ready = root / f"{mode}-{fmt}.ready"
                command = shlex.join([sys.executable, str(Path(__file__).resolve()), "--record", str(capture), str(ready), str(mode == "on")])
                pane = tmux("new-window", "-d", "-P", "-F", "#{pane_id}", "-t", session, command)
                deadline = time.monotonic() + 3
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                if not ready.exists():
                    raise RuntimeError("raw-PTY recorder did not become ready")
                # Allow the terminal's actual modifyOtherKeys request to be parsed.
                time.sleep(0.1)
                for key in ["C-m", "Enter", "C-j"]:
                    before = capture.read_text() if capture.exists() else ""
                    tmux("send-keys", "-t", pane, key)
                    deadline = time.monotonic() + 2
                    while time.monotonic() < deadline:
                        after = capture.read_text() if capture.exists() else ""
                        if len(after) > len(before):
                            break
                        time.sleep(0.01)
                    if len(after) <= len(before):
                        raise RuntimeError(f"no stdin bytes for {mode}/{fmt}/{key}")
                    row = {"extended_keys": mode, "format": fmt, "key": key, "hex": "".join(after[len(before):].splitlines())}
                    rows.append(row)
                    print(json.dumps(row), flush=True)
                tmux("kill-pane", "-t", pane)
        finally:
            # Only this exact owned session; never kill a shared server/socket.
            subprocess.run(["tmux", "-S", socket, "kill-session", "-t", session], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        print("OwnedSessionCleanup=done", flush=True)
    return rows


if __name__ == "__main__":
    if sys.argv[1:2] == ["--record"]:
        record(sys.argv[2], sys.argv[3], sys.argv[4] == "True")
    else:
        rows = probe()
        if sys.argv[1:2] == ["--submit-key"]:
            selected = [row for row in rows if row["key"] == sys.argv[2]]
            assert len(selected) == 3, "production submit key must have all three mode observations"
            assert all(row["hex"] == "0d" for row in selected), selected
            print("ProductionSubmitReturn=verified-three-modes", flush=True)
