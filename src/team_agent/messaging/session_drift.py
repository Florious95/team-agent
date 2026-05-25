from __future__ import annotations

import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog

_UUID = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
_RESUME_THREAD_RE = re.compile(
    rf"(?:Switched to thread|resume|thread)\s+({_UUID})",
    re.IGNORECASE,
)


def extract_thread_id_from_scrollback(scrollback: str) -> str | None:
    if not scrollback:
        return None
    matches = _RESUME_THREAD_RE.findall(scrollback)
    if not matches:
        return None
    return matches[-1].lower()


def detect_session_drift(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    *,
    agent_id: str,
    agent_state: dict[str, Any],
    scrollback: str,
) -> dict[str, Any] | None:
    provider = str(agent_state.get("provider") or "").lower()
    if provider != "codex":
        return None
    stored = str(agent_state.get("session_id") or "").strip()
    if not stored:
        return None
    if str(agent_state.get("status") or "").lower() == "session_drift":
        return None
    actual = extract_thread_id_from_scrollback(scrollback)
    if not actual:
        return None
    if actual.lower() == stored.lower():
        return None
    now = datetime.now(timezone.utc).isoformat()
    event = event_log.write(
        "coordinator.session_drift_detected",
        agent_id=agent_id,
        stored_session_id=stored,
        actual_thread_id=actual,
        status="session_drift",
        provider=provider,
        ts=now,
        remediation="team-agent reset-agent --discard-session <agent>",
    )
    agent_state["status"] = "session_drift"
    agent_state["session_drift"] = {
        "stored_session_id": stored,
        "actual_thread_id": actual,
        "detected_at": now,
        "remediation": "team-agent reset-agent --discard-session <agent>",
    }
    return event


def session_drift_refusal(state, target, leader_id, sender, task_id, event_log):
    if not target or target == leader_id or target == "*":
        return None
    rs = (state.get("agents") or {}).get(target) or {}
    if str(rs.get("status") or "").lower() != "session_drift":
        return None
    info = rs.get("session_drift") or {}
    event_log.write(
        "send.refused_session_drift",
        target=target,
        sender=sender,
        task_id=task_id,
        stored_session_id=info.get("stored_session_id"),
        actual_thread_id=info.get("actual_thread_id"),
    )
    return {
        "ok": False,
        "status": "refused",
        "reason": "session_drift",
        "to": target,
        "action": f"team-agent reset-agent --discard-session {target}",
        "session_drift": info,
    }


__all__ = ["detect_session_drift", "extract_thread_id_from_scrollback", "session_drift_refusal"]
