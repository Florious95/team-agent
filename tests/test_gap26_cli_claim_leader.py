from __future__ import annotations

import json

import pytest

from team_agent.cli.parser import main
from team_agent.events import EventLog
from team_agent.state import load_runtime_state, save_runtime_state


UUID_A = "a" * 32


def test_gap26_cli_claim_leader_first_claim_wins(tmp_path, monkeypatch, capsys) -> None:
    save_runtime_state(tmp_path, _state())
    EventLog(tmp_path).write(
        "leader_receiver.ambiguous_candidates",
        incident_id="inc-1",
        old_pane_id="%old",
        candidates=["%D", "%E"],
        team_id="team-a",
        uuid_prefix=UUID_A[:12],
    )
    monkeypatch.setenv("TEAM_AGENT_LEADER_PANE_ID", "%D")
    monkeypatch.setenv("TEAM_AGENT_LEADER_SESSION_UUID", UUID_A)
    monkeypatch.setattr("team_agent.runtime.core_list_targets", lambda: {"ok": True, "targets": [_target("%D"), _target("%E")]})

    main(["claim-leader", "--workspace", str(tmp_path), "--team", "team-a", "--confirm", "--json"])
    claimed = json.loads(capsys.readouterr().out)

    assert claimed["ok"] is True
    assert claimed["status"] == "claimed"
    assert claimed["leader_receiver"]["pane_id"] == "%D"
    assert claimed["owner_epoch"] == 8
    state = load_runtime_state(tmp_path)
    assert state["leader_receiver"]["pane_id"] == "%D"
    assert state["team_owner"]["owner_epoch"] == 8

    monkeypatch.setenv("TEAM_AGENT_LEADER_PANE_ID", "%E")
    with pytest.raises(SystemExit) as exc:
        main(["claim-leader", "--workspace", str(tmp_path), "--team", "team-a", "--confirm", "--json"])
    refused = json.loads(capsys.readouterr().out)
    assert exc.value.code == 1
    assert refused["reason"] == "owner_epoch_advanced"
    assert refused["bound_pane_id"] == "%D"
    assert "lost the race" in refused["error"]


def _state() -> dict:
    return {
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%old",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": UUID_A,
            "owner_epoch": 7,
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%old",
            "leader_session_uuid": UUID_A,
            "owner_epoch": 7,
        },
        "agents": {},
        "tasks": [],
    }


def _target(pane_id: str) -> dict:
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": pane_id.strip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": "codex",
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": UUID_A},
    }
