from __future__ import annotations

import copy
import errno
import inspect
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from team_agent.coordinator import lifecycle
from team_agent.state import _RUNTIME_STATE_CACHE, runtime_state_path, save_runtime_state


FIXTURE = Path(__file__).parent / "fixtures" / "bug_084_state_resilience" / "state-rich.json"


class Bug084StateResilienceAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        _RUNTIME_STATE_CACHE.clear()

    def tearDown(self) -> None:
        _RUNTIME_STATE_CACHE.clear()

    def test_eacces_retry_exhaustion_self_heals_with_heal_tmp_and_backup_not_truncate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug084-self-heal-") as tmp:
            workspace = Path(tmp)
            original = _fixture_state()
            save_runtime_state(workspace, original)
            path = runtime_state_path(workspace)
            original_text = path.read_text(encoding="utf-8")
            updated = copy.deepcopy(original)
            updated["tasks"].append({"id": "t-self-healed", "title": "after EACCES", "status": "done"})

            real_replace = os.replace
            direct_attempts: list[tuple[str, str]] = []
            heal_sources: list[str] = []

            def flaky_replace(src: object, dst: object) -> None:
                src_path = Path(src)
                dst_path = Path(dst)
                if dst_path == path and src_path != path and ".heal." not in src_path.name:
                    direct_attempts.append((src_path.name, dst_path.name))
                    raise PermissionError(errno.EACCES, "9p rename-over denied", str(dst_path))
                if dst_path == path and ".heal." in src_path.name:
                    heal_sources.append(src_path.name)
                real_replace(src, dst)

            with patch("team_agent.state.os.replace", side_effect=flaky_replace):
                save_runtime_state(workspace, updated)

            self.assertGreaterEqual(len(direct_attempts), 4)
            self.assertTrue(heal_sources, "self-heal must promote a distinct .heal. temp file")
            self.assertEqual(json.loads(path.read_text(encoding="utf-8"))["tasks"][-1]["id"], "t-self-healed")
            backup_files = list(path.parent.glob(f"{path.name}.bak.*"))
            self.assertTrue(backup_files, "self-heal must preserve the previous state in a backup during inode rebuild")
            self.assertEqual(json.loads(backup_files[0].read_text(encoding="utf-8")), json.loads(original_text))
            self.assertNotIn("truncate(", _save_runtime_state_source())

    def test_enospc_is_not_retried_or_self_healed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug084-enospc-") as tmp:
            workspace = Path(tmp)
            state = _fixture_state()
            save_runtime_state(workspace, state)
            calls = 0

            def enospc_once(src: object, dst: object) -> None:
                nonlocal calls
                calls += 1
                raise OSError(errno.ENOSPC, "disk full", str(dst))

            with patch("team_agent.state.os.replace", side_effect=enospc_once):
                with self.assertRaises(OSError) as raised:
                    save_runtime_state(workspace, {**state, "status": "changed"})

            self.assertEqual(raised.exception.errno, errno.ENOSPC)
            self.assertEqual(calls, 1, "non-transient OS errors must not retry")

    def test_deep_equal_cache_returns_before_lock_or_replace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug084-dirty-") as tmp:
            workspace = Path(tmp)
            state = _fixture_state()
            path = runtime_state_path(workspace)
            _RUNTIME_STATE_CACHE[str(path)] = copy.deepcopy(state)

            with patch("team_agent.state.os.replace", side_effect=AssertionError("replace must not run")), \
                 patch("team_agent.runtime._runtime_lock", side_effect=AssertionError("state-save lock must not be taken")):
                save_runtime_state(workspace, copy.deepcopy(state))

            self.assertFalse(path.exists(), "deep-equal cached state should not create or replace state.json")

    def test_save_runtime_state_uses_state_save_runtime_lock_timeout_2_without_business_locks(self) -> None:
        source = _save_runtime_state_source()
        self.assertIn('_runtime_lock(workspace, "state-save", timeout=2.0)', source)
        for forbidden in ("start-agent", "stop-agent", "send", "leader", "remove-agent", "acknowledge-idle"):
            self.assertNotIn(forbidden, source)

    def test_tick_end_save_failure_returns_persistence_degraded_and_logs_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug084-tick-save-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(
                workspace,
                {"session_name": None, "active_team_key": None, "leader": {"id": "leader"}, "agents": {}, "tasks": []},
            )
            with _patched_quiet_tick_dependencies(), \
                 patch("team_agent.state.save_runtime_state", side_effect=PermissionError(errno.EACCES, "denied")):
                result = lifecycle.coordinator_tick(workspace)

            self.assertFalse(result["ok"])
            self.assertFalse(result["stop"])
            self.assertEqual(result["reason"], "persistence_degraded")
            self.assertFalse(result["persisted"])
            self.assertIn("runtime.state.save_failed", _event_names(workspace))

    def test_coordinator_main_catches_tick_errors_backs_off_dedupes_and_resets_on_success(self) -> None:
        from team_agent.coordinator import __main__ as coordinator_main

        with tempfile.TemporaryDirectory(prefix="team-agent-bug084-main-") as tmp:
            workspace = Path(tmp)
            coordinator_main.STOP = False
            tick = MagicMock(side_effect=[
                PermissionError(errno.EACCES, "denied"),
                PermissionError(errno.EACCES, "denied"),
                {"ok": True, "stop": True},
            ])
            with patch("team_agent.coordinator.__main__.runtime.ensure_workspace_dirs"), \
                 patch("team_agent.coordinator.__main__.runtime.coordinator_pid_path", return_value=workspace / "pid"), \
                 patch("team_agent.coordinator.__main__.runtime.write_coordinator_metadata"), \
                 patch("team_agent.coordinator.__main__.runtime.coordinator_tick", tick), \
                 patch("team_agent.coordinator.__main__._tick_interval", return_value=5.0), \
                 patch("team_agent.coordinator.__main__.signal.signal"), \
                 patch("team_agent.coordinator.__main__.time.sleep") as sleep:
                coordinator_main.main(["--workspace", str(workspace)])

            self.assertEqual([call.args[0] for call in sleep.call_args_list[:2]], [5.0, 10.0])
            events = _events(workspace)
            names = [event["event"] for event in events]
            self.assertIn("coordinator.tick_error", names)
            self.assertIn("coordinator.tick_recovered", names)

    def test_bug084_fault_injection_paths_make_zero_provider_network_calls(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug084-provider-zero-") as tmp:
            workspace = Path(tmp)
            state = _fixture_state()
            sentinels = _provider_network_sentinels()
            with patch.dict("sys.modules", sentinels):
                save_runtime_state(workspace, state)

            for sentinel in sentinels.values():
                sentinel.assert_zero_calls()


def _fixture_state() -> dict:
    return json.loads(FIXTURE.read_text(encoding="utf-8"))


def _save_runtime_state_source() -> str:
    return inspect.getsource(save_runtime_state)


def _events(workspace: Path) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def _event_names(workspace: Path) -> list[str]:
    return [event["event"] for event in _events(workspace)]


def _patched_quiet_tick_dependencies():
    return _PatchStack(
        patch("team_agent.coordinator.lifecycle.MessageStore", return_value=MagicMock()),
        patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
        patch("team_agent.runtime._refresh_agent_runtime_statuses"),
        patch("team_agent.runtime._handle_provider_startup_prompts"),
        patch("team_agent.runtime._handle_provider_runtime_prompts"),
        patch("team_agent.runtime._sync_agent_health", return_value={}),
        patch("team_agent.runtime._deliver_pending_messages", return_value=[]),
        patch("team_agent.runtime._fire_due_scheduled_events", return_value=[]),
        patch("team_agent.runtime._detect_stuck_agents", return_value=[]),
        patch("team_agent.runtime._collect_results_and_notify_watchers", return_value={"collected": 0}),
        patch("team_agent.messaging.idle_alerts.detect_cross_worker_deadlocks", return_value=[]),
        patch("team_agent.messaging.activity_detector.detect_compaction_degradation", return_value={}),
        patch("team_agent.messaging.leader_api_errors.detect_leader_api_errors", return_value=[]),
        patch("team_agent.messaging.session_drift.detect_session_drift", return_value=None),
        patch("team_agent.message_store.leader_notification_log.prune_leader_notification_log", return_value=0),
    )


class _PatchStack:
    def __init__(self, *patchers):
        self.patchers = patchers
        self.started = []

    def __enter__(self):
        for patcher in self.patchers:
            self.started.append(patcher.start())
        return self

    def __exit__(self, exc_type, exc, tb):
        for patcher in reversed(self.patchers):
            patcher.stop()
        return False


class _ProviderNetworkSentinel:
    def __init__(self) -> None:
        self.calls: list[str] = []
        self.messages = self
        self.chat = self
        self.completions = self

    def create(self, *args, **kwargs):
        self.calls.append("create")
        raise AssertionError("provider SDK create must not be called")

    def post(self, *args, **kwargs):
        self.calls.append("post")
        raise AssertionError("httpx.post must not be called")

    def assert_zero_calls(self) -> None:
        if self.calls:
            raise AssertionError(f"provider/network calls occurred: {self.calls}")


def _provider_network_sentinels() -> dict:
    return {
        "anthropic": _ProviderNetworkSentinel(),
        "openai": _ProviderNetworkSentinel(),
        "httpx": _ProviderNetworkSentinel(),
    }


if __name__ == "__main__":
    unittest.main()
