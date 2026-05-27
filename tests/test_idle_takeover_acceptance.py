from __future__ import annotations

import importlib
import inspect
import json
import unittest
from pathlib import Path
from typing import Any


FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "idle_takeover"


class EventSink:
    def __init__(self) -> None:
        self.events: list[dict[str, Any]] = []

    def __call__(self, event: Any, **payload: Any) -> None:
        if isinstance(event, dict):
            self.events.append(event)
        else:
            self.events.append({"event": event, **payload})


class IdleTakeoverAcceptanceTests(unittest.TestCase):
    """Gap 32 idle/takeover contract.

    Tests use real Codex rollout / Claude transcript fixtures where available.
    Codex failed-turn and permission-request cases are schema-derived because no
    real local archive contained those records; Mac mini E2E must supplement them.
    """

    def test_01_c1_reminder_arms_only_after_worker_delegation(self) -> None:
        api = self._api()
        leader = self._classify("claude", "claude_end_turn_with_metadata_tail.real.jsonl")
        worker = self._classify("codex", "codex_task_complete.real.jsonl")
        self.assertEqual(leader["state"], "idle")
        self.assertEqual(worker["state"], "idle")

        no_delegation = api.evaluate_takeover_reminder(
            [_node("leader", "leader", leader), _node("worker_a", "worker", worker)],
            monitor_state={"opened_worker_turn_since_ack": False, "all_idle_since": 0.0},
            now_monotonic=120.0,
            debounce_seconds=60.0,
        )
        self.assertFalse(no_delegation["should_ping"], no_delegation)
        self.assertEqual(no_delegation["reason"], "not_armed_no_worker_turn")

        delegated = api.evaluate_takeover_reminder(
            [_node("leader", "leader", leader), _node("worker_a", "worker", worker)],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=120.0,
            debounce_seconds=60.0,
        )
        self.assertTrue(delegated["should_ping"], delegated)

    def test_02_c2_ping_wording_is_neutral_all_idle_checkpoint(self) -> None:
        api = self._api()
        result = api.evaluate_takeover_reminder(
            [
                _node("leader", "leader", {"state": "idle", "turn_id": "leader_turn"}),
                _node("worker_a", "worker", {"state": "idle", "turn_id": "worker_turn"}),
            ],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=120.0,
            debounce_seconds=60.0,
        )
        self.assertTrue(result["should_ping"], result)
        message = result.get("message") or ""
        self.assertIn("all nodes idle", message.lower())
        self.assertIn("acknowledge-idle", message)
        forbidden_claims = ["unfinished work exists", "dropped task", "work remains"]
        self.assertFalse(any(claim in message.lower() for claim in forbidden_claims), message)

    def test_03_c3_ack_suppression_rearms_on_real_turn_open_after_delivery(self) -> None:
        api = self._api()
        sink = EventSink()
        monitor_state = {
            "suppressed": True,
            "acknowledged_at": 10.0,
            "opened_worker_turn_since_ack": False,
            "all_idle_since": None,
        }

        updated = api.record_turn_open_after_delivery(
            monitor_state,
            node_id="worker_a",
            turn_id="turn_after_delivery",
            delivered_message_id="msg_delivered_1",
            now_monotonic=20.0,
            event_sink=sink,
        )

        self.assertFalse(updated.get("suppressed"), updated)
        self.assertTrue(updated.get("opened_worker_turn_since_ack"), updated)
        self.assertEqual(updated.get("last_turn_open", {}).get("turn_id"), "turn_after_delivery")
        self.assertTrue(
            any(e.get("event") == "idle_takeover.turn_open_rearmed" for e in sink.events),
            sink.events,
        )

    def test_04_c4_pid_reuse_open_turn_is_crashed_mid_turn_not_working(self) -> None:
        state = self._classify(
            "codex",
            "codex_open_turn_silent_build.real.jsonl",
            process={
                "expected": {"pid": 1234, "start_time": 100.0, "cmdline": "codex --no-alt-screen"},
                "current": {"pid": 1234, "start_time": 900.0, "cmdline": "python unrelated.py"},
            },
            file_silence_seconds=600.0,
        )

        self.assertEqual(state["state"], "abnormal", state)
        self.assertEqual(state["reason"], "crashed_mid_turn")
        self.assertNotEqual(state["state"], "working")

    def test_05_c5_corrupt_or_changed_provider_format_is_unknown_and_never_idle(self) -> None:
        api = self._api()
        sink = EventSink()
        unknown = api.classify_provider_turn_state(
            "claude",
            _fixture("corrupt_changed_format.jsonl"),
            process=_matching_process(),
            event_sink=sink,
        )
        self.assertEqual(unknown["state"], "unknown", unknown)
        self.assertTrue(unknown.get("diagnostics"), unknown)

        result = api.evaluate_takeover_reminder(
            [
                _node("leader", "leader", {"state": "idle", "turn_id": "leader_turn"}),
                _node("worker_a", "worker", unknown),
            ],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=120.0,
            debounce_seconds=60.0,
        )
        self.assertFalse(result["should_ping"], result)
        self.assertEqual(result["reason"], "node_unknown")

    def test_06_c6_provider_strings_are_confined_to_provider_state_modules(self) -> None:
        neutral_paths = [
            Path("src/team_agent/idle_predicate.py"),
            Path("src/team_agent/abnormal_track.py"),
            Path("src/team_agent/wake.py"),
        ]
        for path in neutral_paths:
            self.assertTrue(path.exists(), f"{path} must exist as a provider-neutral module")
            content = path.read_text(encoding="utf-8").lower()
            self.assertNotIn("codex", content, f"{path} must not contain provider-specific strings")
            self.assertNotIn("claude", content, f"{path} must not contain provider-specific strings")

    def test_07_c7_provider_registry_is_shipped_infra_data_not_user_config(self) -> None:
        registry_module = importlib.import_module("team_agent.provider_state.registry")
        registry = registry_module.get_provider_registry()

        for provider in ("codex", "claude"):
            self.assertIn(provider, registry)
            entry = registry[provider]
            self.assertFalse(entry.get("requires_user_config", False), entry)
            self.assertIn("file_location", entry)
            self.assertIn("event_types", entry)
            self.assertIn("error_lists", entry)

    def test_08_c8_abnormal_notifications_dedupe_by_signature_and_turn(self) -> None:
        api = self._api()
        sink = EventSink()
        record = _json_records("claude_api_error.real.jsonl")[0]
        result = api.process_abnormal_records(
            [record, record],
            registry={"provider": "claude"},
            notification_state={},
            event_sink=sink,
        )

        notifications = result.get("notifications", [])
        self.assertEqual(len(notifications), 1, result)
        self.assertEqual(notifications[0].get("dedupe_key"), ("api_error", record.get("sessionId")))

    def test_09_c9_default_notify_only_for_structured_error_records_and_permission_blocks(self) -> None:
        api = self._api()
        failed = _json_records("codex_app_server_turn_failed.schema-derived.jsonl")[0]
        result = api.process_abnormal_records(
            [failed],
            registry={"provider": "codex"},
            notification_state={},
            event_sink=EventSink(),
        )
        notifications = result.get("notifications", [])
        self.assertEqual(len(notifications), 1, result)
        self.assertEqual(notifications[0].get("state"), "abnormal")
        self.assertEqual(notifications[0].get("raw_record"), failed)
        self.assertTrue(result.get("discovery_log"), result)

        noise = {"type": "new_unknown_record", "payload": {"looks": "structured but not error-class"}}
        noise_result = api.process_abnormal_records(
            [noise],
            registry={"provider": "codex"},
            notification_state={},
            event_sink=EventSink(),
        )
        self.assertEqual(noise_result.get("notifications"), [], noise_result)
        self.assertTrue(noise_result.get("diagnostics"), noise_result)

        permission = self._classify("codex", "codex_permission_request.schema-derived.jsonl")
        self.assertEqual(permission["state"], "blocked_on_human", permission)

    def test_10_c10_whole_team_gone_is_coordinator_independent_and_distinguishes_clean_shutdown(self) -> None:
        api = self._api()
        marker_store: dict[str, Any] = {}
        unexpected = api.detect_whole_team_gone(
            {
                "coordinator": {"alive": False},
                "leader": {"alive": False},
                "provider_processes": [],
                "tmux_sessions": [],
                "clean_shutdown": False,
                "restart_in_progress": False,
            },
            marker_store=marker_store,
            event_sink=EventSink(),
        )
        self.assertTrue(unexpected["notify"], unexpected)
        self.assertEqual(unexpected["state"], "whole_team_gone")
        self.assertTrue(marker_store.get("whole_team_gone"), marker_store)

        for clean_flag in ("clean_shutdown", "restart_in_progress"):
            clean_snapshot = {
                "coordinator": {"alive": False},
                "leader": {"alive": False},
                "provider_processes": [],
                "tmux_sessions": [],
                "clean_shutdown": clean_flag == "clean_shutdown",
                "restart_in_progress": clean_flag == "restart_in_progress",
            }
            clean = api.detect_whole_team_gone(clean_snapshot, marker_store={}, event_sink=EventSink())
            self.assertFalse(clean["notify"], clean)

    def test_11_c11_suspend_intervals_do_not_count_toward_idle_debounce(self) -> None:
        api = self._api()
        result = api.evaluate_takeover_reminder(
            [
                _node("leader", "leader", {"state": "idle", "turn_id": "leader_turn"}),
                _node("worker_a", "worker", {"state": "idle", "turn_id": "worker_turn"}),
            ],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 100.0},
            now_monotonic=200.0,
            debounce_seconds=60.0,
            suspend_intervals=[(120.0, 190.0)],
        )
        self.assertFalse(result["should_ping"], result)
        self.assertEqual(result["reason"], "debounce_active")
        self.assertLess(result.get("active_idle_seconds", 0), 60.0)

    def test_12_c12_idle_interrupted_counts_idle_but_ping_annotates_interrupted_nodes(self) -> None:
        api = self._api()
        leader = self._classify("claude", "claude_end_turn.real.jsonl")
        codex_interrupted = self._classify("codex", "codex_turn_aborted_interrupted.real.jsonl")
        claude_interrupted = self._classify("claude", "claude_interrupted.real.jsonl")
        self.assertEqual(codex_interrupted["state"], "idle_interrupted")
        self.assertEqual(claude_interrupted["state"], "idle_interrupted")

        result = api.evaluate_takeover_reminder(
            [
                _node("leader", "leader", leader),
                _node("worker_codex", "worker", codex_interrupted),
                _node("worker_claude", "worker", claude_interrupted),
            ],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=120.0,
            debounce_seconds=60.0,
        )
        self.assertTrue(result["should_ping"], result)
        interrupted = {item.get("node_id") for item in result.get("annotations", []) if item.get("state") == "idle_interrupted"}
        self.assertEqual(interrupted, {"worker_codex", "worker_claude"})

    def test_13_c13_leader_uses_provider_transcript_and_leader_gone_routes_to_whole_team_gone(self) -> None:
        api = self._api()
        leader = self._classify(
            "claude",
            "claude_end_turn_with_metadata_tail.real.jsonl",
            process=_matching_process(cmdline="claude"),
        )
        self.assertEqual(leader["state"], "idle", leader)
        self.assertEqual(leader["role"], "leader")

        whole_team = api.detect_whole_team_gone(
            {
                "coordinator": {"alive": False},
                "leader": {"alive": False, "provider_state": leader},
                "provider_processes": [],
                "tmux_sessions": [],
                "clean_shutdown": False,
                "restart_in_progress": False,
            },
            marker_store={},
            event_sink=EventSink(),
        )
        self.assertEqual(whole_team["state"], "whole_team_gone", whole_team)
        self.assertNotEqual(whole_team.get("reason"), "crashed_mid_turn")

    def test_14_c14_open_turn_with_long_silence_remains_working_and_blocks_ping(self) -> None:
        api = self._api()
        open_turn = self._classify(
            "codex",
            "codex_open_turn_silent_build.real.jsonl",
            process=_matching_process(cmdline="codex --no-alt-screen"),
            file_silence_seconds=900.0,
        )
        self.assertEqual(open_turn["state"], "working", open_turn)

        result = api.evaluate_takeover_reminder(
            [
                _node("leader", "leader", {"state": "idle", "turn_id": "leader_turn"}),
                _node("worker_a", "worker", open_turn),
            ],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=900.0,
            debounce_seconds=60.0,
        )
        self.assertFalse(result["should_ping"], result)
        self.assertEqual(result["reason"], "node_working")

    def test_15_high_coordinator_tick_uses_idle_takeover_not_legacy_scrollback_idle_fallback(self) -> None:
        lifecycle = importlib.import_module("team_agent.coordinator.lifecycle")
        source = inspect.getsource(lifecycle.coordinator_tick)

        self.assertNotIn(
            "detect_idle_fallbacks",
            source,
            "coordinator tick must not drive Gap 32 reminders through the old scrollback/agent_health idle fallback path",
        )
        self.assertIn("idle_takeover", source)
        self.assertTrue(
            "read_turn_state" in source or "evaluate_takeover_reminder" in source,
            "coordinator tick must call the new provider-file idle/takeover subsystem",
        )

    def test_16_c4_missing_or_partial_process_identity_is_not_working(self) -> None:
        cases = [
            None,
            {"expected": {"pid": 1234}, "current": {"pid": 1234}},
            {"expected": {"pid": 1234, "start_time": 100.0}, "current": {"pid": 1234}},
            {"expected": {"pid": 1234, "cmdline": "codex --no-alt-screen"}, "current": {"pid": 1234}},
        ]

        for process in cases:
            with self.subTest(process=process):
                state = self._classify(
                    "codex",
                    "codex_open_turn_silent_build.real.jsonl",
                    process=process,
                    file_silence_seconds=900.0,
                )
                self.assertNotEqual(state["state"], "working", state)
                self.assertIn(state["state"], {"unknown", "abnormal"}, state)
                self.assertTrue(state.get("diagnostics") or state.get("annotations"), state)

    def test_17_c8_missing_turn_id_errors_are_not_deduped_together(self) -> None:
        api = self._api()
        records = [
            {
                "jsonrpc": "2.0",
                "method": "turn/completed",
                "params": {
                    "threadId": "thread_schema_missing_turn",
                    "turn": {
                        "items": "notLoaded",
                        "status": "failed",
                        "error": {"message": "first failed turn without id"},
                    },
                },
            },
            {
                "jsonrpc": "2.0",
                "method": "turn/completed",
                "params": {
                    "threadId": "thread_schema_missing_turn",
                    "turn": {
                        "items": "notLoaded",
                        "status": "failed",
                        "error": {"message": "second failed turn without id"},
                    },
                },
            },
        ]

        result = api.process_abnormal_records(
            records,
            registry={"provider": "codex"},
            notification_state={},
            event_sink=EventSink(),
        )
        notifications = result.get("notifications", [])
        self.assertEqual(
            len(notifications),
            2,
            "missing turn_id must not collapse distinct structured errors into one global dedupe bucket",
        )
        self.assertEqual(len({n.get("dedupe_key") for n in notifications}), 2, notifications)

    def test_18_c11_overlapping_suspend_intervals_are_merged_before_debounce(self) -> None:
        api = self._api()
        result = api.evaluate_takeover_reminder(
            [
                _node("leader", "leader", {"state": "idle", "turn_id": "leader_turn"}),
                _node("worker_a", "worker", {"state": "idle", "turn_id": "worker_turn"}),
            ],
            monitor_state={"opened_worker_turn_since_ack": True, "all_idle_since": 0.0},
            now_monotonic=120.0,
            debounce_seconds=60.0,
            suspend_intervals=[(10.0, 50.0), (30.0, 70.0), (30.0, 70.0)],
        )

        self.assertTrue(
            result["should_ping"],
            "overlapping/duplicate suspend windows should subtract only the merged 10..70 interval, leaving 60s active idle",
        )
        self.assertGreaterEqual(result.get("active_idle_seconds", 60.0), 60.0)

    def _api(self):
        try:
            return importlib.import_module("team_agent.idle_takeover")
        except Exception as exc:  # pragma: no cover - the failure text is the contract gate
            self.fail(f"Missing Gap 32 public API module team_agent.idle_takeover: {exc}")

    def _classify(self, provider: str, fixture_name: str, **kwargs: Any) -> dict[str, Any]:
        state = self._api().classify_provider_turn_state(provider, _fixture(fixture_name), **kwargs)
        if provider == "claude" and "role" not in state:
            state["role"] = "leader"
        return state


def _fixture(name: str) -> str:
    return (FIXTURE_ROOT / name).read_text(encoding="utf-8")


def _json_records(name: str) -> list[dict[str, Any]]:
    return [json.loads(line) for line in _fixture(name).splitlines() if line.strip()]


def _node(node_id: str, role: str, state: dict[str, Any]) -> dict[str, Any]:
    node = dict(state)
    node["node_id"] = node_id
    node["role"] = role
    return node


def _matching_process(cmdline: str = "codex --no-alt-screen") -> dict[str, Any]:
    return {
        "expected": {"pid": 1234, "start_time": 100.0, "cmdline": cmdline},
        "current": {"pid": 1234, "start_time": 100.0, "cmdline": cmdline},
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
