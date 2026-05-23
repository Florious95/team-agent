from __future__ import annotations

import json
import sqlite3
import uuid
from contextlib import closing
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import runtime_dir
from team_agent.spec import validate_result_envelope


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


class MessageStore:
    SCHEMA_VERSION = 2

    def __init__(self, workspace: Path):
        self.workspace = workspace
        self.path = runtime_dir(workspace) / "team.db"
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._init()

    def connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.path)
        conn.row_factory = sqlite3.Row
        return conn

    def _init(self) -> None:
        with closing(self.connect()) as conn:
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
                conn.execute(f"pragma user_version = {self.SCHEMA_VERSION}")

    def create_message(
        self,
        task_id: str | None,
        sender: str,
        recipient: str,
        content: str,
        reply_to: str | None = None,
        requires_ack: bool = True,
        artifact_refs: list[dict[str, Any]] | None = None,
    ) -> str:
        message_id = f"msg_{uuid.uuid4().hex[:12]}"
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    insert into messages(
                      message_id, task_id, sender, recipient, reply_to, requires_ack,
                      status, content, artifact_refs, created_at, updated_at,
                      delivered_at, acknowledged_at, error, delivery_attempts
                    )
                    values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        message_id,
                        task_id,
                        sender,
                        recipient,
                        reply_to,
                        int(requires_ack),
                        "accepted",
                        content,
                        json.dumps(artifact_refs or []),
                        now,
                        now,
                        None,
                        None,
                        None,
                        0,
                    ),
                )
        return message_id

    def mark(self, message_id: str, status: str, error: str | None = None) -> None:
        now = utcnow()
        delivered_at = now if status in {"injected", "visible", "submitted", "submitted_unverified", "delivered"} else None
        acknowledged_at = now if status == "acknowledged" else None
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    update messages
                    set status = ?,
                        updated_at = ?,
                        delivered_at = coalesce(?, delivered_at),
                        acknowledged_at = coalesce(?, acknowledged_at),
                        error = coalesce(?, error)
                    where message_id = ?
                    """,
                    (status, now, delivered_at, acknowledged_at, error, message_id),
                )
                if status in {
                    "injected",
                    "visible",
                    "submitted",
                    "submitted_unverified",
                    "injected_unverified",
                    "delivery_blocked",
                    "failed",
                }:
                    conn.execute(
                        """
                        insert into delivery_tokens(
                          message_id, unique_token, injected_at, visible_at, failed_at, failure_reason
                        )
                        values (?, ?, ?, ?, ?, ?)
                        on conflict(message_id) do update set
                          visible_at = coalesce(excluded.visible_at, delivery_tokens.visible_at),
                          failed_at = coalesce(excluded.failed_at, delivery_tokens.failed_at),
                          failure_reason = coalesce(excluded.failure_reason, delivery_tokens.failure_reason)
                        """,
                        (
                            message_id,
                            message_id,
                            now,
                            now if status in {"visible", "submitted"} else None,
                            now if status in {"failed", "injected_unverified", "delivery_blocked"} else None,
                            error if status in {"failed", "injected_unverified", "delivery_blocked"} else None,
                        ),
                    )
                if status == "acknowledged":
                    conn.execute(
                        "update delivery_tokens set consumed_at = coalesce(consumed_at, ?) where message_id = ?",
                        (now, message_id),
                    )

    def defer_delivery(self, message_id: str, status: str, reason: str) -> None:
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    update messages
                    set status = ?,
                        updated_at = ?,
                        error = ?,
                        delivery_attempts = delivery_attempts + 1
                    where message_id = ?
                    """,
                    (status, now, reason, message_id),
                )

    def claim_for_delivery(self, message_id: str) -> bool:
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                result = conn.execute(
                    """
                    update messages
                    set status = 'target_resolved',
                        updated_at = ?,
                        delivery_attempts = delivery_attempts + 1
                    where message_id = ?
                      and status in ('pending', 'accepted', 'queued_until_idle', 'queued_until_start', 'queued_stopped', 'queued_pane_missing')
                    """,
                    (now, message_id),
                )
        return result.rowcount == 1

    def fail_timeouts(self, timeout_sec: int, exclude_recipients: set[str] | None = None) -> list[str]:
        cutoff = datetime.now(timezone.utc) - timedelta(seconds=timeout_sec)
        failed: list[str] = []
        exclude_recipients = exclude_recipients or set()
        with closing(self.connect()) as conn:
            with conn:
                rows = conn.execute(
                    "select message_id, recipient, updated_at from messages where requires_ack = 1 and status in ('pending','accepted','target_resolved','injected','visible','submitted','delivered')"
                ).fetchall()
                for row in rows:
                    if row["recipient"] in exclude_recipients:
                        continue
                    try:
                        updated = datetime.fromisoformat(row["updated_at"])
                    except ValueError:
                        continue
                    if updated < cutoff:
                        failed.append(row["message_id"])
                        conn.execute(
                            "update messages set status = ?, updated_at = ?, error = ? where message_id = ?",
                            ("failed", utcnow(), f"ack timeout after {timeout_sec}s", row["message_id"]),
                        )
        return failed

    def messages(self) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute("select * from messages order by created_at").fetchall()
        return [dict(row) for row in rows]

    def inbox(self, agent_id: str, limit: int = 20) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute(
                """
                select * from messages
                where sender = ? or recipient = ?
                order by created_at desc
                limit ?
                """,
                (agent_id, agent_id, limit),
            ).fetchall()
        return [dict(row) for row in reversed(rows)]

    def delivery_tokens(self) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute("select * from delivery_tokens order by injected_at").fetchall()
        return [dict(row) for row in rows]

    def upsert_agent_health(
        self,
        agent_id: str,
        status: str,
        last_output_at: str | None = None,
        context_usage_pct: int | None = None,
        current_task_id: str | None = None,
    ) -> None:
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    insert into agent_health(agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)
                    values (?, ?, ?, ?, ?, ?)
                    on conflict(agent_id) do update set
                      status = excluded.status,
                      last_output_at = coalesce(excluded.last_output_at, agent_health.last_output_at),
                      context_usage_pct = excluded.context_usage_pct,
                      current_task_id = excluded.current_task_id,
                      updated_at = excluded.updated_at
                    """,
                    (agent_id, status, last_output_at, context_usage_pct, current_task_id, now),
                )

    def agent_health(self) -> dict[str, dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute("select * from agent_health order by agent_id").fetchall()
        return {row["agent_id"]: dict(row) for row in rows}

    def delete_agent_health(self, agent_id: str) -> bool:
        with closing(self.connect()) as conn:
            with conn:
                cur = conn.execute(
                    "delete from agent_health where agent_id = ?",
                    (agent_id,),
                )
        return cur.rowcount > 0

    def gc_agent_health(self, valid_agent_ids: Any) -> list[str]:
        # Caller must pass the workspace-wide set of live agent_ids across every
        # team sharing this team.db. Rows whose agent_id is not in the set are
        # deleted. If two teams share a workspace, the caller is responsible for
        # computing the union before invoking this helper; otherwise live agents
        # from a sibling team will be swept.
        valid = {str(agent_id) for agent_id in valid_agent_ids}
        with closing(self.connect()) as conn:
            with conn:
                rows = conn.execute("select agent_id from agent_health").fetchall()
                stale = [row["agent_id"] for row in rows if row["agent_id"] not in valid]
                if stale:
                    placeholders = ",".join("?" for _ in stale)
                    conn.execute(
                        f"delete from agent_health where agent_id in ({placeholders})",
                        stale,
                    )
        return stale

    def add_scheduled_event(self, due_at: str, target: str, kind: str, payload: dict[str, Any]) -> int:
        with closing(self.connect()) as conn:
            with conn:
                cur = conn.execute(
                    """
                    insert into scheduled_events(due_at, target, kind, payload_json, status, created_at)
                    values (?, ?, ?, ?, 'pending', ?)
                    """,
                    (due_at, target, kind, json.dumps(payload, ensure_ascii=False), utcnow()),
                )
                return int(cur.lastrowid)

    def due_scheduled_events(self, now: str | None = None) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute(
                """
                select * from scheduled_events
                where status = 'pending' and due_at <= ?
                order by due_at, id
                """,
                (now or utcnow(),),
            ).fetchall()
        return [dict(row) for row in rows]

    def mark_scheduled_event(self, event_id: int, status: str, result: dict[str, Any]) -> None:
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    update scheduled_events
                    set status = ?, fired_at = ?, result_json = ?
                    where id = ?
                    """,
                    (status, utcnow(), json.dumps(result, ensure_ascii=False), event_id),
                )

    def allow_peer(self, a: str, b: str) -> None:
        now = utcnow()
        pairs = [(a, b), (b, a)]
        with closing(self.connect()) as conn:
            with conn:
                conn.executemany(
                    "insert or ignore into peer_allowlist(a, b, created_at) values (?, ?, ?)",
                    [(left, right, now) for left, right in pairs],
                )

    def peer_allowed(self, a: str, b: str) -> bool:
        with closing(self.connect()) as conn:
            row = conn.execute("select 1 from peer_allowlist where a = ? and b = ?", (a, b)).fetchone()
        return row is not None

    def message_counts(self) -> dict[str, int]:
        counts: dict[str, int] = {}
        with closing(self.connect()) as conn:
            rows = conn.execute("select status, count(*) as n from messages group by status").fetchall()
        for row in rows:
            counts[row["status"]] = row["n"]
        return counts

    def result_counts(self) -> dict[str, Any]:
        counts: dict[str, Any] = {"total": 0, "uncollected": 0, "collected": 0, "invalid": 0, "by_status": {}}
        with closing(self.connect()) as conn:
            rows = conn.execute("select status, count(*) as n from results group by status").fetchall()
        for row in rows:
            status = row["status"]
            n = row["n"]
            counts["total"] += n
            if status == "collected":
                counts["collected"] += n
            elif status == "invalid":
                counts["invalid"] += n
            else:
                counts["uncollected"] += n
                counts["by_status"][status] = n
        return counts

    def create_result_watcher(
        self,
        task_id: str | None,
        agent_id: str | None,
        message_id: str | None,
        leader_id: str = "leader",
    ) -> str:
        watcher_id = f"watch_{uuid.uuid4().hex[:12]}"
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    insert into result_watchers(
                      watcher_id, task_id, agent_id, message_id, leader_id, status, created_at
                    )
                    values (?, ?, ?, ?, ?, 'pending', ?)
                    """,
                    (watcher_id, task_id, agent_id, message_id, leader_id, utcnow()),
                )
        return watcher_id

    def pending_result_watchers(self) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute(
                "select * from result_watchers where status = 'pending' order by created_at"
            ).fetchall()
        return [dict(row) for row in rows]

    def retryable_result_watchers(self) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute(
                "select * from result_watchers where status in ('pending', 'notify_failed') order by created_at"
            ).fetchall()
        return [dict(row) for row in rows]

    def result_watchers(self) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute("select * from result_watchers order by created_at").fetchall()
        return [dict(row) for row in rows]

    def mark_result_watcher(
        self,
        watcher_id: str,
        status: str,
        result_id: str | None = None,
        notified_message_id: str | None = None,
        error: str | None = None,
    ) -> None:
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    update result_watchers
                    set status = ?,
                        completed_at = ?,
                        result_id = coalesce(?, result_id),
                        notified_message_id = coalesce(?, notified_message_id),
                        error = coalesce(?, error)
                    where watcher_id = ?
                    """,
                    (status, utcnow(), result_id, notified_message_id, error, watcher_id),
                )

    def add_result(self, envelope: dict[str, Any]) -> str:
        validate_result_envelope(envelope)
        result_id = f"res_{uuid.uuid4().hex[:12]}"
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    insert into results(result_id, task_id, agent_id, envelope, status, created_at)
                    values (?, ?, ?, ?, ?, ?)
                    """,
                    (
                        result_id,
                        envelope["task_id"],
                        envelope["agent_id"],
                        json.dumps(envelope, ensure_ascii=False),
                        envelope["status"],
                        utcnow(),
                    ),
                )
        return result_id

    def acknowledge_task_messages(self, task_id: str, agent_id: str) -> list[str]:
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                rows = conn.execute(
                    """
                    select message_id from messages
                    where task_id = ? and recipient = ? and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (task_id, agent_id),
                ).fetchall()
                ids = [row["message_id"] for row in rows]
                conn.execute(
                    """
                    update messages
                    set status = 'acknowledged', acknowledged_at = ?, updated_at = ?
                    where task_id = ? and recipient = ? and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (now, now, task_id, agent_id),
                )
        return ids

    def acknowledge_message(self, message_id: str, agent_id: str) -> list[str]:
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                row = conn.execute(
                    """
                    select message_id from messages
                    where message_id = ? and recipient = ? and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (message_id, agent_id),
                ).fetchone()
                if not row:
                    return []
                conn.execute(
                    """
                    update messages
                    set status = 'acknowledged', acknowledged_at = ?, updated_at = ?
                    where message_id = ? and recipient = ? and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (now, now, message_id, agent_id),
                )
        return [message_id]

    def results(self, uncollected_only: bool = False) -> list[dict[str, Any]]:
        query = "select * from results order by created_at"
        if uncollected_only:
            query = "select * from results where status not in ('collected', 'invalid') order by created_at"
        with closing(self.connect()) as conn:
            rows = conn.execute(query).fetchall()
        return [dict(row) for row in rows]

    def result_by_id(self, result_id: str) -> dict[str, Any] | None:
        with closing(self.connect()) as conn:
            row = conn.execute("select * from results where result_id = ?", (result_id,)).fetchone()
        return dict(row) if row else None

    def latest_results(self, limit: int = 5) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute(
                """
                select * from results
                where status != 'invalid'
                order by created_at desc
                limit ?
                """,
                (limit,),
            ).fetchall()
        return [dict(row) for row in reversed(rows)]

    def mark_result_collected(self, result_id: str) -> None:
        with closing(self.connect()) as conn:
            with conn:
                conn.execute("update results set status = 'collected' where result_id = ?", (result_id,))

    def mark_result_invalid(self, result_id: str, error: str) -> None:
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    "update results set status = 'invalid' where result_id = ?",
                    (result_id,),
                )
