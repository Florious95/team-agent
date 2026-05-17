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


def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat()


class MessageStore:
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
                      error text
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
                    insert into messages values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                if status in {"injected", "visible", "submitted", "submitted_unverified", "injected_unverified", "failed"}:
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
                            now if status in {"failed", "injected_unverified"} else None,
                            error if status in {"failed", "injected_unverified"} else None,
                        ),
                    )
                if status == "acknowledged":
                    conn.execute(
                        "update delivery_tokens set consumed_at = coalesce(consumed_at, ?) where message_id = ?",
                        (now, message_id),
                    )

    def claim_for_delivery(self, message_id: str) -> bool:
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                result = conn.execute(
                    """
                    update messages
                    set status = 'target_resolved',
                        updated_at = ?
                    where message_id = ?
                      and status in ('pending', 'accepted')
                    """,
                    (now, message_id),
                )
        return result.rowcount == 1

    def fail_timeouts(self, timeout_sec: int) -> list[str]:
        cutoff = datetime.now(timezone.utc) - timedelta(seconds=timeout_sec)
        failed: list[str] = []
        with closing(self.connect()) as conn:
            with conn:
                rows = conn.execute(
                    "select message_id, updated_at from messages where requires_ack = 1 and status in ('pending','accepted','target_resolved','injected','visible','submitted','delivered')"
                ).fetchall()
                for row in rows:
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
                    "insert into results values (?, ?, ?, ?, ?, ?)",
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
