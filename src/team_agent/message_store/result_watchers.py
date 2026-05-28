from __future__ import annotations

import uuid
from contextlib import closing
from typing import Any

from team_agent.message_store.schema_migration import MANAGED_TABLE_LAYOUTS
from team_agent.message_store.schema import utcnow


RESULT_WATCHER_SELECT = ", ".join(MANAGED_TABLE_LAYOUTS["result_watchers"])


def create_result_watcher(
    self,
    task_id: str | None,
    agent_id: str | None,
    message_id: str | None,
    leader_id: str = "leader",
    owner_team_id: str | None = None,
) -> str:
    watcher_id = f"watch_{uuid.uuid4().hex[:12]}"
    with closing(self.connect()) as conn:
        with conn:
            conn.execute(
                """
                insert into result_watchers(
                  watcher_id, owner_team_id, task_id, agent_id, message_id, leader_id, status, created_at
                )
                values (?, ?, ?, ?, ?, ?, 'pending', ?)
                """,
                (watcher_id, owner_team_id, task_id, agent_id, message_id, leader_id, utcnow()),
            )
    return watcher_id

def pending_result_watchers(self, owner_team_id: str | None = None) -> list[dict[str, Any]]:
    with closing(self.connect()) as conn:
        if owner_team_id is None:
            rows = conn.execute(
                f"select {RESULT_WATCHER_SELECT} from result_watchers where status = 'pending' order by created_at"
            ).fetchall()
        else:
            rows = conn.execute(
                f"""
                select {RESULT_WATCHER_SELECT} from result_watchers
                where status = 'pending' and (owner_team_id = ? or owner_team_id is null)
                order by created_at
                """,
                (owner_team_id,),
            ).fetchall()
    return [dict(row) for row in rows]

def retryable_result_watchers(self) -> list[dict[str, Any]]:
    with closing(self.connect()) as conn:
        rows = conn.execute(
            f"select {RESULT_WATCHER_SELECT} from result_watchers where status in ('pending', 'notify_failed') order by created_at"
        ).fetchall()
    return [dict(row) for row in rows]

def result_watchers(self, owner_team_id: str | None = None) -> list[dict[str, Any]]:
    with closing(self.connect()) as conn:
        if owner_team_id is None:
            rows = conn.execute(f"select {RESULT_WATCHER_SELECT} from result_watchers order by created_at").fetchall()
        else:
            rows = conn.execute(
                f"select {RESULT_WATCHER_SELECT} from result_watchers where owner_team_id = ? or owner_team_id is null order by created_at",
                (owner_team_id,),
            ).fetchall()
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


def requeue_delivery_exhausted_watchers(self) -> list[str]:
    with closing(self.connect()) as conn:
        with conn:
            rows = conn.execute(
                "select watcher_id from result_watchers where status = 'delivery_exhausted'"
            ).fetchall()
            watcher_ids = [row["watcher_id"] for row in rows]
            if watcher_ids:
                # Phase D hotfix-3 (78055bc) cleared notified_message_id here; Gap 32 dedupe
                # reverses that — preserve notified_message_id so the retry path can re-confirm
                # (or skip if the same result_id was already injected on a different pane_id).
                conn.execute(
                    "update result_watchers "
                    "set status = 'notify_failed', error = null, completed_at = null "
                    "where status = 'delivery_exhausted'"
                )
    return watcher_ids


def claim_leader_notification(
    store: Any,
    owner_team_id: str | None,
    result_id: str | None,
    watcher_id: str,
    proposed_token: str,
) -> dict[str, Any]:
    """DEPRECATED (Stage 12 roundtable retirement). The watcher-table UPSERT did not
    actually prevent duplicate leader-pane injections in Mac mini real flow because two
    independent code paths (scheduled_event branch + result_watchers branch) emit
    deliver_attempt without coordinating at the watcher level. Replaced by
    leader_notification_log.claim_leader_notification_delivery consulted inside
    _send_to_leader_receiver. Kept here as a no-op shim so legacy callers / tests that
    still import this symbol don't crash on import — but it does NOT perform a claim and
    should NOT be used in new code."""
    if not result_id:
        return {"status": "deprecated_noop", "canonical_message_id": None}
    return {"status": "deprecated_noop", "canonical_message_id": None}


