from __future__ import annotations

from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.state import save_runtime_state


UUID_A = "a" * 32


def test_gap26_sticky_bind_branch_refused(tmp_path, monkeypatch) -> None:
    state = _state(UUID_A)
    save_runtime_state(tmp_path, state)
    injected: list[str] = []

    with (
        patch("team_agent.runtime._tmux_pane_info", return_value=_pane("%A", UUID_A)),
        patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="› idle", stderr="")),
        patch("team_agent.runtime._tmux_inject_text", side_effect=lambda target, *_args, **_kwargs: injected.append(target) or {"ok": True}),
    ):
        delivered = runtime._send_to_leader_receiver(
            tmp_path,
            state,
            "leader",
            "sticky delivery",
            "task-1",
            "worker",
            False,
            EventLog(tmp_path),
        )

    assert delivered["ok"] is True
    assert injected == ["%A"]
    assert state["leader_receiver"]["pane_id"] == "%A"
    assert state["team_owner"]["owner_epoch"] == 7

    monkeypatch.setenv("TEAM_AGENT_LEADER_SESSION_UUID", "b" * 32)
    monkeypatch.delenv("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", raising=False)
    with patch("team_agent.runtime._tmux_inject_text") as inject:
        refused = runtime._send_to_leader_receiver(
            tmp_path,
            state,
            "leader",
            "side pane should not deliver",
            "task-1",
            "worker",
            False,
            EventLog(tmp_path),
        )

    assert refused["ok"] is False
    assert refused["status"] == "refused"
    assert refused["bound_pane_id"] == "%A"
    assert "team-agent claim-leader --confirm" in refused["error"]
    inject.assert_not_called()


def _state(uuid: str) -> dict:
    return {
        "workspace": "",
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%A",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": uuid,
            "owner_epoch": 7,
        },
        "leader": {"id": "leader"},
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%A",
            "leader_session_uuid": uuid,
        },
        "agents": {"worker": {"status": "running", "provider": "fake"}},
        "tasks": [{"id": "task-1", "assignee": "worker", "status": "running"}],
    }


def _pane(pane_id: str, uuid: str) -> dict[str, object]:
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": pane_id.strip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": "codex",
        "pane_active": "1",
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": uuid},
    }
