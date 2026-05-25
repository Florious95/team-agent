from __future__ import annotations

import concurrent.futures
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

    def test_concurrent_writers_wait_instead_of_database_locked(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-store-concurrent-") as tmp:
            workspace = Path(tmp)
            MessageStore(workspace)
            errors: list[str] = []

            def write_one(index: int) -> tuple[int, str, int]:
                try:
                    store = MessageStore(workspace)
                    message_id = store.create_message(f"task-{index}", f"sender-{index}", "worker", f"payload-{index}")
                    store.mark(message_id, "submitted")
                    messages = store.inbox("worker", limit=100)
                    return index, message_id, len(messages)
                except Exception as exc:
                    errors.append(str(exc))
                    raise

            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                results = list(pool.map(write_one, range(24)))

            self.assertFalse([error for error in errors if "database is locked" in error.lower()])
            self.assertEqual([index for index, _, _ in results], list(range(24)))
            messages = MessageStore(workspace).messages()
            self.assertEqual(len(messages), 24)
            self.assertEqual({message["status"] for message in messages}, {"submitted"})


if __name__ == "__main__":
    unittest.main()
