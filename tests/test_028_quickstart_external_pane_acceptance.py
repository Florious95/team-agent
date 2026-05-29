from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock
from unittest.mock import patch

from team_agent import runtime
from team_agent import _legacy_pane_discovery as legacy_panes
from team_agent.cli.e2e import _fake_spec
from team_agent.events import EventLog
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.leader import (
    _caller_pane_eligibility,
    _pane_is_live_leader,
    _try_readopt_leader_pane,
)
from team_agent.messaging.leader import claim_leader_receiver
from team_agent.messaging.leader_panes import (
    _leader_command_looks_usable,
    _validate_leader_receiver,
)
from team_agent.simple_yaml import dumps
from team_agent.state import apply_first_time_leader_binding, load_runtime_state, save_runtime_state, validate_leader_uuid_from_targets


REAL_CLAUDE_CODE_BINARY_COMMAND = "2.1.154"


class QuickStartExternalPaneAcceptanceTests(unittest.TestCase):
    def test_1_pane_is_usable_leader_accepts_real_2_1_154_command_when_cwd_matches_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-pane-usable-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            usable = legacy_panes._pane_is_usable_leader(pane, "claude_code", workspace)

        self.assertTrue(usable, pane)

    def test_2_resolve_leader_pane_adopts_current_client_with_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-resolve-current-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            with patch("team_agent._legacy_pane_discovery.run_cmd", side_effect=_tmux_run_cmd(current=pane, panes=[])):
                try:
                    resolved, discovery = runtime._resolve_leader_pane(
                        None,
                        "claude_code",
                        workspace=workspace,
                        require_current=True,
                    )
                except TeamAgentRuntimeError as exc:
                    self.fail(f"current client pane with matching cwd must be adopted, got: {exc}")

        self.assertEqual(discovery, "current_client")
        self.assertEqual(resolved["pane_id"], "%3622")

    def test_3_resolve_leader_pane_rejects_current_client_when_cwd_does_not_match_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-wanted-") as wanted, tempfile.TemporaryDirectory(prefix="ta-028-other-") as other:
            workspace = Path(wanted)
            pane = _pane("%3622", Path(other), command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            with patch("team_agent._legacy_pane_discovery.run_cmd", side_effect=_tmux_run_cmd(current=pane, panes=[])):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime._resolve_leader_pane(
                        None,
                        "claude_code",
                        workspace=workspace,
                        require_current=True,
                    )

    def test_4_quick_start_facing_error_does_not_recommend_nonexistent_pane_option(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-quick-start-error-") as wanted, tempfile.TemporaryDirectory(prefix="ta-028-other-") as other:
            workspace = Path(wanted)
            pane = _pane("%3622", Path(other), command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            with patch("team_agent._legacy_pane_discovery.run_cmd", side_effect=_tmux_run_cmd(current=pane, panes=[])):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime._resolve_leader_pane(
                        None,
                        "claude_code",
                        workspace=workspace,
                        require_current=True,
                    )

        message = str(ctx.exception)
        self.assertIn("could not locate a tmux-managed leader pane", message)
        self.assertNotIn("--pane", message)

    def test_5_first_time_leader_binding_does_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-delivery-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "uuid-owner",
            }
            state: dict = {}
            identity = {
                "leader_session_uuid": "uuid-owner",
                "machine_fingerprint": "machine-a",
            }
            result = apply_first_time_leader_binding(
                workspace,
                state,
                dict(receiver),
                dict(pane),
                identity,
                source="launch",
            )

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_pane_wrong_command", result)

    def test_6_worker_to_leader_receiver_validation_does_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-validate-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "uuid-owner",
            }
            with patch("team_agent._legacy_pane_discovery._tmux_pane_info", return_value=dict(pane)), patch(
                "team_agent.messaging.leader_panes.run_cmd",
                return_value=Mock(returncode=0, stdout="leader idle\n", stderr=""),
            ):
                result = _validate_leader_receiver(dict(receiver))

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_pane_wrong_command", result)

    def test_7_receiver_claim_does_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-claim-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            claim_state = {
                "team_owner": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "uuid-owner",
                    "machine_fingerprint": "machine-a",
                },
                "leader_receiver": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "uuid-owner",
                    "owner_epoch": 1,
                },
            }
            claim_candidate = dict(pane)
            claim_candidate["provider"] = "claude_code"
            result = claim_leader_receiver(
                workspace,
                claim_state,
                claim_candidate,
                EventLog(workspace),
                confirm=True,
            )

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "wrong_command", result)

    def test_8_leader_command_looks_usable_accepts_any_non_empty_command(self) -> None:
        for command in [REAL_CLAUDE_CODE_BINARY_COMMAND, "node", "custom-agent-cli", "/opt/bin/some-wrapper"]:
            with self.subTest(command=command):
                self.assertTrue(_leader_command_looks_usable(command, "claude_code"))

        self.assertFalse(_leader_command_looks_usable("", "claude_code"))

    def test_9_attach_receiver_rebind_helpers_do_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-leader-helpers-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            command_only_pane = dict(pane)
            command_only_pane.pop("leader_session_uuid", None)
            state = {"leader_receiver": {"pane_id": "%old", "provider": "claude_code", "leader_session_uuid": "uuid-owner"}}
            receiver = {"pane_id": "%old", "provider": "claude_code", "leader_session_uuid": "uuid-owner"}
            owner_record = {"pane_id": "%old", "provider": "claude_code", "leader_session_uuid": "uuid-owner"}
            targets = {"ok": True, "targets": [dict(pane)]}

            eligibility = _caller_pane_eligibility(dict(command_only_pane), workspace)
            readopt = _try_readopt_leader_pane(
                workspace,
                state,
                receiver,
                dict(pane),
                targets,
                owner_record,
                "claude_code",
                "manual",
                EventLog(workspace),
            )

        self.assertTrue(_pane_is_live_leader(command_only_pane), command_only_pane)
        self.assertTrue(eligibility.get("ok"), eligibility)
        self.assertIsNotNone(readopt, "attach/readopt must not reject 2.1.154 by command name")

    def test_10_state_uuid_validation_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-receiver-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "owner-uuid-from-state",
            }
            targets = {"ok": True, "targets": [dict(pane)]}

            result = validate_leader_uuid_from_targets(dict(receiver), targets)

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_uuid_missing", result)

    def test_11_receiver_validation_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-receiver-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "owner-uuid-from-state",
            }
            with patch("team_agent._legacy_pane_discovery._tmux_pane_info", return_value=dict(pane)), patch(
                "team_agent.messaging.leader_panes.run_cmd",
                return_value=Mock(returncode=0, stdout="leader idle\n", stderr=""),
            ):
                result = _validate_leader_receiver(dict(receiver))

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_uuid_missing", result)

    def test_12_receiver_claim_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-claim-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            claim_state = {
                "team_owner": {
                    "pane_id": "%3622",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "machine_fingerprint": "machine-a",
                },
                "leader_receiver": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "owner_epoch": 1,
                },
            }
            candidate = dict(pane)
            candidate["provider"] = "claude_code"
            result = claim_leader_receiver(workspace, claim_state, candidate, EventLog(workspace), confirm=True)

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "uuid_mismatch", result)

    def test_13_different_live_pane_still_cannot_claim_over_owner_without_uuid(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-isolation-") as tmp:
            workspace = Path(tmp)
            candidate = _pane("%not-owner", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            candidate["provider"] = "claude_code"
            claim_state = {
                "team_owner": {
                    "pane_id": "%owner",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "machine_fingerprint": "machine-a",
                },
                "leader_receiver": {
                    "pane_id": "%owner",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "owner_epoch": 1,
                },
            }
            result = claim_leader_receiver(workspace, claim_state, candidate, EventLog(workspace), confirm=True)

        self.assertFalse(result.get("ok"), result)
        self.assertIn(result.get("reason"), {"uuid_mismatch", "owner_pane_mismatch"}, result)

    def test_14_restart_preserves_top_level_spec_session_and_team_dir_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-restart-identity-") as tmp:
            workspace = Path(tmp)
            spec, spec_path, team_dir = _write_current_team_spec(workspace)
            save_runtime_state(
                workspace,
                _restartable_state(workspace, spec, spec_path, team_dir),
            )
            started_windows: set[str] = set()

            with patch("team_agent.runtime.run_cmd", side_effect=_fake_tmux_run_cmd(started_windows)), patch(
                "team_agent.runtime.start_coordinator",
                return_value={"ok": True, "pid": 123, "status": "started"},
            ):
                result = runtime.restart(workspace, team="current")

            state_after = load_runtime_state(workspace)

        self.assertTrue(result.get("ok"), result)
        self.assertTrue(state_after.get("spec_path"), state_after)
        self.assertTrue(state_after.get("session_name"), state_after)
        self.assertTrue(state_after.get("team_dir"), state_after)

    def test_15_send_resolves_team_spec_from_team_dir_when_spec_path_is_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-send-team-dir-") as tmp:
            workspace = Path(tmp)
            spec, _spec_path, team_dir = _write_current_team_spec(workspace)
            state = _restartable_state(workspace, spec, None, team_dir)
            state.pop("spec_path", None)
            state["teams"]["current"].pop("spec_path", None)
            save_runtime_state(workspace, state)

            with patch(
                "team_agent.messaging.send._deliver_pending_message",
                return_value={"ok": True, "status": "submitted", "message_id": "msg-delivered"},
            ):
                try:
                    result = runtime.send_message(
                        workspace,
                        "fake_impl",
                        "hello despite missing spec_path",
                        sender="leader",
                        requires_ack=False,
                        wait_visible=False,
                        block_until_delivered=False,
                        team="current",
                    )
                except Exception as exc:
                    self.fail(f"send must resolve spec from team_dir instead of falling back to missing root spec: {exc}")

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "target_not_in_team", result)


def _pane(pane_id: str, cwd: Path, *, command: str) -> dict[str, str]:
    return {
        "pane_id": pane_id,
        "session_name": "remote-control",
        "window_index": "1",
        "window_name": "leader",
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": command,
        "pane_active": "1",
        "pane_current_path": str(cwd),
        "session_attached": "1",
        "pane_in_mode": "0",
    }


def _tmux_line(pane: dict[str, str]) -> str:
    return "\t".join(
        [
            pane["pane_id"],
            pane["session_name"],
            pane["window_index"],
            pane["window_name"],
            pane["pane_index"],
            pane["pane_tty"],
            pane["pane_current_command"],
            pane["pane_active"],
            pane["pane_current_path"],
            pane["session_attached"],
            pane["pane_in_mode"],
        ]
    )


def _tmux_run_cmd(*, current: dict[str, str] | None, panes: list[dict[str, str]]):
    def fake_run_cmd(args: list[str], timeout: int = 20):
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
            if current is None:
                proc.returncode = 1
                proc.stderr = "no current client"
            else:
                proc.stdout = _tmux_line(current)
            return proc
        if args[:3] == ["tmux", "list-panes", "-a"]:
            proc.stdout = "\n".join(_tmux_line(pane) for pane in panes)
            return proc
        raise AssertionError(args)

    return fake_run_cmd


def _write_current_team_spec(workspace: Path) -> tuple[dict, Path, Path]:
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    spec = _fake_spec(workspace)
    spec["team"]["name"] = "current"
    spec["runtime"]["session_name"] = "team-current"
    spec_path = team_dir / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec, spec_path, team_dir


def _restartable_state(workspace: Path, spec: dict, spec_path: Path | None, team_dir: Path) -> dict:
    state = {
        "workspace": str(workspace),
        "spec_path": str(spec_path) if spec_path is not None else None,
        "team_dir": str(team_dir),
        "session_name": spec["runtime"]["session_name"],
        "active_team_key": "current",
        "leader": spec["leader"],
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
    }
    compact = dict(state)
    compact.pop("teams", None)
    state["teams"] = {"current": dict(compact)}
    return state


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
