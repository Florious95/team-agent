from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

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


class ResultDeliveryContractTests(unittest.TestCase):
    def test_result_delivery_retries_same_result_id_and_reaches_leader_exactly_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-delivery-contract-") as tmp:
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
            delivered_to_leader: list[str] = []

            def flaky_leader_delivery(*args, **kwargs):
                content = args[2]
                self.assertIn(result_id, content, "retry must carry the original stored result_id")
                self.assertNotIn("pending", content.lower(), "leader should see the result itself, not a pending placeholder")
                if not delivered_to_leader:
                    delivered_to_leader.append("failed-attempt-no-screen")
                    return {"ok": False, "status": "capture_missing_token", "reason": "capture_missing_token"}
                delivered_to_leader.append(content)
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with patch("team_agent.runtime.send_message", side_effect=flaky_leader_delivery):
                first = runtime.coordinator_tick(workspace)
                second = runtime.coordinator_tick(workspace)

            self.assertTrue(first["ok"])
            self.assertTrue(second["ok"])
            successful_deliveries = [item for item in delivered_to_leader if item != "failed-attempt-no-screen"]
            self.assertEqual(len(successful_deliveries), 1, "leader pane must show the final result exactly once")
            self.assertIn(result_id, successful_deliveries[0])
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["watcher_id"], watcher_id)
            self.assertEqual(watcher["status"], "notified")
            self.assertEqual(watcher["result_id"], result_id)


if __name__ == "__main__":
    unittest.main(verbosity=2)
