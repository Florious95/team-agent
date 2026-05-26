from __future__ import annotations

import unittest
from pathlib import Path

from team_agent.messaging.tmux_prompt import detect_non_input_scrollback


FIXTURES = Path(__file__).with_name("fixtures") / "pane_captures"


class PaneStateClassifierAcceptanceTests(unittest.TestCase):
    def test_user_confirmed_codex_trust_prompt_is_awaiting_trust_prompt(self) -> None:
        capture = _fixture("codex-awaiting-trust-slice2-103705-worker.txt")

        self.assertEqual(detect_non_input_scrollback(capture), "codex_trust_prompt")

    def test_restart_codex_trust_prompt_is_awaiting_trust_prompt(self) -> None:
        capture = _fixture("codex-awaiting-trust-slice2-restart-130040-worker.txt")

        self.assertEqual(detect_non_input_scrollback(capture), "codex_trust_prompt")

    def test_codex_idle_launch_with_old_trust_history_is_input_ready(self) -> None:
        capture = _fixture("codex-input-ready-smoke-20260525T073435-worker-b-launch.txt")

        self.assertIsNone(detect_non_input_scrollback(capture))

    def test_codex_idle_status_with_old_trust_history_is_input_ready(self) -> None:
        capture = _fixture("codex-input-ready-smoke-20260525T071651-worker-a-status.txt")

        self.assertIsNone(detect_non_input_scrollback(capture))

    def test_slice2_claude_idle_leader_prompt_is_input_ready(self) -> None:
        capture = _fixture("claude-input-ready-slice2-103705-leader.txt")

        self.assertIsNone(detect_non_input_scrollback(capture))

    def test_unknown_noise_fixture_is_unclassified(self) -> None:
        capture = _fixture("unknown-unclassified-noise.txt")

        self.assertIsNone(detect_non_input_scrollback(capture))


def _fixture(name: str) -> str:
    return (FIXTURES / name).read_text(encoding="utf-8")


if __name__ == "__main__":
    unittest.main(verbosity=2)
