from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent.leader import autobind_leader_receiver_from_env
from team_agent.state import derive_leader_session_uuid, load_runtime_state, save_runtime_state


def test_first_time_quick_start_no_team_owner_accepts_visible_claude_pane_without_env_uuid(tmp_path, monkeypatch) -> None:
    _seed_first_time_state(tmp_path)
    monkeypatch.setenv("TMUX_PANE", "%422")
    monkeypatch.setenv("USER", "alice")
    monkeypatch.setenv("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-a")
    pane = _pane("%422", tmp_path, command="claude.exe")

    with (
        patch("team_agent.runtime._resolve_leader_pane", return_value=(pane, "current_client")),
        patch("team_agent.runtime._validate_leader_receiver", side_effect=AssertionError("strict gate should not run")),
        patch("team_agent.runtime.run_cmd", side_effect=_ok_run_cmd),
    ):
        receiver = autobind_leader_receiver_from_env(tmp_path, "claude_code", source="quick_start")

    assert receiver is not None
    assert receiver["pane_id"] == "%422"
    assert receiver["leader_session_uuid"] == _expected_uuid(tmp_path)


def test_first_time_quick_start_writes_team_owner_with_derived_uuid(tmp_path, monkeypatch) -> None:
    _seed_first_time_state(tmp_path)
    monkeypatch.setenv("TMUX_PANE", "%422")
    monkeypatch.setenv("USER", "alice")
    monkeypatch.setenv("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-a")
    pane = _pane("%422", tmp_path, command="claude.exe")

    with (
        patch("team_agent.runtime._resolve_leader_pane", return_value=(pane, "current_client")),
        patch("team_agent.runtime.run_cmd", side_effect=_ok_run_cmd),
    ):
        autobind_leader_receiver_from_env(tmp_path, "claude_code", source="quick_start")

    owner = load_runtime_state(tmp_path)["team_owner"]
    assert owner["pane_id"] == "%422"
    assert owner["provider"] == "claude_code"
    assert owner["machine_fingerprint"] == "machine-a"
    assert owner["leader_session_uuid"] == _expected_uuid(tmp_path)
    assert owner["owner_epoch"] == 0
    assert owner["claimed_via"] == "quick_start"


def test_first_time_quick_start_writes_leader_receiver_pointing_to_visible_pane(tmp_path, monkeypatch) -> None:
    _seed_first_time_state(tmp_path)
    monkeypatch.setenv("TMUX_PANE", "%422")
    monkeypatch.setenv("USER", "alice")
    monkeypatch.setenv("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-a")
    pane = _pane("%422", tmp_path, command="claude.exe")
    calls: list[list[str]] = []

    def fake_run_cmd(args: list[str], timeout: int = 20):
        calls.append(args)
        return Mock(returncode=0, stdout="", stderr="")

    with (
        patch("team_agent.runtime._resolve_leader_pane", return_value=(pane, "current_client")),
        patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
    ):
        autobind_leader_receiver_from_env(tmp_path, "claude_code", source="quick_start")

    receiver = load_runtime_state(tmp_path)["leader_receiver"]
    assert receiver["pane_id"] == "%422"
    assert receiver["pane_current_command"] == "claude.exe"
    assert receiver["leader_session_uuid"] == _expected_uuid(tmp_path)
    assert ["tmux", "set-environment", "-t", "leader-session", "TEAM_AGENT_LEADER_SESSION_UUID", _expected_uuid(tmp_path)] in calls


def test_established_team_strict_gate_still_rejects_pane_with_wrong_env_uuid(tmp_path, monkeypatch) -> None:
    expected_uuid = _seed_established_state(tmp_path)
    monkeypatch.setenv("TMUX_PANE", "%422")
    pane = _pane("%422", tmp_path, command="claude.exe")

    with _strict_gate_patches(pane, actual_uuid="b" * 32):
        receiver = autobind_leader_receiver_from_env(tmp_path, "claude_code", source="quick_start")

    state = load_runtime_state(tmp_path)
    assert receiver is None
    assert "leader_receiver" not in state
    assert state["team_owner"]["leader_session_uuid"] == expected_uuid
    skipped = [event for event in _events(tmp_path) if event.get("event") == "leader_receiver.autobind_skipped"]
    assert skipped
    assert "strict UUID gate applies" in skipped[-1]["error"]
    assert "first quick-start uses cwd+command match only" in skipped[-1]["error"]


def test_established_team_strict_gate_accepts_pane_with_matching_env_uuid(tmp_path, monkeypatch) -> None:
    expected_uuid = _seed_established_state(tmp_path)
    monkeypatch.setenv("TMUX_PANE", "%422")
    pane = _pane("%422", tmp_path, command="claude.exe")

    with _strict_gate_patches(pane, actual_uuid=expected_uuid):
        receiver = autobind_leader_receiver_from_env(tmp_path, "claude_code", source="quick_start")

    state = load_runtime_state(tmp_path)
    assert receiver is not None
    assert state["team_owner"]["leader_session_uuid"] == expected_uuid
    assert state["leader_receiver"]["pane_id"] == "%422"
    assert state["leader_receiver"]["leader_session_uuid"] == expected_uuid


def _seed_first_time_state(workspace: Path) -> None:
    save_runtime_state(
        workspace,
        {
            "workspace": str(workspace),
            "session_name": "team-first",
            "leader": {"id": "leader", "provider": "claude_code"},
            "agents": {},
            "tasks": [],
        },
    )


def _seed_established_state(workspace: Path) -> str:
    uuid = _expected_uuid(workspace)
    save_runtime_state(
        workspace,
        {
            "workspace": str(workspace),
            "session_name": "team-first",
            "team_owner": {
                "pane_id": "%old",
                "provider": "claude_code",
                "machine_fingerprint": "machine-a",
                "leader_session_uuid": uuid,
            },
            "leader": {"id": "leader", "provider": "claude_code"},
            "agents": {},
            "tasks": [],
        },
    )
    return uuid


def _expected_uuid(workspace: Path) -> str:
    return derive_leader_session_uuid("machine-a", str(workspace.resolve()), "alice", "team-first")


def _pane(pane_id: str, workspace: Path, *, command: str) -> dict[str, str]:
    return {
        "pane_id": pane_id,
        "session_name": "leader-session",
        "window_index": "1",
        "window_name": "leader",
        "pane_index": "0",
        "pane_tty": "/dev/ttys001",
        "pane_current_command": command,
        "pane_active": "1",
        "pane_current_path": str(workspace.resolve()),
        "session_attached": "1",
    }


def _ok_run_cmd(args: list[str], timeout: int = 20):
    return Mock(returncode=0, stdout="", stderr="")


def _strict_gate_patches(pane: dict[str, str], *, actual_uuid: str):
    target = {**pane, "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": actual_uuid}}
    return patch.multiple(
        "team_agent.runtime",
        _resolve_leader_pane=Mock(return_value=(pane, "current_client")),
        _tmux_pane_info=Mock(return_value=pane),
        core_list_targets=Mock(return_value={"ok": True, "targets": [target]}),
        run_cmd=Mock(return_value=Mock(returncode=0, stdout="leader prompt", stderr="")),
    )


def _events(workspace: Path) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
