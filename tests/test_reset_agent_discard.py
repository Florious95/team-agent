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


class ResetAgentDiscardSessionTests(unittest.TestCase):
    def test_discard_session_prevents_event_log_repair_from_reusing_old_session(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-discard-session-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            old_session_id = "old-session-from-events"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-discard-session",
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": old_session_id,
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            events_path = workspace / ".team" / "logs" / "events.jsonl"
            events_path.parent.mkdir(parents=True, exist_ok=True)
            events_path.write_text(
                json.dumps(
                    {
                        "event": "session.captured",
                        "agent_id": "fake_impl",
                        "provider": "fake",
                        "session_id": old_session_id,
                        "rollout_path": "/tmp/old-rollout.jsonl",
                        "attribution_confidence": "high",
                    }
                )
                + "\n",
                encoding="utf-8",
            )
            windows = {"fake_impl"}

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join(sorted(windows))
                elif args[:3] == ["tmux", "kill-window", "-t"]:
                    windows.discard(args[3].split(":", 1)[1])
                elif args[:2] == ["tmux", "new-window"]:
                    windows.add(args[5])
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                result = runtime.reset_agent(workspace, "fake_impl", discard_session=True, open_display=False)

            self.assertEqual(result["started"]["start_mode"], "fresh")
            self.assertIsNone(result["started"]["session_id"])
            self.assertIsNone(load_runtime_state(workspace)["agents"]["fake_impl"].get("session_id"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
