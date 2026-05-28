from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli.e2e import _fake_spec
from team_agent.leader_binding import bind_owner_from_caller_pane
from team_agent.simple_yaml import dumps
from team_agent.state import check_team_owner, load_runtime_state, save_runtime_state


class PaneAsIdentityAcceptanceTests(unittest.TestCase):
    def test_1_bind_owner_accepts_any_caller_command_and_only_missing_tmux_pane_refuses(self) -> None:
        commands = ["2.1.154", "", "node", "not-a-known-leader-command"]
        for command in commands:
            with self.subTest(command=command or "<empty>"), tempfile.TemporaryDirectory(prefix="ta-027-bind-") as tmp:
                workspace = Path(tmp)
                with patch.dict(os.environ, {"TMUX_PANE": "%caller", "USER": "alice"}, clear=True), patch(
                    "team_agent.leader_binding.run_cmd",
                    return_value=SimpleNamespace(returncode=0, stdout=f"{command}\n", stderr=""),
                ):
                    result = bind_owner_from_caller_pane(workspace, "team-a")

                self.assertTrue(result.get("ok"), result)
                self.assertEqual(result.get("caller_pane_id"), "%caller")
                self.assertEqual(result.get("caller_current_command"), command)

        with tempfile.TemporaryDirectory(prefix="ta-027-bind-missing-") as tmp:
            with patch.dict(os.environ, {}, clear=True):
                missing = bind_owner_from_caller_pane(Path(tmp), "team-a")
        self.assertFalse(missing.get("ok"), missing)
        self.assertEqual(missing.get("reason"), "caller_pane_missing")

    def test_2_check_team_owner_allows_same_pane_even_when_uuid_differs_and_reads_tmux_pane(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-027-owner-same-pane-") as tmp:
            workspace = Path(tmp)
            state = _owner_state(workspace, owner_pane="%live", owner_uuid="old-version-uuid")
            env = {
                "TMUX_PANE": "%live",
                "TEAM_AGENT_LEADER_SESSION_UUID": "new-version-uuid",
                "USER": "alice",
            }
            with patch.dict(os.environ, env, clear=True):
                refusal = check_team_owner(state)

        self.assertIsNone(refusal, refusal)

    def test_3_dead_owner_pane_does_not_lock_out_new_live_caller(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-027-owner-dead-") as tmp:
            workspace = Path(tmp)
            state = _owner_state(workspace, owner_pane="%dead-owner", owner_uuid="old-version-uuid")
            env = {
                "TMUX_PANE": "%new-live",
                "TEAM_AGENT_LEADER_SESSION_UUID": "new-version-uuid",
                "USER": "alice",
            }
            with patch.dict(os.environ, env, clear=True):
                refusal = check_team_owner(state)

        self.assertIsNone(refusal, refusal)

    def test_4_restart_does_not_keep_stale_dead_receiver_when_autobind_cannot_resolve_live_pane(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-027-restart-stale-receiver-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            stale_receiver = {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": "%dead",
                "session_name": "termclaude-dead",
                "window_index": "0",
                "window_name": "leader",
                "pane_index": "0",
                "pane_tty": "/dev/ttys999",
                "pane_current_command": "claude",
                "leader_session_uuid": "old-owner-uuid",
            }
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-agent-fake-e2e",
                    "leader": spec["leader"],
                    "team_owner": {
                        "pane_id": "%live",
                        "provider": "codex",
                        "leader_session_uuid": "old-owner-uuid",
                        "machine_fingerprint": "machine-a",
                    },
                    "leader_receiver": dict(stale_receiver),
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "agent_id": "fake_impl",
                            "window": "fake_impl",
                            "session_id": "fake-session-1",
                            "mcp_config": str(workspace / ".team/runtime/mcp/fake_impl.json"),
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            started_windows: set[str] = set()
            env = {
                "TMUX_PANE": "%live",
                "TEAM_AGENT_LEADER_PANE_ID": "%live",
                "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE": "old-owner-uuid",
                "USER": "alice",
            }
            with patch.dict(os.environ, env, clear=True), patch(
                "team_agent.runtime.run_cmd", side_effect=_fake_tmux_run_cmd(started_windows)
            ), patch(
                "team_agent.runtime.start_coordinator",
                return_value={"ok": True, "pid": 123, "status": "started"},
            ), patch(
                "team_agent.leader.attach_leader_to_state",
                side_effect=RuntimeError("official binary command 2.1.154 autobind unresolved"),
            ):
                result = runtime.restart(workspace)

            self.assertTrue(result.get("ok"), result)
            receiver_after = load_runtime_state(workspace).get("leader_receiver") or {}
            self.assertNotEqual(receiver_after.get("pane_id"), "%dead", receiver_after)
            self.assertNotEqual(receiver_after.get("session_name"), "termclaude-dead", receiver_after)


def _owner_state(workspace: Path, *, owner_pane: str, owner_uuid: str) -> dict:
    return {
        "workspace": str(workspace),
        "session_name": "team-a",
        "team_owner": {
            "pane_id": owner_pane,
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": owner_uuid,
        },
        "agents": {},
        "tasks": [],
    }


def _fake_tmux_run_cmd(started_windows: set[str]):
    def fake_run_cmd(args: list[str], timeout: int = 20):
        proc = Mock(returncode=1 if args[:2] == ["tmux", "has-session"] else 0, stdout="", stderr="")
        if args[:3] == ["tmux", "new-session", "-d"]:
            started_windows.add(args[6])
        elif args[:2] == ["tmux", "new-window"]:
            started_windows.add(args[5])
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            proc.stdout = "\n".join(sorted(started_windows))
        return proc

    return fake_run_cmd


if __name__ == "__main__":
    unittest.main(verbosity=2)