def _claim_leader_notification_disabled_impl(  # legacy reference for archaeology
    store: Any,
    owner_team_id: str | None,
    result_id: str | None,
    watcher_id: str,
    proposed_token: str,
) -> dict[str, Any]:
    if not result_id:
        return {"status": "no_result_id", "canonical_message_id": None}
    with closing(store.connect()) as conn:
        conn.isolation_level = None
        try:
            conn.execute("BEGIN IMMEDIATE")
            if owner_team_id is None:
                sibling = conn.execute(
                    "select notified_message_id from result_watchers "
                    "where result_id = ? and notified_message_id is not null "
                    "order by coalesce(completed_at, created_at) limit 1",
                    (result_id,),
                ).fetchone()
            else:
                sibling = conn.execute(
                    "select notified_message_id from result_watchers "
                    "where result_id = ? and notified_message_id is not null "
                    "and (owner_team_id = ? or owner_team_id is null) "
                    "order by coalesce(completed_at, created_at) limit 1",
                    (result_id, owner_team_id),
                ).fetchone()
            if sibling and sibling["notified_message_id"]:
                conn.execute("COMMIT")
                return {"status": "already_notified_by", "canonical_message_id": sibling["notified_message_id"]}
            cur = conn.execute(
                "update result_watchers "
                "set notified_message_id = ?, result_id = coalesce(result_id, ?) "
                "where watcher_id = ? and notified_message_id is null",
                (proposed_token, result_id, watcher_id),
            )
            if cur.rowcount == 1:
                conn.execute("COMMIT")
                return {"status": "claimed_by_you", "canonical_message_id": proposed_token}
            row = conn.execute(
                "select notified_message_id from result_watchers where watcher_id = ?",
                (watcher_id,),
            ).fetchone()
            conn.execute("COMMIT")
            return {
                "status": "already_notified_by",
                "canonical_message_id": (row["notified_message_id"] if row else None) or None,
            }
        except Exception:
            try:
                conn.execute("ROLLBACK")
            except Exception:
                pass
            raise
        finally:
            conn.isolation_level = ""  # restore default


def release_leader_notification_claim(
    store: Any,
    watcher_id: str,
    expected_token: str,
) -> bool:
    """Release a sentinel claim after delivery failure so the next retry can re-claim.
    Returns True iff we released the claim we owned (rowcount == 1)."""
    with closing(store.connect()) as conn:
        with conn:
            cur = conn.execute(
                "update result_watchers set notified_message_id = null "
                "where watcher_id = ? and notified_message_id = ?",
                (watcher_id, expected_token),
            )
            return cur.rowcount == 1


def promote_leader_notification_id(
    store: Any,
    watcher_id: str,
    sentinel_token: str,
    real_message_id: str,
) -> bool:
    """After successful delivery, replace the sentinel claim with the real message_id.
    Returns True iff the promotion succeeded (rowcount == 1)."""
    with closing(store.connect()) as conn:
        with conn:
            cur = conn.execute(
                "update result_watchers set notified_message_id = ? "
                "where watcher_id = ? and notified_message_id = ?",
                (real_message_id, watcher_id, sentinel_token),
            )
            return cur.rowcount == 1


def leader_notified_message_id_for_result(
    store: Any,
    owner_team_id: str | None,
    result_id: str | None,
) -> str | None:
    if not result_id:
        return None
    with closing(store.connect()) as conn:
        if owner_team_id is None:
            row = conn.execute(
                "select notified_message_id from result_watchers "
                "where result_id = ? and notified_message_id is not null "
                "order by coalesce(completed_at, created_at) limit 1",
                (result_id,),
            ).fetchone()
        else:
            row = conn.execute(
                "select notified_message_id from result_watchers "
                "where result_id = ? and notified_message_id is not null "
                "and (owner_team_id = ? or owner_team_id is null) "
                "order by coalesce(completed_at, created_at) limit 1",
                (result_id, owner_team_id),
            ).fetchone()
    return row["notified_message_id"] if row else None
