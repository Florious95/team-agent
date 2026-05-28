"""Acceptance contract for truncated Codex trust prompt workspace paths.

The runtime-held worker workspace cwd is the source of truth. The path rendered
in the Codex trust prompt is a guard, and real terminal captures can truncate it.
"""
from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.messaging.leader_panes import attempt_trust_auto_answer


FIXTURE_DIR = Path(__file__).parent / "fixtures" / "pane_captures"
REAL_WORKSPACE = Path(
    "/Users/alauda/team-agent-test/workspaces/0.2.4-bundled-20260528T014841Z-gap39"
)


def _ok_inject() -> dict[str, Any]:
    return {"ok": True}


def _trust_prompt_for(path_text: str) -> str:
    return (
        f"> You are in {path_text}\n\n"
        "  Do you trust the contents of this directory? Working with untrusted contents\n"
        "  comes with higher risk of prompt injection. Trusting the directory allows\n"
        "  project-local config, hooks, and exec policies to load.\n\n"
        "› 1. Yes, continue\n"
        "  2. No, quit\n\n"
        "  Press enter to continue\n"
    )


class TrustTruncatedWorkspaceAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="trust-truncated-workspace-")
        self.log_root = Path(self._tmp_ctx.name).resolve()
        self.event_log = EventLog(self.log_root)
        self._env_backup = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"

    def tearDown(self) -> None:
        if self._env_backup is None:
            os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        else:
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = self._env_backup
        self._tmp_ctx.cleanup()

    def _events(self) -> list[dict[str, Any]]:
        path = self.log_root / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def _answer(
        self,
        capture: str,
        workspace: Path = REAL_WORKSPACE,
        *,
        pane_width: int | None = None,
    ) -> tuple[dict[str, Any], Any]:
        state = {"pane_width": pane_width} if pane_width is not None else None
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()) as mock_inject:
            result = attempt_trust_auto_answer(
                workspace,
                "%worker",
                capture,
                self.event_log,
                spec={},
                state=state,
            )
        return result, mock_inject

    def test_real_codex_hard_tail_truncated_workspace_path_is_accepted(self) -> None:
        capture = (FIXTURE_DIR / "codex-trust-truncated-workspace-hard-tail.txt").read_text(encoding="utf-8")
        pane_width = int(
            (FIXTURE_DIR / "codex-trust-truncated-workspace-hard-tail.pane_width.txt").read_text(encoding="utf-8")
        )

        result, mock_inject = self._answer(capture, pane_width=pane_width)

        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")
        mock_inject.assert_called_once()
        answered = [ev for ev in self._events() if ev.get("event") == "leader_panes.trust_auto_answered"]
        self.assertEqual(len(answered), 1)
        self.assertEqual(answered[0]["workspace"], str(REAL_WORKSPACE))

    def test_middle_ellipsis_workspace_path_is_accepted_when_head_and_tail_match(self) -> None:
        for ellipsis in ("...", "…"):
            with self.subTest(ellipsis=ellipsis):
                display_path = (
                    "/Users/alauda/team-agent-test/workspaces/"
                    f"0.2.4-bundled-20260528T014{ellipsis}841Z-gap39"
                )

                result, mock_inject = self._answer(_trust_prompt_for(display_path), pane_width=120)

                self.assertTrue(result["answered"])
                self.assertEqual(result["reason"], "trust_auto_answered")
                mock_inject.assert_called_once()

    def test_full_untruncated_workspace_path_is_still_accepted(self) -> None:
        result, mock_inject = self._answer(_trust_prompt_for(str(REAL_WORKSPACE)), pane_width=140)

        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")
        mock_inject.assert_called_once()

    def test_genuinely_different_workspace_path_is_still_refused(self) -> None:
        result, mock_inject = self._answer(_trust_prompt_for("/some/other/place"), pane_width=120)

        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_inject.assert_not_called()
        refused = [ev for ev in self._events() if ev.get("event") == "leader_panes.trust_auto_answer_refused"]
        self.assertEqual(len(refused), 1)

    def test_ancestor_workspace_prompt_path_is_accepted_as_boundary_safe_truncation(self) -> None:
        ancestor = "/Users/alauda/team-agent-test/workspaces"

        capture = _trust_prompt_for(ancestor)
        pane_width = len(capture.splitlines()[0])

        result, mock_inject = self._answer(capture, pane_width=pane_width)

        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")
        mock_inject.assert_called_once()

    def test_shared_prefix_sibling_paths_are_refused_when_prompt_token_is_not_boundary_truncated(self) -> None:
        cases = [
            (
                Path("/tmp/team-agent-contract/repo-backup"),
                "/tmp/team-agent-contract/repo",
            ),
            (
                Path("/tmp/team-agent-contract/repo"),
                "/tmp/team-agent-contract/repo-backup",
            ),
        ]
        for workspace, captured_path in cases:
            with self.subTest(workspace=str(workspace), captured_path=captured_path):
                capture = _trust_prompt_for(captured_path)
                pane_width = len(capture.splitlines()[0]) + 10

                result, mock_inject = self._answer(capture, workspace, pane_width=pane_width)

                self.assertFalse(result["answered"])
                self.assertEqual(result["reason"], "workspace_dir_mismatch")
                mock_inject.assert_not_called()
