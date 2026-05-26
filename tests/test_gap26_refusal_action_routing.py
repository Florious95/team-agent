from __future__ import annotations

from team_agent.state import check_team_owner, derive_leader_session_uuid


def test_same_uuid_different_pane_id_refusal_hints_claim_leader(tmp_path, monkeypatch) -> None:
    uuid = _uuid(tmp_path)
    monkeypatch.setenv("TEAM_AGENT_LEADER_SESSION_UUID", uuid)
    monkeypatch.setenv("TEAM_AGENT_LEADER_PANE_ID", "%branch")

    refusal = check_team_owner(_state(tmp_path, uuid))

    assert refusal is not None
    assert refusal["reason"] == "team_owner_mismatch"
    assert refusal["action"] == "team-agent claim-leader --confirm"


def test_different_uuid_refusal_hints_takeover(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("TEAM_AGENT_LEADER_SESSION_UUID", "b" * 32)
    monkeypatch.setenv("TEAM_AGENT_LEADER_PANE_ID", "%branch")

    refusal = check_team_owner(_state(tmp_path, _uuid(tmp_path)))

    assert refusal is not None
    assert refusal["reason"] == "team_owner_mismatch"
    assert refusal["action"] == "team-agent takeover --confirm"


def test_refusal_envelope_carries_reason_kind(tmp_path, monkeypatch) -> None:
    uuid = _uuid(tmp_path)
    monkeypatch.setenv("TEAM_AGENT_LEADER_SESSION_UUID", uuid)
    monkeypatch.setenv("TEAM_AGENT_LEADER_PANE_ID", "%branch")
    same_uuid = check_team_owner(_state(tmp_path, uuid))

    monkeypatch.setenv("TEAM_AGENT_LEADER_SESSION_UUID", "c" * 32)
    different_uuid = check_team_owner(_state(tmp_path, uuid))

    assert same_uuid is not None
    assert different_uuid is not None
    assert same_uuid["reason_kind"] == "sticky_bind_collision"
    assert different_uuid["reason_kind"] == "owner_takeover_required"


def _uuid(tmp_path) -> str:
    return derive_leader_session_uuid("machine-a", str(tmp_path.resolve()), "alice", "team-a")


def _state(tmp_path, uuid: str) -> dict:
    return {
        "workspace": str(tmp_path),
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%owner",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": uuid,
        },
        "agents": {},
        "tasks": [],
    }
