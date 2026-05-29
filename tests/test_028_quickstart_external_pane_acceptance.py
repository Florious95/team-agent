from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock
from unittest.mock import patch

from team_agent import runtime
from team_agent import _legacy_pane_discovery as legacy_panes
from team_agent.errors import RuntimeError as TeamAgentRuntimeError


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
