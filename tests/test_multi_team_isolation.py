from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path

from team_agent.message_store import MessageStore


class MultiTeamIsolationTests(unittest.TestCase):
    def test_same_workspace_messages_results_and_idle_state_are_partitioned_by_owner_team_id(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-multi-team-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)

            msg_a = store.create_message(
                None,
                "leader_a",
                "worker_a",
                "message only for team A",
                owner_team_id="team_a",
            )
            msg_b = store.create_message(
                None,
                "leader_b",
                "worker_b",
                "message only for team B",
                owner_team_id="team_b",
            )
            store.add_scheduled_event(
                "2026-05-24T00:00:00+00:00",
                "leader_a",
                "idle_reminder",
                {"agent_id": "worker_a"},
                owner_team_id="team_a",
            )
            store.add_scheduled_event(
                "2026-05-24T00:00:00+00:00",
                "leader_b",
                "idle_reminder",
                {"agent_id": "worker_b"},
                owner_team_id="team_b",
            )
            store.upsert_agent_health("worker_a", "IDLE", owner_team_id="team_a")
            store.upsert_agent_health("worker_b", "IDLE", owner_team_id="team_b")

            rows = store.messages(owner_team_id="team_a")
            self.assertEqual([row["message_id"] for row in rows], [msg_a])
            self.assertNotIn(msg_b, [row["message_id"] for row in rows])
            self.assertTrue(all(row["owner_team_id"] == "team_a" for row in rows))
            all_rows = _select_all(workspace, "messages")
            self.assertEqual({row["owner_team_id"] for row in all_rows}, {"team_a", "team_b"})

            scheduled = _select_all(workspace, "scheduled_events")
            self.assertEqual({row["owner_team_id"] for row in scheduled}, {"team_a", "team_b"})
            health = _select_all(workspace, "agent_health")
            self.assertEqual({row["owner_team_id"] for row in health}, {"team_a", "team_b"})


def _select_all(workspace: Path, table: str) -> list[dict]:
    path = workspace / ".team" / "runtime" / "team.db"
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    try:
        return [dict(row) for row in conn.execute(f"select * from {table} order by rowid").fetchall()]
    finally:
        conn.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
