from __future__ import annotations

import hashlib
import re
import time
from pathlib import Path
from typing import Any


def ghostty_command() -> str | None:
    from team_agent.runtime import shutil_which
    return shutil_which("ghostty") or (
        "/Applications/Ghostty.app/Contents/MacOS/ghostty"
        if Path("/Applications/Ghostty.app/Contents/MacOS/ghostty").exists()
        else None
    )


def ghostty_app_exists() -> bool:
    return Path("/Applications/Ghostty.app").exists()


def ghostty_pids_by_title(title: str, wait_s: float = 0.0) -> list[int]:
    from team_agent.runtime import run_cmd
    deadline = time.monotonic() + max(wait_s, 0.0)
    while True:
        pgrep = run_cmd(["pgrep", "-f", f"--title={title}"], timeout=5)
        if pgrep.returncode == 0:
            pids = [int(pid) for pid in pgrep.stdout.split() if pid.isdigit()]
            if pids:
                return pids
        if time.monotonic() >= deadline:
            return []
        time.sleep(0.2)


def ghostty_attach_args(display_session: str, title: str) -> list[str]:
    return [
        "open",
        "-na",
        "Ghostty.app",
        "--args",
        f"--title={title}",
        "-e",
        "tmux",
        "attach-session",
        "-t",
        display_session,
    ]


def ghostty_display_session_name(session_name: str, window_name: str) -> str:
    raw = f"{session_name}:{window_name}"
    digest = hashlib.sha1(raw.encode("utf-8")).hexdigest()[:8]
    safe_session = re.sub(r"[^A-Za-z0-9_.-]", "_", session_name)[:80].strip("._-") or "team"
    safe_window = re.sub(r"[^A-Za-z0-9_.-]", "_", window_name)[:40].strip("._-") or "agent"
    return f"{safe_session}__display__{safe_window}__{digest}"


def prepare_ghostty_display_session(session_name: str, window_name: str, display_session: str) -> dict[str, Any]:
    from team_agent.runtime import _tmux_session_exists, _tmux_window_exists, run_cmd
    if not _tmux_window_exists(session_name, window_name):
        return {"ok": False, "reason": "tmux_target_missing"}
    if display_session == session_name:
        return {"ok": False, "reason": "display_session_conflicts_with_base_session"}
    if _tmux_session_exists(display_session):
        proc = run_cmd(["tmux", "kill-session", "-t", display_session], timeout=10)
        if proc.returncode != 0:
            return {"ok": False, "reason": "display_session_cleanup_failed", "error": proc.stderr.strip()}
    proc = run_cmd(["tmux", "new-session", "-d", "-t", session_name, "-s", display_session], timeout=10)
    if proc.returncode != 0:
        return {"ok": False, "reason": "display_session_create_failed", "error": proc.stderr.strip()}
    proc = run_cmd(["tmux", "select-window", "-t", f"{display_session}:{window_name}"], timeout=10)
    if proc.returncode != 0:
        run_cmd(["tmux", "kill-session", "-t", display_session], timeout=10)
        return {"ok": False, "reason": "display_session_select_window_failed", "error": proc.stderr.strip()}
    return {"ok": True, "display_session": display_session}
