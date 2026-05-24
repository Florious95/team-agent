from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.messaging.deps import (
    EventLog,
    _runtime_lock,
    load_spec,
    select_runtime_state,
    team_state_key,
)
from team_agent.messaging.send import _send_single_message_unlocked


def deliver_stored_message(
    workspace: Path,
    target: str | None,
    content: str,
    *,
    task_id: str | None = None,
    sender: str = "coordinator",
    requires_ack: bool = False,
    wait_visible: bool = False,
    timeout: float = 30.0,
    team: str | None = None,
) -> dict[str, Any]:
    with _runtime_lock(workspace, "send"):
        state = select_runtime_state(workspace, team)
        spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
        spec = load_spec(spec_path)
        return _send_single_message_unlocked(
            workspace,
            state,
            spec,
            EventLog(workspace),
            target,
            content,
            task_id=task_id,
            sender=sender,
            requires_ack=requires_ack,
            wait_visible=wait_visible,
            timeout=timeout,
            route_task_id=False,
            owner_team_id=team_state_key(state),
        )
