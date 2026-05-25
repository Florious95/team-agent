from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.display.ghostty import (
    ghostty_app_exists,
    ghostty_attach_args,
    ghostty_display_session_name,
    ghostty_pids_by_title,
    prepare_ghostty_display_session,
)
from team_agent.display.workspace import open_ghostty_workspace


def open_worker_displays(
    workspace: Path,
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
    display_backend: str = "ghostty_window",
) -> dict[str, dict[str, Any]]:
    if not jobs:
        return {}
    if display_backend == "ghostty_workspace":
        return open_ghostty_workspace(workspace, session_name, jobs, event_log)
    if len(jobs) == 1:
        agent_id, agent = jobs[0]
        return {agent_id: open_ghostty_worker_window(workspace, session_name, agent_id, agent, event_log)}
    results: dict[str, dict[str, Any]] = {}
    max_workers = min(4, len(jobs))
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = {
            executor.submit(open_ghostty_worker_window, workspace, session_name, agent_id, agent, event_log): agent_id
            for agent_id, agent in jobs
        }
        for future in as_completed(futures):
            agent_id = futures[future]
            try:
                results[agent_id] = future.result()
            except Exception as exc:
                display = {
                    "backend": "ghostty_window",
                    "status": "blocked",
                    "reason": "display_open_exception",
                    "error": str(exc),
                    "fallback": "tmux_headless",
                }
                event_log.write("display.ghostty_blocked", agent_id=agent_id, **display)
                results[agent_id] = display
    return results


def open_ghostty_worker_window(
    workspace: Path,
    session_name: str,
    window_name: str,
    agent: dict[str, Any],
    event_log: EventLog,
) -> dict[str, Any]:
    from team_agent.runtime import run_cmd
    _ = workspace
    if not ghostty_app_exists():
        blocker = {
            "backend": "ghostty_window",
            "status": "blocked",
            "reason": "ghostty_app_missing",
            "fallback": "tmux_headless",
        }
        event_log.write("display.ghostty_blocked", agent_id=agent["id"], **blocker)
        return blocker
    title = f"team-agent:{agent['id']}:{agent.get('role', '')}"
    display_session = ghostty_display_session_name(session_name, window_name)
    prepared = prepare_ghostty_display_session(session_name, window_name, display_session)
    if not prepared["ok"]:
        blocker = {
            "backend": "ghostty_window",
            "status": "blocked",
            "reason": prepared["reason"],
            "error": prepared.get("error"),
            "target": f"{session_name}:{window_name}",
            "display_session": display_session,
            "fallback": "tmux_headless",
        }
        event_log.write("display.ghostty_blocked", agent_id=agent["id"], **blocker)
        return blocker
    launch_args = ghostty_attach_args(display_session, title)
    proc = run_cmd(launch_args, timeout=10)
    display = {
        "backend": "ghostty_window",
        "status": "opened" if proc.returncode == 0 else "blocked",
        "title": title,
        "target": f"{session_name}:{window_name}",
        "display_session": display_session,
        "launch_args": launch_args,
        "pid": None,
        "pids": [],
        "tty": None,
        "fallback": "tmux_headless",
        "note": "Ghostty opens a dedicated linked tmux session per worker so each display has an independent active window; runtime injection remains tmux-backed.",
    }
    if proc.returncode != 0:
        display["reason"] = proc.stderr.strip() or proc.stdout.strip() or "open Ghostty.app failed"
    else:
        display["pids"] = ghostty_pids_by_title(title, wait_s=3.0)
        display["pid"] = display["pids"][0] if display["pids"] else None
    event_log.write("display.ghostty_window", agent_id=agent["id"], **display)
    return display
