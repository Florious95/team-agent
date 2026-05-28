from __future__ import annotations

import os
import platform as platform_module
from pathlib import Path
from typing import Any

from team_agent.display.ghostty import ghostty_display_session_name
from team_agent.display.tiling import (
    display_pane_title,
    prepare_tmux_attached_panes,
    set_tmux_display_pane_title,
    team_scoped_display_window_name,
)
from team_agent.display.workspace import (
    kill_ghostty_workspace_linked_sessions,
)
from team_agent.events import EventLog


ADAPTIVE_BLOCK_REASONS = {
    "leader_not_in_tmux",
    "split_failed",
    "window_create_failed",
    "worker_session_missing",
    "not_implemented_this_platform",
    "aggregator_rebuild_failed",
}


def probe_display_capabilities(
    env: dict[str, str] | None = None,
    platform: str | None = None,
    tmux: Any | None = None,
) -> dict[str, Any]:
    env_map = dict({} if env is None else env)
    platform_name = _display_platform(platform, env_map)
    unsupported = platform_name.startswith("win") or platform_name in {"windows", "wsl"}
    tmux_info = _current_tmux_info(tmux, env_map) if tmux is not None else {}
    leader_session = tmux_info.get("leader_session") or env_map.get("TEAM_AGENT_LEADER_SESSION_NAME")
    leader_pane = tmux_info.get("leader_pane") or env_map.get("TMUX_PANE") or env_map.get("TEAM_AGENT_LEADER_PANE_ID")
    in_tmux = bool(env_map.get("TMUX") or env_map.get("TMUX_PANE") or tmux_info.get("ok")) and not unsupported
    caps = {
        "tmux_append_windows": bool(in_tmux and not unsupported),
        "adaptive_display": bool(in_tmux and not unsupported),
    }
    return {
        "in_tmux": in_tmux,
        "platform": platform_name,
        "leader_session": leader_session,
        "leader_pane": leader_pane,
        "caps": caps,
        "adaptive_status": "not_implemented_this_platform" if unsupported else ("available" if in_tmux else "leader_not_in_tmux"),
        "reason": "not_implemented_this_platform" if unsupported else (None if in_tmux else "leader_not_in_tmux"),
    }


def open_adaptive_display(
    workspace: Path,
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
    capability_probe: dict[str, Any] | None = None,
) -> dict[str, dict[str, Any]]:
    from team_agent.runtime import run_cmd
    _ = workspace
    probe = capability_probe or probe_display_capabilities(env=dict(os.environ), tmux=run_cmd)
    if probe.get("reason") == "not_implemented_this_platform":
        return adaptive_blocked(jobs, event_log, "not_implemented_this_platform", platform=probe.get("platform"))
    leader_session = str(probe.get("leader_session") or _state_leader_session(workspace) or "")
    if not probe.get("in_tmux") or not leader_session:
        return adaptive_blocked(jobs, event_log, "leader_not_in_tmux", platform=probe.get("platform"))

    linked_results = prepare_adaptive_linked_sessions(session_name, jobs)
    displays: dict[str, dict[str, Any]] = {}
    linked_jobs: list[tuple[str, dict[str, Any], str]] = []
    for agent_id, agent in jobs:
        linked = linked_results.get(agent_id, {})
        linked_session = linked.get("linked_session") or ghostty_display_session_name(session_name, agent_id)
        if linked.get("ok"):
            linked_jobs.append((agent_id, agent, linked_session))
            continue
        displays.update(
            adaptive_blocked(
                [(agent_id, agent)],
                event_log,
                "worker_session_missing",
                leader_session=leader_session,
                linked_sessions={agent_id: linked_session},
                error=linked.get("error") or linked.get("reason"),
                target=f"{session_name}:{agent_id}",
            )
        )
    if displays:
        kill_ghostty_workspace_linked_sessions([linked_session for _agent_id, _agent, linked_session in linked_jobs])
        return adaptive_blocked(
            jobs,
            event_log,
            "worker_session_missing",
            leader_session=leader_session,
            linked_sessions={agent_id: linked.get("linked_session") for agent_id, linked in linked_results.items()},
            error=next((display.get("error") for display in displays.values() if display.get("error")), None),
        )
    if not linked_jobs:
        return displays

    close_adaptive_windows(leader_session, session_name, event_log)
    prepared = prepare_adaptive_windows(leader_session, session_name, linked_jobs)
    if not prepared["ok"]:
        close_adaptive_windows(leader_session, session_name, event_log)
        kill_ghostty_workspace_linked_sessions([linked_session for _agent_id, _agent, linked_session in linked_jobs])
        displays.update(
            adaptive_blocked(
                [(agent_id, agent) for agent_id, agent, _linked_session in linked_jobs],
                event_log,
                prepared["reason"],
                leader_session=leader_session,
                linked_sessions={agent_id: linked_session for agent_id, _agent, linked_session in linked_jobs},
                error=prepared.get("error"),
                target=prepared.get("target"),
            )
        )
        return displays

    panes = {pane["agent_id"]: pane for pane in prepared["panes"]}
    for agent_id, agent, linked_session in linked_jobs:
        pane = panes.get(agent_id, {})
        display = {
            "backend": "adaptive",
            "status": "opened",
            "window": pane.get("window_name"),
            "workspace_window": pane.get("window_name"),
            "pane_id": pane.get("pane_id"),
            "pane_title": pane.get("title") or display_pane_title(agent),
            "target": f"{session_name}:{agent_id}",
            "target_worker_session": f"{session_name}:{agent_id}",
            "linked_session": linked_session,
            "leader_session": leader_session,
            "display_session": leader_session,
            "fallback": "tmux_headless",
            "note": "Adaptive display appends tagged tmux windows to the leader session; each pane attaches to a linked worker session.",
        }
        event_log.write("display.adaptive_opened", agent_id=agent_id, worker_id=agent_id, **display)
        displays[agent_id] = display
    return displays


