from __future__ import annotations

import ast
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest.mock import patch

from tests.contracts.legacy_team_db_fixture import (
    CURRENT_LAYOUTS,
    FIXTURE_COUNTS,
    LEGACY_LAYOUTS,
    MIGRATED_TABLES,
    all_table_counts,
    build_legacy_workspace,
    table_count,
    table_layout,
)


REPO = Path(__file__).resolve().parents[1]
SRC = REPO / "src"


class SchemaMigrationAcceptanceTests(unittest.TestCase):
    def test_1_initialize_schema_checks_layout_before_column_presence_migrations(self) -> None:
        from team_agent.message_store import schema

        with tempfile.TemporaryDirectory(prefix="gap46-c1-") as tmp:
            workspace = Path(tmp)
            db_path = build_legacy_workspace(workspace)
            observed: list[tuple[str, tuple[str, ...]]] = []
            original = schema._ensure_table_columns

            def wrapped(conn: sqlite3.Connection, table: str, required: set[str], migrations=None) -> None:
                if table in MIGRATED_TABLES:
                    observed.append((table, tuple(row[1] for row in conn.execute(f"pragma table_info({table})"))))
                original(conn, table, required, migrations)

            conn = sqlite3.connect(db_path)
            try:
                with patch.object(schema, "_ensure_table_columns", side_effect=wrapped):
                    schema.initialize_schema(conn)
            finally:
                conn.close()

            first_by_table = {table: layout for table, layout in observed}
            self.assertEqual(first_by_table["messages"], CURRENT_LAYOUTS["messages"])
            self.assertNotEqual(first_by_table["messages"], LEGACY_LAYOUTS["messages"])

    def test_2_layout_rebuild_is_atomic_when_crashing_between_insert_and_rename(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap46-c2-") as tmp:
            workspace = Path(tmp)
            db_path = build_legacy_workspace(workspace)
            script = textwrap.dedent(
                f"""
                import os
                import sqlite3
                from pathlib import Path
                from team_agent.message_store.schema import initialize_schema

                os.environ["TEAM_AGENT_SCHEMA_MIGRATION_CRASH_AT"] = "after_insert_before_rename"
                conn = sqlite3.connect({str(db_path)!r})
                initialize_schema(conn)
                conn.close()
                """
            )
            proc = subprocess.run(
                [sys.executable, "-c", script],
                cwd=str(REPO),
                env={**os.environ, "PYTHONPATH": f"{SRC}{os.pathsep}{REPO}"},
                text=True,
                capture_output=True,
                timeout=10,
                check=False,
            )

            self.assertNotEqual(proc.returncode, 0, "fault injection must crash the migration process")
            layouts = {table: table_layout(db_path, table) for table in MIGRATED_TABLES}
            self.assertTrue(
                all(layouts[table] == LEGACY_LAYOUTS[table] for table in MIGRATED_TABLES)
                or all(layouts[table] == CURRENT_LAYOUTS[table] for table in MIGRATED_TABLES),
                layouts,
            )

    def test_3_backup_is_written_before_rewrite_and_preserves_original_row_counts(self) -> None:
        from team_agent.message_store import MessageStore

        with tempfile.TemporaryDirectory(prefix="gap46-c3-") as tmp:
            workspace = Path(tmp)
            db_path = build_legacy_workspace(workspace)
            before_counts = all_table_counts(db_path)

            MessageStore(workspace)

            backups = sorted((workspace / ".team" / "runtime").glob("team.db.pre-migration-*.bak"))
            self.assertGreaterEqual(len(backups), 1)
            backup_counts = all_table_counts(backups[-1])
            self.assertEqual(backup_counts, before_counts)

    def test_4_generic_rebuild_repairs_results_messages_scheduled_events_and_agent_health(self) -> None:
        from team_agent.message_store import MessageStore

        with tempfile.TemporaryDirectory(prefix="gap46-c4-") as tmp:
            workspace = Path(tmp)
            db_path = build_legacy_workspace(workspace)

            MessageStore(workspace)

            for table in MIGRATED_TABLES:
                self.assertEqual(table_layout(db_path, table), CURRENT_LAYOUTS[table], table)
                self.assertEqual(table_count(db_path, table), FIXTURE_COUNTS[table], table)

    def test_5_message_store_managed_table_access_uses_explicit_columns_and_named_rows(self) -> None:
        violations = _message_store_access_violations()
        self.assertEqual(violations, [])

    def test_6_user_version_migrations_chain_in_registered_order_to_schema_version(self) -> None:
        from team_agent.message_store import schema

        seen: list[int] = []

        def migration(version: int):
            def _run(conn: sqlite3.Connection) -> None:
                seen.append(version)
                conn.execute("create table if not exists migration_seen(version integer)")
                conn.execute("insert into migration_seen(version) values (?)", (version,))
            return _run

        conn = sqlite3.connect(":memory:")
        try:
            conn.execute("pragma user_version = 0")
            with patch.object(schema, "SCHEMA_VERSION", 3), \
                 patch.object(schema, "SCHEMA_MIGRATIONS", {1: migration(1), 2: migration(2), 3: migration(3)}, create=True):
                schema.initialize_schema(conn)
            version = conn.execute("pragma user_version").fetchone()[0]
        finally:
            conn.close()

        self.assertEqual(seen, [1, 2, 3])
        self.assertEqual(version, 3)

    def test_7_doctor_without_fix_schema_is_read_only_and_reports_layout_diff(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap46-c13-") as tmp:
            workspace = Path(tmp)
            db_path = build_legacy_workspace(workspace)
            before_hash = _sha256(db_path)

            proc = _run_team_agent_cli(["doctor", "--json"], workspace)

            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(_sha256(db_path), before_hash, "doctor without --fix-schema must not mutate team.db")
            payload = json.loads(proc.stdout)
            self.assertFalse(payload["coordinator"]["schema_ok"])
            self.assertIn("layout", json.dumps(payload, sort_keys=True).lower())
            self.assertIn("--fix-schema", json.dumps(payload, sort_keys=True))

    def test_8_doctor_fix_schema_rebuilds_with_backup_and_refuses_active_lock(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap46-c14-") as tmp:
            workspace = Path(tmp)
            db_path = build_legacy_workspace(workspace)
            lock = sqlite3.connect(db_path, timeout=1.0, isolation_level=None)
            try:
                lock.execute("BEGIN EXCLUSIVE")
                proc = _run_team_agent_cli(["doctor", "--fix-schema", "--json"], workspace)
            finally:
                lock.execute("ROLLBACK")
                lock.close()

            combined = f"{proc.stdout}\n{proc.stderr}"
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("schema.layout_rebuild_blocked", combined)
            self.assertIn("active_lock", combined)
            self.assertEqual(list((workspace / ".team" / "runtime").glob("team.db.pre-migration-*.bak")), [])

            proc = _run_team_agent_cli(["doctor", "--fix-schema", "--json"], workspace)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertGreaterEqual(len(list((workspace / ".team" / "runtime").glob("team.db.pre-migration-*.bak"))), 1)
            for table in MIGRATED_TABLES:
                self.assertEqual(table_layout(db_path, table), CURRENT_LAYOUTS[table], table)

    def test_9_layout_rebuild_audit_event_includes_row_count_equality(self) -> None:
        from team_agent.message_store import MessageStore

        with tempfile.TemporaryDirectory(prefix="gap46-c21-") as tmp:
            workspace = Path(tmp)
            build_legacy_workspace(workspace)

            MessageStore(workspace)

            events_path = workspace / ".team" / "logs" / "events.jsonl"
            self.assertTrue(events_path.exists(), "schema rebuild must emit audit events")
            events = [
                json.loads(line)
                for line in events_path.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            rebuilds = [event for event in events if event.get("event") == "schema.layout_rebuild"]
            rebuilt_tables = {event.get("table") for event in rebuilds}
            self.assertTrue(set(MIGRATED_TABLES).issubset(rebuilt_tables))
            for event in rebuilds:
                self.assertIn("backup_path", event)
                self.assertEqual(event["row_count_before"], event["row_count_after"])
                self.assertEqual(tuple(event["to_layout_columns"]), CURRENT_LAYOUTS[event["table"]])


def _run_team_agent_cli(args: list[str], workspace: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-c", "from team_agent.cli.parser import main; main()", *args],
        cwd=str(workspace),
        env={**os.environ, "PYTHONPATH": str(SRC)},
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _message_store_access_violations() -> list[str]:
    violations: list[str] = []
    for path in sorted((SRC / "team_agent" / "message_store").glob("*.py")):
        text = path.read_text(encoding="utf-8")
        lowered = " ".join(text.lower().split())
        if "select *" in lowered:
            violations.append(f"{path.relative_to(REPO)} uses SELECT *")
        tree = ast.parse(text, filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, ast.Subscript) and isinstance(node.slice, ast.Constant) and isinstance(node.slice.value, int):
                line = text.splitlines()[node.lineno - 1].strip()
                if "pragma table_info" in line:
                    continue
                violations.append(f"{path.relative_to(REPO)}:{node.lineno} uses positional row access")
    return violations


if __name__ == "__main__":
    unittest.main(verbosity=2)
