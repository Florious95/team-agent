from __future__ import annotations

import shutil
import sqlite3
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable


MANAGED_TABLE_LAYOUTS: dict[str, tuple[str, ...]] = {
    "messages": (
        "message_id", "owner_team_id", "task_id", "sender", "recipient", "reply_to",
        "requires_ack", "status", "content", "artifact_refs", "created_at", "updated_at",
        "delivered_at", "acknowledged_at", "error", "delivery_attempts",
    ),
    "results": ("result_id", "owner_team_id", "task_id", "agent_id", "envelope", "status", "created_at"),
    "scheduled_events": (
        "id", "owner_team_id", "due_at", "target", "kind", "payload_json", "status",
        "created_at", "fired_at", "result_json",
    ),
    "delivery_tokens": (
        "message_id", "unique_token", "injected_at", "visible_at", "consumed_at",
        "failed_at", "failure_reason",
    ),
    "agent_health": (
        "owner_team_id", "agent_id", "status", "last_output_at", "context_usage_pct",
        "current_task_id", "updated_at",
    ),
    "peer_allowlist": ("a", "b", "created_at"),
    "result_watchers": (
        "watcher_id", "owner_team_id", "task_id", "agent_id", "message_id", "leader_id",
        "status", "created_at", "completed_at", "result_id", "notified_message_id", "error",
    ),
    "leader_notification_log": (
        "result_id", "owner_team_id", "owner_epoch", "leader_session_uuid",
        "notified_message_id", "notified_at", "leader_pane_id_at_notify", "envelope_content_hash",
    ),
}


CREATE_TABLE_SQL: dict[str, str] = {
    "messages": """
        create table if not exists {table} (
          message_id text primary key,
          owner_team_id text,
          task_id text,
          sender text,
          recipient text,
          reply_to text,
          requires_ack integer,
          status text,
          content text,
          artifact_refs text,
          created_at text,
          updated_at text,
          delivered_at text,
          acknowledged_at text,
          error text,
          delivery_attempts integer not null default 0
        )
    """,
    "results": """
        create table if not exists {table} (
          result_id text primary key,
          owner_team_id text,
          task_id text not null,
          agent_id text not null,
          envelope text not null,
          status text not null,
          created_at text not null
        )
    """,
    "scheduled_events": """
        create table if not exists {table} (
          id integer primary key,
          owner_team_id text,
          due_at text not null,
          target text not null,
          kind text not null,
          payload_json text not null,
          status text not null,
          created_at text not null,
          fired_at text,
          result_json text
        )
    """,
    "delivery_tokens": """
        create table if not exists {table} (
          message_id text primary key,
          unique_token text not null,
          injected_at text not null,
          visible_at text,
          consumed_at text,
          failed_at text,
          failure_reason text
        )
    """,
    "agent_health": """
        create table if not exists {table} (
          owner_team_id text,
          agent_id text not null,
          status text not null,
          last_output_at text,
          context_usage_pct integer,
          current_task_id text,
          updated_at text not null,
          unique(owner_team_id, agent_id)
        )
    """,
    "peer_allowlist": """
        create table if not exists {table} (
          a text not null,
          b text not null,
          created_at text not null,
          primary key (a, b)
        )
    """,
    "result_watchers": """
        create table if not exists {table} (
          watcher_id text primary key,
          owner_team_id text,
          task_id text,
          agent_id text,
          message_id text,
          leader_id text not null,
          status text not null,
          created_at text not null,
          completed_at text,
          result_id text,
          notified_message_id text,
          error text
        )
    """,
    "leader_notification_log": """
        create table if not exists {table} (
          result_id text not null,
          owner_team_id text not null default '',
          owner_epoch integer not null default 0,
          leader_session_uuid text,
          notified_message_id text not null,
          notified_at text not null,
          leader_pane_id_at_notify text,
          envelope_content_hash text,
          primary key (result_id, owner_team_id, owner_epoch)
        )
    """,
}


INDEX_SQL: tuple[str, ...] = (
    "create index if not exists idx_leader_notification_log_uuid on leader_notification_log(leader_session_uuid, notified_at)",
    "create index if not exists idx_leader_notification_log_team_epoch on leader_notification_log(owner_team_id, owner_epoch, notified_at)",
    "create index if not exists idx_messages_owner_team_id on messages(owner_team_id)",
    "create index if not exists idx_scheduled_events_owner_team_id on scheduled_events(owner_team_id)",
    "create index if not exists idx_agent_health_owner_team_id on agent_health(owner_team_id)",
    "create index if not exists idx_result_watchers_owner_team_id on result_watchers(owner_team_id)",
)


SCHEMA_MIGRATIONS: dict[int, Callable[[sqlite3.Connection], None]] = {
    1: lambda _conn: None,
    2: lambda _conn: None,
    3: lambda _conn: None,
}


def table_layout(conn: sqlite3.Connection, table: str) -> tuple[str, ...]:
    return tuple(str(row["name"]) for row in conn.execute(f"pragma table_info({table})").fetchall())


