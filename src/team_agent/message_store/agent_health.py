from __future__ import annotations

from contextlib import closing
from typing import Any

from team_agent.message_store.schema import utcnow


def upsert_agent_health(
    self,
    agent_id: str,
    status: str,
    last_output_at: str | None = None,
    context_usage_pct: int | None = None,
    current_task_id: str | None = None,
    owner_team_id: str | None = None,
) -> None:
    now = utcnow()
    with closing(self.connect()) as conn:
        with conn:
            if owner_team_id is None:
                updated = conn.execute(
                    """
                    update agent_health
                    set status = ?,
                        last_output_at = coalesce(?, last_output_at),
                        context_usage_pct = ?,
                        current_task_id = ?,
                        updated_at = ?
                    where owner_team_id is null and agent_id = ?
                    """,
                    (status, last_output_at, context_usage_pct, current_task_id, now, agent_id),
                )
                if updated.rowcount:
                    return
            conn.execute(
                """
                insert into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)
                values (?, ?, ?, ?, ?, ?, ?)
                on conflict(owner_team_id, agent_id) do update set
                  status = excluded.status,
                  last_output_at = coalesce(excluded.last_output_at, agent_health.last_output_at),
                  context_usage_pct = excluded.context_usage_pct,
                  current_task_id = excluded.current_task_id,
                  updated_at = excluded.updated_at
                """,
                (owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, now),
            )

def agent_health(self, owner_team_id: str | None = None) -> dict[str, dict[str, Any]]:
    with closing(self.connect()) as conn:
        if owner_team_id is None:
            rows = conn.execute("select * from agent_health order by agent_id").fetchall()
        else:
            rows = conn.execute(
                "select * from agent_health where owner_team_id = ? or owner_team_id is null order by agent_id",
                (owner_team_id,),
            ).fetchall()
    return {row["agent_id"]: dict(row) for row in rows}

def delete_agent_health(self, agent_id: str, owner_team_id: str | None = None) -> bool:
    with closing(self.connect()) as conn:
        with conn:
            if owner_team_id is None:
                cur = conn.execute("delete from agent_health where agent_id = ?", (agent_id,))
            else:
                cur = conn.execute(
                    "delete from agent_health where agent_id = ? and (owner_team_id = ? or owner_team_id is null)",
                    (agent_id, owner_team_id),
                )
    return cur.rowcount > 0

def gc_agent_health(self, valid_agent_ids: Any, owner_team_id: str | None = None) -> list[str]:
    # Caller must pass the workspace-wide set of live agent_ids across every
    # team sharing this team.db. Rows whose agent_id is not in the set are
    # deleted. If two teams share a workspace, the caller is responsible for
    # computing the union before invoking this helper; otherwise live agents
    # from a sibling team will be swept. Input is validated before any DB
    # mutation so a derivation bug that silently produces None or non-str
    # entries cannot delete sibling-team rows by accident.
    valid: set[str] = set()
    for entry in valid_agent_ids:
        if not isinstance(entry, str):
            raise TypeError(
                f"gc_agent_health requires str agent_ids; got {type(entry).__name__}"
            )
        if not entry:
            raise ValueError("gc_agent_health does not accept empty agent_ids")
        valid.add(entry)
    with closing(self.connect()) as conn:
        with conn:
            if owner_team_id is None:
                rows = conn.execute("select agent_id from agent_health").fetchall()
            else:
                rows = conn.execute(
                    "select agent_id from agent_health where owner_team_id = ? or owner_team_id is null",
                    (owner_team_id,),
                ).fetchall()
            stale = [row["agent_id"] for row in rows if row["agent_id"] not in valid]
            if stale:
                placeholders = ",".join("?" for _ in stale)
                if owner_team_id is None:
                    conn.execute(f"delete from agent_health where agent_id in ({placeholders})", stale)
                else:
                    conn.execute(
                        f"delete from agent_health where agent_id in ({placeholders}) and (owner_team_id = ? or owner_team_id is null)",
                        [*stale, owner_team_id],
                    )
    return stale
