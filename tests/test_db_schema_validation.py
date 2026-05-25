from __future__ import annotations

import importlib.util
import re
import sqlite3
import unittest
from pathlib import Path

_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})

from team_agent import coordinator


class DatabaseSchemaValidationTests(unittest.TestCase):
    def test_wrong_schema_refuses_with_expected_and_actual_columns(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-db-schema-validation-") as tmp:
            workspace = Path(tmp)
            db_path = workspace / ".team" / "runtime" / "team.db"
            db_path.parent.mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(db_path)
            try:
                conn.execute(
                    """
                    create table results (
                      result_id text primary key,
                      task_id text not null,
                      agent_id text not null,
                      envelope text not null,
                      created_at text not null
                    )
                    """
                )
                conn.execute("pragma user_version = 1")
                conn.commit()
            finally:
                conn.close()

            health = coordinator.message_store_schema_health(workspace)

        self.assertFalse(health["schema_ok"])
        self.assertEqual(health["reason"], "schema_mismatch")
        self.assertEqual(health["table"], "results")
        self.assertIn("status", health["expected_columns"])
        self.assertNotIn("status", health["actual_columns"])
        self.assertIn("message_store_schema_version", health["schema"])

    def test_message_store_writes_use_explicit_column_lists(self) -> None:
        store_sources = "\n".join(
            path.read_text(encoding="utf-8")
            for path in (Path(__file__).parents[1] / "src" / "team_agent" / "message_store").glob("*.py")
        )
        implicit_inserts = re.findall(r"insert\s+into\s+\w+\s+values\s*\(", store_sources, flags=re.IGNORECASE)
        self.assertEqual(implicit_inserts, [], "message_store inserts must name columns explicitly")


if __name__ == "__main__":
    unittest.main(verbosity=2)