def ensure_schema_indexes(conn: sqlite3.Connection) -> None:
    for statement in INDEX_SQL:
        conn.execute(statement)


def schema_diagnosis(workspace: Path, *, schema_version: int) -> dict[str, Any]:
    db_path = workspace / ".team" / "runtime" / "team.db"
    backup_path = _backup_path(db_path, _read_user_version(db_path))
    if not db_path.exists():
        return {
            "ok": True,
            "status": "missing",
            "db_path": str(db_path),
            "schema_version": schema_version,
            "user_version": 0,
            "layout_diffs": {},
            "recommended_action": "No team.db exists yet; initialize_schema will create it on first use.",
            "would_backup_path": str(backup_path),
        }
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        user_version = _pragma_user_version(conn)
        diffs = _layout_diffs(conn)
    finally:
        conn.close()
    return {
        "ok": not diffs and user_version == schema_version,
        "status": "ok" if not diffs and user_version == schema_version else "schema_repair_available",
        "db_path": str(db_path),
        "schema_version": schema_version,
        "user_version": user_version,
        "layout_diffs": diffs,
        "recommended_action": "run team-agent doctor --fix-schema --json" if diffs else "none",
        "would_backup_path": str(backup_path),
    }


def ensure_table_layout(
    conn: sqlite3.Connection,
    *,
    schema_version: int,
    db_path: Path | None = None,
) -> list[dict[str, Any]]:
    # 0.2.6 CI hotfix #3: hold the SQLite RESERVED lock across the diff
    # read AND the rebuild writes. Earlier code only acquired BEGIN
    # IMMEDIATE inside ``_rebuild_tables`` — so a concurrent writer
    # could squeeze a row in between ``_layout_diffs`` and the rebuild,
    # and the post-rebuild row-count drift check fired with
    # before != after even though no schema layout was clobbered.
    # Wrapping the whole sequence makes the drift check an invariant
    # (no writer can interleave once we hold the lock).
    conn.row_factory = sqlite3.Row
    _run_version_migrations(conn, schema_version)
    started_tx = False
    if not conn.in_transaction:
        conn.execute("BEGIN IMMEDIATE")
        started_tx = True
    try:
        diffs = _layout_diffs(conn)
        if not diffs:
            if started_tx:
                conn.execute("COMMIT")
                started_tx = False
            return []
        db_path = db_path or _db_path_from_conn(conn)
        if db_path is None:
            raise RuntimeError("cannot rebuild team.db layout without a database path")
        backup_path = _backup_path(db_path, _pragma_user_version(conn))
        backup_path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(db_path, backup_path)
        events = _rebuild_tables(conn, diffs, backup_path)
        if started_tx:
            conn.execute("COMMIT")
            started_tx = False
    except Exception:
        if started_tx and conn.in_transaction:
            try:
                conn.execute("ROLLBACK")
            except sqlite3.Error:
                pass
        raise
    _emit_rebuild_events(db_path, events)
    return events


def fix_schema_layout(workspace: Path, *, schema_version: int) -> dict[str, Any]:
    db_path = workspace / ".team" / "runtime" / "team.db"
    if not db_path.exists():
        return schema_diagnosis(workspace, schema_version=schema_version)
    lock_check = _db_lock_status(db_path)
    if lock_check is not None:
        from team_agent.events import EventLog
        EventLog(workspace).write("schema.layout_rebuild_blocked", reason=lock_check, db_path=str(db_path))
        return {"ok": False, "status": "blocked", "reason": lock_check, "event": "schema.layout_rebuild_blocked", "db_path": str(db_path)}
    conn = sqlite3.connect(db_path, timeout=30.0, isolation_level=None)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA busy_timeout=30000")
    try:
        events = ensure_table_layout(conn, schema_version=schema_version, db_path=db_path)
        conn.execute(f"pragma user_version = {schema_version}")
    finally:
        conn.close()
    diagnosis = schema_diagnosis(workspace, schema_version=schema_version)
    diagnosis.update({"fixed": True, "rebuilds": events})
    return diagnosis


def _run_version_migrations(conn: sqlite3.Connection, schema_version: int) -> None:
    current = _pragma_user_version(conn)
    for version in range(current, schema_version + 1):
        SCHEMA_MIGRATIONS.get(version, lambda _conn: None)(conn)


def _layout_diffs(conn: sqlite3.Connection) -> dict[str, dict[str, Any]]:
    diffs: dict[str, dict[str, Any]] = {}
    if not any(_table_exists(conn, table) for table in MANAGED_TABLE_LAYOUTS):
        return diffs
    for table, expected in MANAGED_TABLE_LAYOUTS.items():
        if not _table_exists(conn, table):
            diffs[table] = {
                "expected": list(expected),
                "actual": [],
                "missing": True,
            }
            continue
        actual = table_layout(conn, table)
        if actual != expected:
            diffs[table] = {
                "expected": list(expected),
                "actual": list(actual),
            }
    return diffs


