from __future__ import annotations

from typing import Any

from team_agent.events import EventLog
from team_agent.display.adaptive import close_adaptive_display
from team_agent.display.ghostty import ghostty_pids_by_title
from team_agent.display.workspace import kill_ghostty_workspace_linked_sessions


def close_team_display_backends(state: dict[str, Any], event_log: EventLog) -> dict[str, Any]:
    result = close_adaptive_display(state, event_log)
    close_ghostty_workspace(state, event_log)
    return result


def close_ghostty_display(
    agent_id: str,
    agent_state: dict[str, Any],
    event_log: EventLog,
) -> None:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    display = agent_state.get("display") or {}
    if display.get("backend") != "ghostty_window":
        return
    display_session = display.get("display_session")
    pids = [str(pid) for pid in display.get("pids", []) if str(pid).isdigit()]
    title = display.get("title")
    if not pids and title:
        pids = [str(pid) for pid in ghostty_pids_by_title(str(title))]
    killed: list[str] = []
    for pid in pids:
        proc = run_cmd(["kill", pid], timeout=5)
        if proc.returncode == 0:
            killed.append(pid)
    if killed:
        event_log.write("display.ghostty_closed", agent_id=agent_id, pids=killed, title=title)
    if display_session and _tmux_session_exists(str(display_session)):
        proc = run_cmd(["tmux", "kill-session", "-t", str(display_session)], timeout=10)
        if proc.returncode == 0:
            event_log.write("display.ghostty_display_session_closed", agent_id=agent_id, display_session=display_session)
        else:
            event_log.write(
                "display.ghostty_display_session_close_failed",
                agent_id=agent_id,
                display_session=display_session,
                error=proc.stderr.strip(),
            )


def close_ghostty_workspace_slot(
    agent_id: str,
    display: dict[str, Any],
    event_log: EventLog,
) -> None:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    pane_id = display.get("pane_id")
    linked_session = display.get("linked_session")
    stopped_title = f"stopped: {agent_id}"
    relabeled = False
    if pane_id:
        proc = run_cmd(["tmux", "select-pane", "-t", str(pane_id), "-T", stopped_title], timeout=10)
        if proc.returncode == 0:
            relabeled = True
        else:
            event_log.write(
                "display.ghostty_workspace_slot_relabel_failed",
                agent_id=agent_id,
                pane_id=pane_id,
                error=proc.stderr.strip(),
            )
    linked_session_closed = False
    if linked_session and _tmux_session_exists(str(linked_session)):
        proc = run_cmd(["tmux", "kill-session", "-t", str(linked_session)], timeout=10)
        if proc.returncode == 0:
            linked_session_closed = True
        else:
            event_log.write(
                "display.ghostty_workspace_slot_linked_session_close_failed",
                agent_id=agent_id,
                linked_session=linked_session,
                error=proc.stderr.strip(),
            )
    display["status"] = "stopped"
    display["pane_title"] = stopped_title
    event_log.write(
        "display.ghostty_workspace_slot_closed",
        agent_id=agent_id,
        pane_id=pane_id,
        linked_session=linked_session,
        relabeled=relabeled,
        linked_session_closed=linked_session_closed,
    )


def close_ghostty_workspace(state: dict[str, Any], event_log: EventLog) -> None:
    from team_agent.runtime import _tmux_session_exists, run_cmd
    displays = [
        (agent_id, agent_state.get("display") or {})
        for agent_id, agent_state in state.get("agents", {}).items()
        if (agent_state.get("display") or {}).get("backend") == "ghostty_workspace"
    ]
    if not displays:
        return
    aggregator_session = next(
        (
            str(display.get("aggregator_session") or display.get("display_session"))
            for _agent_id, display in displays
            if display.get("aggregator_session") or display.get("display_session")
        ),
        None,
    )
    title = next((str(display.get("title")) for _agent_id, display in displays if display.get("title")), None)
    pids = {
        str(pid)
        for _agent_id, display in displays
        for pid in display.get("pids", [])
        if str(pid).isdigit()
    }
    if not pids and title:
        pids = {str(pid) for pid in ghostty_pids_by_title(str(title))}

    aggregator_closed = False
    if aggregator_session and _tmux_session_exists(aggregator_session):
        proc = run_cmd(["tmux", "kill-session", "-t", aggregator_session], timeout=10)
        if proc.returncode == 0:
            aggregator_closed = True
        else:
            event_log.write(
                "display.ghostty_workspace_close_failed",
                aggregator_session=aggregator_session,
                error=proc.stderr.strip(),
            )

    linked_sessions = [
        str(display.get("linked_session"))
        for _agent_id, display in displays
        if display.get("linked_session")
    ]
    linked_closed = kill_ghostty_workspace_linked_sessions(linked_sessions)

    killed: list[str] = []
    for pid in sorted(pids):
        proc = run_cmd(["kill", pid], timeout=5)
        if proc.returncode == 0:
            killed.append(pid)
    event_log.write(
        "display.ghostty_workspace_closed",
        pids=killed,
        title=title,
        aggregator_session=aggregator_session,
        linked_sessions=linked_closed,
        aggregator_closed=aggregator_closed,
    )
