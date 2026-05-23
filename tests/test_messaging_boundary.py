from __future__ import annotations

import inspect
import tempfile
import unittest
from pathlib import Path

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging import delivery, deps, leader_panes, results, send


class MessagingBoundaryTests(unittest.TestCase):
    def test_runtime_wrapper_signatures_match_extracted_modules(self) -> None:
        pairs = [
            (runtime.send_message, send.send_message),
            (runtime.collect, results.collect),
            (runtime._deliver_pending_message, delivery._deliver_pending_message),
            (runtime._resolve_leader_pane, leader_panes._resolve_leader_pane),
        ]
        for wrapper, extracted in pairs:
            with self.subTest(wrapper=wrapper.__name__):
                self.assertEqual(inspect.signature(wrapper), inspect.signature(extracted))

    def test_messaging_deps_assert_explicit_runtime_symbols(self) -> None:
        self.assertIn("run_cmd", deps._RUNTIME_PATCH_POINTS)
        self.assertIn("_tmux_inject_text", deps._RUNTIME_PATCH_POINTS)
        self.assertNotIn("_sync_runtime_globals", deps.__dict__)

    def test_delivery_missing_message_fails_without_mutating_store(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-delivery-boundary-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            result = delivery._deliver_pending_message(workspace, {"agents": {}}, "msg_missing")
            self.assertEqual(result["reason"], "message_missing")
            self.assertEqual(store.messages(), [])

    def test_leader_pane_explicit_missing_target_fails(self) -> None:
        with self.assertRaises(runtime.RuntimeError) as ctx:
            leader_panes._resolve_leader_pane("%definitely-missing", "codex")
        self.assertIn("tmux pane not found", str(ctx.exception))

    def test_send_requires_target_or_task(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-boundary-") as tmp:
            with self.assertRaises(runtime.RuntimeError):
                send._send_single_message_unlocked(
                    Path(tmp),
                    {"tasks": [], "agents": {}},
                    {"leader": {"id": "leader"}, "agents": []},
                    EventLog(Path(tmp)),
                    None,
                    "hello",
                    wait_visible=False,
                )


if __name__ == "__main__":
    unittest.main()
