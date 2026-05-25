from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_BASE_PATH = Path(__file__).with_name("test_remove_agent_failure_injection.py")
_SPEC = importlib.util.spec_from_file_location("remove_agent_failure_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})


class RemoveAgentHealthGcIntegrationTests(unittest.TestCase):
    def test_removed_agent_health_row_is_gone_after_remove(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-health-gc-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            store = MessageStore(workspace)
            self.assertIn("fake_impl", store.agent_health())
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                runtime.remove_agent(workspace, "fake_impl")
            self.assertNotIn("fake_impl", store.agent_health())

    def test_remove_does_not_touch_other_team_health_rows(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rm-health-crossteam-") as tmp:
            workspace = _setup_workspace(tmp, dynamic=True, with_role_file=True)
            store = MessageStore(workspace)
            store.upsert_agent_health("other_team_worker", "RUNNING", current_task_id="task_other")
            with patch.object(runtime, "_tmux_window_exists", return_value=False):
                runtime.remove_agent(workspace, "fake_impl")
            remaining = store.agent_health()
            self.assertNotIn("fake_impl", remaining)
            self.assertIn("other_team_worker", remaining)
            self.assertEqual(remaining["other_team_worker"]["status"], "RUNNING")


if __name__ == "__main__":
    unittest.main()
