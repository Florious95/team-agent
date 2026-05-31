from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.compiler import compile_team
from team_agent.display import open_worker_displays
from team_agent.events import EventLog
from team_agent.state import load_runtime_state, save_runtime_state

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


class UXPolishAcceptanceTests(unittest.TestCase):
    def test_start_agent_returns_one_top_level_coordinator_block(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-0210-start-coordinator-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-0210-start",
                    "agents": {"fake_impl": {"status": "missing", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )

            windows: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join(sorted(windows))
                elif args[:2] == ["tmux", "new-window"]:
                    windows.add(args[5])
                return proc

            coordinator = {"ok": True, "pid": 321, "status": "started", "log": "coordinator.log"}
            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value=coordinator),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                result = runtime.start_agent(workspace, "fake_impl", open_display=False, allow_fresh=True)

            self.assertEqual(result.get("coordinator"), coordinator)
            self.assertEqual(_recursive_key_count(result, "coordinator"), 1, result)

    def test_reset_agent_hoists_coordinator_once_and_removes_nested_operation_copies(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-0210-reset-coordinator-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-0210-reset",
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "old-session",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
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

            coordinator = {"ok": True, "pid": 654, "status": "started", "log": "coordinator.log"}
            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value=coordinator),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                result = runtime.reset_agent(workspace, "fake_impl", discard_session=True, open_display=False)

            self.assertEqual(result.get("coordinator"), coordinator)
            self.assertNotIn("coordinator", result.get("stopped", {}), result)
            self.assertNotIn("coordinator", result.get("started", {}), result)
            self.assertEqual(_recursive_key_count(result, "coordinator"), 1, result)

    def test_quick_start_without_display_backend_compiles_adaptive_default(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-0210-default-backend-") as tmp:
            workspace = Path(tmp)
            team_dir = _write_doc_team(workspace)
            spec = compile_team(team_dir, workspace / "team.spec.yaml")["spec"]

            self.assertNotIn("display_backend:", (team_dir / "TEAM.md").read_text(encoding="utf-8"))
            self.assertEqual(spec["runtime"]["display_backend"], "adaptive")

    def test_worker_display_backend_omission_uses_adaptive_not_ghostty_window(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-0210-display-default-") as tmp:
            workspace = Path(tmp)
            event_log = EventLog(workspace)
            adaptive = Mock(return_value={"fake_impl": {"backend": "adaptive", "status": "opened"}})
            ghostty = Mock(return_value={"backend": "ghostty_window", "status": "opened"})

            with (
                patch("team_agent.display.worker_window.open_adaptive_display", side_effect=adaptive),
                patch("team_agent.display.worker_window.open_ghostty_worker_window", side_effect=ghostty),
            ):
                displays = open_worker_displays(
                    workspace,
                    "team-0210-display-default",
                    [("fake_impl", {"id": "fake_impl", "provider": "fake"})],
                    event_log,
                )

            self.assertEqual(displays["fake_impl"]["backend"], "adaptive")
            adaptive.assert_called_once()
            ghostty.assert_not_called()


def _recursive_key_count(value: object, key: str) -> int:
    if isinstance(value, dict):
        return sum(1 for item_key in value if item_key == key) + sum(_recursive_key_count(item, key) for item in value.values())
    if isinstance(value, list):
        return sum(_recursive_key_count(item, key) for item in value)
    return 0


if __name__ == "__main__":
    unittest.main(verbosity=2)
