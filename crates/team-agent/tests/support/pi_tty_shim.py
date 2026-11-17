"""Controlled Pi composer fixture; no provider/model and no synthetic delivery receipt.

The real injection must finish bracketed paste and deliver a separate Enter.
Only then does the controlled worker report the original bytes over real MCP stdio.
"""

import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import termios
import time
import tty

BEGIN = b"\x1b[200~"
END = b"\x1b[201~"


def mcp_input(call):
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "controlled-pi-tty", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, **call},
    ]
    return "".join(json.dumps(request) + "\n" for request in requests)


def mcp_exchange(binary, workspace, agent, call):
    """One real submit session; preserve the actual exit-code receipt."""
    env = os.environ.copy()
    env["TEAM_AGENT_ID"] = agent
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    child = subprocess.run(
        [binary, "mcp-server", "--workspace", str(workspace)],
        input=mcp_input(call),
        capture_output=True, text=True, env=env, timeout=10,
    )
    replies = [json.loads(line) for line in child.stdout.splitlines() if line.strip()]
    return child, next((reply for reply in replies if reply.get("id") == 2), {})


def wait_startup_replies(child, phase):
    """Read complete protocol frames, not child EOF; partial lines stay bounded."""
    deadline = time.monotonic() + 10
    pending = b""
    expected = 1
    os.set_blocking(child.stdout.fileno(), False)
    while True:
        while b"\n" in pending:
            if time.monotonic() >= deadline:
                raise TimeoutError("controlled MCP startup response timed out")
            line, pending = pending.split(b"\n", 1)
            reply = json.loads(line)
            if not isinstance(reply, dict) or reply.get("jsonrpc") != "2.0":
                raise RuntimeError("invalid controlled MCP startup frame")
            if "id" not in reply and isinstance(reply.get("method"), str):
                continue
            if (type(reply.get("id")) is not int or reply["id"] != expected
                    or "error" in reply or not isinstance(reply.get("result"), dict)):
                raise RuntimeError("invalid controlled MCP startup response")
            if expected == 1:
                phase("initialize_reply")
                expected = 2
            else:
                if not isinstance(reply["result"].get("tools"), list):
                    raise RuntimeError("controlled MCP tools/list response has no tools list")
                phase("tools_list_reply")
                return
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([child.stdout], [], [], remaining)[0]:
            raise TimeoutError("controlled MCP startup response timed out")
        chunk = os.read(child.stdout.fileno(), 4096)
        if not chunk:
            raise RuntimeError("controlled MCP closed stdout before tools/list response")
        pending += chunk
        if len(pending) > 1024 * 1024:
            raise RuntimeError("controlled MCP startup frame exceeds 1 MiB")


def stop_mcp(child, phase, graceful):
    killed = False
    try:
        if graceful:
            child.stdin.close()
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
    finally:
        # Signals/EOF/errors must not give a publishing child a grace window
        # in which it can recreate a workspace being removed by the test.
        # This also handles a signal interrupting the normal graceful wait.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGHUP, signal.SIG_IGN)
        if child.poll() is None:
            child.kill()
            killed = True
        child.stdin.close()
        try:
            child.wait(timeout=2)
        finally:
            child.stdout.close()
            phase("child_exit" if child.poll() is not None else "child_unreaped",
                  pid=child.pid, exit_code=child.poll(), killed=killed)


def main():
    binary, workspace, agent = sys.argv[1:]
    workspace = Path(workspace)
    events = workspace / ".role-config-launch" / f"{agent}.tty.jsonl"
    original = termios.tcgetattr(0)
    transcript = []
    draft = bytearray()
    pending = b""
    pasting = False
    startup_child = None
    startup_stderr = None
    spawning_child = False
    termination_signal = None
    graceful_exit = False
    exit_error = None
    phase_start = last_phase = time.monotonic_ns()
    phases = events.with_name(f"{agent}.mcp-phases.jsonl")

    def phase(event, **fields):
        nonlocal last_phase
        now = time.monotonic_ns()
        with phases.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps({"event": event, "monotonic_ns": now,
                                    "wall_time_ns": time.time_ns(),
                                    "elapsed_ms": (now - phase_start) / 1_000_000,
                                    "phase_ms": (now - last_phase) / 1_000_000,
                                    **fields}) + "\n")
        last_phase = now

    def terminate(signum, _frame):
        nonlocal termination_signal
        termination_signal = signum
        # Do not lose the child handle between Popen returning and assignment.
        if not spawning_child:
            raise SystemExit(128 + signum)

    # tmux teardown may signal the shim rather than send a terminal control key.
    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGHUP, terminate)

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
        record("enter_received", key=key, payload=payload,
               input_sha256=hashlib.sha256(bytes(draft)).hexdigest())
        transcript.extend(payload.split("\n"))
        draft.clear()
        render()
        child, reply = mcp_exchange(binary, workspace, agent, {"method": "tools/call", "params": {
            "name": "report_result", "arguments": {"summary": payload, "status": "completed"}}})
        result = reply.get("result", {})
        record("mcp_result", exit_code=child.returncode, response=reply)
        if child.returncode != 0 or "error" in reply or result.get("isError") or not result:
            raise RuntimeError(f"controlled MCP report failed: {reply!r} {child.stderr}")

    try:
        tty.setraw(0)
        os.write(1, b"\x1b[?1049h\x1b[?2004h")
        # Real Pi keeps this session alive. Readiness follows the tools/list
        # response, not EOF after the server's post-response evidence publish.
        # report_result retains its separate real exchange and actual exit code.
        env = os.environ.copy()
        env["TEAM_AGENT_ID"] = agent
        env.pop("TMUX", None)
        env.pop("TMUX_PANE", None)
        startup_stderr = events.with_name(f"{agent}.mcp.stderr.log").open("ab")
        spawning_child = True
        try:
            startup_child = subprocess.Popen(
                [binary, "mcp-server", "--workspace", str(workspace)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=startup_stderr,
                env=env, bufsize=0,
            )
        finally:
            spawning_child = False
        if termination_signal is not None:
            raise SystemExit(128 + termination_signal)
        phase("spawn", pid=startup_child.pid)
        # Fixed startup frames fit in the empty pipe; no application payload.
        startup_child.stdin.write(mcp_input({"method": "tools/list"}).encode())
        wait_startup_replies(startup_child, phase)
        if startup_child.poll() is not None:
            raise RuntimeError("controlled MCP exited before composer readiness")
        render()
        record("ready")
        phase("ready")
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
                        graceful_exit = True
                        return
                    if key in (10, 13):
                        submit(key)
                    elif key == 27:
                        raise RuntimeError("unexpected controlled input escape")
                    else:
                        draft.append(key)
            if not pasting and draft:
                render()
    except BaseException as error:
        exit_error = error
        raise
    finally:
        try:
            if startup_child is not None:
                stop_mcp(startup_child, phase, graceful_exit and termination_signal is None)
            if exit_error is not None:
                phase("shim_exit", error_type=type(exit_error).__name__, error=str(exit_error))
        finally:
            if startup_stderr is not None:
                startup_stderr.close()
            try:
                os.write(1, b"\x1b[?2004l\x1b[?1049l")
            finally:
                termios.tcsetattr(0, termios.TCSANOW, original)


if __name__ == "__main__":
    main()