def _rebuild_tables(
    conn: sqlite3.Connection,
    diffs: dict[str, dict[str, Any]],
    backup_path: Path,
) -> list[dict[str, Any]]:
    # Caller (``ensure_table_layout``) now holds the BEGIN IMMEDIATE for
    # the full diff+rebuild window, so the drift check between ``before``
    # and ``after`` is an invariant rather than a concurrency race. The
    # inner BEGIN/COMMIT/ROLLBACK was removed alongside that move (a
    # nested BEGIN raises in SQLite and would mask the outer lock).
    events: list[dict[str, Any]] = []
    for table, diff in diffs.items():
        expected = MANAGED_TABLE_LAYOUTS[table]
        actual = tuple(diff["actual"])
        before = 0 if diff.get("missing") else _table_count(conn, table)
        if diff.get("missing"):
            conn.execute(CREATE_TABLE_SQL[table].format(table=table))
            after = _table_count(conn, table)
            if after != before:
                raise RuntimeError(f"schema rebuild row count changed for {table}: {before} != {after}")
            events.append({
                "table": table,
                "from_layout_columns": [],
                "to_layout_columns": list(expected),
                "backup_path": str(backup_path),
                "row_count_before": before,
                "row_count_after": after,
                "missing": True,
            })
            continue
        temp = f"__team_agent_rebuild_{table}"
        old = f"__team_agent_old_{table}"
        conn.execute(f"drop table if exists {temp}")
        conn.execute(f"drop table if exists {old}")
        conn.execute(CREATE_TABLE_SQL[table].format(table=temp))
        common = [column for column in expected if column in actual]
        column_sql = ", ".join(common)
        conn.execute(f"insert into {temp}({column_sql}) select {column_sql} from {table}")
        _maybe_fault_after_insert()
        conn.execute(f"alter table {table} rename to {old}")
        conn.execute(f"alter table {temp} rename to {table}")
        conn.execute(f"drop table {old}")
        after = _table_count(conn, table)
        if before != after:
            raise RuntimeError(f"schema rebuild row count changed for {table}: {before} != {after}")
        events.append({
            "table": table,
            "from_layout_columns": list(actual),
            "to_layout_columns": list(expected),
            "backup_path": str(backup_path),
            "row_count_before": before,
            "row_count_after": after,
        })
    ensure_schema_indexes(conn)
    return events


def _emit_rebuild_events(db_path: Path, events: list[dict[str, Any]]) -> None:
    workspace = _workspace_from_db_path(db_path)
    if workspace is None:
        return
    from team_agent.events import EventLog
    log = EventLog(workspace)
    for event in events:
        log.write("schema.layout_rebuild", **event)


def _db_lock_status(db_path: Path) -> str | None:
    conn = sqlite3.connect(db_path, timeout=0.0, isolation_level=None)
    try:
        conn.execute("BEGIN IMMEDIATE")
        conn.execute("ROLLBACK")
        return None
    except sqlite3.OperationalError as exc:
        message = str(exc).lower()
        if "locked" in message or "busy" in message:
            return "active_lock"
        raise
    finally:
        conn.close()


def _table_count(conn: sqlite3.Connection, table: str) -> int:
    row = conn.execute(f"select count(*) as n from {table}").fetchone()
    return int(row["n"])


def _table_exists(conn: sqlite3.Connection, table: str) -> bool:
    row = conn.execute(
        "select name from sqlite_master where type = 'table' and name = ?",
        (table,),
    ).fetchone()
    return row is not None


def _pragma_user_version(conn: sqlite3.Connection) -> int:
    row = conn.execute("pragma user_version").fetchone()
    return int(row["user_version"])


def _read_user_version(db_path: Path) -> int:
    if not db_path.exists():
        return 0
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        return _pragma_user_version(conn)
    finally:
        conn.close()


def _db_path_from_conn(conn: sqlite3.Connection) -> Path | None:
    row = conn.execute("pragma database_list").fetchone()
    if not row:
        return None
    filename = str(row["file"])
    return Path(filename) if filename else None


def _workspace_from_db_path(db_path: Path) -> Path | None:
    parts = db_path.parts
    if len(parts) >= 3 and parts[-3:] == (".team", "runtime", "team.db"):
        return db_path.parent.parent.parent
    return None


def _backup_path(db_path: Path, user_version: int) -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return db_path.with_name(f"team.db.pre-migration-{stamp}-from-v{user_version}.bak")


def _maybe_fault_after_insert() -> None:
    allowed_keys = {
        "TEAM_AGENT_SCHEMA_MIGRATION_CRASH_AT",
        "GAP46_TEST_CRASH",
        "GAP46_TEST_PARTIAL_REBUILD",
    }
    allowed_values = {"1", "crash", "partial", "after_insert_before_rename"}
    for key in allowed_keys:
        value = os.environ.get(key)
        if value in allowed_values:
            os._exit(97)
