from __future__ import annotations

import hashlib
import re
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any

from team_agent.events import EventLog
from team_agent.display.ghostty import (
    ghostty_app_exists,
    ghostty_attach_args,
    ghostty_display_session_name,
    ghostty_pids_by_title,
    prepare_ghostty_display_session,
)
from team_agent.display.tiling import (
    DISPLAY_PANES_PER_WINDOW,
    display_pane_title,
    display_window_name,
    prepare_tmux_attached_panes,
    set_tmux_display_pane_title,
    tmux_attach_pane_command,
    tmux_stdout_last_line as _tmux_stdout_last_line,
)


GHOSTTY_WORKSPACE_PANES_PER_WINDOW = DISPLAY_PANES_PER_WINDOW


def open_ghostty_workspace(
    workspace,
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
) -> dict[str, dict[str, Any]]:
    from team_agent.runtime import run_cmd
    _ = workspace
    if not ghostty_app_exists():
        return ghostty_workspace_blocked(jobs, event_log, "ghostty_app_missing")
    aggregator_session = ghostty_workspace_aggregator_name(session_name)
    linked_results = prepare_ghostty_workspace_linked_sessions(session_name, jobs)
    displays: dict[str, dict[str, Any]] = {}
    linked_jobs: list[tuple[str, dict[str, Any], str]] = []
    for agent_id, agent in jobs:
        linked = linked_results.get(agent_id, {})
        linked_session = linked.get("linked_session") or ghostty_display_session_name(session_name, agent_id)
        if linked.get("ok"):
            linked_jobs.append((agent_id, agent, linked_session))
            continue
        displays.update(
            ghostty_workspace_blocked(
                [(agent_id, agent)],
                event_log,
                linked.get("reason", "display_session_create_failed"),
                aggregator_session=aggregator_session,
                linked_sessions={agent_id: linked_session},
                error=linked.get("error"),
                target=f"{session_name}:{agent_id}",
            )
        )
    if not linked_jobs:
        return displays
    prepared = prepare_ghostty_workspace_aggregator(aggregator_session, linked_jobs)
    if not prepared["ok"]:
        kill_ghostty_workspace_linked_sessions([linked_session for _agent_id, _agent, linked_session in linked_jobs])
        displays.update(
            ghostty_workspace_blocked(
                [(agent_id, agent) for agent_id, agent, _linked_session in linked_jobs],
                event_log,
                prepared["reason"],
                aggregator_session=aggregator_session,
                linked_sessions={agent_id: linked_session for agent_id, _agent, linked_session in linked_jobs},
                error=prepared.get("error"),
                target=prepared.get("target"),
            )
        )
        return displays
    title = f"team-agent:{session_name}:workspace"
    launch_args = ghostty_attach_args(aggregator_session, title)
    proc = run_cmd(launch_args, timeout=10)
    if proc.returncode != 0:
        run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        kill_ghostty_workspace_linked_sessions([linked_session for _agent_id, _agent, linked_session in linked_jobs])
        displays.update(
            ghostty_workspace_blocked(
                [(agent_id, agent) for agent_id, agent, _linked_session in linked_jobs],
                event_log,
                "open Ghostty.app failed",
                aggregator_session=aggregator_session,
                linked_sessions={agent_id: linked_session for agent_id, _agent, linked_session in linked_jobs},
                error=proc.stderr.strip() or proc.stdout.strip(),
            )
        )
        return displays
    pids = ghostty_pids_by_title(title, wait_s=3.0)
    panes = {pane["agent_id"]: pane for pane in prepared["panes"]}
    for agent_id, agent, linked_session in linked_jobs:
        pane = panes.get(agent_id, {})
        display = {
            "backend": "ghostty_workspace",
            "status": "opened",
            "title": title,
            "pane_title": pane.get("title") or ghostty_workspace_pane_title(agent),
            "target": f"{session_name}:{agent_id}",
            "linked_session": linked_session,
            "aggregator_session": aggregator_session,
            "display_session": aggregator_session,
            "workspace_window": pane.get("window_name"),
            "pane_id": pane.get("pane_id"),
            "launch_args": launch_args,
            "pid": pids[0] if pids else None,
            "pids": pids,
            "tty": None,
            "fallback": "tmux_headless",
            "note": "Ghostty opens one aggregator tmux session; each pane attaches to a distinct linked session pinned to one base worker window, so runtime injection remains session:agent_id addressed.",
        }
        event_log.write("display.ghostty_workspace", agent_id=agent_id, **display)
        displays[agent_id] = display
    return displays


