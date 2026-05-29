"""Atomic exactly-once dedupe at the leader-pane injection boundary.

The current key is (result_id, owner_team_id, owner_epoch). The legacy
leader_session_uuid argument is retained as nullable audit/compatibility data.
"""
from __future__ import annotations

from contextlib import closing
from datetime import datetime, timedelta, timezone
import sqlite3
import time
from typing import Any
import zlib

from team_agent.message_store.schema_migration import MANAGED_TABLE_LAYOUTS


LEADER_NOTIFICATION_SELECT = ", ".join(MANAGED_TABLE_LAYOUTS["leader_notification_log"])


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
    leader_session_uuid: str | None = None,
    owner_epoch: int | None = None,
    proposed_message_id: str,
    envelope_hash: str,
    owner_team_id: str | None,
    pane_id: str | None,
) -> dict[str, Any]:
    """Atomic claim. INSERT OR IGNORE rowcount=1 means this caller won."""
    team_key = owner_team_id or ""
    if owner_epoch is None:
        owner_epoch = _legacy_epoch_from_uuid(leader_session_uuid)
    delay = 0.05
    row = None
    for attempt in range(6):
        now = datetime.now(timezone.utc).isoformat()
        try:
            with closing(store.connect()) as conn:
                with conn:
                    cur = conn.execute(
                        "insert or ignore into leader_notification_log("
                        "  result_id, owner_team_id, owner_epoch, leader_session_uuid,"
                        "  notified_message_id, notified_at, leader_pane_id_at_notify, envelope_content_hash"
                        ") values (?, ?, ?, ?, ?, ?, ?, ?)",
                        (
                            result_id, team_key, int(owner_epoch), leader_session_uuid,
                            proposed_message_id, now, pane_id, envelope_hash,
                        ),
                    )
                    if cur.rowcount == 1:
                        _remember_row(store, {
                            "result_id": result_id,
                            "owner_team_id": team_key,
                            "owner_epoch": int(owner_epoch),
                            "leader_session_uuid": leader_session_uuid,
                            "notified_message_id": proposed_message_id,
                            "notified_at": now,
                            "leader_pane_id_at_notify": pane_id,
                            "envelope_content_hash": envelope_hash,
                        })
                        return {
                            "status": "claimed_by_you",
                            "notified_message_id": proposed_message_id,
                            "notified_at": now,
                            "envelope_content_hash": envelope_hash,
                        }
                    row = conn.execute(
                        "select notified_message_id, notified_at, envelope_content_hash, "
                        "leader_pane_id_at_notify from leader_notification_log "
                        "where result_id = ? and owner_team_id = ? and owner_epoch = ?",
                        (result_id, team_key, int(owner_epoch)),
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
    return {
        "status": "already_notified_by",
        "notified_message_id": row["notified_message_id"],
        "notified_at": row["notified_at"],
        "envelope_content_hash": row["envelope_content_hash"],
        "leader_pane_id_at_notify": row["leader_pane_id_at_notify"],
    }


def peek_leader_notification(
    store: Any,
    *,
    result_id: str,
    leader_session_uuid: str | None = None,
    owner_team_id: str | None = None,
    owner_epoch: int | None = None,
) -> dict[str, Any] | None:
    """Read-only fast-path peek (Stage 12). Returns the existing log row for
    (result_id, leader_session_uuid) or None. Used by notify_result_watchers to short-
    circuit before calling deliver_stored_message; the authoritative atomic claim still
    happens at the _send_to_leader_receiver injection boundary."""
    team_key = owner_team_id or ""
    if owner_epoch is None:
        owner_epoch = _legacy_epoch_from_uuid(leader_session_uuid)
    with closing(store.connect()) as conn:
        if owner_team_id is None and leader_session_uuid:
            row = conn.execute(
                "select notified_message_id, notified_at, envelope_content_hash, "
                "leader_pane_id_at_notify, owner_team_id from leader_notification_log "
                "where result_id = ? and leader_session_uuid = ? order by notified_at limit 1",
                (result_id, leader_session_uuid),
            ).fetchone()
        else:
            row = conn.execute(
                "select notified_message_id, notified_at, envelope_content_hash, "
                "leader_pane_id_at_notify, owner_team_id from leader_notification_log "
                "where result_id = ? and owner_team_id = ? and owner_epoch = ?",
                (result_id, team_key, int(owner_epoch)),
            ).fetchone()
    if row is None:
        return None
    return {
        "notified_message_id": row["notified_message_id"],
        "notified_at": row["notified_at"],
        "envelope_content_hash": row["envelope_content_hash"],
        "leader_pane_id_at_notify": row["leader_pane_id_at_notify"],
        "owner_team_id": row["owner_team_id"],
    }


def _legacy_epoch_from_uuid(leader_session_uuid: str | None) -> int:
    value = str(leader_session_uuid or "")
    return int(zlib.crc32(value.encode("utf-8")) & 0x7FFFFFFF)


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
    try:
        with closing(store.connect()) as conn:
            if owner_team_id is None:
                rows = conn.execute(
                    f"select {LEADER_NOTIFICATION_SELECT} from leader_notification_log order by notified_at"
                ).fetchall()
            else:
                rows = conn.execute(
                    f"select {LEADER_NOTIFICATION_SELECT} from leader_notification_log where owner_team_id = ? "
                    "or owner_team_id is null order by notified_at",
                    (owner_team_id,),
                ).fetchall()
        return [dict(row) for row in rows]
    except sqlite3.OperationalError:
        remembered = list(getattr(store, "_leader_notification_log_rows", []))
        if owner_team_id is not None:
            remembered = [row for row in remembered if row.get("owner_team_id") in {owner_team_id, None}]
        return remembered


def _remember_row(store: Any, row: dict[str, Any]) -> None:
    rows = list(getattr(store, "_leader_notification_log_rows", []))
    rows.append(row)
    try:
        setattr(store, "_leader_notification_log_rows", rows)
    except Exception:
        pass


__all__ = [
    "claim_leader_notification_delivery",
    "peek_leader_notification",
    "prune_leader_notification_log",
    "leader_notification_log_rows",
]
