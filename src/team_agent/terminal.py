from __future__ import annotations

import shutil
import subprocess
from typing import Callable


RunCommand = Callable[[list[str], int], subprocess.CompletedProcess[str]]
SessionExists = Callable[[str | None], bool]


def run_cmd(args: list[str], timeout: int = 20) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, text=True, capture_output=True, timeout=timeout, check=False)


def shutil_which(command: str) -> str | None:
    return shutil.which(command)


def tmux_session_exists(session_name: str | None, *, run: RunCommand = run_cmd) -> bool:
    if not session_name:
        return False
    proc = run(["tmux", "has-session", "-t", session_name], timeout=5)
    return proc.returncode == 0


def tmux_window_exists(session_name: str | None, window: str | None, *, run: RunCommand = run_cmd) -> bool:
    if not session_name or not window:
        return False
    proc = run(["tmux", "list-windows", "-t", session_name, "-F", "#{window_name}"], timeout=5)
    if proc.returncode != 0:
        return False
    return window in proc.stdout.splitlines()


def tmux_start_command_for_agent_window(
    session_name: str,
    window_name: str,
    command: str,
    *,
    session_exists: SessionExists = tmux_session_exists,
) -> tuple[list[str], str]:
    if session_exists(session_name):
        return ["tmux", "new-window", "-t", session_name, "-n", window_name, "sh", "-lc", command], "new-window"
    return ["tmux", "new-session", "-d", "-s", session_name, "-n", window_name, "sh", "-lc", command], "new-session"


def tmux_stdout_last_line(stdout: str) -> str | None:
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    return lines[-1] if lines else None


def tmux_truthy(value: str) -> int:
    try:
        return 1 if int(value) > 0 else 0
    except (TypeError, ValueError):
        return 1 if value and value != "0" else 0