def ghostty_workspace_blocked(
    jobs: list[tuple[str, dict[str, Any]]],
    event_log: EventLog,
    reason: str,
    aggregator_session: str | None = None,
    linked_sessions: dict[str, str] | None = None,
    error: str | None = None,
    target: str | None = None,
) -> dict[str, dict[str, Any]]:
    displays: dict[str, dict[str, Any]] = {}
    for agent_id, _agent in jobs:
        linked_session = (linked_sessions or {}).get(agent_id)
        display = {
            "backend": "ghostty_workspace",
            "status": "blocked",
            "reason": reason,
            "error": error,
            "target": target or f"{agent_id}",
            "linked_session": linked_session,
            "aggregator_session": aggregator_session,
            "display_session": aggregator_session,
            "fallback": "tmux_headless",
        }
        event_log.write("display.ghostty_workspace_blocked", agent_id=agent_id, **display)
        displays[agent_id] = display
    return displays


def ghostty_workspace_aggregator_name(session_name: str) -> str:
    raw = f"{session_name}:workspace"
    digest = hashlib.sha1(raw.encode("utf-8")).hexdigest()[:8]
    safe_session = re.sub(r"[^A-Za-z0-9_.-]", "_", session_name)[:80].strip("._-") or "team"
    return f"{safe_session}__display__workspace__{digest}"


def ghostty_workspace_window_name(index: int) -> str:
    return display_window_name(index)


def ghostty_workspace_pane_command(linked_session: str) -> str:
    return tmux_attach_pane_command(linked_session)


def ghostty_workspace_pane_title(agent: dict[str, Any]) -> str:
    return display_pane_title(agent)


def prepare_ghostty_workspace_linked_sessions(
    session_name: str,
    jobs: list[tuple[str, dict[str, Any]]],
) -> dict[str, dict[str, Any]]:
    def prepare(agent_id: str) -> dict[str, Any]:
        linked_session = ghostty_display_session_name(session_name, agent_id)
        result = prepare_ghostty_display_session(session_name, agent_id, linked_session)
        result["linked_session"] = linked_session
        return result

    if len(jobs) == 1:
        agent_id, _agent = jobs[0]
        return {agent_id: prepare(agent_id)}
    results: dict[str, dict[str, Any]] = {}
    max_workers = min(4, len(jobs))
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = {executor.submit(prepare, agent_id): agent_id for agent_id, _agent in jobs}
        for future in as_completed(futures):
            agent_id = futures[future]
            try:
                results[agent_id] = future.result()
            except Exception as exc:
                results[agent_id] = {
                    "ok": False,
                    "reason": "display_session_create_exception",
                    "error": str(exc),
                    "linked_session": ghostty_display_session_name(session_name, agent_id),
                }
    return results


def prepare_ghostty_workspace_aggregator(
    aggregator_session: str,
    linked_jobs: list[tuple[str, dict[str, Any], str]],
) -> dict[str, Any]:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    if _tmux_session_exists(aggregator_session):
        proc = run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        if proc.returncode != 0:
            return {"ok": False, "reason": "display_session_cleanup_failed", "error": proc.stderr.strip()}
    prepared = prepare_tmux_attached_panes(
        aggregator_session,
        linked_jobs,
        window_name_for_index=ghostty_workspace_window_name,
        create_first_as_session=True,
        panes_per_window=GHOSTTY_WORKSPACE_PANES_PER_WINDOW,
        cleanup_session=aggregator_session,
        enable_mouse=True,
        select_first_window=True,
        reason_map={
            "create_session": "display_session_create_failed",
            "create_window": "display_session_window_create_failed",
            "title": "display_session_pane_title_failed",
            "remain": "display_session_remain_on_exit_failed",
            "split": "display_session_split_failed",
            "layout": "display_session_layout_failed",
            "mouse": "display_session_mouse_failed",
        },
    )
    if prepared.get("ok"):
        prepared["aggregator_session"] = aggregator_session
    return prepared


def set_ghostty_workspace_pane_title(pane_id: str, title: str) -> dict[str, Any]:
    return set_tmux_display_pane_title(pane_id, title, "display_session_pane_title_failed")


