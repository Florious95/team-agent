from __future__ import annotations

import importlib.util
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


class WorkerSessionDriftDetectionTests(unittest.TestCase):
    def test_resume_injection_drift_is_detected_or_prevented(self) -> None:
        stored_session_id = "S1"
        injected_thread_id = "1a0be169-ffa6-4263-8d50-a68645cc74c0"
        with tempfile.TemporaryDirectory(prefix="team-agent-session-drift-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "drift-team",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "codex",
                            "window": "fake_impl",
                            "session_id": stored_session_id,
                        }
                    },
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = (
                        f"resume {injected_thread_id}\n"
                        f"Switched to thread {injected_thread_id}\n"
                        "› Find and fix a bug in @filename\n"
                    )
                elif args[:3] == ["tmux", "display-message", "-p"]:
                    proc.stdout = f"%1\tdrift-team\t0\tfake_impl\t0\t/dev/ttys001\tnode\t1\t{workspace}\t1\n"
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                tick = runtime.coordinator_tick(workspace)

            events = _events(workspace)
            drift_event = next(
                (event for event in events if event.get("event") == "coordinator.session_drift_detected"),
                None,
            )
            state = load_runtime_state(workspace)
            agent_state = state["agents"]["fake_impl"]
            prevention_event = next(
                (
                    event
                    for event in events
                    if event.get("event") in {
                        "paste_buffer_hygiene.prevented_resume_injection",
                        "coordinator.session_drift_prevented",
                    }
                ),
                None,
            )
            if prevention_event:
                self.assertEqual(agent_state.get("session_id"), stored_session_id)
                return
            self.assertIsNotNone(
                drift_event,
                "resume-thread contamination must either be prevented before reaching Codex or emit coordinator.session_drift_detected",
            )
            self.assertEqual(drift_event["stored_session_id"], stored_session_id)
            self.assertEqual(drift_event["actual_thread_id"], injected_thread_id)
            self.assertIn(drift_event.get("status"), {"halt", "session_drift"})
            self.assertIn(agent_state.get("status"), {"session_drift", "halted", "error"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
