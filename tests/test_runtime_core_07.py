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

class RuntimeTests07(unittest.TestCase):
    def test_startup_verify_rejects_window_that_disappears_immediately(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-startup-stability-") as tmp:
            workspace = Path(tmp)
            calls = 0

            def fake_run_cmd(args: list[str], timeout: int = 20):
                nonlocal calls
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    calls += 1
                    if calls == 1:
                        proc.stdout = "coder\n"
                    else:
                        proc.returncode = 1
                        proc.stderr = "can't find session"
                return proc

            adapter = Mock()
            adapter.handle_startup_prompts.return_value = []
            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                ok = runtime._handle_startup_prompts_and_verify_window(
                    adapter,
                    runtime.EventLog(workspace),
                    "restart",
                    "coder",
                    "claude_code",
                    "team-quick-exit",
                    "resumed",
                )

            self.assertFalse(ok)
            event = _events(workspace)[-1]
            self.assertEqual(event["event"], "restart.window_missing_after_start")
            self.assertTrue(event["saw_window"])

    def test_codex_adapter_captures_rollout_by_cwd_and_ignores_other_workers(self) -> None:
        adapter = get_adapter("codex")
        with tempfile.TemporaryDirectory(prefix="team-agent-codex-rollout-") as tmp:
            root = Path(tmp) / "sessions"
            worker_a = Path(tmp) / "worker-a"
            worker_b = Path(tmp) / "worker-b"
            worker_a.mkdir()
            worker_b.mkdir()
            spawn_time = datetime(2026, 5, 16, 2, 45, tzinfo=timezone.utc)
            _write_codex_rollout(root, "019e2eac-0000-7000-a000-000000000001", worker_a, spawn_time)
            _write_codex_rollout(root, "019e2eac-0000-7000-a000-000000000002", worker_b, spawn_time)
            captured = adapter.capture_session_id(
                "worker-b",
                {"cwd": str(worker_b), "spawn_time": spawn_time.isoformat(), "sessions_root": str(root)},
                timeout_s=0,
            )
            self.assertEqual(captured["session_id"], "019e2eac-0000-7000-a000-000000000002")
            self.assertEqual(captured["captured_via"], "fs_watch")
            self.assertEqual(captured["attribution_confidence"], "high")
            missing = adapter.capture_session_id(
                "worker-missing",
                {"cwd": str(Path(tmp) / "missing"), "spawn_time": spawn_time.isoformat(), "sessions_root": str(root)},
                timeout_s=0,
            )
            self.assertIsNone(missing)
            _write_codex_rollout(root, "019e2eac-0000-7000-a000-000000000003", worker_a, spawn_time)
            second_same_cwd = adapter.capture_session_id(
                "worker-a-2",
                {
                    "cwd": str(worker_a),
                    "spawn_time": spawn_time.isoformat(),
                    "sessions_root": str(root),
                    "exclude_session_ids": ["019e2eac-0000-7000-a000-000000000001"],
                },
                timeout_s=0,
            )
            self.assertEqual(second_same_cwd["session_id"], "019e2eac-0000-7000-a000-000000000003")

    def test_codex_model_validation_rejects_display_name_instead_of_slug(self) -> None:
        adapter = get_adapter("codex")
        old_cache = getattr(adapter, "_model_catalog_cache", None)
        adapter._model_catalog_cache = None
        proc = Mock(
            returncode=0,
            stdout=json.dumps(
                {
                    "models": [
                        {"slug": "gpt-5.3-codex-spark", "display_name": "GPT-5.3-Codex-Spark"},
                        {"slug": "gpt-5.5", "display_name": "GPT-5.5"},
                    ]
                }
            ),
            stderr="",
        )
        try:
            with patch.object(adapter, "is_installed", return_value=True), patch("team_agent.provider_cli.codex.subprocess.run", return_value=proc):
                invalid = adapter.validate_model("GPT-5.3-Codex-Spark")
                valid = adapter.validate_model("gpt-5.3-codex-spark")
        finally:
            adapter._model_catalog_cache = old_cache
        self.assertFalse(invalid["ok"])
        self.assertEqual(invalid["reason"], "model_id_not_exact")
        self.assertEqual(invalid["suggested_model"], "gpt-5.3-codex-spark")
        self.assertTrue(valid["ok"])

    def test_codex_model_validation_fails_closed_when_catalog_unavailable(self) -> None:
        adapter = get_adapter("codex")
        old_cache = getattr(adapter, "_model_catalog_cache", None)
        adapter._model_catalog_cache = None
        proc = Mock(returncode=1, stdout="", stderr="catalog unavailable")
        try:
            with patch.object(adapter, "is_installed", return_value=True), patch("team_agent.provider_cli.codex.subprocess.run", return_value=proc):
                result = adapter.validate_model("gpt-5.5")
        finally:
            adapter._model_catalog_cache = old_cache
        self.assertFalse(result["ok"])
        self.assertEqual(result["status"], "model_catalog_unavailable")
        self.assertEqual(result["reason"], "model_catalog_command_failed")

    def test_codex_startup_prompt_ignores_stale_trust_text_after_ready_prompt(self) -> None:
        adapter = get_adapter("codex")
        capture = Mock(
            returncode=0,
            stdout=(
                "Do you trust the contents of this directory?\n"
                "Press enter to continue\n"
                "OpenAI Codex\n"
                "› "
            ),
            stderr="",
        )

        def fake_run(args: list[str], **_: Any) -> Mock:
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                return capture
            raise AssertionError(f"unexpected command: {args}")

        with patch("team_agent.provider_cli.codex.subprocess.run", side_effect=fake_run):
            handled = adapter.handle_startup_prompts("team-stale", "worker", checks=1, sleep_s=0.0)

        self.assertEqual(handled, [])

    def test_runtime_startup_prompt_checks_are_bounded_per_spawn(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-startup-check-limit-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-startup-check",
                "agents": {
                    "coder": {
                        "status": "running",
                        "provider": "codex",
                        "window": "coder",
                        "spawned_at": "2026-05-16T00:00:00+00:00",
                    }
                },
            }
            adapter = Mock()
            adapter.handle_startup_prompts.return_value = [{"prompt": "codex_workspace_trust", "action": "sent_enter"}]
            with (
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
            ):
                for _ in range(5):
                    runtime._handle_provider_startup_prompts(workspace, state, runtime.EventLog(workspace))

            self.assertEqual(adapter.handle_startup_prompts.call_count, runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT)
            self.assertEqual(
                state["agents"]["coder"]["startup_prompt_check_count"],
                runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT,
            )
            events = [event for event in _events(workspace) if event["event"] == "runtime.startup_prompt_handled"]
            self.assertEqual(len(events), runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT)

    def test_launch_blocks_invalid_model_before_tmux_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-invalid-model-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec["agents"][0]["model"] = "GPT-5.3-Codex-Spark"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            adapter.validate_model.return_value = {
                "ok": False,
                "provider": "codex",
                "model": "GPT-5.3-Codex-Spark",
                "reason": "model_id_not_exact",
                "suggested_model": "gpt-5.3-codex-spark",
            }
            with (
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd") as run_cmd_mock,
            ):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime.launch(spec_path, auto_approve=True)
            self.assertIn("use 'gpt-5.3-codex-spark'", str(ctx.exception))
            run_cmd_mock.assert_not_called()
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "launch.model_check" and not e["ok"] for e in events))

    def test_restart_resumes_known_sessions_and_fresh_spawns_missing_sessions(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-unit-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            state = {
                "spec_path": str(spec_path),
                "workspace": str(workspace),
                "session_name": "team-restart-unit",
                "leader": spec["leader"],
                "agents": {
                    "fake_impl": {
                        "status": "stopped",
                        "provider": "fake",
                        "window": "fake_impl",
                        "session_id": "fake-session-1",
                        "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / "fake_impl.json"),
                    }
                },
                "tasks": spec["tasks"],
                "display_backend": "none",
            }
            save_runtime_state(workspace, state)

            started_windows: set[str] = set()
            fake_run_cmd = _make_fake_tmux_window_run_cmd(started_windows)

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
                "team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 321, "status": "started"}
            ):
                resumed = runtime.restart(workspace)
            self.assertEqual(resumed["agents"][0]["restart_mode"], "resumed")
            self.assertEqual(load_runtime_state(workspace)["agents"]["fake_impl"]["session_id"], "fake-session-1")

            state = load_runtime_state(workspace)
            state["agents"]["fake_impl"]["session_id"] = None
            save_runtime_state(workspace, state)
            started_windows.clear()
            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
                "team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 322, "status": "started"}
            ):
                fresh = runtime.restart(workspace)
            self.assertEqual(fresh["agents"][0]["restart_mode"], "fresh")
            fresh_agent = load_runtime_state(workspace)["agents"]["fake_impl"]
            for key in runtime.SESSION_CAPTURE_FIELDS:
                self.assertIsNone(fresh_agent[key])
            self.assertEqual(fresh_agent["spawn_cwd"], str(workspace))
            self.assertTrue(any(e["event"] == "restart.fresh_spawn" for e in _events(workspace)))

    def test_restart_requires_team_selector_when_multiple_snapshots_exist(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-multiple-") as tmp:
            workspace = Path(tmp)
            for session_name, session_id in [("team-alpha", "session-alpha"), ("team-beta", "session-beta")]:
                spec = _fake_spec(workspace)
                spec["team"]["name"] = session_name.replace("team-", "")
                spec["runtime"]["session_name"] = session_name
                spec_path = workspace / f"{session_name}.spec.yaml"
                spec_path.write_text(dumps(spec), encoding="utf-8")
                state = {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": session_name,
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "agent_id": "fake_impl",
                            "window": "fake_impl",
                            "session_id": session_id,
                            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / "fake_impl.json"),
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                }
                runtime._save_team_runtime_snapshot(workspace, state)

            with self.assertRaises(TeamAgentRuntimeError) as ctx:
                runtime.restart(workspace)
            self.assertIn("multiple restartable teams", str(ctx.exception))
            self.assertIn("team-alpha", str(ctx.exception))
            self.assertIn("team-beta", str(ctx.exception))

            started_windows: set[str] = set()
            fake_run_cmd = _make_fake_tmux_window_run_cmd(started_windows)

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
                "team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 321, "status": "started"}
            ):
                result = runtime.restart(workspace, team="team-alpha")

            self.assertEqual(result["session_name"], "team-alpha")
            self.assertEqual(result["agents"][0]["restart_mode"], "resumed")
            self.assertEqual(load_runtime_state(workspace)["agents"]["fake_impl"]["session_id"], "session-alpha")

    def test_restart_opens_displays_after_all_worker_windows_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-display-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["display_backend"] = "ghostty_window"
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            spec["agents"].append(peer)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-restart-display",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "stopped", "provider": "fake", "window": "fake_impl", "session_id": None},
                        "fake_peer": {"status": "stopped", "provider": "fake", "window": "fake_peer", "session_id": None},
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "ghostty_window",
                },
            )
            started_windows: set[str] = set()
            display_snapshots: list[set[str]] = []
            fake_run_cmd = _make_fake_tmux_window_run_cmd(started_windows)

            def fake_open_display(workspace_arg, session_name, window_name, agent, event_log):
                display_snapshots.append(set(started_windows))
                return {"status": "opened", "target": f"{session_name}:{window_name}"}

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._open_ghostty_worker_window", side_effect=fake_open_display),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.restart(workspace, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(started_windows, {"fake_impl", "fake_peer"})
            self.assertEqual(len(display_snapshots), 2)
            self.assertTrue(all(snapshot == {"fake_impl", "fake_peer"} for snapshot in display_snapshots))

    def test_restart_rebuilds_ghostty_workspace_without_pane_addressing_base_windows(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-workspace-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec_with_agents(workspace, 2)
            spec["runtime"]["display_backend"] = "ghostty_workspace"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            session_name = "team-restart-workspace"
            aggregator_session = runtime._ghostty_workspace_aggregator_name(session_name)
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": session_name,
                    "leader": spec["leader"],
                    "agents": {
                        agent["id"]: {
                            "status": "stopped",
                            "provider": "fake",
                            "window": agent["id"],
                            "session_id": f"{agent['id']}-session",
                        }
                        for agent in spec["agents"]
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "ghostty_workspace",
                },
            )
            started_windows: set[str] = set()
            calls: list[list[str]] = []
            fake_run_cmd = _make_fake_ghostty_workspace_run_cmd(
                started_windows,
                calls,
                existing_sessions={aggregator_session},
            )

            with (
                patch("team_agent.runtime._ghostty_app_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"])
            self.assertEqual(started_windows, {agent["id"] for agent in spec["agents"]})
            self.assertIn(["tmux", "kill-session", "-t", aggregator_session], calls)
            workspace_session_starts = [
                call for call in calls if call[:3] == ["tmux", "new-session", "-d"] and "-P" in call
            ]
            self.assertEqual(len(workspace_session_starts), 1)
            for call in calls:
                if call[:2] in (["tmux", "send-keys"], ["tmux", "capture-pane"]) or call[:3] == ["tmux", "list-windows", "-t"]:
                    self.assertFalse(any(f"{session_name}:" in arg and "." in arg for arg in call))
            state = load_runtime_state(workspace)
            self.assertEqual(state["display_backend"], "ghostty_workspace")
            for agent in spec["agents"]:
                self.assertEqual(state["agents"][agent["id"]]["display"]["target"], f"{session_name}:{agent['id']}")
                self.assertEqual(state["agents"][agent["id"]]["display"]["aggregator_session"], aggregator_session)
            self.assertEqual(len({state["agents"][agent["id"]]["display"]["linked_session"] for agent in spec["agents"]}), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
