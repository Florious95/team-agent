from __future__ import annotations

import json

from team_agent.cli.parser import main
from team_agent.state import save_runtime_state


UUID_A = "a" * 32


def test_gap26_cli_identity_json(tmp_path, monkeypatch, capsys) -> None:
    monkeypatch.setenv("USER", "alice")
    monkeypatch.setenv("TEAM_AGENT_LEADER_PANE_ID", "%D")
    save_runtime_state(
        tmp_path,
        {
            "workspace": str(tmp_path),
            "session_name": "team-a",
            "team_owner": {
                "pane_id": "%D",
                "provider": "codex",
                "machine_fingerprint": "machine-a",
                "leader_session_uuid": UUID_A,
            },
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": "%D",
                "leader_session_uuid": UUID_A,
                "attached_at": "2026-05-25T00:00:00+00:00",
            },
            "agents": {},
            "tasks": [],
        },
    )

    main(["identity", "--workspace", str(tmp_path), "--team", "team-a", "--json"])
    identity = json.loads(capsys.readouterr().out)

    assert identity["ok"] is True
    assert identity["uuid_prefix"] == UUID_A[:12]
    assert len(identity["uuid_prefix"]) == 12
    assert identity["machine_fingerprint"] == "machine-a"
    assert identity["workspace_abspath"] == str(tmp_path.resolve())
    assert identity["os_user"] == "alice"
    assert identity["team_id"] == "team-a"
    assert identity["current_pane_id"] == "%D"
    assert identity["last_seen_at"] == "2026-05-25T00:00:00+00:00"
    assert identity["source"] == "derived"
