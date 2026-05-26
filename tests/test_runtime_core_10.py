from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

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

class RuntimeTests10(unittest.TestCase):
    def test_restart_passes_inherited_dangerous_permissions_to_resume_and_fresh_workers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-inherit-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            fresh_agent = copy.deepcopy(spec["agents"][0])
            fresh_agent["id"] = "fake_fresh"
            spec["agents"].append(fresh_agent)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-restart-inherit",
                    "agents": {
                        "fake_impl": {"status": "stopped", "provider": "fake", "window": "fake_impl", "session_id": "fake-session-1"},
                        "fake_fresh": {"status": "stopped", "provider": "fake", "window": "fake_fresh", "session_id": None},
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            captured_runtime: list[dict[str, Any]] = []

            def fake_resume_command(agent, previous, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            def fake_fresh_command(agent, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            started_windows: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=1 if args[:2] == ["tmux", "has-session"] else 0, stdout="", stderr="")
                if args[:3] == ["tmux", "new-session", "-d"]:
                    started_windows.add(args[6])
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join(sorted(started_windows))
                return proc

            with (
                patch(
                    "team_agent.runtime._detect_inherited_dangerous_permissions",
                    return_value={
                        "enabled": True,
                        "provider": "claude",
                        "flag": "--dangerously-skip-permissions",
                    },
                ),
                patch("team_agent.runtime.shell_resume_command_for_agent", side_effect=fake_resume_command),
                patch("team_agent.runtime.shell_command_for_agent", side_effect=fake_fresh_command),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 444, "status": "started"}),
            ):
                # Stage 7 S5 atomicity: fake_fresh has session_id=None so the
                # default refuses; opt into --allow-fresh to exercise the
                # mixed resume+fresh path this test was written for.
                restarted = runtime.restart(workspace, allow_fresh=True)

            self.assertTrue(restarted["ok"])
            self.assertEqual(len(captured_runtime), 2)
            self.assertTrue(all(item["dangerous_auto_approve"] for item in captured_runtime))
            self.assertTrue(all(item["dangerous_auto_approve_inherited"] for item in captured_runtime))
            self.assertEqual({item["dangerous_auto_approve_source"] for item in captured_runtime}, {"leader_process"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
