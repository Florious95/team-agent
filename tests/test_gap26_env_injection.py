from __future__ import annotations

from unittest.mock import Mock, patch

from team_agent.leader import leader_start_plan
from team_agent.providers import shell_command
from team_agent.state import derive_leader_session_uuid


def test_gap26_env_injection_for_leader_and_not_worker(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("USER", "alice")
    monkeypatch.setenv("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-a")
    monkeypatch.setenv("TMUX", "/tmp/tmux.sock,1,0")
    expected = derive_leader_session_uuid("machine-a", str(tmp_path.resolve()), "alice", "current")
    adapter = Mock(command_name="codex", is_installed=lambda: True)

    with patch("team_agent.runtime.get_adapter", return_value=adapter):
        plan = leader_start_plan("codex", ["--foo"], tmp_path)

    assert plan["mode"] == "exec_provider"
    assert plan["env"]["TEAM_AGENT_LEADER_SESSION_UUID"] == expected
    assert plan["env"]["TEAM_AGENT_MACHINE_FINGERPRINT"] == "machine-a"
    assert plan["env"]["TEAM_AGENT_TEAM_ID"] == "current"

    monkeypatch.delenv("TMUX")
    with (
        patch("team_agent.runtime.get_adapter", return_value=adapter),
        patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
        patch("team_agent.runtime._tmux_session_exists", return_value=False),
    ):
        tmux_plan = leader_start_plan("codex", [], tmp_path)

    assert tmux_plan["leader_env"]["TEAM_AGENT_LEADER_SESSION_UUID"] == expected
    assert expected in tmux_plan["argv"][-1]

    worker_command = shell_command(["codex"], "worker", tmp_path)
    assert "TEAM_AGENT_ID=worker" in worker_command
    assert "TEAM_AGENT_LEADER_SESSION_UUID" not in worker_command
