from __future__ import annotations

import sqlite3
from datetime import datetime, timezone


MESSAGE_COLUMNS = {
    "message_id",
    "task_id",
    "sender",
    "recipient",
    "reply_to",
    "requires_ack",
    "status",
    "content",
    "artifact_refs",
    "created_at",
    "updated_at",
    "delivered_at",
    "acknowledged_at",
    "error",
    "delivery_attempts",
}
RESULT_COLUMNS = {"result_id", "task_id", "agent_id", "envelope", "status", "created_at"}
SCHEDULED_EVENT_COLUMNS = {
    "id",
    "due_at",
    "target",
    "kind",
    "payload_json",
    "status",
    "created_at",
    "fired_at",
    "result_json",
}
DELIVERY_TOKEN_COLUMNS = {
    "message_id",
    "unique_token",
    "injected_at",
    "visible_at",
    "consumed_at",
    "failed_at",
    "failure_reason",
}
AGENT_HEALTH_COLUMNS = {
    "agent_id",
    "status",
    "last_output_at",
    "context_usage_pct",
    "current_task_id",
    "updated_at",
}
PEER_ALLOWLIST_COLUMNS = {"a", "b", "created_at"}
RESULT_WATCHER_COLUMNS = {
    "watcher_id",
    "task_id",
    "agent_id",
    "message_id",
    "leader_id",
    "status",
    "created_at",
    "completed_at",
    "result_id",
    "notified_message_id",
    "error",
}


def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat()

SCHEMA_VERSION = 2


def _table_columns(conn: sqlite3.Connection, table: str) -> set[str]:
    return {row[1] for row in conn.execute(f"pragma table_info({table})").fetchall()}


def _ensure_table_columns(
    conn: sqlite3.Connection,
    table: str,
    required: set[str],
    migrations: dict[str, str] | None = None,
) -> None:
    columns = _table_columns(conn, table)
    missing = required - columns
    migrations = migrations or {}
    unsupported = missing - set(migrations)
    if unsupported:
        names = ", ".join(sorted(unsupported))
        raise RuntimeError(f"team.db table {table} is missing required column(s): {names}")
    for name in sorted(missing):
        conn.execute(migrations[name])


def initialize_schema(conn: sqlite3.Connection) -> None:
    with conn:
        conn.execute(
            """
            create table if not exists messages (
              message_id text primary key,
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
            """
        )
        conn.execute(
            """
            create table if not exists results (
              result_id text primary key,
              task_id text not null,
              agent_id text not null,
              envelope text not null,
              status text not null,
              created_at text not null
            )
            """
        )
        conn.execute(
            """
            create table if not exists scheduled_events (
              id integer primary key,
              due_at text not null,
              target text not null,
              kind text not null,
              payload_json text not null,
              status text not null,
              created_at text not null,
              fired_at text,
              result_json text
            )
            """
        )
        conn.execute(
            """
            create table if not exists delivery_tokens (
              message_id text primary key,
              unique_token text not null,
              injected_at text not null,
              visible_at text,
              consumed_at text,
              failed_at text,
              failure_reason text
            )
            """
        )
        conn.execute(
            """
            create table if not exists agent_health (
              agent_id text primary key,
              status text not null,
              last_output_at text,
              context_usage_pct integer,
              current_task_id text,
              updated_at text not null
            )
            """
        )
        conn.execute(
            """
            create table if not exists peer_allowlist (
              a text not null,
              b text not null,
              created_at text not null,
              primary key (a, b)
            )
            """
        )
        conn.execute(
            """
            create table if not exists result_watchers (
              watcher_id text primary key,
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
            """
        )
        _ensure_table_columns(
            conn,
            "messages",
            MESSAGE_COLUMNS,
            {
                "delivery_attempts": (
                    "alter table messages add column delivery_attempts integer not null default 0"
                )
            },
        )
        _ensure_table_columns(conn, "results", RESULT_COLUMNS)
        _ensure_table_columns(conn, "scheduled_events", SCHEDULED_EVENT_COLUMNS)
        _ensure_table_columns(conn, "delivery_tokens", DELIVERY_TOKEN_COLUMNS)
        _ensure_table_columns(conn, "agent_health", AGENT_HEALTH_COLUMNS)
        _ensure_table_columns(conn, "peer_allowlist", PEER_ALLOWLIST_COLUMNS)
        _ensure_table_columns(conn, "result_watchers", RESULT_WATCHER_COLUMNS)
        conn.execute(f"pragma user_version = {SCHEMA_VERSION}")
