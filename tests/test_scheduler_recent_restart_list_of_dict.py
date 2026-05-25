from __future__ import annotations

from datetime import datetime, timedelta, timezone
from typing import Any

from team_agent.messaging.scheduler import _recent_restart_or_reset_event


class FakeEventLog:
    def __init__(self, events: list[dict[str, Any]]) -> None:
        self._events = events

    def tail(self, _limit: int) -> list[dict[str, Any]]:
        return self._events


def _restart_event(agents: list[Any]) -> dict[str, Any]:
    return {
        "event": "restart.complete",
        "ts": datetime.now(timezone.utc).isoformat(),
        "agents": agents,
    }


def test_restart_complete_agents_dict_shape_matches_agent_id_without_type_error() -> None:
    event = _restart_event(
        [
            {"agent_id": "developer", "restart_mode": "resumed", "session_id": "sess-1"},
            {"agent_id": "spark-reviewer", "restart_mode": "fresh", "session_id": "sess-2"},
        ]
    )

    result = _recent_restart_or_reset_event(
        FakeEventLog([event]),
        "developer",
        datetime.now(timezone.utc) - timedelta(minutes=5),
    )

    assert result is event


def test_restart_complete_agents_mixed_string_and_dict_shapes_match() -> None:
    event = _restart_event([{"agent_id": "X"}, "Y"])
    since = datetime.now(timezone.utc) - timedelta(minutes=5)

    assert _recent_restart_or_reset_event(FakeEventLog([event]), "X", since) is event
    assert _recent_restart_or_reset_event(FakeEventLog([event]), "Y", since) is event
    assert _recent_restart_or_reset_event(FakeEventLog([event]), "Z", since) is None