def open_ghostty_workspace_agent_display(
    session_name: str,
    agent_id: str,
    agent: dict[str, Any],
    previous_display: dict[str, Any],
    event_log: EventLog,
) -> dict[str, Any]:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    if not ghostty_app_exists():
        return ghostty_workspace_blocked(
            [(agent_id, agent)],
            event_log,
            "ghostty_app_missing",
            aggregator_session=ghostty_workspace_aggregator_name(session_name),
            linked_sessions={agent_id: ghostty_display_session_name(session_name, agent_id)},
            target=f"{session_name}:{agent_id}",
        )[agent_id]
    aggregator_session = str(
        previous_display.get("aggregator_session")
        or previous_display.get("display_session")
        or ghostty_workspace_aggregator_name(session_name)
    )
    linked_session = ghostty_display_session_name(session_name, agent_id)
    prepared = prepare_ghostty_display_session(session_name, agent_id, linked_session)
    if not prepared["ok"]:
        return ghostty_workspace_blocked(
            [(agent_id, agent)],
            event_log,
            prepared["reason"],
            aggregator_session=aggregator_session,
            linked_sessions={agent_id: linked_session},
            error=prepared.get("error"),
            target=f"{session_name}:{agent_id}",
        )[agent_id]
    if not _tmux_session_exists(aggregator_session):
        return ghostty_workspace_partial_update_display(
            session_name,
            agent_id,
            agent,
            event_log,
            reason="aggregator_session_missing",
            note="pane refresh requires full team restart",
        )

    pane_title = ghostty_workspace_pane_title(agent)
    command = ghostty_workspace_pane_command(linked_session)
    pane_id = str(previous_display.get("pane_id") or "")
    workspace_window = str(previous_display.get("workspace_window") or ghostty_workspace_window_name(0))
    refreshed = False
    if pane_id:
        proc = run_cmd(["tmux", "respawn-pane", "-k", "-t", pane_id, command], timeout=10)
        refreshed = proc.returncode == 0
    if not refreshed:
        proc = run_cmd(
            [
                "tmux",
                "split-window",
                "-t",
                f"{aggregator_session}:{workspace_window}",
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
                command,
            ],
            timeout=10,
        )
        if proc.returncode != 0:
            return ghostty_workspace_partial_update_display(
                session_name,
                agent_id,
                agent,
                event_log,
                reason="aggregator_pane_refresh_failed",
                note=proc.stderr.strip() or "pane refresh requires full team restart",
            )
        pane_id = _tmux_stdout_last_line(proc.stdout) or pane_id
    title_result = set_ghostty_workspace_pane_title(pane_id, pane_title)
    if not title_result["ok"]:
        return ghostty_workspace_partial_update_display(
            session_name,
            agent_id,
            agent,
            event_log,
            reason=title_result["reason"],
            note=title_result.get("error") or "pane refresh requires full team restart",
        )
    run_cmd(["tmux", "select-layout", "-t", f"{aggregator_session}:{workspace_window}", "even-horizontal"], timeout=10)
    title = str(previous_display.get("title") or f"team-agent:{session_name}:workspace")
    pids = [int(pid) for pid in previous_display.get("pids", []) if str(pid).isdigit()]
    display = {
        "backend": "ghostty_workspace",
        "status": "opened",
        "title": title,
        "pane_title": pane_title,
        "target": f"{session_name}:{agent_id}",
        "linked_session": linked_session,
        "aggregator_session": aggregator_session,
        "display_session": aggregator_session,
        "workspace_window": workspace_window,
        "pane_id": pane_id,
        "pid": pids[0] if pids else None,
        "pids": pids,
        "tty": None,
        "fallback": "tmux_headless",
        "note": "Refreshed this worker's Ghostty workspace pane by respawning it against a distinct linked session.",
    }
    event_log.write("display.ghostty_workspace", agent_id=agent_id, **display)
    return display


def ghostty_workspace_partial_update_display(
    session_name: str,
    agent_id: str,
    agent: dict[str, Any],
    event_log: EventLog,
    reason: str = "partial_update_requires_team_restart",
    note: str = "pane refresh requires full team restart",
) -> dict[str, Any]:
    aggregator_session = ghostty_workspace_aggregator_name(session_name)
    display = {
        "backend": "ghostty_workspace",
        "status": "blocked",
        "reason": reason,
        "target": f"{session_name}:{agent_id}",
        "linked_session": ghostty_display_session_name(session_name, agent_id),
        "aggregator_session": aggregator_session,
        "display_session": aggregator_session,
        "pane_title": ghostty_workspace_pane_title(agent),
        "fallback": "tmux_headless",
        "note": note,
        "action": "restart the team to rebuild the Ghostty workspace layout",
    }
    event_log.write("display.ghostty_workspace_partial_update", agent_id=agent_id, **display)
    return display


def kill_ghostty_workspace_linked_sessions(linked_sessions: list[str]) -> list[str]:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    killed: list[str] = []
    for linked_session in dict.fromkeys(linked_sessions):
        if _tmux_session_exists(linked_session):
            proc = run_cmd(["tmux", "kill-session", "-t", linked_session], timeout=10)
            if proc.returncode == 0:
                killed.append(linked_session)
    return killed
