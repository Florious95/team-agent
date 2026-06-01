from __future__ import annotations

import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.events import EventLog
from team_agent.providers import get_adapter
from team_agent.state import SESSION_CAPTURE_FIELDS, SESSION_STATE_FIELDS


# Stage 7 S6 (2026-05-27): capture_agent_session used to do a single adapter
# call and silently return None on miss, leaving status='running' workers with
# session_id=null. Slow worker startups (Codex writing the rollout file a few
# tenths of a second after window creation) raced this check. We now poll on a
# small interval inside the caller's timeout_s budget so the adapter's own
# fast-path call doesn't have to absorb all the latency on its own.
_CAPTURE_POLL_INTERVAL_SECONDS = 0.05


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
        # capture_missing_sessions is invoked from coordinator_tick, diagnose,
        # status, etc. with very short timeouts; a transient miss should NOT
        # crash those paths. The loud raise contract belongs to direct callers
        # (e.g. lifecycle start/restart) who own the worker's atomicity.
        result = capture_agent_session(
            workspace,
            agent_id,
            agent_state,
            event_log,
            timeout_s=timeout_s,
            exclude_session_ids=known_session_ids,
            raise_on_missed=False,
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
    raise_on_missed: bool = True,
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
        "auth_mode": agent_state.get("auth_mode"),
    }
    deadline = time.monotonic() + max(timeout_s, 0.0)
    while True:
        # Pass timeout_s=0 so the adapter does a single fast-path check; the
        # outer loop owns the polling budget so behaviour stays consistent
        # whether or not the adapter has its own internal sleep.
        result = adapter.capture_session_id(agent_id, spawn_context, timeout_s=0)
        if isinstance(result, dict) and (result.get("session_id") or result.get("rollout_path")):
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
        if time.monotonic() >= deadline:
            break
        time.sleep(_CAPTURE_POLL_INTERVAL_SECONDS)
    # Timeout. Slice 1 atomicity contract: a worker whose status is 'running'
    # must NEVER be left with session_id=null — that half-state is what made
    # Mac mini Stage 7 S5/S6 unreproducible and breaks resume on next restart.
    # Emit a structured attention event so the coordinator/operator sees the
    # miss, then raise so callers cannot accidentally treat the None as a
    # silent "no-op". Non-running workers (still starting, paused, stopped)
    # legitimately have no session yet, so they still get the silent-None
    # return that existing callers expect.
    if agent_state.get("status") == "running":
        event_log.write(
            "session.capture_required_attention",
            agent_id=agent_id,
            provider=agent_state.get("provider"),
            timeout_s=timeout_s,
            spawn_cwd=agent_state.get("spawn_cwd"),
            session_name=agent_state.get("session_name"),
            window=agent_state.get("window", agent_id),
        )
        if raise_on_missed:
            raise TeamAgentRuntimeError(
                f"Failed to capture session_id for agent {agent_id}: adapter "
                f"did not produce a session within {timeout_s}s. Worker is "
                "running but unidentifiable; this is a Slice 1 atomicity "
                "violation."
            )
    return None


def copy_session_metadata(target: dict[str, Any], source: dict[str, Any]) -> None:
    for key in SESSION_STATE_FIELDS:
        target[key] = source.get(key)


def clear_session_capture_fields(target: dict[str, Any]) -> None:
    for key in SESSION_CAPTURE_FIELDS:
        target[key] = None
