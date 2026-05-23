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

class LifecycleStartAgentTests(unittest.TestCase):
    def test_start_agent_repairs_missing_worker_window_without_restart(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-start-agent-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-start-agent",
                    "agents": {
                        "fake_impl": {
                            "status": "missing",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "fake-session-1",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            calls: list[list[str]] = []
            captured_runtime: list[dict[str, Any]] = []
            started_windows: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "other\n" + "\n".join(sorted(started_windows))
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                return proc

            def fake_resume_command(agent, previous, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            with (
                patch(
                    "team_agent.runtime._detect_inherited_dangerous_permissions",
                    return_value={
                        "enabled": True,
                        "provider": "codex",
                        "flag": "--dangerously-bypass-approvals-and-sandbox",
                    },
                ),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.shell_resume_command_for_agent", side_effect=fake_resume_command),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.start_agent(workspace, "fake_impl", open_display=False, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(result["start_mode"], "resumed")
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve"])
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve_inherited"])
            self.assertIn(["tmux", "new-window", "-t", "team-start-agent", "-n", "fake_impl", "sh", "-lc", "true"], calls)
            state = load_runtime_state(workspace)
            self.assertEqual(state["agents"]["fake_impl"]["status"], "running")
            self.assertTrue(any(e["event"] == "start_agent.complete" for e in _events(workspace)))

    def test_start_agent_ghostty_workspace_respawns_existing_workspace_pane(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-start-agent-workspace-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec_with_agents(workspace, 2)
            spec["runtime"]["display_backend"] = "ghostty_workspace"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            session_name = "team-start-agent-workspace"
            aggregator_session = runtime._ghostty_workspace_aggregator_name(session_name)
            peer_linked = runtime._ghostty_display_session_name(session_name, "fake_peer")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": session_name,
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl", "session_id": "session-impl"},
                        "fake_peer": {
                            "status": "missing",
                            "provider": "fake",
                            "window": "fake_peer",
                            "session_id": None,
                            "display": {
                                "backend": "ghostty_workspace",
                                "title": "team-agent:team-start-agent-workspace:workspace",
                                "aggregator_session": aggregator_session,
                                "display_session": aggregator_session,
                                "linked_session": peer_linked,
                                "pane_id": "%77",
                                "pids": [4321],
                            },
                        },
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "ghostty_workspace",
                },
            )
            started_windows = {"fake_impl"}
            calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "has-session"]:
                    proc.returncode = 0 if args[-1] in {session_name, aggregator_session} else 1
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join(sorted(started_windows))
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                return proc

            with (
                patch("team_agent.runtime._ghostty_app_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.start_agent(workspace, "fake_peer", allow_fresh=True)

            self.assertTrue(result["ok"])
            display = load_runtime_state(workspace)["agents"]["fake_peer"]["display"]
            self.assertEqual(display["backend"], "ghostty_workspace")
            self.assertEqual(display["status"], "opened")
            self.assertEqual(display["linked_session"], peer_linked)
            self.assertEqual(display["aggregator_session"], aggregator_session)
            self.assertEqual(display["pane_id"], "%77")
            self.assertEqual(result["display_target"], display)
            self.assertIn(["tmux", "respawn-pane", "-k", "-t", "%77", runtime._ghostty_workspace_pane_command(peer_linked)], calls)
            self.assertTrue(any(e["event"] == "display.ghostty_workspace" and e["agent_id"] == "fake_peer" for e in _events(workspace)))

    def test_start_agent_falls_back_to_fresh_when_resume_window_exits(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-start-agent-fallback-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-start-agent-fallback",
                    "agents": {
                        "fake_impl": {
                            "status": "missing",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "stale-session",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            start_modes: list[str] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n" if start_modes and start_modes[-1] == "fresh" else "other\n"
                elif args[:2] == ["tmux", "new-window"]:
                    command = args[-1]
                    start_modes.append("fresh" if command == "fresh-command" else "resumed")
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.shell_resume_command_for_agent", return_value="resume-command"),
                patch("team_agent.runtime.shell_command_for_agent", return_value="fresh-command"),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 334, "status": "started"}),
            ):
                result = runtime.start_agent(workspace, "fake_impl", open_display=False, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(result["start_mode"], "fresh")
            self.assertEqual(start_modes, ["resumed", "fresh"])
            agent_state = load_runtime_state(workspace)["agents"]["fake_impl"]
            for key in runtime.SESSION_CAPTURE_FIELDS:
                self.assertIsNone(agent_state[key])
            self.assertEqual(agent_state["spawn_cwd"], str(workspace))
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "start_agent.window_missing_after_start" for e in events))
            self.assertTrue(any(e["event"] == "start_agent.resume_window_missing_fallback_fresh" for e in events))

    def test_shutdown_is_idempotent_when_tmux_session_disappears_during_kill(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-shutdown-race-") as tmp:
            workspace = Path(tmp)
            session_name = "team-shutdown-race"
            save_runtime_state(
                workspace,
                {
                    "session_name": session_name,
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-impl",
                            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / "fake_impl.json"),
                        }
                    },
                    "tasks": [],
                },
            )
            calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "kill-session", "-t"] and args[-1] == session_name:
                    proc.returncode = 1
                    proc.stderr = f"can't find session: {session_name}"
                return proc

            with (
                patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                result = runtime.shutdown(workspace)

            self.assertTrue(result["ok"])
            self.assertEqual(load_runtime_state(workspace)["agents"]["fake_impl"]["status"], "stopped")
            self.assertTrue(any(e["event"] == "shutdown.idempotent" and e["reason"] == "session disappeared before kill" for e in _events(workspace)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
