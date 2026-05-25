from __future__ import annotations

import json
import re
from pathlib import Path

from team_agent.state import derive_leader_session_uuid, load_runtime_state, runtime_state_path


def test_gap26_identity_uuid_formula_and_legacy_migration(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("USER", "alice")
    workspace_a = tmp_path / "workspace-a"
    workspace_b = tmp_path / "workspace-b"
    team_a = workspace_a / ".team" / "team-a"
    team_a.mkdir(parents=True)
    uuid_a = derive_leader_session_uuid("machine-a", str(workspace_a.resolve()), "alice", "team-a")

    assert re.fullmatch(r"[0-9a-f]{32}", uuid_a)
    assert derive_leader_session_uuid("machine-a", str(workspace_a.resolve()), "alice", "team-a") == uuid_a
    assert derive_leader_session_uuid("machine-b", str(workspace_a.resolve()), "alice", "team-a") != uuid_a
    assert derive_leader_session_uuid("machine-a", str(workspace_b.resolve()), "alice", "team-a") != uuid_a
    assert derive_leader_session_uuid("machine-a", str(workspace_a.resolve()), "bob", "team-a") != uuid_a
    assert derive_leader_session_uuid("machine-a", str(workspace_a.resolve()), "alice", "team-b") != uuid_a

    legacy = _legacy_state(workspace_a, team_a, "%stale-a")
    state_path = runtime_state_path(workspace_a)
    state_path.parent.mkdir(parents=True)
    state_path.write_text(json.dumps(legacy), encoding="utf-8")

    loaded = load_runtime_state(workspace_a)

    assert loaded["team_owner"]["leader_session_uuid"] == uuid_a
    assert loaded["leader_receiver"]["leader_session_uuid"] == uuid_a
    persisted = json.loads(state_path.read_text(encoding="utf-8"))
    assert persisted["team_owner"]["leader_session_uuid"] == uuid_a
    assert persisted["leader_receiver"]["leader_session_uuid"] == uuid_a
    assert persisted["leader_receiver"]["pane_id"] == "%stale-a"

    legacy_with_different_pane = _legacy_state(workspace_a, team_a, "%stale-b")
    state_path.write_text(json.dumps(legacy_with_different_pane), encoding="utf-8")
    reloaded = load_runtime_state(workspace_a)
    assert reloaded["leader_receiver"]["leader_session_uuid"] == uuid_a


def _legacy_state(workspace: Path, team_dir: Path, pane_id: str) -> dict:
    return {
        "workspace": str(workspace),
        "team_dir": str(team_dir),
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%owner-old",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": pane_id,
        },
        "agents": {},
        "tasks": [],
    }