def prepare_adaptive_windows(
    leader_session: str,
    session_name: str,
    linked_jobs: list[tuple[str, dict[str, Any], str]],
) -> dict[str, Any]:
    prepared = prepare_tmux_attached_panes(
        leader_session,
        linked_jobs,
        window_name_for_index=lambda index: team_scoped_display_window_name(session_name, index),
        create_first_as_session=False,
        reason_map={
            "create_window": "window_create_failed",
            "title": "aggregator_rebuild_failed",
            "remain": "aggregator_rebuild_failed",
            "split": "split_failed",
            "layout": "aggregator_rebuild_failed",
        },
        stderr_reason_allowlist=ADAPTIVE_BLOCK_REASONS,
    )
    if prepared.get("ok"):
        prepared["leader_session"] = leader_session
    return prepared


def prepare_adaptive_linked_sessions(
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
) -> dict[str, dict[str, Any]]:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    results: dict[str, dict[str, Any]] = {}
    for agent_id, _agent in jobs:
        linked_session = ghostty_display_session_name(session_name, agent_id)
        if linked_session == session_name:
            results[agent_id] = {"ok": False, "reason": "worker_session_missing", "linked_session": linked_session}
            continue
        if _tmux_session_exists(linked_session):
            cleanup = run_cmd(["tmux", "kill-session", "-t", linked_session], timeout=10)
            if cleanup.returncode != 0:
                results[agent_id] = {
                    "ok": False,
                    "reason": "worker_session_missing",
                    "error": cleanup.stderr.strip(),
                    "linked_session": linked_session,
                }
                continue
        created = run_cmd(["tmux", "new-session", "-d", "-t", session_name, "-s", linked_session], timeout=10)
        if created.returncode != 0:
            results[agent_id] = {
                "ok": False,
                "reason": "worker_session_missing",
                "error": created.stderr.strip() or created.stdout.strip(),
                "linked_session": linked_session,
            }
            continue
        selected = run_cmd(["tmux", "select-window", "-t", f"{linked_session}:{agent_id}"], timeout=10)
        if selected.returncode != 0:
            run_cmd(["tmux", "kill-session", "-t", linked_session], timeout=10)
            results[agent_id] = {
                "ok": False,
                "reason": "worker_session_missing",
                "error": selected.stderr.strip() or selected.stdout.strip(),
                "linked_session": linked_session,
            }
            continue
        results[agent_id] = {"ok": True, "linked_session": linked_session}
    return results


