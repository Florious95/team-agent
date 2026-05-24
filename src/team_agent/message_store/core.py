from __future__ import annotations

import json
import sqlite3
import uuid
from contextlib import closing
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from . import agent_health as _agent_health
from . import result_watchers as _result_watchers
from .schema import SCHEMA_VERSION, initialize_schema, utcnow
from team_agent.paths import runtime_dir
from team_agent.spec import validate_result_envelope


class MessageStore:
    SCHEMA_VERSION = SCHEMA_VERSION

    def __init__(self, workspace: Path):
        self.workspace = workspace
        self.path = runtime_dir(workspace) / "team.db"
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._init()

    def connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.path, timeout=30.0, isolation_level=None)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA busy_timeout=30000")
        return conn

    def _init(self) -> None:
        with closing(self.connect()) as conn:
            initialize_schema(conn)

    def create_message(
        self,
        task_id: str | None,
        sender: str,
        recipient: str,
        content: str,
        reply_to: str | None = None,
        requires_ack: bool = True,
        artifact_refs: list[dict[str, Any]] | None = None,
        owner_team_id: str | None = None,
    ) -> str:
        message_id = f"msg_{uuid.uuid4().hex[:12]}"
        now = utcnow()
        with closing(self.connect()) as conn:
            with conn:
                conn.execute(
                    """
                    insert into messages(
                      message_id, owner_team_id, task_id, sender, recipient, reply_to, requires_ack,
                      status, content, artifact_refs, created_at, updated_at,
                      delivered_at, acknowledged_at, error, delivery_attempts
                    )
                    values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        message_id,
                        owner_team_id,
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
                    set status = case
                            when status = 'acknowledged'
                             and ? in ('injected', 'visible', 'submitted', 'submitted_unverified', 'delivered')
                            then status
                            else ?
                        end,
                        updated_at = ?,
                        delivered_at = coalesce(?, delivered_at),
                        acknowledged_at = coalesce(?, acknowledged_at),
                        error = coalesce(?, error)
                    where message_id = ?
                    """,
                    (status, status, now, delivered_at, acknowledged_at, error, message_id),
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

    def messages(self, owner_team_id: str | None = None) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            if owner_team_id is None:
                rows = conn.execute("select * from messages order by created_at").fetchall()
            else:
                rows = conn.execute(
                    "select * from messages where owner_team_id = ? or owner_team_id is null order by created_at",
                    (owner_team_id,),
                ).fetchall()
        return [dict(row) for row in rows]

    def inbox(self, agent_id: str, limit: int = 20, owner_team_id: str | None = None) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            if owner_team_id is None:
                rows = conn.execute(
                    """
                    select * from messages
                    where sender = ? or recipient = ?
                    order by created_at desc
                    limit ?
                    """,
                    (agent_id, agent_id, limit),
                ).fetchall()
            else:
                rows = conn.execute(
                    """
                    select * from messages
                    where (sender = ? or recipient = ?)
                      and (owner_team_id = ? or owner_team_id is null)
                    order by created_at desc
                    limit ?
                    """,
                    (agent_id, agent_id, owner_team_id, limit),
                ).fetchall()
        return [dict(row) for row in reversed(rows)]

    def delivery_tokens(self) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            rows = conn.execute("select * from delivery_tokens order by injected_at").fetchall()
        return [dict(row) for row in rows]

    def add_scheduled_event(
        self,
        due_at: str,
        target: str,
        kind: str,
        payload: dict[str, Any],
        owner_team_id: str | None = None,
    ) -> int:
        with closing(self.connect()) as conn:
            with conn:
                cur = conn.execute(
                    """
                    insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at)
                    values (?, ?, ?, ?, ?, 'pending', ?)
                    """,
                    (owner_team_id, due_at, target, kind, json.dumps(payload, ensure_ascii=False), utcnow()),
                )
                return int(cur.lastrowid)

    def due_scheduled_events(self, now: str | None = None, owner_team_id: str | None = None) -> list[dict[str, Any]]:
        with closing(self.connect()) as conn:
            if owner_team_id is None:
                rows = conn.execute(
                    """
                    select * from scheduled_events
                    where status = 'pending' and due_at <= ?
                    order by due_at, id
                    """,
                    (now or utcnow(),),
                ).fetchall()
            else:
                rows = conn.execute(
                    """
                    select * from scheduled_events
                    where status = 'pending' and due_at <= ?
                      and (owner_team_id = ? or owner_team_id is null)
                    order by due_at, id
                    """,
                    (now or utcnow(), owner_team_id),
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

    def add_result(self, envelope: dict[str, Any], owner_team_id: str | None = None) -> str:
        _ = owner_team_id
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

    def acknowledge_task_messages(self, task_id: str, agent_id: str, owner_team_id: str | None = None) -> list[str]:
        now = utcnow()
        owner_clause = "and (owner_team_id = ? or owner_team_id is null)" if owner_team_id else ""
        owner_args = (owner_team_id,) if owner_team_id else ()
        with closing(self.connect()) as conn:
            with conn:
                rows = conn.execute(
                    f"""
                    select message_id from messages
                    where task_id = ? and recipient = ? {owner_clause}
                      and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (task_id, agent_id, *owner_args),
                ).fetchall()
                ids = [row["message_id"] for row in rows]
                conn.execute(
                    f"""
                    update messages
                    set status = 'acknowledged', acknowledged_at = ?, updated_at = ?
                    where task_id = ? and recipient = ? {owner_clause}
                      and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (now, now, task_id, agent_id, *owner_args),
                )
        return ids

    def acknowledge_message(self, message_id: str, agent_id: str, owner_team_id: str | None = None) -> list[str]:
        now = utcnow()
        owner_clause = "and (owner_team_id = ? or owner_team_id is null)" if owner_team_id else ""
        owner_args = (owner_team_id,) if owner_team_id else ()
        with closing(self.connect()) as conn:
            with conn:
                row = conn.execute(
                    f"""
                    select message_id from messages
                    where message_id = ? and recipient = ? {owner_clause}
                      and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (message_id, agent_id, *owner_args),
                ).fetchone()
                if not row:
                    return []
                conn.execute(
                    f"""
                    update messages
                    set status = 'acknowledged', acknowledged_at = ?, updated_at = ?
                    where message_id = ? and recipient = ? {owner_clause}
                      and status in ('pending', 'accepted', 'target_resolved', 'injected', 'visible', 'submitted', 'delivered')
                    """,
                    (now, now, message_id, agent_id, *owner_args),
                )
        return [message_id]

    def results(self, uncollected_only: bool = False, owner_team_id: str | None = None) -> list[dict[str, Any]]:
        _ = owner_team_id
        clauses: list[str] = []
        args: list[Any] = []
        if uncollected_only:
            clauses.append("status not in ('collected', 'invalid')")
        where = " where " + " and ".join(clauses) if clauses else ""
        query = f"select * from results{where} order by created_at"
        with closing(self.connect()) as conn:
            rows = conn.execute(query, args).fetchall()
        return [dict(row) for row in rows]

    def result_by_id(self, result_id: str) -> dict[str, Any] | None:
        with closing(self.connect()) as conn:
            row = conn.execute("select * from results where result_id = ?", (result_id,)).fetchone()
        return dict(row) if row else None

    def latest_results(self, limit: int = 5, owner_team_id: str | None = None) -> list[dict[str, Any]]:
        _ = owner_team_id
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



MessageStore.upsert_agent_health = _agent_health.upsert_agent_health
MessageStore.agent_health = _agent_health.agent_health
MessageStore.delete_agent_health = _agent_health.delete_agent_health
MessageStore.gc_agent_health = _agent_health.gc_agent_health
MessageStore.create_result_watcher = _result_watchers.create_result_watcher
MessageStore.pending_result_watchers = _result_watchers.pending_result_watchers
MessageStore.retryable_result_watchers = _result_watchers.retryable_result_watchers
MessageStore.result_watchers = _result_watchers.result_watchers
MessageStore.mark_result_watcher = _result_watchers.mark_result_watcher
