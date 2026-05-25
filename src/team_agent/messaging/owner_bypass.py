from __future__ import annotations

from typing import Any

from team_agent.events import EventLog
from team_agent.state import worker_sender_bypasses_owner_gate


def apply_worker_sender_bypass(
    state: dict[str, Any],
    sender: str | None,
    target: Any,
    task_id: str | None,
    event_log: EventLog,
) -> bool:
    via = worker_sender_bypasses_owner_gate(state, sender)
    if not via:
        return False
    event_log.write(
        "send.bypassed_owner_gate_worker_sender",
        sender=sender,
        env_team_agent_id=via,
        target=target if isinstance(target, str) else None,
        task_id=task_id,
    )
    return True


__all__ = ["apply_worker_sender_bypass"]
