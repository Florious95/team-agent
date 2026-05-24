from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent.events import EventLog

_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})


class ResultDeliveryDedupTests(unittest.TestCase):
    def test_result_watcher_does_not_duplicate_report_result_leader_notification(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-delivery-dedup-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            message_id = store.create_message(
                "task_impl",
                "fake_impl",
                "leader",
                f"Task task_impl reported success from fake_impl: done\nResult id: {result_id}",
                requires_ack=False,
            )
            store.mark(message_id, "submitted")

            with patch("team_agent.runtime.send_message", side_effect=AssertionError("duplicate result notification")):
                tick = runtime._collect_results_and_notify_watchers(workspace, EventLog(workspace))

            self.assertTrue(tick["ok"])
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["watcher_id"], watcher_id)
            self.assertEqual(watcher["status"], "notified")
            self.assertEqual(watcher["notified_message_id"], message_id)


if __name__ == "__main__":
    unittest.main(verbosity=2)
