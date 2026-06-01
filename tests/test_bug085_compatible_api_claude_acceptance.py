from __future__ import annotations

import inspect
import json
import os
import tempfile
import time
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import MagicMock, patch

from team_agent.coordinator import lifecycle
from team_agent.events import EventLog
from team_agent.idle_predicate import evaluate_takeover_reminder
from team_agent.idle_takeover_wiring import build_idle_nodes
from team_agent.provider_cli.adapter import ResumeUnavailable
from team_agent.provider_cli.claude import ClaudeCodeAdapter, claude_project_dir
from team_agent.restart.selection import state_has_restart_context
from team_agent.sessions.capture import capture_missing_sessions, clear_session_capture_fields
from team_agent.state import save_runtime_state


FIXTURE = Path(__file__).parent / "fixtures" / "bug_085_compatible_api_claude" / "compatible_api_claude_idle_bad_first_line.jsonl"
REAL_CLAUDE_IDLE_FIXTURE = Path(__file__).parent / "fixtures" / "idle_takeover" / "claude_end_turn_with_metadata_tail.real.jsonl"


class Bug085CompatibleApiClaudeAcceptanceTests(unittest.TestCase):
    def test_compatible_api_fallback_fills_rollout_path_without_session_id(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-fallback-") as tmp:
            workspace, root, transcript = _workspace_with_transcript(Path(tmp))
            state = {"agents": {"compatible_worker": _agent_state(workspace, root, auth_mode="compatible_api")}}

            captured = capture_missing_sessions(workspace, state, EventLog(workspace), timeout_s=0.0)

            agent = state["agents"]["compatible_worker"]
            self.assertEqual(captured, ["compatible_worker"])
            self.assertIsNone(agent["session_id"])
            self.assertEqual(agent["rollout_path"], str(transcript))
            self.assertEqual(agent["captured_via"], "fs_mtime_fallback")
            self.assertEqual(agent["attribution_confidence"], "low")

    def test_strict_capture_wins_before_compatible_api_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-strict-") as tmp:
            workspace, root, transcript = _workspace_with_transcript(Path(tmp))
            state = {"agents": {"compatible_worker": _agent_state(workspace, root, auth_mode="compatible_api")}}
            strict = {
                "session_id": "strict-session-id",
                "rollout_path": str(transcript.with_name("strict.jsonl")),
                "captured_via": "fs_watch",
                "confidence": "high",
            }

            with patch("team_agent.provider_cli.claude.find_claude_transcript", return_value=strict):
                captured = capture_missing_sessions(workspace, state, EventLog(workspace), timeout_s=0.0)

            agent = state["agents"]["compatible_worker"]
            self.assertEqual(captured, ["compatible_worker"])
            self.assertEqual(agent["session_id"], "strict-session-id")
            self.assertEqual(agent["captured_via"], "fs_watch")
            self.assertNotEqual(agent["rollout_path"], str(transcript))

    def test_native_claude_does_not_use_compatible_api_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-native-") as tmp:
            workspace, root, _transcript = _workspace_with_transcript(Path(tmp))
            state = {"agents": {"native_worker": _agent_state(workspace, root, auth_mode="subscription")}}

            captured = capture_missing_sessions(workspace, state, EventLog(workspace), timeout_s=0.0)

            agent = state["agents"]["native_worker"]
            self.assertEqual(captured, [])
            self.assertIsNone(agent["session_id"])
            self.assertIsNone(agent["rollout_path"])

    def test_half_state_build_idle_nodes_classifies_fixture_idle_not_unknown(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-idle-node-") as tmp:
            workspace, _root, transcript = _workspace_with_transcript(Path(tmp))
            state = {
                "agents": {
                    "compatible_worker": {
                        "provider": "claude",
                        "auth_mode": "compatible_api",
                        "status": "running",
                        "session_id": None,
                        "rollout_path": str(transcript),
                    }
                }
            }

            nodes = build_idle_nodes(state)

            self.assertEqual(nodes[0]["node_id"], "compatible_worker")
            self.assertEqual(nodes[0]["state"], "idle")

    def test_native_claude_code_idle_transcript_counts_for_takeover_after_capture(self) -> None:
        state = {
            "agents": {
                "claude_worker": {
                    "provider": "claude_code",
                    "auth_mode": None,
                    "status": "running",
                    "session_id": "e4cc5db3-b70e-4c64-8263-73cb9dcc86db",
                    "rollout_path": str(REAL_CLAUDE_IDLE_FIXTURE),
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "first_send_at": "2026-06-01T18:03:20+00:00",
                },
                "codex_worker": {
                    "provider": "codex",
                    "status": "running",
                    "session_id": "codex-session",
                    "rollout_path": str(Path(__file__).parent / "fixtures" / "idle_takeover" / "codex_task_complete.real.jsonl"),
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "first_send_at": "2026-06-01T18:03:20+00:00",
                },
            }
        }

        nodes = build_idle_nodes(state)

        self.assertEqual({node["node_id"]: node["state"] for node in nodes}, {"claude_worker": "idle", "codex_worker": "idle"})
        events: list[tuple[str, dict]] = []
        result = evaluate_takeover_reminder(
            nodes,
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=90.0,
            debounce_seconds=60.0,
            event_sink=lambda name, fields: events.append((name, fields)),
        )
        self.assertTrue(result["should_ping"], result)
        self.assertIn("idle_takeover.ping", [name for name, _fields in events])

    def test_missing_claude_code_rollout_path_remains_unknown_and_never_counts_as_idle(self) -> None:
        nodes = build_idle_nodes({
            "agents": {
                "claude_worker": {
                    "provider": "claude_code",
                    "status": "running",
                    "session_id": None,
                    "rollout_path": None,
                }
            }
        })

        self.assertEqual(nodes[0]["state"], "unknown")
        result = evaluate_takeover_reminder(
            nodes,
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=90.0,
            debounce_seconds=60.0,
        )
        self.assertFalse(result["should_ping"], result)
        self.assertEqual(result["reason"], "node_unknown")

    def test_half_state_resume_restart_reset_and_status_consumers_are_safe(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-half-state-") as tmp:
            workspace, _root, transcript = _workspace_with_transcript(Path(tmp))
            agent = {
                "provider": "claude",
                "auth_mode": "compatible_api",
                "session_id": None,
                "rollout_path": str(transcript),
                "captured_at": "2026-06-02T00:00:02+00:00",
                "captured_via": "fs_mtime_fallback",
                "attribution_confidence": "low",
                "spawn_cwd": str(workspace),
            }

            with self.assertRaises(ResumeUnavailable):
                ClaudeCodeAdapter().build_resume_command(agent, workspace)
            self.assertTrue(state_has_restart_context({"agents": {"compatible_worker": agent}}))
            clear_session_capture_fields(agent)
            self.assertIsNone(agent["session_id"])
            self.assertIsNone(agent["rollout_path"])
            self.assertIsNone(agent["captured_via"])

    def test_no_ping_reason_change_emits_deduped_idle_takeover_no_ping(self) -> None:
        events: list[tuple[str, dict]] = []
        monitor = None
        unknown = [{"node_id": "w1", "role": "worker", "state": "unknown"}]
        working = [{"node_id": "w1", "role": "worker", "state": "working"}]

        for nodes in (unknown, unknown, working):
            result = evaluate_takeover_reminder(
                nodes,
                monitor_state=monitor,
                now_monotonic=10.0,
                debounce_seconds=60.0,
                event_sink=lambda name, fields: events.append((name, fields)),
            )
            monitor = result["monitor_state"]

        self.assertEqual([name for name, _fields in events], ["idle_takeover.no_ping", "idle_takeover.no_ping"])
        self.assertEqual([fields["reason"] for _name, fields in events], ["node_unknown", "node_working"])

    def test_coordinator_unknown_persistent_diagnostic_does_not_ping_or_count_idle(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-unknown-persistent-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": None,
                "leader": {"id": "leader"},
                "agents": {
                    "compatible_worker": {
                        "provider": "claude",
                        "auth_mode": "compatible_api",
                        "status": "running",
                        "session_id": None,
                        "rollout_path": "/tmp/missing-compatible-api.jsonl",
                    }
                },
                "tasks": [],
                "coordinator": {},
            }
            save_runtime_state(workspace, state)
            unknown_node = {
                "node_id": "compatible_worker",
                "role": "worker",
                "state": "unknown",
                "provider": "claude",
                "auth_mode": "compatible_api",
                "rollout_path": "/tmp/missing-compatible-api.jsonl",
            }
            with _patched_quiet_tick_dependencies(), \
                 patch("team_agent.idle_takeover_wiring.build_idle_nodes", return_value=[unknown_node]), \
                 patch("team_agent.idle_takeover_wiring.push_idle_reminder") as push:
                for _ in range(60):
                    lifecycle.coordinator_tick(workspace)

            events = _events(workspace)
            unknown_events = [event for event in events if event.get("event") == "idle_takeover.unknown_persistent"]
            self.assertTrue(unknown_events)
            self.assertEqual(unknown_events[-1]["node_id"], "compatible_worker")
            self.assertEqual(unknown_events[-1]["provider"], "claude")
            self.assertEqual(unknown_events[-1]["auth_mode"], "compatible_api")
            self.assertGreaterEqual(unknown_events[-1]["consecutive_ticks"], 60)
            self.assertEqual(push.call_count, 0)
            self.assertNotIn("idle_takeover.ping", [event["event"] for event in events])

    def test_unknown_recovery_clears_counter_and_emits_reason_change(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-unknown-recover-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(
                workspace,
                {
                    "session_name": None,
                    "leader": {"id": "leader"},
                    "agents": {"compatible_worker": {"provider": "claude", "auth_mode": "compatible_api", "status": "running"}},
                    "tasks": [],
                    "coordinator": {"unknown_ticks": {"compatible_worker": 61}},
                },
            )
            recovered_node = {"node_id": "compatible_worker", "role": "worker", "state": "idle", "provider": "claude", "auth_mode": "compatible_api"}
            with _patched_quiet_tick_dependencies(), \
                 patch("team_agent.idle_takeover_wiring.build_idle_nodes", return_value=[recovered_node]):
                lifecycle.coordinator_tick(workspace)

            saved = json.loads((workspace / ".team" / "runtime" / "state.json").read_text(encoding="utf-8"))
            self.assertNotIn("compatible_worker", saved["coordinator"].get("unknown_ticks", {}))
            self.assertIn("idle_takeover.no_ping", _event_names(workspace))

    def test_fallback_helper_lint_does_not_call_strict_find_claude_transcript(self) -> None:
        from team_agent.provider_cli import claude

        self.assertTrue(hasattr(claude, "find_compatible_api_claude_transcript_fallback"))
        source = inspect.getsource(claude.find_compatible_api_claude_transcript_fallback)
        self.assertNotIn("find_claude_transcript(", source)
        self.assertIn("glob", source)
        self.assertIn("stat", source)

    def test_bug085_fixture_paths_make_zero_provider_network_calls(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bug085-provider-zero-") as tmp:
            workspace, root, _transcript = _workspace_with_transcript(Path(tmp))
            state = {"agents": {"compatible_worker": _agent_state(workspace, root, auth_mode="compatible_api")}}
            sentinels = _provider_network_sentinels()
            with patch.dict("sys.modules", sentinels):
                capture_missing_sessions(workspace, state, EventLog(workspace), timeout_s=0.0)

            for sentinel in sentinels.values():
                sentinel.assert_zero_calls()


def _workspace_with_transcript(root_tmp: Path) -> tuple[Path, Path, Path]:
    workspace = root_tmp / "workspace"
    workspace.mkdir()
    projects_root = root_tmp / "claude-projects"
    project_dir = claude_project_dir(projects_root, workspace)
    project_dir.mkdir(parents=True)
    transcript = project_dir / "compatible-api-idle.jsonl"
    text = FIXTURE.read_text(encoding="utf-8").replace("__WORKSPACE__", str(workspace))
    transcript.write_text(text, encoding="utf-8")
    now = time.time()
    os.utime(transcript, (now, now))
    return workspace, projects_root, transcript


def _agent_state(workspace: Path, root: Path, *, auth_mode: str | None) -> dict:
    return {
        "provider": "claude",
        "auth_mode": auth_mode,
        "status": "starting",
        "session_name": "team-bug085",
        "window": "compatible_worker",
        "spawn_cwd": str(workspace),
        "spawned_at": datetime.now(timezone.utc).isoformat(),
        "claude_projects_root": str(root),
        "session_id": None,
        "rollout_path": None,
        "captured_at": None,
        "captured_via": None,
        "attribution_confidence": None,
    }


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

    def __enter__(self):
        for patcher in self.patchers:
            patcher.start()
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
