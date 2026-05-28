from __future__ import annotations

import re
import unittest
from pathlib import Path


class PositiveSourceLintAcceptanceTests(unittest.TestCase):
    def test_c24_no_reverse_enumeration_sort_take_first_idiom_in_positive_source_paths(self) -> None:
        paths = [
            _repo_root() / "src/team_agent/state.py",
            _repo_root() / "src/team_agent/messaging/leader_panes.py",
            _repo_root() / "src/team_agent/cli/parser.py",
        ]
        paths.extend(sorted((_repo_root() / "src/team_agent/messaging").glob("*.py")))
        paths.extend(sorted((_repo_root() / "src/team_agent/mcp_server").glob("*.py")))
        violations: list[str] = []
        for path in paths:
            text = path.read_text(encoding="utf-8")
            compact = re.sub(r"\s+", " ", text)
            if "setdefault(team_state_key" in text:
                violations.append(f"{path.relative_to(_repo_root())}: top-level state setdefault candidate")
            if re.search(r"list[-_]panes|list[-_]windows|list[-_]clients", text):
                violations.append(f"{path.relative_to(_repo_root())}: tmux reverse enumeration")
            if "ranked = sorted" in compact or "best = [" in compact or "sorted(candidates" in compact:
                violations.append(f"{path.relative_to(_repo_root())}: heuristic sort/take-first candidate ranking")
            if "pane_active" in text or "current_client" in text:
                violations.append(f"{path.relative_to(_repo_root())}: owner/candidate decision mentions pane_active/current_client heuristic")
        self.assertEqual(violations, [])


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


if __name__ == "__main__":
    unittest.main(verbosity=2)
