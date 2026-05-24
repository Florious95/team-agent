from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.paths import logs_dir
from team_agent.profiles import prepare_agent_profile_launch
from team_agent.providers import ResumeUnavailable
from team_agent.sessions.capture import clear_session_capture_fields, copy_session_metadata


def attach_profile_resume_root(workspace: Path, command_agent: dict[str, Any], previous: dict[str, Any]) -> dict[str, Any]:
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if not profile_launch:
        return previous
    command_agent["_provider_profile"] = profile_launch
    root = profile_launch.get("claude_projects_root")
    if not root:
        return previous
    prepared = dict(previous)
    prepared["claude_projects_root"] = root
    return prepared


def prepare_resume_state(
    workspace: Path,
    agent_id: str,
    previous: dict[str, Any],
    adapter: Any,
    event_log: EventLog,
    exclude_session_ids: set[str] | None = None,
    allow_fresh_on_resume_failure: bool = False,
) -> dict[str, Any]:
    prepared = dict(previous)
    session_id = prepared.get("session_id")
    if session_id and adapter.session_is_resumable(prepared, workspace):
        return prepared
    if session_id:
        event_log.write(
            "resume.session_unverified",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            session_id=session_id,
            captured_via=prepared.get("captured_via"),
            spawn_cwd=prepared.get("spawn_cwd"),
        )
    else:
        event_log.write(
            "resume.session_missing_repair_attempt",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            spawn_cwd=prepared.get("spawn_cwd"),
        )
    repaired = recover_resume_session_from_events(workspace, agent_id, prepared, adapter, exclude_session_ids or set())
    if not repaired:
        repaired = adapter.recover_session_id(agent_id, prepared, workspace, exclude_session_ids or set())
    if repaired:
        copy_session_metadata(prepared, repaired)
        event_log.write(
            "resume.session_repaired",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            old_session_id=session_id,
            session_id=prepared.get("session_id"),
            rollout_path=prepared.get("rollout_path"),
            captured_via=prepared.get("captured_via"),
            attribution_confidence=prepared.get("attribution_confidence"),
        )
        return prepared
    if session_id and not allow_fresh_on_resume_failure:
        event_log.write(
            "resume.session_required_missing",
            agent_id=agent_id,
            provider=prepared.get("provider"),
            old_session_id=session_id,
            rollout_path=prepared.get("rollout_path"),
            reason="provider transcript not found",
        )
        raise ResumeUnavailable(
            f"Cannot resume agent {agent_id}: stored session {session_id} is not available. "
            "Use --allow-fresh only if losing that worker context is acceptable."
        )
    clear_session_capture_fields(prepared)
    event_log.write(
        "resume.session_unavailable",
        agent_id=agent_id,
        provider=prepared.get("provider"),
        old_session_id=session_id,
        reason="provider transcript not found",
    )
    return prepared


def recover_resume_session_from_events(
    workspace: Path,
    agent_id: str,
    previous: dict[str, Any],
    adapter: Any,
    exclude_session_ids: set[str],
) -> dict[str, Any] | None:
    events_path = logs_dir(workspace) / "events.jsonl"
    try:
        lines = events_path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    current_session_id = str(previous.get("session_id") or "")
    for line in reversed(lines):
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("event") != "session.captured" or event.get("agent_id") != agent_id:
            continue
        session_id = str(event.get("session_id") or "")
        if not session_id or session_id == current_session_id or session_id in exclude_session_ids:
            continue
        candidate = dict(previous)
        candidate.update(
            {
                "session_id": session_id,
                "rollout_path": event.get("rollout_path"),
                "captured_at": event.get("ts"),
                "captured_via": "event_log_repair",
                "attribution_confidence": event.get("attribution_confidence"),
            }
        )
        if adapter.session_is_resumable(candidate, workspace):
            return candidate
    return None
