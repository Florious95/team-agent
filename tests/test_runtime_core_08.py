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

class RuntimeTests08(unittest.TestCase):
    def test_restart_recompiles_team_docs_and_closes_stale_ghostty_window_display(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-doc-display-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: doc-restart
objective: Restart from docs.
provider: fake
display_backend: ghostty_window
---

Restart test team.
""",
                encoding="utf-8",
            )
            (team / "agents" / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Worker
provider: fake
tools:
  - mcp_team
---

Fake worker.
""",
                encoding="utf-8",
            )
            spec_path = team / "team.spec.yaml"
            spec = compile_team(team, spec_path)["spec"]
            stale_display_session = runtime._ghostty_display_session_name(spec["runtime"]["session_name"], "fake_impl")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "team_dir": str(team),
                    "session_name": spec["runtime"]["session_name"],
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "fake-session-1",
                            "display": {
                                "backend": "ghostty_window",
                                "display_session": stale_display_session,
                                "pids": [1234],
                            },
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "ghostty_window",
                },
            )
            team_doc = team / "TEAM.md"
            team_doc.write_text(team_doc.read_text(encoding="utf-8").replace("ghostty_window", "ghostty_workspace"), encoding="utf-8")
            started_windows: set[str] = set()
            calls: list[list[str]] = []
            fake_run_cmd = _make_fake_ghostty_workspace_run_cmd(
                started_windows,
                calls,
                existing_sessions={stale_display_session},
            )

            with (
                patch("team_agent.runtime._ghostty_app_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"])
            state = load_runtime_state(workspace)
            self.assertEqual(load_spec(spec_path)["runtime"]["display_backend"], "ghostty_workspace")
            self.assertEqual(state["display_backend"], "ghostty_workspace")
            self.assertEqual(state["agents"]["fake_impl"]["display"]["backend"], "ghostty_workspace")
            self.assertIn(["kill", "1234"], calls)
            self.assertIn(["tmux", "kill-session", "-t", stale_display_session], calls)

    def test_restart_first_resume_exit_fallback_recreates_session_and_opens_display(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-first-fallback-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["display_backend"] = "ghostty_window"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            (workspace / "docs").mkdir()
            (workspace / "docs" / "requirements.md").write_text("requirements already written", encoding="utf-8")
            (workspace / "contract").mkdir()
            (workspace / "contract" / "README.md").write_text("contract already written", encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-restart-first-fallback",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "stale-session",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            session_exists = False
            started_windows: set[str] = set()
            list_checks = 0
            starts: list[tuple[str, str, str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                nonlocal session_exists, list_checks
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "has-session"]:
                    proc.returncode = 0 if session_exists else 1
                elif args[:3] == ["tmux", "new-session", "-d"]:
                    session_exists = True
                    started_windows.clear()
                    started_windows.add(args[6])
                    starts.append(("new-session", args[6], args[-1]))
                elif args[:2] == ["tmux", "new-window"]:
                    if not session_exists:
                        proc.returncode = 1
                        proc.stderr = f"can't find window: {args[3]}"
                    else:
                        started_windows.add(args[5])
                        starts.append(("new-window", args[5], args[-1]))
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    list_checks += 1
                    if list_checks == 1:
                        session_exists = False
                        started_windows.clear()
                        proc.returncode = 1
                        proc.stderr = "can't find session: team-restart-first-fallback"
                    else:
                        proc.stdout = "\n".join(sorted(started_windows))
                return proc

            def fake_fresh_command(agent, workspace_arg, mcp_config):
                return "fresh-command"

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.shell_resume_command_for_agent", return_value="resume-command"),
                patch("team_agent.runtime.shell_command_for_agent", side_effect=fake_fresh_command),
                patch(
                    "team_agent.runtime._open_ghostty_worker_window",
                    return_value={"status": "opened", "target": "team-restart-first-fallback:fake_impl"},
                ) as open_display,
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 335, "status": "started"}),
            ):
                result = runtime.restart(workspace, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(result["agents"][0]["restart_mode"], "fresh")
            self.assertEqual(starts, [("new-session", "fake_impl", "resume-command"), ("new-session", "fake_impl", "fresh-command")])
            open_display.assert_called_once()
            state = load_runtime_state(workspace)
            self.assertEqual(state["display_backend"], "ghostty_window")
            self.assertEqual(state["agents"]["fake_impl"]["display"]["status"], "opened")
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "restart.window_missing_after_start" for e in events))
            self.assertTrue(any(e["event"] == "restart.resume_window_missing_fallback_fresh" for e in events))
            fresh_starts = [e for e in events if e["event"] == "restart.agent_start" and e.get("restart_mode") == "fresh"]
            self.assertEqual(fresh_starts[-1]["tmux_start_mode"], "new-session")

    def test_shutdown_checks_session_capture_before_kill(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-shutdown-capture-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-shutdown-capture",
                "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl", "session_id": None}},
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            with patch("team_agent.runtime._capture_missing_sessions", return_value=[] ) as capture, patch(
                "team_agent.runtime._tmux_session_exists", return_value=False
            ):
                runtime.shutdown(workspace)
            capture.assert_called_once()
            self.assertTrue(any(e["event"] == "shutdown.session_capture_checked" for e in _events(workspace)))

    def test_shutdown_closes_ghostty_display_session_before_base_session_without_pid(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-shutdown-display-") as tmp:
            workspace = Path(tmp)
            session_name = "team-shutdown-display"
            display_session = "team-shutdown-display__display__fake_impl__12345678"
            state = {
                "session_name": session_name,
                "agents": {
                    "fake_impl": {
                        "status": "running",
                        "provider": "fake",
                        "window": "fake_impl",
                        "session_id": "fake-session",
                        "display": {
                            "backend": "ghostty_window",
                            "title": "team-agent:fake_impl:Implementation Worker",
                            "display_session": display_session,
                            "pids": [],
                        },
                    }
                },
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                if args[:2] == ["pgrep", "-f"]:
                    return Mock(returncode=1, stdout="", stderr="")
                return Mock(returncode=0, stdout="", stderr="")

            def fake_session_exists(name: str | None) -> bool:
                return name in {session_name, display_session}

            with (
                patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
                patch("team_agent.runtime._tmux_session_exists", side_effect=fake_session_exists),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                runtime.shutdown(workspace)

            kill_targets = [call[-1] for call in calls if call[:3] == ["tmux", "kill-session", "-t"]]
            self.assertIn(display_session, kill_targets)
            self.assertIn(session_name, kill_targets)
            self.assertLess(kill_targets.index(display_session), kill_targets.index(session_name))
            self.assertTrue(any(e["event"] == "display.ghostty_display_session_closed" for e in _events(workspace)))

    def test_shutdown_closes_shared_ghostty_workspace_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-shutdown-workspace-") as tmp:
            workspace = Path(tmp)
            session_name = "team-shutdown-workspace"
            aggregator_session = runtime._ghostty_workspace_aggregator_name(session_name)
            impl_linked = runtime._ghostty_display_session_name(session_name, "fake_impl")
            peer_linked = runtime._ghostty_display_session_name(session_name, "fake_peer")
            state = {
                "session_name": session_name,
                "agents": {
                    "fake_impl": {
                        "status": "running",
                        "provider": "fake",
                        "window": "fake_impl",
                        "session_id": "session-impl",
                        "display": {
                            "backend": "ghostty_workspace",
                            "title": "team-agent:team-shutdown-workspace:workspace",
                            "aggregator_session": aggregator_session,
                            "display_session": aggregator_session,
                            "linked_session": impl_linked,
                            "pids": [12345],
                        },
                    },
                    "fake_peer": {
                        "status": "running",
                        "provider": "fake",
                        "window": "fake_peer",
                        "session_id": "session-peer",
                        "display": {
                            "backend": "ghostty_workspace",
                            "title": "team-agent:team-shutdown-workspace:workspace",
                            "aggregator_session": aggregator_session,
                            "display_session": aggregator_session,
                            "linked_session": peer_linked,
                            "pids": [12345],
                        },
                    },
                },
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                return Mock(returncode=0, stdout="", stderr="")

            def fake_session_exists(name: str | None) -> bool:
                return name in {session_name, aggregator_session, impl_linked, peer_linked}

            with (
                patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
                patch("team_agent.runtime._tmux_session_exists", side_effect=fake_session_exists),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                runtime.shutdown(workspace)

            kill_targets = [call[-1] for call in calls if call[:3] == ["tmux", "kill-session", "-t"]]
            self.assertEqual(kill_targets.count(aggregator_session), 1)
            self.assertEqual(kill_targets.count(impl_linked), 1)
            self.assertEqual(kill_targets.count(peer_linked), 1)
            self.assertEqual(kill_targets.count(session_name), 1)
            self.assertEqual(len([call for call in calls if call == ["kill", "12345"]]), 1)
            self.assertLess(kill_targets.index(aggregator_session), kill_targets.index(impl_linked))
            self.assertLess(kill_targets.index(aggregator_session), kill_targets.index(peer_linked))
            self.assertLess(kill_targets.index(peer_linked), kill_targets.index(session_name))
            self.assertLess(calls.index(["tmux", "kill-session", "-t", peer_linked]), calls.index(["kill", "12345"]))
            self.assertEqual(len([e for e in _events(workspace) if e["event"] == "display.ghostty_workspace_closed"]), 1)

    def test_repair_state_is_task_scoped_and_validated(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-repair-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            second = copy.deepcopy(spec["tasks"][0])
            second["id"] = "task_other"
            spec["tasks"].append(second)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": copy.deepcopy(spec["tasks"]),
                },
            )
            result = runtime.repair_state(workspace, "task_impl", assignee="leader", status_value="pending", summary="manual repair")
            self.assertTrue(result["ok"])
            state = load_runtime_state(workspace)
            by_id = {task["id"]: task for task in state["tasks"]}
            self.assertEqual(by_id["task_impl"]["assignee"], "leader")
            self.assertEqual(by_id["task_impl"]["last_result_summary"], "manual repair")
            self.assertNotEqual(by_id["task_other"].get("last_result_summary"), "manual repair")
            with self.assertRaises(runtime.RuntimeError):
                runtime.repair_state(workspace, "task_impl", assignee="nobody")
            with self.assertRaises(runtime.RuntimeError):
                runtime.repair_state(workspace, "task_impl", status_value="almost_done")

    def test_human_confirmation_can_be_confirmed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-human-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["human_confirmation"] = True
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "missing-human-test",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            sent = runtime.send_message(workspace, None, "confirmed", task_id="task_impl", confirm_human=True)
            self.assertFalse(sent["ok"])
            state = load_runtime_state(workspace)
            self.assertTrue(state["tasks"][0]["human_confirmed"])
            self.assertEqual(state["tasks"][0]["assignee"], "fake_impl")
            self.assertTrue(any(e["event"] == "send.human_confirmation_granted" for e in _events(workspace)))

    def test_invalid_result_file_does_not_mutate_task_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-invalid-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["retry_limit"] = 1
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})
            result_path = workspace / "bad-result.json"
            result_path.write_text(
                json.dumps(
                    {
                        "schema_version": "result_envelope_v1",
                        "task_id": "task_impl",
                        "agent_id": "fake_impl",
                        "status": "success",
                    }
                ),
                encoding="utf-8",
            )
            result = runtime.collect(workspace, result_file=result_path)
            self.assertFalse(result["ok"])
            self.assertIsNone(result["invalid_results"][0]["result_id"])
            self.assertIn("/summary", result["invalid_results"][0]["error"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "pending")
            self.assertIsNone(state["tasks"][0]["assignee"])
            self.assertTrue(any(e["event"] == "collect.invalid_result" for e in _events(workspace)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
