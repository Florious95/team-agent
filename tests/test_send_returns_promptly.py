from __future__ import annotations

import importlib.util
import time
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


class SendReturnsPromptlyTests(unittest.TestCase):
    def test_mcp_send_returns_after_durable_submission_without_waiting_for_busy_worker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-prompt-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-send-prompt",
                    "agents": {
                        "fake_impl": {
                            "status": "busy",
                            "provider": "fake",
                            "window": "fake_impl",
                        }
                    },
                    "tasks": spec["tasks"],
                },
            )

            def slow_delivery(workspace_arg, state, message_id, wait_visible=True, timeout=30.0):
                time.sleep(2.5)
                return {"ok": False, "status": "failed", "reason": "worker_busy_timeout"}

            tools = TeamOrchestratorTools(workspace)
            started = time.monotonic()
            with patch("team_agent.runtime._deliver_pending_message", side_effect=slow_delivery):
                result = tools.send_message("fake_impl", "queued while worker is busy", sender="leader")
            elapsed = time.monotonic() - started

            self.assertLess(elapsed, 2.0, "send_message must return after durable DB submission, not worker consumption")
            row = MessageStore(workspace).messages()[0]
            self.assertEqual(row["recipient"], "fake_impl")
            self.assertEqual(result["status"], "accepted")
            self.assertTrue(result["delivery_pending"])
            self.assertEqual(result["poll_via"], f"team-agent inbox {row['message_id']}")
            self.assertNotEqual(result["status"], "delivered")
            self.assertNotEqual(result.get("message_status"), "submitted")


if __name__ == "__main__":
    unittest.main(verbosity=2)
