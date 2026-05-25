from __future__ import annotations

import json
from unittest.mock import Mock, patch

import pytest

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.state import save_runtime_state


UUID_A = "a" * 32
UUID_B = "b" * 32


@pytest.mark.parametrize(
    ("old_pane", "reason"),
    [
        (None, "leader_pane_missing"),
        ("zsh", "leader_pane_wrong_command"),
        ("wrong_uuid", "leader_uuid_mismatch"),
    ],
)
def test_gap26_invalidation_rules_and_single_candidate_rebind(tmp_path, old_pane, reason) -> None:
    state = _state()
    save_runtime_state(tmp_path, state)
    injected: list[str] = []

    def pane_info(target):
        if target == "%old":
            if old_pane is None:
                return None
            if old_pane == "zsh":
                return _pane("%old", UUID_A, command="zsh")
            return _pane("%old", UUID_B)
        if target == "%new":
            return _pane("%new", UUID_A)
        return None

    with (
        patch("team_agent.runtime._tmux_pane_info", side_effect=pane_info),
        patch("team_agent.messaging.leader_panes.core_list_targets", return_value={"ok": True, "targets": [_target("%new", UUID_A)]}),
        patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="› idle", stderr="")),
        patch("team_agent.runtime._tmux_inject_text", side_effect=lambda target, *_args, **_kwargs: injected.append(target) or {"ok": True}),
    ):
        delivered = runtime._send_to_leader_receiver(
            tmp_path,
            state,
            "leader",
            "rebind delivery",
            "task-1",
            "worker",
            False,
            EventLog(tmp_path),
        )

    assert delivered["ok"] is True
    assert injected == ["%new"]
    assert state["leader_receiver"]["pane_id"] == "%new"
    events = _events(tmp_path)
    applied = [event for event in events if event.get("event") == "leader_receiver.rebind_applied"]
    assert applied
    assert applied[-1]["old_pane_id"] == "%old"
    assert applied[-1]["new_pane_id"] == "%new"
    assert applied[-1]["reason"] == reason
    assert applied[-1]["uuid_prefix"] == UUID_A[:8]
    attempts = [event for event in events if event.get("event") == "leader_receiver.deliver_attempt"]
    assert attempts[-1]["target"] == "%new"


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


def _target(pane_id: str, uuid: str) -> dict[str, object]:
    pane = _pane(pane_id, uuid)
    pane["fingerprint"] = f"leaders|1|0|{pane_id}"
    return pane


def _pane(pane_id: str, uuid: str, command: str = "codex") -> dict[str, object]:
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": pane_id.strip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": command,
        "pane_active": "1",
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": uuid},
    }


def _events(workspace) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
