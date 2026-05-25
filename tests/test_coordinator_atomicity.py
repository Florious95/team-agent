from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent import runtime


class CoordinatorAtomicityTests(unittest.TestCase):
    def test_start_coordinator_aborts_when_incompatible_running_process_cannot_stop(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-atomic-") as tmp:
            workspace = Path(tmp)
            health = {
                "ok": False,
                "status": "running",
                "pid": 12345,
                "metadata_ok": False,
                "metadata": {"protocol_version": 1},
                "schema_ok": True,
            }
            stop_result = {"ok": False, "status": "kill_failed", "pid": 12345, "error": "permission denied"}
            with (
                patch("team_agent.coordinator.lifecycle.coordinator_health", return_value=health),
                patch("team_agent.coordinator.lifecycle.stop_coordinator", return_value=stop_result),
                patch("team_agent.coordinator.lifecycle.subprocess.Popen") as popen,
            ):
                result = runtime.start_coordinator(workspace)

            self.assertFalse(result["ok"])
            self.assertEqual(result["status"], "restart_incompatible_stop_failed")
            self.assertEqual(result["pid"], 12345)
            popen.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
