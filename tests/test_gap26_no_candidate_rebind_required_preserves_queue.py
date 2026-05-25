from __future__ import annotations

import json
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging.results import _notify_leader_of_report_result
from team_agent.state import save_runtime_state


UUID_A = "a" * 32


def test_gap26_no_candidate_rebind_required_preserves_queue(tmp_path) -> None:
    save_runtime_state(tmp_path, _state())
    store = MessageStore(tmp_path)

    with (
        patch("team_agent.runtime._validate_leader_receiver", return_value={"ok": False, "reason": "leader_pane_missing", "error": "gone"}),
        patch("team_agent.messaging.leader_panes.core_list_targets", return_value={"ok": True, "targets": []}),
        patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "status": "started", "pid": 123}),
    ):
        queued = _notify_leader_of_report_result(tmp_path, _result_envelope(), "res_1", EventLog(tmp_path))

    assert queued["ok"] is True
    assert queued["status"] == "queued"
    pending = store.due_scheduled_events("9999-01-01T00:00:00+00:00")
    assert len(pending) == 1
    assert pending[0]["kind"] == "send"
    events = _events(tmp_path)
    required = [event for event in events if event.get("event") == "leader_receiver.rebind_required"]
    assert len(required) == 1
    assert required[0]["old_pane_id"] == "%old"
    assert required[0]["reason"] == "leader_pane_missing"
    assert required[0]["uuid_prefix"] == UUID_A[:8]
    assert "claim-leader --confirm" in required[0]["recovery_action"]


def _state() -> dict:
    return {
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%old",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": UUID_A,
        },
        "leader": {"id": "leader"},
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%old",
            "leader_session_uuid": UUID_A,
        },
        "agents": {"worker": {"status": "running", "provider": "fake"}},
        "tasks": [{"id": "task-1", "assignee": "worker", "status": "running"}],
    }


def _result_envelope() -> dict:
    return {
        "task_id": "task-1",
        "agent_id": "worker",
        "status": "success",
        "summary": "done",
    }


def _events(workspace) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
