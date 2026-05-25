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


class FanoutSendTests(unittest.TestCase):
    def test_list_target_fanout_stores_one_message_per_agent_and_reports_partial_delivery(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-fanout-send-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"] = [
                {**spec["agents"][0], "id": "agent_a"},
                {**spec["agents"][0], "id": "agent_b"},
                {**spec["agents"][0], "id": "agent_c"},
            ]
            spec["runtime"]["max_active_agents"] = 3
            spec["runtime"]["startup_order"] = ["agent_a", "agent_b", "agent_c"]
            spec["routing"]["default_assignee"] = "agent_a"
            spec["routing"]["rules"] = []
            spec["tasks"][0]["id"] = "task_fanout"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-fanout",
                    "leader": spec["leader"],
                    "agents": {
                        agent_id: {"status": "running", "provider": "fake", "window": agent_id}
                        for agent_id in ("agent_a", "agent_b", "agent_c")
                    },
                    "tasks": spec["tasks"],
                },
            )

            def fake_deliver(workspace_arg, state, message_id, wait_visible=True, timeout=30.0):
                row = next(row for row in MessageStore(workspace_arg).messages() if row["message_id"] == message_id)
                if row["recipient"] == "agent_b":
                    MessageStore(workspace_arg).mark(message_id, "failed", "pane_mode_cancel_failed")
                    return {"ok": False, "status": "failed", "reason": "pane_mode_cancel_failed"}
                MessageStore(workspace_arg).mark(message_id, "submitted")
                return {"ok": True, "status": "submitted"}

            with patch("team_agent.runtime._deliver_pending_message", side_effect=fake_deliver):
                result = runtime.send_message(
                    workspace,
                    ["agent_a", "agent_b", "agent_c"],
                    "same instruction, separate copies",
                    task_id="task_fanout",
                    wait_visible=False,
                )

            self.assertTrue(result["ok"], "fan-out should accept the aggregate request even when one recipient fails")
            self.assertEqual(result["status"], "fanout_partial")
            self.assertEqual(result["to"], ["agent_a", "agent_b", "agent_c"])
            self.assertEqual(result["delivered_count"], 2)
            self.assertEqual(result["failed_count"], 1)
            self.assertEqual(
                {item["to"]: item["status"] for item in result["deliveries"]},
                {"agent_a": "submitted", "agent_b": "failed", "agent_c": "submitted"},
            )
            rows = MessageStore(workspace).messages()
            self.assertEqual([row["recipient"] for row in rows], ["agent_a", "agent_b", "agent_c"])
            self.assertEqual(len({row["message_id"] for row in rows}), 3)
            self.assertTrue(all(row["content"] == "same instruction, separate copies" for row in rows))


if __name__ == "__main__":
    unittest.main(verbosity=2)