def adaptive_blocked(
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
    reason: str,
    leader_session: str | None = None,
    linked_sessions: dict[str, str] | None = None,
    error: str | None = None,
    target: str | None = None,
    platform: str | None = None,
) -> dict[str, dict[str, Any]]:
    reason = reason if reason in ADAPTIVE_BLOCK_REASONS else "aggregator_rebuild_failed"
    displays: dict[str, dict[str, Any]] = {}
    for agent_id, _agent in jobs:
        display = {
            "backend": "adaptive",
            "status": "blocked",
            "reason": reason,
            "error": error,
            "target": target or f"{agent_id}",
            "target_worker_session": target or f"{agent_id}",
            "leader_session": leader_session,
            "linked_session": (linked_sessions or {}).get(agent_id),
            "display_session": leader_session,
            "fallback": "tmux_headless",
            "hint": "Start the leader inside tmux to enable adaptive team display." if reason == "leader_not_in_tmux" else None,
            "platform": platform,
        }
        event_log.write("display.adaptive_blocked", agent_id=agent_id, worker_id=agent_id, **display)
        displays[agent_id] = display
    return displays


def close_adaptive_display(state: dict[str, Any], event_log: EventLog) -> None:
    displays = [
        (agent_id, agent_state.get("display") or {})
        for agent_id, agent_state in state.get("agents", {}).items()
        if (agent_state.get("display") or {}).get("backend") == "adaptive"
    ]
    if not displays:
        return
    killed_windows: list[str] = []
    linked_sessions: list[str] = []
    for _agent_id, display in displays:
        linked = display.get("linked_session")
        if linked:
            linked_sessions.append(str(linked))
    seen_targets: set[str] = set()
    for _agent_id, display in displays:
        leader_session = str(display.get("leader_session") or "")
        window_name = str(display.get("workspace_window") or display.get("window") or "")
        if not leader_session or not window_name:
            continue
        target = f"{leader_session}:{window_name}"
        if target in seen_targets:
            continue
        seen_targets.add(target)
        if kill_adaptive_window(target):
            killed_windows.append(target)
    linked_closed = kill_ghostty_workspace_linked_sessions(linked_sessions)
    event_log.write("display.adaptive_closed", windows=killed_windows, linked_sessions=linked_closed)


def close_adaptive_windows(leader_session: str, session_name: str, event_log: EventLog | None = None) -> list[str]:
    from team_agent.runtime import run_cmd
    prefix = f"team-agent:{session_name}:overview"
    proc = run_cmd(["tmux", "list-windows", "-t", leader_session, "-F", "#{window_name}"], timeout=10)
    if proc.returncode != 0:
        return []
    killed: list[str] = []
    for window_name in proc.stdout.splitlines():
        if window_name != prefix and not window_name.startswith(f"{prefix}-"):
            continue
        target = f"{leader_session}:{window_name}"
        if kill_adaptive_window(target):
            killed.append(target)
    if event_log is not None and killed:
        event_log.write("display.adaptive_stale_windows_closed", leader_session=leader_session, windows=killed)
    return killed


def kill_adaptive_window(target: str) -> bool:
    from team_agent.runtime import run_cmd
    proc = run_cmd(["tmux", "kill-window", "-t", target], timeout=10)
    return proc.returncode == 0


def set_adaptive_pane_title(pane_id: str, title: str) -> dict[str, Any]:
    return set_tmux_display_pane_title(pane_id, title, "aggregator_rebuild_failed")


def _display_platform(value: str | None, env: dict[str, str]) -> str:
    if value:
        return value.lower()
    if env.get("WSL_DISTRO_NAME") or env.get("WSL_INTEROP"):
        return "wsl"
    return platform_module.system().lower()


