"""Stage 12 (Gap 26 ∩ Gap 32 roundtable consolidation 2026-05-26): atomic exactly-once
dedupe at the leader-pane injection boundary, keyed by (result_id, leader_session_uuid).

Replaces the bad6484 watcher-table UPSERT approach. UNIQUE primary key + SQLite
INSERT OR IGNORE gives an atomic claim that works across processes (CLI subprocess
vs coordinator daemon) and across threads without an advisory lock. Distinct
leader_session_uuid values (e.g. after takeover) each get their own row so a
re-takeover legitimately allows another delivery for the same result_id.
"""
from __future__ import annotations

from contextlib import closing
from datetime import datetime, timedelta, timezone
import sqlite3
import time
from typing import Any


def _sqlite_locked(exc: sqlite3.OperationalError) -> bool:
    message = str(exc).lower()
    return (
        "database is locked" in message
        or "database table is locked" in message
        or "database schema is locked" in message
    )


def claim_leader_notification_delivery(
    store: Any,
    *,
    result_id: str,
    leader_session_uuid: str,
    proposed_message_id: str,
    envelope_hash: str,
    owner_team_id: str | None,
    pane_id: str | None,
) -> dict[str, Any]:
    """Atomic claim. INSERT OR IGNORE → rowcount=1 means we won, fire the inject.
    rowcount=0 means a prior row exists for (result_id, leader_session_uuid); SELECT
    it and return so the caller can decide to suppress (same envelope_hash) or surface
    legitimate-duplicate (different envelope_hash)."""
    delay = 0.05
    row = None
    for attempt in range(6):
        now = datetime.now(timezone.utc).isoformat()
        try:
            with closing(store.connect()) as conn:
                with conn:
                    cur = conn.execute(
                        "insert or ignore into leader_notification_log("
                        "  result_id, leader_session_uuid, notified_message_id, notified_at,"
                        "  leader_pane_id_at_notify, envelope_content_hash, owner_team_id"
                        ") values (?, ?, ?, ?, ?, ?, ?)",
                        (
                            result_id, leader_session_uuid, proposed_message_id, now,
                            pane_id, envelope_hash, owner_team_id,
                        ),
                    )
                    if cur.rowcount == 1:
                        return {
                            "status": "claimed_by_you",
                            "notified_message_id": proposed_message_id,
                            "notified_at": now,
                            "envelope_content_hash": envelope_hash,
                        }
                    row = conn.execute(
                        "select notified_message_id, notified_at, envelope_content_hash, "
                        "leader_pane_id_at_notify from leader_notification_log "
                        "where result_id = ? and leader_session_uuid = ?",
                        (result_id, leader_session_uuid),
                    ).fetchone()
            break
        except sqlite3.OperationalError as exc:
            if not _sqlite_locked(exc) or attempt == 5:
                raise
            time.sleep(delay)
            delay *= 2
    if row is None:
        # Should not happen (INSERT OR IGNORE returned 0 → row must exist), but be defensive.
        return {"status": "claimed_by_you", "notified_message_id": proposed_message_id,
                "notified_at": now, "envelope_content_hash": envelope_hash}
    prev_message_id, prev_ts, prev_hash, prev_pane = row[0], row[1], row[2], row[3]
    return {
        "status": "already_notified_by",
        "notified_message_id": prev_message_id,
        "notified_at": prev_ts,
        "envelope_content_hash": prev_hash,
        "leader_pane_id_at_notify": prev_pane,
    }


def peek_leader_notification(
    store: Any,
    *,
    result_id: str,
    leader_session_uuid: str,
) -> dict[str, Any] | None:
    """Read-only fast-path peek (Stage 12). Returns the existing log row for
    (result_id, leader_session_uuid) or None. Used by notify_result_watchers to short-
    circuit before calling deliver_stored_message; the authoritative atomic claim still
    happens at the _send_to_leader_receiver injection boundary."""
    with closing(store.connect()) as conn:
        row = conn.execute(
            "select notified_message_id, notified_at, envelope_content_hash, "
            "leader_pane_id_at_notify, owner_team_id from leader_notification_log "
            "where result_id = ? and leader_session_uuid = ?",
            (result_id, leader_session_uuid),
        ).fetchone()
    if row is None:
        return None
    return {
        "notified_message_id": row[0],
        "notified_at": row[1],
        "envelope_content_hash": row[2],
        "leader_pane_id_at_notify": row[3],
        "owner_team_id": row[4],
    }


def prune_leader_notification_log(store: Any, *, max_age_hours: int = 24) -> int:
    """Coordinator-tick maintenance: drop rows older than max_age_hours. Cheap, bounded."""
    cutoff = (datetime.now(timezone.utc) - timedelta(hours=max_age_hours)).isoformat()
    with closing(store.connect()) as conn:
        with conn:
            cur = conn.execute(
                "delete from leader_notification_log where notified_at < ?",
                (cutoff,),
            )
            return cur.rowcount or 0


def leader_notification_log_rows(store: Any, *, owner_team_id: str | None = None) -> list[dict[str, Any]]:
    """Test/diagnostic accessor. Returns all rows (optionally team-scoped)."""
    with closing(store.connect()) as conn:
        if owner_team_id is None:
            rows = conn.execute(
                "select * from leader_notification_log order by notified_at"
            ).fetchall()
        else:
            rows = conn.execute(
                "select * from leader_notification_log where owner_team_id = ? "
                "or owner_team_id is null order by notified_at",
                (owner_team_id,),
            ).fetchall()
    return [dict(row) for row in rows]


__all__ = [
    "claim_leader_notification_delivery",
    "peek_leader_notification",
    "prune_leader_notification_log",
    "leader_notification_log_rows",
]
