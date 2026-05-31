from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.messaging.leader_panes import attempt_trust_auto_answer


FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "bug_082_codex_trust"


class Bug082CodexOwnWorkspaceTrustAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="bug082-trust-")
        self.root = Path(self._tmp_ctx.name).resolve()
        self.workspace = self.root / "workspace-root"
        self.workspace.mkdir()
        self.event_log = EventLog(self.workspace)
        self._env_backup = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)

    def tearDown(self) -> None:
        if self._env_backup is None:
            os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        else:
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = self._env_backup
        self._tmp_ctx.cleanup()

    def test_t1_quick_start_own_workspace_trust_prompt_auto_answers_by_default(self) -> None:
        result, mock_inject = self._answer(
            self.workspace,
            stage="quick_start",
            spec={"runtime": {"auto_trust_own_workspace": False}},
        )

        self.assert_auto_answered_with_one_enter(result, mock_inject)

    def test_t2_first_inject_own_workspace_auto_answer_is_idempotent_per_pane_and_capture(self) -> None:
        capture = self.trust_prompt(str(self.workspace))
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value={"ok": True}) as mock_inject:
            first = self.call_answer(self.workspace, capture, stage="first_inject")
            second = self.call_answer(self.workspace, capture, stage="first_inject")

        self.assertTrue(first["answered"], first)
        self.assertTrue(second.get("answered") or second.get("idempotent"), second)
        mock_inject.assert_called_once()
        self.assertEqual(mock_inject.call_args[0][:3], ("%worker", "1", "Enter"))

    def test_t3_takeover_restart_own_workspace_trust_prompt_auto_answers_by_default(self) -> None:
        result, mock_inject = self._answer(self.workspace, stage="takeover_restart")

        self.assert_auto_answered_with_one_enter(result, mock_inject)

    def test_foreign_same_basename_different_parent_is_prompted_to_leader_not_auto_answered(self) -> None:
        foreign_parent = self.root / "other-parent"
        foreign_parent.mkdir()
        foreign = foreign_parent / self.workspace.name
        foreign.mkdir()

        result, mock_inject = self._answer_foreign(foreign)

        self.assert_prompt_leader_without_answer(result, mock_inject)

    def test_foreign_symlink_prompt_path_resolves_outside_workspace_and_is_not_own(self) -> None:
        foreign = self.root / "foreign-target"
        foreign.mkdir()
        symlink = self.root / "workspace-root-link"
        try:
            symlink.symlink_to(foreign, target_is_directory=True)
        except OSError:
            self.skipTest("filesystem does not support symlinks")

        result, mock_inject = self._answer_foreign(symlink)

        self.assert_prompt_leader_without_answer(result, mock_inject)

    def test_foreign_substring_prefix_path_is_not_own_workspace(self) -> None:
        foreign = self.root / f"{self.workspace.name}-backup"
        foreign.mkdir()

        result, mock_inject = self._answer_foreign(foreign)

        self.assert_prompt_leader_without_answer(result, mock_inject)

    def test_foreign_prompt_without_path_does_not_reverse_infer_workspace_from_cwd(self) -> None:
        capture = self.trust_prompt_without_path()
        with self.poison_legacy_opt_ins(), patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = self.call_answer(self.workspace, capture, stage="first_inject")

        self.assert_prompt_leader_without_answer(result, mock_inject)

    def test_auto_answer_path_makes_no_provider_calls(self) -> None:
        with patch("team_agent.providers.get_adapter") as provider_lookup, \
             patch("team_agent.provider_cli.codex.subprocess.run") as provider_run:
            result, mock_inject = self._answer(self.workspace, stage="quick_start")

        self.assert_auto_answered_with_one_enter(result, mock_inject)
        provider_lookup.assert_not_called()
        provider_run.assert_not_called()

    def _answer(
        self,
        prompt_path: Path,
        *,
        stage: str,
        spec: dict[str, Any] | None = None,
    ) -> tuple[dict[str, Any], Any]:
        capture = self.trust_prompt(str(prompt_path))
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value={"ok": True}) as mock_inject:
            result = self.call_answer(self.workspace, capture, stage=stage, spec=spec)
        return result, mock_inject

    def _answer_foreign(self, prompt_path: Path) -> tuple[dict[str, Any], Any]:
        capture = self.trust_prompt(str(prompt_path))
        with self.poison_legacy_opt_ins(), patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = self.call_answer(self.workspace, capture, stage="first_inject")
        return result, mock_inject

    def call_answer(
        self,
        workspace: Path,
        capture: str,
        *,
        stage: str,
        spec: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        state = {
            "trust_auto_answer_stage": stage,
            "agent_id": "developer",
            "provider": "codex",
            "workspace_root": str(workspace),
        }
        return attempt_trust_auto_answer(
            workspace,
            "%worker",
            capture,
            self.event_log,
            spec=spec or {},
            state=state,
        )

    def poison_legacy_opt_ins(self):
        class Poison:
            def __enter__(inner_self):
                inner_self.old_env = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
                os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
                return inner_self

            def __exit__(inner_self, exc_type, exc, tb):
                if inner_self.old_env is None:
                    os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
                else:
                    os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = inner_self.old_env

        return Poison()

    def trust_prompt(self, path_text: str) -> str:
        return (FIXTURE_ROOT / "codex_trust_prompt_template.txt").read_text(encoding="utf-8").replace("__PATH__", path_text)

    def trust_prompt_without_path(self) -> str:
        return "\n".join(
            line
            for line in self.trust_prompt("__PATH__").splitlines()
            if "__PATH__" not in line
        )

    def assert_auto_answered_with_one_enter(self, result: dict[str, Any], mock_inject: Any) -> None:
        self.assertTrue(result.get("answered"), result)
        self.assertEqual(result.get("reason"), "trust_auto_answered")
        mock_inject.assert_called_once()
        self.assertEqual(mock_inject.call_args[0][:3], ("%worker", "1", "Enter"))
        self.assertTrue(mock_inject.call_args.kwargs.get("bypass_non_input_gate"))

    def assert_prompt_leader_without_answer(self, result: dict[str, Any], mock_inject: Any) -> None:
        self.assertFalse(result.get("answered"), result)
        self.assertEqual(result.get("action"), "prompt_leader", result)
        self.assertIn(result.get("reason"), {"foreign_workspace", "workspace_dir_mismatch"}, result)
        mock_inject.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
