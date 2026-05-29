from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock
from unittest.mock import patch

from team_agent import runtime
from team_agent import _legacy_pane_discovery as legacy_panes
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
from team_agent.state import apply_first_time_leader_binding


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


if __name__ == "__main__":
    unittest.main(verbosity=2)
