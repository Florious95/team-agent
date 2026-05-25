from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.providers import get_adapter
from team_agent.state import SESSION_CAPTURE_FIELDS, SESSION_STATE_FIELDS


def capture_missing_sessions(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    timeout_s: float,
    log_miss: bool = True,
) -> list[str]:
    captured: list[str] = []
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("session_id"):
            continue
        known_session_ids = {
            str(item.get("session_id"))
            for aid, item in state.get("agents", {}).items()
            if aid != agent_id and item.get("session_id")
        }
        result = capture_agent_session(
            workspace,
            agent_id,
            agent_state,
            event_log,
            timeout_s=timeout_s,
            exclude_session_ids=known_session_ids,
        )
        if result:
            captured.append(agent_id)
        elif log_miss:
            event_log.write(
                "session.capture_timeout",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                timeout_s=timeout_s,
                spawn_cwd=agent_state.get("spawn_cwd"),
            )
    return captured


def capture_agent_session(
    workspace: Path,
    agent_id: str,
    agent_state: dict[str, Any],
    event_log: EventLog,
    timeout_s: float,
    exclude_session_ids: set[str] | None = None,
) -> dict[str, Any] | None:
    if agent_state.get("session_id"):
        return None
    adapter = get_adapter(agent_state["provider"])
    spawn_context = {
        "agent_id": agent_id,
        "cwd": agent_state.get("spawn_cwd") or str(workspace),
        "spawn_time": agent_state.get("spawned_at") or datetime.now(timezone.utc).isoformat(),
        "tmux_target": f"{agent_state.get('session_name', '')}:{agent_state.get('window', agent_id)}",
        "predetermined_session_id": agent_state.get("_pending_session_id"),
        "exclude_session_ids": sorted(exclude_session_ids or set()),
        "claude_projects_root": agent_state.get("claude_projects_root"),
    }
    result = adapter.capture_session_id(agent_id, spawn_context, timeout_s=timeout_s)
    if not isinstance(result, dict) or not result.get("session_id"):
        return None
    copy_session_metadata(agent_state, result)
    agent_state.pop("_pending_session_id", None)
    event_log.write(
        "session.captured",
        agent_id=agent_id,
        provider=agent_state.get("provider"),
        session_id=agent_state.get("session_id"),
        rollout_path=agent_state.get("rollout_path"),
        captured_via=agent_state.get("captured_via"),
        attribution_confidence=agent_state.get("attribution_confidence"),
    )
    return result


def copy_session_metadata(target: dict[str, Any], source: dict[str, Any]) -> None:
    for key in SESSION_STATE_FIELDS:
        target[key] = source.get(key)


def clear_session_capture_fields(target: dict[str, Any]) -> None:
    for key in SESSION_CAPTURE_FIELDS:
        target[key] = None
