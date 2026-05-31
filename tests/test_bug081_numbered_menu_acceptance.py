from __future__ import annotations

import unittest
from pathlib import Path

from team_agent.messaging.tmux_prompt import detect_non_input_scrollback


FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "bug_081_numbered_menu"


class Bug081NumberedMenuAcceptanceTests(unittest.TestCase):
    def test_leader_markdown_numbered_prose_with_baked_spinner_is_not_numbered_menu(self) -> None:
        capture = _fixture("leader_markdown_numbered_prose_false_positive.txt")

        self.assertIsNone(detect_non_input_scrollback(capture))

    def test_real_interactive_provider_menus_remain_detected(self) -> None:
        cases = {
            "claude_resume_numbered_menu.txt": "numbered_menu",
            "codex_trust_prompt.txt": "codex_trust_prompt",
            "claude_trust_numbered_menu.txt": "numbered_menu",
            "y_n_confirm.txt": "y_n_confirm",
        }
        for fixture_name, expected in cases.items():
            with self.subTest(fixture=fixture_name):
                self.assertEqual(detect_non_input_scrollback(_fixture(fixture_name)), expected)


def _fixture(name: str) -> str:
    return (FIXTURE_ROOT / name).read_text(encoding="utf-8")


if __name__ == "__main__":
    unittest.main(verbosity=2)
