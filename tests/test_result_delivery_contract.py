from __future__ import annotations

import importlib.util
import os
import unittest
from pathlib import Path
from unittest.mock import patch

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

from team_agent.events import EventLog


class ResultDeliveryContractTests(unittest.TestCase):
    def test_result_delivery_retries_same_result_id_and_reaches_leader_exactly_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-delivery-contract-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            delivered_to_leader: list[str] = []

            def flaky_leader_delivery(*args, **kwargs):
                content = args[2]
                self.assertIn(result_id, content, "retry must carry the original stored result_id")
                self.assertNotIn("pending", content.lower(), "leader should see the result itself, not a pending placeholder")
                if not delivered_to_leader:
                    delivered_to_leader.append("failed-attempt-no-screen")
                    return {"ok": False, "status": "capture_missing_token", "reason": "capture_missing_token"}
                delivered_to_leader.append(content)
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with patch("team_agent.runtime.send_message", side_effect=flaky_leader_delivery):
                first = runtime.coordinator_tick(workspace)
                second = runtime.coordinator_tick(workspace)

            self.assertTrue(first["ok"])
            self.assertTrue(second["ok"])
            successful_deliveries = [item for item in delivered_to_leader if item != "failed-attempt-no-screen"]
            self.assertEqual(len(successful_deliveries), 1, "leader pane must show the final result exactly once")
            self.assertIn(result_id, successful_deliveries[0])
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["watcher_id"], watcher_id)
            self.assertEqual(watcher["status"], "notified")
            self.assertEqual(watcher["result_id"], result_id)

    def test_owner_scoped_result_delivery_does_not_require_coordinator_leader_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-delivery-owner-env-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "owner-team",
                    "team_owner": {
                        "pane_id": "%owner",
                        "provider": "codex",
                        "machine_fingerprint": "machine-a",
                    },
                    "leader": spec["leader"],
                    "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%owner"},
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader", owner_team_id="owner-team")
            result_id = store.add_result(_result_envelope("success"))
            store.mark_result_collected(result_id)
            store.mark_result_watcher(watcher_id, "notify_failed", result_id=result_id, error="prior transient failure")
            delivered: list[str] = []

            def fake_leader_delivery(_workspace, state, _leader_id, content, *_args, **_kwargs):
                self.assertEqual(state["team_owner"]["pane_id"], "%owner")
                delivered.append(content)
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with (
                patch.dict(os.environ, {}, clear=True),
                patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader_delivery),
            ):
                tick = runtime._collect_results_and_notify_watchers(workspace, EventLog(workspace))

            self.assertTrue(tick["ok"])
            self.assertEqual(len(delivered), 1)
            self.assertIn(result_id, delivered[0])
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["watcher_id"], watcher_id)
            self.assertEqual(watcher["status"], "notified")

    def test_result_watcher_does_not_duplicate_report_result_leader_notification(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-delivery-dedup-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            message_id = store.create_message(
                "task_impl",
                "fake_impl",
                "leader",
                f"Task task_impl reported success from fake_impl: done\nResult id: {result_id}",
                requires_ack=False,
            )
            store.mark(message_id, "submitted")

            with patch("team_agent.runtime.send_message", side_effect=AssertionError("duplicate result notification")):
                tick = runtime._collect_results_and_notify_watchers(workspace, EventLog(workspace))

            self.assertTrue(tick["ok"])
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["watcher_id"], watcher_id)
            self.assertEqual(watcher["status"], "notified")
            self.assertEqual(watcher["notified_message_id"], message_id)


    def test_attach_leader_requeues_delivery_exhausted_watchers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-attach-requeue-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "exhausted-team",
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            store.mark_result_collected(result_id)
            store.mark_result_watcher(
                watcher_id,
                "delivery_exhausted",
                result_id=result_id,
                notified_message_id="msg_dead",
                error="retry budget exhausted",
            )
            before = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(before["status"], "delivery_exhausted")
            self.assertEqual(before["notified_message_id"], "msg_dead")

            with patch("team_agent.runtime._attach_leader_to_state", return_value=({"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%new"}, {"ok": True})):
                attached = runtime.attach_leader(workspace, pane="%new", provider="codex")

            self.assertTrue(attached.get("ok"))
            self.assertEqual(attached.get("requeued_exhausted_watchers"), [watcher_id])
            after = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(after["status"], "notify_failed")
            self.assertIsNone(after["error"])
            # Gap 32 dedupe: requeue must preserve notified_message_id so the retry path
            # can dedupe against a prior successful injection (reverses 78055bc).
            self.assertEqual(after["notified_message_id"], "msg_dead")
            requeued_events = [e for e in _events(workspace) if e.get("event") == "result_watcher.requeued"]
            self.assertEqual(len(requeued_events), 1)
            self.assertEqual(requeued_events[0]["watcher_id"], watcher_id)
            self.assertEqual(requeued_events[0]["new_pane_id"], "%new")

    def test_exhausted_watcher_redelivers_after_attach_leader_to_new_pane(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-f1-fullcycle-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "f1-team",
                    "leader": spec["leader"],
                    "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%dead"},
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_originally_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            store.mark_result_collected(result_id)
            # Gap 32: a failed delivery never persists notified_message_id (would otherwise
            # short-circuit the dedupe lookup and prevent the legitimate re-delivery this test
            # exercises). The 78055bc clear-on-requeue logic existed to undo the prior bug of
            # writing it for failed attempts; we no longer write it in the first place.
            store.mark_result_watcher(
                watcher_id,
                "delivery_exhausted",
                result_id=result_id,
                error="leader_pane_missing",
            )
            event_log = EventLog(workspace)
            for attempt in range(1, 6):
                event_log.write(
                    "result_watcher.notified",
                    watcher_id=watcher_id,
                    result_id=result_id,
                    task_id="task_impl",
                    agent_id="fake_impl",
                    ok=False,
                    delivery_status="fallback",
                    error="leader_pane_missing",
                    attempt=attempt,
                )

            from team_agent.messaging.result_delivery import result_delivery_attempts
            self.assertEqual(result_delivery_attempts(event_log, watcher_id, result_id), 5)

            def fake_attach(_workspace, state, *, pane, provider, event_log, source):
                receiver = {"mode": "direct_tmux", "status": "attached", "provider": provider, "pane_id": pane}
                state["leader_receiver"] = receiver
                return receiver, {"ok": True}

            with patch("team_agent.runtime._attach_leader_to_state", side_effect=fake_attach):
                runtime.attach_leader(workspace, pane="%alive", provider="codex")

            self.assertEqual(result_delivery_attempts(event_log, watcher_id, result_id), 0, "result_watcher.requeued event must reset the attempts counter")

            delivered_to_pane: list[str] = []

            def fake_leader_delivery(_workspace, state, _leader_id, content, *_args, **_kwargs):
                delivered_to_pane.append(state.get("leader_receiver", {}).get("pane_id"))
                return {"ok": True, "message_id": "msg_fresh", "status": "submitted"}

            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader_delivery):
                tick = runtime._collect_results_and_notify_watchers(workspace, event_log)

            self.assertTrue(tick["ok"])
            self.assertEqual(delivered_to_pane, ["%alive"], "retry must dispatch against the freshly-attached leader pane, not the dead one")
            after = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(after["watcher_id"], watcher_id)
            self.assertEqual(after["status"], "notified")
            self.assertEqual(after["notified_message_id"], "msg_fresh")

    def test_multiple_watcher_rows_for_same_result_dedupe_to_one_delivery(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watcher-dedupe-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "dedupe-team",
                    "leader": spec["leader"],
                    "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%owner"},
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            primary_id = store.create_result_watcher("task_impl", "fake_impl", "msg_a", "leader")
            extra_id_1 = store.create_result_watcher("task_impl", "fake_impl", "msg_b", "leader")
            extra_id_2 = store.create_result_watcher("task_impl", "fake_impl", "msg_c", "leader")
            result_id = store.add_result(_result_envelope("success"))
            delivered: list[str] = []

            def fake_leader_delivery(_workspace, _state, _leader_id, content, *_args, **_kwargs):
                delivered.append(content)
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader_delivery):
                tick = runtime._collect_results_and_notify_watchers(workspace, EventLog(workspace))

            self.assertTrue(tick["ok"])
            self.assertEqual(len(delivered), 1, "only one watcher should deliver per result; others must be superseded")
            watchers = {w["watcher_id"]: w for w in MessageStore(workspace).result_watchers()}
            self.assertEqual(watchers[primary_id]["status"], "notified")
            self.assertEqual(watchers[extra_id_1]["status"], "superseded")
            self.assertEqual(watchers[extra_id_2]["status"], "superseded")