def _current_tmux_info(tmux: Any, env: dict[str, str]) -> dict[str, Any]:
    pane = env.get("TMUX_PANE") or ""
    commands: list[list[str]] = []
    if pane:
        commands.insert(0, ["tmux", "display-message", "-p", "-t", pane, "-F", "#{session_name}\t#{pane_id}"])
        commands.insert(1, ["tmux", "display-message", "-p", "-t", pane, "-F", "#{session_name}"])
        commands.insert(2, ["tmux", "display-message", "-p", "-t", pane, "#{session_name}"])
    if env.get("TMUX"):
        commands.extend(
            [
                ["tmux", "display-message", "-p", "-F", "#{session_name}\t#{pane_id}"],
                ["tmux", "display-message", "-p", "-F", "#{session_name}"],
                ["tmux", "display-message", "-p", "#{session_name}\t#{pane_id}"],
                ["tmux", "display-message", "-p", "#{session_name}"],
            ]
        )
    for command in commands:
        proc = _call_tmux(tmux, command)
        parsed = _parse_tmux_session_pane(proc)
        if parsed:
            return parsed
    if pane:
        listed = _leader_from_tmux_panes(tmux, pane)
        if listed:
            return listed
        session = _first_tmux_session(tmux)
        if session:
            return {"ok": True, "leader_session": session, "leader_pane": pane}
    return {"ok": False}


def _call_tmux(tmux: Any, args: list[str]) -> Any | None:
    try:
        if callable(tmux):
            try:
                return tmux(args, timeout=5)
            except TypeError:
                return tmux(args)
        if hasattr(tmux, "run_cmd"):
            return tmux.run_cmd(args)
    except Exception:
        return None
    return None


def _parse_tmux_session_pane(proc: Any | None) -> dict[str, Any] | None:
    if not proc or getattr(proc, "returncode", 1) != 0:
        return None
    parts = str(getattr(proc, "stdout", "")).strip().split("\t")
    if len(parts) >= 2 and parts[0].startswith("%") and parts[1]:
        return {"ok": True, "leader_session": parts[1], "leader_pane": parts[0]}
    if len(parts) >= 2 and parts[0]:
        return {"ok": True, "leader_session": parts[0], "leader_pane": parts[1]}
    if len(parts) == 1 and parts[0] and not parts[0].startswith("%"):
        return {"ok": True, "leader_session": parts[0], "leader_pane": None}
    return None


def _leader_from_tmux_panes(tmux: Any, pane: str) -> dict[str, Any] | None:
    proc = _call_tmux(
        tmux,
        [
            "tmux",
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_name}\t#{pane_current_command}\t#{pane_active}",
        ],
    )
    if not proc or getattr(proc, "returncode", 1) != 0:
        return None
    rows = [line.split("\t") for line in str(getattr(proc, "stdout", "")).splitlines() if line.strip()]
    if pane:
        for row in rows:
            if len(row) >= 2 and row[0] == pane:
                return {"ok": True, "leader_session": row[1], "leader_pane": row[0]}
    for row in rows:
        if len(row) >= 3 and _leader_shaped_command(row[2]):
            return {"ok": True, "leader_session": row[1], "leader_pane": row[0]}
    if rows and len(rows[0]) >= 2:
        return {"ok": True, "leader_session": rows[0][1], "leader_pane": rows[0][0]}
    return None


def _leader_shaped_command(command: str) -> bool:
    lowered = command.lower()
    return any(token in lowered for token in ("claude", "codex", "fake"))


def _first_tmux_session(tmux: Any) -> str | None:
    for command in (
        ["tmux", "list-clients", "-F", "#{client_session}"],
        ["tmux", "list-sessions", "-F", "#{session_name}"],
    ):
        proc = _call_tmux(tmux, command)
        if not proc or getattr(proc, "returncode", 1) != 0:
            continue
        for line in str(getattr(proc, "stdout", "")).splitlines():
            if line.strip():
                return line.strip()
    return None


def _state_leader_session(workspace: Path) -> str | None:
    try:
        from team_agent.state import load_runtime_state
        state = load_runtime_state(workspace)
    except Exception:
        return None
    receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    session_name = receiver.get("session_name")
    return str(session_name) if session_name else None
