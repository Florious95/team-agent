from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

from team_agent.quality_gates import load_line_count_allowlist


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "tools" / "check_line_count_gate.py"
SPEC = importlib.util.spec_from_file_location("check_line_count_gate", SCRIPT)
check_line_count_gate = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(check_line_count_gate)


class AllowlistSchemaConsistencyTests(unittest.TestCase):
    def test_quality_gate_and_cli_loader_parse_same_new_schema(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-allowlist-schema-") as tmp:
            path = Path(tmp) / "line_count_allowlist.json"
            path.write_text(
                (
                    '{"approved_exceptions": {"src/team_agent/runtime.py": {"max_lines": 1000}}, '
                    '"temporary_debt": {"src/team_agent/legacy.py": {"reason": "diagnostic only"}}}'
                ),
                encoding="utf-8",
            )

            quality_payload = load_line_count_allowlist(path)
            cli_payload, cli_error = check_line_count_gate._load_allowlist(path)

            self.assertIsNone(cli_error)
            self.assertEqual(cli_payload, quality_payload)
            self.assertEqual(set(quality_payload), {"approved_exceptions", "temporary_debt"})

    def test_quality_gate_and_cli_loader_reject_legacy_schema_with_same_message(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-allowlist-legacy-") as tmp:
            path = Path(tmp) / "line_count_allowlist.json"
            path.write_text('{"temporary_allowlist": {"src/team_agent/runtime.py": {"max_lines": 1000}}}', encoding="utf-8")

            with self.assertRaises(ValueError) as quality_ctx:
                load_line_count_allowlist(path)
            _payload, cli_error = check_line_count_gate._load_allowlist(path)

            self.assertEqual(cli_error, str(quality_ctx.exception))
            self.assertIn("unexpected top-level key(s): temporary_allowlist", cli_error)
            self.assertIn("approved_exceptions", cli_error)
            self.assertIn("temporary_debt", cli_error)


if __name__ == "__main__":
    unittest.main(verbosity=2)
