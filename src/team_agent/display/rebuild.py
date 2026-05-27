from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.events import EventLog


def rebuild_restart_display_after_rebind(
    display_backend: str,
    workspace: Path,
    session_name: str,
    spec: dict[str, Any],
    event_log: EventLog,
    restarted: list[dict[str, Any]],
    receiver: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if display_backend != "adaptive":
        return {}
    from team_agent.restart.snapshot import save_team_runtime_snapshot
    from team_agent.state import load_runtime_state, save_runtime_state, write_team_state
    state = load_runtime_state(workspace)
    state, display_results = rebuild_adaptive_display_after_rebind(
        workspace,
        session_name,
        spec,
        state,
        event_log,
        save_runtime_state,
        save_team_runtime_snapshot,
        write_team_state,
        receiver=receiver,
    )
    for item in restarted:
        display = display_results.get(item["agent_id"])
        if display:
            item["display_target"] = display
    return state


def rebuild_adaptive_display_after_rebind(
    workspace: Path,
    session_name: str,
    spec: dict[str, Any],
    state: dict[str, Any],
    event_log: EventLog,
    save_state: Any,
    save_snapshot: Any,
    write_team_state: Any,
    receiver: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    if receiver:
        state["leader_receiver"] = receiver
    receiver = receiver or (state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {})
    rebind_session = latest_rebind_session(event_log)
    if rebind_session:
        receiver = {**receiver, "session_name": rebind_session}
    jobs = [
        (agent["id"], agent)
        for agent in spec.get("agents", [])
        if agent["id"] in state.get("agents", {}) and state["agents"][agent["id"]].get("status") == "running"
    ]
    from team_agent.runtime import _open_worker_displays
    display_results = _open_worker_displays(
        workspace,
        session_name,
        jobs,
        event_log,
        "adaptive",
        capability_probe={
            "in_tmux": bool(receiver.get("session_name")),
            "leader_session": receiver.get("session_name"),
            "leader_pane": receiver.get("pane_id"),
            "platform": None,
            "caps": {"adaptive_display": bool(receiver.get("session_name"))},
            "reason": None if receiver.get("session_name") else "leader_not_in_tmux",
        },
    )
    for agent_id, display in display_results.items():
        if agent_id in state.get("agents", {}):
            state["agents"][agent_id]["display"] = display
    event_log.write(
        "display.adaptive_rebuilt",
        session=session_name,
        workers=sorted(display_results),
        leader_session=next((display.get("leader_session") for display in display_results.values()), None),
        stale_windows_recreated=True,
    )
    save_state(workspace, state)
    save_snapshot(workspace, state)
    write_team_state(workspace, spec, state)
    return state, display_results


def latest_rebind_session(event_log: EventLog) -> str | None:
    for event in reversed(event_log.tail(50)):
        if event.get("event") != "leader_receiver.rebind_applied":
            continue
        session = event.get("new_session_name") or event.get("session_name")
        if session:
            return str(session)
    return None
