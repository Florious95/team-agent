from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import team_agent.message_store as message_store
from team_agent.message_store import MessageStore


class MessageStoreBoundaryTests(unittest.TestCase):
    def test_package_reexports_public_store_surface(self) -> None:
        for name in message_store._REQUIRED_EXPORTS:
            self.assertIn(name, message_store.__all__)
            self.assertTrue(hasattr(message_store, name))
        self.assertIs(message_store.MessageStore, MessageStore)

    def test_split_method_groups_are_bound_to_message_store(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-store-boundary-") as tmp:
            store = MessageStore(Path(tmp))
            store.upsert_agent_health("worker", "IDLE")
            self.assertIn("worker", store.agent_health())
            watcher_id = store.create_result_watcher("task", "worker", None)
            self.assertEqual(store.pending_result_watchers()[0]["watcher_id"], watcher_id)


if __name__ == "__main__":
    unittest.main()
