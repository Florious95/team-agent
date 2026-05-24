from __future__ import annotations

import importlib.util
import json
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

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


class LeaderReceiverRebindTests(unittest.TestCase):
    def test_report_result_with_stale_leader_receiver_rebinds_or_emits_required_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-rebind-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-leader-rebind",
                    "leader": spec["leader"],
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "codex",
                        "pane_id": "%stale",
                        "session_name": "old-leader",
                    },
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                runtime.report_result(
                    workspace,
                    {
                        "schema_version": "result_envelope_v1",
                        "task_id": "task_impl",
                        "agent_id": "fake_impl",
                        "status": "success",
                        "summary": "done",
                        "artifacts": [],
                        "changes": [],
                        "tests": [],
                        "risks": [],
                        "next_actions": [],
                    },
                )

            state = load_runtime_state(workspace)
            events_path = workspace / ".team" / "logs" / "events.jsonl"
            events = [
                json.loads(line)
                for line in events_path.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            self.assertTrue(
                state.get("leader_receiver", {}).get("pane_id") != "%stale"
                or any(event.get("event") == "leader_receiver.rebind_required" for event in events),
                "report_result must not silently queue against a stale leader pane",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
