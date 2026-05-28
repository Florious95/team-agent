"""Deterministic legacy team.db synthesizer for Gap 46 schema migration tests.

The tables below are created with raw SQL on purpose. They model the physical
layout produced by older releases after ADD COLUMN migrations: owner_team_id is
present, but appended at the end instead of occupying the current canonical
position. Tests use this file as a stable fixture builder rather than checking a
binary sqlite database into git.
"""
from __future__ import annotations

import json
import sqlite3
from pathlib import Path


MIGRATED_TABLES = ("messages", "results", "scheduled_events", "agent_health")

CURRENT_LAYOUTS = {
    "messages": (
        "message_id",
        "owner_team_id",
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
    ),
    "results": (
        "result_id",
        "owner_team_id",
        "task_id",
        "agent_id",
        "envelope",
        "status",
        "created_at",
    ),
    "scheduled_events": (
        "id",
        "owner_team_id",
        "due_at",
        "target",
        "kind",
        "payload_json",
        "status",
        "created_at",
        "fired_at",
        "result_json",
    ),
    "agent_health": (
        "owner_team_id",
        "agent_id",
        "status",
        "last_output_at",
        "context_usage_pct",
        "current_task_id",
        "updated_at",
    ),
}

LEGACY_LAYOUTS = {
    "messages": (
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
        "owner_team_id",
    ),
    "results": (
        "result_id",
        "task_id",
        "agent_id",
        "envelope",
        "status",
        "created_at",
        "owner_team_id",
    ),
    "scheduled_events": (
        "id",
        "due_at",
        "target",
        "kind",
        "payload_json",
        "status",
        "created_at",
        "fired_at",
        "result_json",
        "owner_team_id",
    ),
    "agent_health": (
        "agent_id",
        "status",
        "last_output_at",
        "context_usage_pct",
        "current_task_id",
        "updated_at",
        "owner_team_id",
    ),
}

FIXTURE_COUNTS = {
    "messages": 2,
    "results": 2,
    "scheduled_events": 2,
    "agent_health": 2,
}


def build_legacy_workspace(workspace: Path, *, user_version: int = 1) -> Path:
    db_path = workspace / ".team" / "runtime" / "team.db"
    db_path.parent.mkdir(parents=True, exist_ok=True)
    if db_path.exists():
        db_path.unlink()
    conn = sqlite3.connect(db_path)
    try:
        _create_legacy_schema(conn)
        _insert_fixture_rows(conn)
        conn.execute(f"pragma user_version = {user_version}")
        conn.commit()
    finally:
        conn.close()
    return db_path


def table_layout(db_path: Path, table: str) -> tuple[str, ...]:
    conn = sqlite3.connect(db_path)
    try:
        return tuple(row[1] for row in conn.execute(f"pragma table_info({table})").fetchall())
    finally:
        conn.close()


def table_count(db_path: Path, table: str) -> int:
    conn = sqlite3.connect(db_path)
    try:
        return int(conn.execute(f"select count(*) from {table}").fetchone()[0])
    finally:
        conn.close()


def all_table_counts(db_path: Path) -> dict[str, int]:
    return {table: table_count(db_path, table) for table in MIGRATED_TABLES}


def _create_legacy_schema(conn: sqlite3.Connection) -> None:
    conn.execute(
        """
        create table messages (
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
          delivery_attempts integer not null default 0,
          owner_team_id text
        )
        """
    )
    conn.execute(
        """
        create table results (
          result_id text primary key,
          task_id text not null,
          agent_id text not null,
          envelope text not null,
          status text not null,
          created_at text not null,
          owner_team_id text
        )
        """
    )
    conn.execute(
        """
        create table scheduled_events (
          id integer primary key,
          due_at text not null,
          target text not null,
          kind text not null,
          payload_json text not null,
          status text not null,
          created_at text not null,
          fired_at text,
          result_json text,
          owner_team_id text
        )
        """
    )
    conn.execute(
        """
        create table agent_health (
          agent_id text not null,
          status text not null,
          last_output_at text,
          context_usage_pct integer,
          current_task_id text,
          updated_at text not null,
          owner_team_id text,
          unique(agent_id, owner_team_id)
        )
        """
    )


def _insert_fixture_rows(conn: sqlite3.Connection) -> None:
    now = "2026-05-28T00:00:00+00:00"
    envelope = json.dumps(
        {
            "schema_version": "result_envelope_v1",
            "task_id": "task_legacy",
            "agent_id": "worker_a",
            "status": "success",
            "summary": "legacy result",
            "changes": [],
            "tests": [],
            "risks": [],
            "artifacts": [],
            "next_actions": [],
        },
        ensure_ascii=False,
    )
    conn.executemany(
        """
        insert into messages(
          message_id, task_id, sender, recipient, reply_to, requires_ack, status,
          content, artifact_refs, created_at, updated_at, delivered_at,
          acknowledged_at, error, delivery_attempts, owner_team_id
        )
        values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        [
            ("msg_legacy_a", "task_legacy", "leader", "worker_a", None, 1, "accepted", "hello", "[]", now, now, None, None, None, 0, "team-a"),
            ("msg_legacy_b", "task_legacy", "worker_a", "leader", None, 0, "submitted", "done", "[]", now, now, now, None, None, 1, "team-a"),
        ],
    )
    conn.executemany(
        """
        insert into results(result_id, task_id, agent_id, envelope, status, created_at, owner_team_id)
        values (?, ?, ?, ?, ?, ?, ?)
        """,
        [
            ("res_legacy_a", "task_legacy", "worker_a", envelope, "success", now, "team-a"),
            ("res_legacy_b", "task_legacy", "worker_b", envelope, "collected", now, "team-a"),
        ],
    )
    conn.executemany(
        """
        insert into scheduled_events(
          id, due_at, target, kind, payload_json, status, created_at, fired_at,
          result_json, owner_team_id
        )
        values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        [
            (1, now, "worker_a", "trust_retry", "{}", "pending", now, None, None, "team-a"),
            (2, now, "worker_b", "watcher_retry", "{}", "fired", now, now, "{}", "team-a"),
        ],
    )
    conn.executemany(
        """
        insert into agent_health(
          agent_id, status, last_output_at, context_usage_pct, current_task_id,
          updated_at, owner_team_id
        )
        values (?, ?, ?, ?, ?, ?, ?)
        """,
        [
            ("worker_a", "running", now, 12, "task_legacy", now, "team-a"),
            ("worker_b", "idle", now, 4, None, now, "team-a"),
        ],
    )
