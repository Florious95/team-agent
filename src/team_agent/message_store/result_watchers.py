from __future__ import annotations

import uuid
from contextlib import closing
from typing import Any

from team_agent.message_store.schema import utcnow


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
            rows = conn.execute("select * from result_watchers where status = 'pending' order by created_at").fetchall()
        else:
            rows = conn.execute(
                """
                select * from result_watchers
                where status = 'pending' and (owner_team_id = ? or owner_team_id is null)
                order by created_at
                """,
                (owner_team_id,),
            ).fetchall()
    return [dict(row) for row in rows]

def retryable_result_watchers(self) -> list[dict[str, Any]]:
    with closing(self.connect()) as conn:
        rows = conn.execute(
            "select * from result_watchers where status in ('pending', 'notify_failed') order by created_at"
        ).fetchall()
    return [dict(row) for row in rows]

def result_watchers(self, owner_team_id: str | None = None) -> list[dict[str, Any]]:
    with closing(self.connect()) as conn:
        if owner_team_id is None:
            rows = conn.execute("select * from result_watchers order by created_at").fetchall()
        else:
            rows = conn.execute(
                "select * from result_watchers where owner_team_id = ? or owner_team_id is null order by created_at",
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
            watcher_ids = [row[0] for row in rows]
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
    return row[0] if row else None
