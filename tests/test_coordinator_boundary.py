from __future__ import annotations

import inspect
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import coordinator, runtime
from team_agent.message_store import MessageStore


class CoordinatorBoundaryTests(unittest.TestCase):
    """Pin runtime.py <-> coordinator/ contract via a small identity smoke
    plus per-helper behavioral probes plus one end-to-end probe for the
    main orchestration symbol (start_coordinator). Lesson from the
    77d40dc/3d13086 reviews: exhaustive `assertIs` per alias is
    over-coupled; representative identity + behavior catches the same
    drift without coupling the test surface to every symbol name."""

    def test_runtime_alias_smoke(self) -> None:
        # Two representative aliases prove the runtime re-export wiring is
        # live; behavior tests below catch per-helper drift.
        self.assertIs(runtime.start_coordinator, coordinator.start_coordinator)
        self.assertIs(runtime.coordinator_health, coordinator.coordinator_health)
        self.assertEqual(runtime.COORDINATOR_PROTOCOL_VERSION, coordinator.COORDINATOR_PROTOCOL_VERSION)

    def test_helpers_have_explicit_signatures(self) -> None:
        for fn in (
            coordinator.start_coordinator,
            coordinator.stop_coordinator,
            coordinator.coordinator_health,
            coordinator.coordinator_tick,
            coordinator.write_coordinator_metadata,
            coordinator.read_coordinator_metadata,
            coordinator.coordinator_metadata_ok,
            coordinator.pid_is_running,
            coordinator.message_store_schema_health,
            coordinator.coordinator_pid_path,
            coordinator.coordinator_meta_path,
            coordinator.coordinator_log_path,
        ):
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{fn.__name__} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{fn.__name__} uses **kwargs")

    def test_modules_do_not_top_level_import_runtime(self) -> None:
        for module_name in (
            "team_agent.coordinator.paths",
            "team_agent.coordinator.metadata",
            "team_agent.coordinator.lifecycle",
            "team_agent.coordinator",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            for line in source.splitlines():
                if not line or line.startswith((" ", "\t")):
                    continue
                self.assertFalse(
                    line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                    f"{module_name} top-level imports runtime: {line!r}",
                )


class PathHelperProbeTests(unittest.TestCase):
    def test_paths_point_to_runtime_dir(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-paths-") as tmp:
            workspace = Path(tmp)
            self.assertEqual(coordinator.coordinator_pid_path(workspace).name, "coordinator.pid")
            self.assertEqual(coordinator.coordinator_meta_path(workspace).name, "coordinator.json")
            self.assertEqual(coordinator.coordinator_log_path(workspace).name, "coordinator.log")
            for path in (
                coordinator.coordinator_pid_path(workspace),
                coordinator.coordinator_meta_path(workspace),
                coordinator.coordinator_log_path(workspace),
            ):
                self.assertEqual(path.parent.name, "runtime")


class PidIsRunningProbeTests(unittest.TestCase):
    def test_returns_false_when_kill_signal_raises(self) -> None:
        with patch("team_agent.coordinator.metadata.os.kill", side_effect=OSError):
            self.assertFalse(coordinator.pid_is_running(99999))

    def test_returns_false_for_zombie(self) -> None:
        zombie = Mock(returncode=0, stdout="Z\n", stderr="")
        with patch("team_agent.coordinator.metadata.os.kill", return_value=None), \
             patch("team_agent.runtime.run_cmd", return_value=zombie):
            self.assertFalse(coordinator.pid_is_running(os.getpid()))

    def test_returns_true_for_live_process(self) -> None:
        live = Mock(returncode=0, stdout="R\n", stderr="")
        with patch("team_agent.coordinator.metadata.os.kill", return_value=None), \
             patch("team_agent.runtime.run_cmd", return_value=live):
            self.assertTrue(coordinator.pid_is_running(os.getpid()))


class MetadataIoProbeTests(unittest.TestCase):
    def test_read_returns_none_for_missing_or_invalid_file(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-meta-missing-") as tmp:
            self.assertIsNone(coordinator.read_coordinator_metadata(Path(tmp)))
            coordinator.coordinator_meta_path(Path(tmp)).parent.mkdir(parents=True, exist_ok=True)
            coordinator.coordinator_meta_path(Path(tmp)).write_text("{bad", encoding="utf-8")
            self.assertIsNone(coordinator.read_coordinator_metadata(Path(tmp)))

    def test_read_returns_none_for_non_dict_json(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-meta-list-") as tmp:
            workspace = Path(tmp)
            path = coordinator.coordinator_meta_path(workspace)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("[1, 2]", encoding="utf-8")
            self.assertIsNone(coordinator.read_coordinator_metadata(workspace))

    def test_write_persists_pid_and_protocol_fields(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-meta-write-") as tmp:
            workspace = Path(tmp)
            coordinator.write_coordinator_metadata(workspace, 4242, source="test")
            loaded = json.loads(coordinator.coordinator_meta_path(workspace).read_text(encoding="utf-8"))
            self.assertEqual(loaded["pid"], 4242)
            self.assertEqual(loaded["protocol_version"], coordinator.COORDINATOR_PROTOCOL_VERSION)
            self.assertEqual(loaded["message_store_schema_version"], MessageStore.SCHEMA_VERSION)
            self.assertEqual(loaded["source"], "test")
            self.assertIn("updated_at", loaded)


class MetadataOkProbeTests(unittest.TestCase):
    def test_rejects_missing_metadata(self) -> None:
        self.assertFalse(coordinator.coordinator_metadata_ok(None, 1))

    def test_rejects_pid_mismatch(self) -> None:
        meta = {
            "pid": 1,
            "protocol_version": coordinator.COORDINATOR_PROTOCOL_VERSION,
            "message_store_schema_version": MessageStore.SCHEMA_VERSION,
        }
        self.assertFalse(coordinator.coordinator_metadata_ok(meta, 2))

    def test_rejects_protocol_drift(self) -> None:
        meta = {
            "pid": 1,
            "protocol_version": coordinator.COORDINATOR_PROTOCOL_VERSION + 1,
            "message_store_schema_version": MessageStore.SCHEMA_VERSION,
        }
        self.assertFalse(coordinator.coordinator_metadata_ok(meta, 1))

    def test_accepts_exact_match(self) -> None:
        meta = {
            "pid": 1,
            "protocol_version": coordinator.COORDINATOR_PROTOCOL_VERSION,
            "message_store_schema_version": MessageStore.SCHEMA_VERSION,
        }
        self.assertTrue(coordinator.coordinator_metadata_ok(meta, 1))


class MessageStoreSchemaHealthProbeTests(unittest.TestCase):
    def test_reports_schema_version_when_store_constructs_cleanly(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-schema-ok-") as tmp:
            out = coordinator.message_store_schema_health(Path(tmp))
        self.assertTrue(out["schema_ok"])
        self.assertEqual(out["schema"]["message_store_schema_version"], MessageStore.SCHEMA_VERSION)

    def test_reports_failure_when_store_construction_raises(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-schema-fail-") as tmp:
            workspace = Path(tmp)
            with patch("team_agent.coordinator.lifecycle.MessageStore", side_effect=RuntimeError("boom")):
                out = coordinator.message_store_schema_health(workspace)
        self.assertFalse(out["schema_ok"])
        self.assertEqual(out["schema_error"], "boom")


class CoordinatorHealthProbeTests(unittest.TestCase):
    def test_missing_pid_file_reports_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-health-missing-") as tmp:
            out = coordinator.coordinator_health(Path(tmp))
        self.assertFalse(out["ok"])
        self.assertEqual(out["status"], "missing")
        self.assertIsNone(out["pid"])

    def test_invalid_pid_returns_invalid_status(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-health-invalid-") as tmp:
            workspace = Path(tmp)
            pid_path = coordinator.coordinator_pid_path(workspace)
            pid_path.parent.mkdir(parents=True, exist_ok=True)
            pid_path.write_text("not-a-pid", encoding="utf-8")
            out = coordinator.coordinator_health(workspace)
        self.assertFalse(out["ok"])
        self.assertEqual(out["status"], "invalid_pid")


class StopCoordinatorProbeTests(unittest.TestCase):
    def test_missing_pid_file_returns_ok_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-stop-missing-") as tmp:
            out = coordinator.stop_coordinator(Path(tmp))
        self.assertEqual(out, {"ok": True, "status": "missing"})

    def test_invalid_pid_file_is_cleaned_up(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-stop-invalid-") as tmp:
            workspace = Path(tmp)
            pid_path = coordinator.coordinator_pid_path(workspace)
            pid_path.parent.mkdir(parents=True, exist_ok=True)
            pid_path.write_text("not-a-pid", encoding="utf-8")
            out = coordinator.stop_coordinator(workspace)
            self.assertEqual(out["status"], "invalid_pid_removed")
            self.assertFalse(pid_path.exists())

    def test_running_pid_is_sigtermed_and_cleaned(self) -> None:
        import signal as signal_module
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-stop-running-") as tmp:
            workspace = Path(tmp)
            pid_path = coordinator.coordinator_pid_path(workspace)
            pid_path.parent.mkdir(parents=True, exist_ok=True)
            pid_path.write_text("12345", encoding="utf-8")
            kill_calls: list[tuple[int, int]] = []

            def fake_kill(pid: int, sig: int) -> None:
                kill_calls.append((pid, int(sig)))

            with patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="R", stderr="")), \
                 patch("os.kill", side_effect=fake_kill):
                out = coordinator.stop_coordinator(workspace)
        self.assertTrue(out["ok"])
        self.assertEqual(out["status"], "stopped")
        self.assertEqual(out["pid"], 12345)
        # First call is pid_is_running probe (signal 0), second is SIGTERM.
        self.assertEqual(kill_calls, [(12345, 0), (12345, int(signal_module.SIGTERM))])
        self.assertFalse(pid_path.exists())


class StartCoordinatorEndToEndProbeTests(unittest.TestCase):
    """End-to-end probe for the MAIN orchestration symbol per spark guidance:
    exercise the full start_coordinator path with patched filesystem +
    subprocess.Popen so we do not actually spawn a daemon."""

    def test_start_writes_pid_metadata_and_returns_started(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-start-e2e-") as tmp:
            workspace = Path(tmp)
            fake_proc = Mock(pid=9999)
            with patch("team_agent.coordinator.lifecycle.subprocess.Popen", return_value=fake_proc) as popen_mock:
                out = coordinator.start_coordinator(workspace)
            self.assertTrue(out["ok"])
            self.assertEqual(out["pid"], 9999)
            self.assertEqual(out["status"], "started")
            popen_mock.assert_called_once()
            # pid file + metadata file written, schema metadata correct
            pid_path = coordinator.coordinator_pid_path(workspace)
            meta_path = coordinator.coordinator_meta_path(workspace)
            self.assertTrue(pid_path.exists())
            self.assertEqual(pid_path.read_text(encoding="utf-8"), "9999")
            self.assertTrue(meta_path.exists())
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            self.assertEqual(meta["pid"], 9999)
            self.assertEqual(meta["protocol_version"], coordinator.COORDINATOR_PROTOCOL_VERSION)
            self.assertEqual(meta["source"], "start")
            # Health should now report running for the same pid given mocked liveness.
            with patch("team_agent.coordinator.metadata.os.kill", return_value=None), \
                 patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="R", stderr="")):
                health = coordinator.coordinator_health(workspace)
            self.assertTrue(health["ok"])
            self.assertEqual(health["pid"], 9999)
            self.assertEqual(health["status"], "running")

    def test_start_returns_already_running_when_health_ok(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-start-already-") as tmp:
            workspace = Path(tmp)
            healthy = {
                "ok": True,
                "status": "running",
                "pid": 4321,
                "metadata": None,
                "metadata_ok": True,
                "schema_ok": True,
            }
            with patch("team_agent.coordinator.lifecycle.coordinator_health", return_value=healthy), \
                 patch("team_agent.coordinator.lifecycle.subprocess.Popen") as popen_mock:
                out = coordinator.start_coordinator(workspace)
            popen_mock.assert_not_called()
        self.assertEqual(out["status"], "already_running")
        self.assertEqual(out["pid"], 4321)

    def test_start_returns_schema_incompatible_when_schema_check_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-start-schema-") as tmp:
            workspace = Path(tmp)
            bad_health = {
                "ok": False,
                "status": "missing",
                "pid": None,
                "metadata": None,
                "metadata_ok": False,
                "schema_ok": False,
                "schema_error": "schema mismatch",
                "schema": {"message_store_schema_version": MessageStore.SCHEMA_VERSION},
            }
            with patch("team_agent.coordinator.lifecycle.coordinator_health", return_value=bad_health), \
                 patch("team_agent.coordinator.lifecycle.subprocess.Popen") as popen_mock:
                out = coordinator.start_coordinator(workspace)
            popen_mock.assert_not_called()
        self.assertFalse(out["ok"])
        self.assertEqual(out["status"], "schema_incompatible")
        self.assertEqual(out["error"], "schema mismatch")


class StopCoordinatorEndToEndProbeTests(unittest.TestCase):
    """End-to-end probes for stop_coordinator mirroring the start_coordinator
    e2e style: stop-race partial cleanup, dead-process cleanup, SIGTERM
    failure path, and event log emission."""

    def test_dead_process_still_clears_pid_and_meta_files(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-stop-dead-") as tmp:
            workspace = Path(tmp)
            pid_path = coordinator.coordinator_pid_path(workspace)
            meta_path = coordinator.coordinator_meta_path(workspace)
            pid_path.parent.mkdir(parents=True, exist_ok=True)
            pid_path.write_text("99999", encoding="utf-8")
            coordinator.write_coordinator_metadata(workspace, 99999, source="test")
            self.assertTrue(meta_path.exists())
            with patch("os.kill", side_effect=OSError), \
                 patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=1, stdout="", stderr="")):
                out = coordinator.stop_coordinator(workspace)
            self.assertTrue(out["ok"])
            self.assertEqual(out["status"], "stopped")
            self.assertEqual(out["pid"], 99999)
            self.assertFalse(pid_path.exists())
            self.assertFalse(meta_path.exists())

    def test_sigterm_failure_returns_kill_failed_and_leaves_pid_file(self) -> None:
        import signal as signal_module
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-stop-kill-fail-") as tmp:
            workspace = Path(tmp)
            pid_path = coordinator.coordinator_pid_path(workspace)
            pid_path.parent.mkdir(parents=True, exist_ok=True)
            pid_path.write_text("12345", encoding="utf-8")

            def fake_kill(pid: int, sig: int) -> None:
                if int(sig) == int(signal_module.SIGTERM):
                    raise PermissionError("not your process")

            with patch("os.kill", side_effect=fake_kill), \
                 patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="R", stderr="")):
                out = coordinator.stop_coordinator(workspace)
            self.assertFalse(out["ok"])
            self.assertEqual(out["status"], "kill_failed")
            self.assertEqual(out["pid"], 12345)
            # pid file deliberately left untouched so the operator can retry / inspect
            self.assertTrue(pid_path.exists())

    def test_stop_writes_event_log_on_success(self) -> None:
        import json as _json
        import signal as signal_module
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-stop-event-") as tmp:
            workspace = Path(tmp)
            pid_path = coordinator.coordinator_pid_path(workspace)
            pid_path.parent.mkdir(parents=True, exist_ok=True)
            pid_path.write_text("55555", encoding="utf-8")
            kill_calls: list[tuple[int, int]] = []

            def fake_kill(pid: int, sig: int) -> None:
                kill_calls.append((pid, int(sig)))

            with patch("os.kill", side_effect=fake_kill), \
                 patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="R", stderr="")):
                out = coordinator.stop_coordinator(workspace)
            self.assertTrue(out["ok"])
            self.assertIn((55555, int(signal_module.SIGTERM)), kill_calls)
            log_path = workspace / ".team" / "logs" / "events.jsonl"
            self.assertTrue(log_path.exists())
            events = [_json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines() if line]
            stopped = [evt for evt in events if evt.get("event") == "coordinator.stopped"]
            self.assertEqual(len(stopped), 1)
            self.assertEqual(stopped[0]["pid"], 55555)


class CoordinatorTickEndToEndProbeTests(unittest.TestCase):
    """End-to-end probes for coordinator_tick covering the stop-loop
    termination path (tmux session missing -> ok=False with stop=True)
    plus a clean tick (every runtime side effect routed through patches)."""

    def _seed_workspace(self, workspace: Path) -> None:
        from team_agent.state import save_runtime_state
        save_runtime_state(
            workspace,
            {
                "session_name": "team-tick",
                "leader": {"id": "leader"},
                "agents": {},
                "tasks": [],
            },
        )

    def test_missing_tmux_session_signals_stop(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-tick-stop-") as tmp:
            workspace = Path(tmp)
            self._seed_workspace(workspace)
            with patch("team_agent.runtime._tmux_session_exists", return_value=False):
                out = coordinator.coordinator_tick(workspace)
        self.assertFalse(out["ok"])
        self.assertTrue(out["stop"])
        self.assertEqual(out["reason"], "tmux_session_missing")
        # Event log records the session-missing reason
        events_path = workspace / ".team" / "logs" / "events.jsonl"
        if events_path.exists():
            import json as _json
            events = [_json.loads(line) for line in events_path.read_text(encoding="utf-8").splitlines() if line]
            self.assertTrue(any(evt.get("event") == "coordinator.session_missing" for evt in events))

    def test_clean_tick_returns_ok_and_does_not_signal_stop(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-tick-ok-") as tmp:
            workspace = Path(tmp)
            self._seed_workspace(workspace)
            with patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime._capture_missing_sessions", return_value=[]), \
                 patch("team_agent.runtime._refresh_agent_runtime_statuses"), \
                 patch("team_agent.runtime._handle_provider_startup_prompts"), \
                 patch("team_agent.runtime._handle_provider_runtime_prompts"), \
                 patch("team_agent.runtime._sync_agent_health"), \
                 patch("team_agent.runtime._deliver_pending_messages", return_value=[]), \
                 patch("team_agent.runtime._fire_due_scheduled_events", return_value=[]), \
                 patch("team_agent.runtime._detect_stuck_agents", return_value=[]), \
                 patch("team_agent.runtime._collect_results_and_notify_watchers", return_value={"collected": 0}):
                out = coordinator.coordinator_tick(workspace)
        self.assertTrue(out["ok"])
        self.assertFalse(out["stop"])
        self.assertEqual(out["delivered"], [])
        self.assertEqual(out["scheduled"], [])
        self.assertEqual(out["stuck"], [])
        self.assertEqual(out["results"], {"collected": 0})


if __name__ == "__main__":
    unittest.main()
