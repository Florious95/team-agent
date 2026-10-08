#!/usr/bin/env python3
"""Controlled TTY fixture only. This is not Kiro, an agent, or an MCP implementation."""
import argparse
import base64
import json
import os
import re
import hashlib
import sys
import termios
import tty

parser = argparse.ArgumentParser()
parser.add_argument("--mode", choices=["single", "confirm", "confirm-two", "retry", "queue", "startup"], default="single")
parser.add_argument("--events", required=True)
parser.add_argument("--prompt", default="")
parser.add_argument("--server", default="")
args = parser.parse_args()
os.umask(0o077)
log = open(args.events, "x", encoding="utf-8")


def event(kind, **fields):
    log.write(json.dumps({"kind": kind, **fields}, ensure_ascii=False) + "\n")
    log.flush()
    os.fsync(log.fileno())


def surface(name, message="-"):
    # Erase the controlled screen, so fixtures describe the current UI, not history.
    sys.stdout.write("\x1b[2J\x1b[H" + name + "|" + message + "\r\n")
    sys.stdout.flush()


fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
try:
    tty.setraw(fd)
    sys.stdout.write("\x1b[?2004h")
    sys.stdout.flush()
    event("started", pid=os.getpid(), cwd=os.getcwd(), fixture_env=os.environ.get("CONTRACT_FIXTURE_ENV"), old_scope_present="CONTRACT_OLD_SCOPE" in os.environ, inherited_team_identity_present=any(key.startswith("TEAM_AGENT_") for key in os.environ))
    surface("WARNING" if args.mode == "startup" else "READY")
    pending = b""
    payload = b""
    in_paste = False
    message = "-"
    keys = 0
    while True:
        chunk = os.read(fd, 4096)
        if not chunk:
            break
        pending += chunk
        while pending:
            if in_paste:
                end = pending.find(b"\x1b[201~")
                if end < 0:
                    # Keep only enough tail to recognize a split paste terminator.
                    keep = min(len(pending), 5)
                    payload += pending[:-keep] if keep else pending
                    pending = pending[-keep:] if keep else b""
                    break
                payload += pending[:end]
                pending = pending[end + 6:]
                in_paste = False
                matches = re.findall(rb"\[team-agent-token:([A-Za-z0-9_-]+)\]", payload)
                message = matches[-1].decode("ascii") if matches else "-"
                event("paste", payload_b64=base64.b64encode(payload).decode("ascii"), payload_sha256=hashlib.sha256(payload).hexdigest(), payload_bytes=len(payload), message=message)
                surface("PASTED", message)
                continue
            if pending.startswith(b"\x1b[200~"):
                pending = pending[6:]
                payload = b""
                in_paste = True
                keys = 0
                continue
            if b"\x1b[200~".startswith(pending):
                break
            byte, pending = pending[0], pending[1:]
            if byte in (10, 13):
                keys += 1
                event("key", key="Enter" if byte == 13 else "LF", ordinal=keys)
                if args.mode == "startup" and not payload:
                    surface("READY")
                    event("startup_ack")
                elif args.mode in ("confirm", "confirm-two") and keys == 1:
                    surface("CONFIRM1", message)
                elif args.mode == "confirm-two" and keys == 2:
                    surface("CONFIRM2", message)
                elif args.mode == "retry" and keys <= 3:
                    surface("RETRY", message)
                elif args.mode == "queue" and keys <= 2:
                    surface("QUEUE", message)
                else:
                    event("native_accepted", message=message)
                    surface("ACCEPTED", message)
            else:
                event("unexpected_byte", byte=byte)
finally:
    try:
        termios.tcsetattr(fd, termios.TCSANOW, old)
    except (OSError, termios.error):
        pass
    log.close()