class SchemaPreflightMigrationTests(unittest.TestCase):
    def test_preflight_does_not_block_migratable_owner_team_id_column(self) -> None:
        import sqlite3
        from team_agent import coordinator
        with tempfile.TemporaryDirectory(prefix="team-agent-preflight-migratable-") as tmp:
            workspace = Path(tmp)
            db_path = workspace / ".team" / "runtime" / "team.db"
            db_path.parent.mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(db_path)
            try:
                conn.execute(
                    """
                    create table messages (
                      message_id text primary key,
                      task_id text,
                      sender text,
                      recipient text,
                      reply_to text,
                      requires_ack integer,
                      status text,
                      content text,
                      artifact_refs text,
                      created_at text,
                      updated_at text,
                      delivered_at text,
                      acknowledged_at text,
                      error text
                    )
                    """
                )
                conn.commit()
            finally:
                conn.close()

            health = coordinator.message_store_schema_health(workspace)

        self.assertTrue(health["schema_ok"], health)
        self.assertNotIn("reason", health)

    def test_schema_incompatible_envelope_includes_action_hint(self) -> None:
        import sqlite3
        from team_agent import coordinator
        with tempfile.TemporaryDirectory(prefix="team-agent-action-hint-") as tmp:
            workspace = Path(tmp)
            db_path = workspace / ".team" / "runtime" / "team.db"
            db_path.parent.mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(db_path)
            try:
                conn.execute(
                    """
                    create table results (
                      result_id text primary key,
                      task_id text not null,
                      agent_id text not null,
                      envelope text not null,
                      created_at text not null
                    )
                    """
                )
                conn.commit()
            finally:
                conn.close()

            health = coordinator.message_store_schema_health(workspace)

        self.assertFalse(health["schema_ok"])
        self.assertIn("action", health)
        self.assertIn("repair-state --schema", health["action"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
