from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.messaging import result_delivery

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


_TEAM = "team-gap32"


def _bootstrap_workspace(workspace: Path, *, owner_team_id: str | None = _TEAM) -> tuple[MessageStore, dict]:
    spec = _fake_spec(workspace)
    task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    state = {
        "spec_path": str(spec_path),
        "team_dir": str(workspace / ".team" / (owner_team_id or "default")),
        "session_name": owner_team_id or "team-default",
        "leader": spec["leader"],
        "leader_receiver": {
            "mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%leader",
        },
        "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
        "tasks": [task],
    }
    save_runtime_state(workspace, state)
    return MessageStore(workspace), state


def _result(result_id: str | None = None) -> dict:
    entry = {
        "result_id": result_id,
        "task_id": "task_impl",
        "agent_id": "fake_impl",
        "status": "success",
        "summary": "done",
        "tests": [],
        "scope": "task",
    }
    return entry


class Gap32ResultDedupeTests(unittest.TestCase):

    def test_gap32_same_result_after_notified_suppresses_injection(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap32-1-") as tmp:
            workspace = Path(tmp)
            store, _ = _bootstrap_workspace(workspace)
            event_log = EventLog(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader", owner_team_id=_TEAM)
            result_id = store.add_result(_result_envelope("success"), owner_team_id=_TEAM)
            store.mark_result_watcher(watcher_id, "notified", result_id=result_id, notified_message_id="msg_first")

            # Same watcher rescheduled for a redelivery (rebind retry); status flipped back.
            store.mark_result_watcher(watcher_id, "notify_failed", result_id=result_id)
            refetched = store.result_watchers(owner_team_id=_TEAM)[0]
            self.assertEqual(refetched["notified_message_id"], "msg_first",
                "Gap 32 reversal of 78055bc: notified_message_id must survive requeue")

            send_calls: list[dict] = []

            def fake_send(*args, **kwargs):
                send_calls.append({"args": args, "kwargs": kwargs})
                return {"ok": True, "status": "submitted", "message_id": "msg_second"}

            with patch.object(result_delivery, "deliver_stored_message", side_effect=fake_send), \
                 patch.object(result_delivery, "send_message", side_effect=fake_send):
                outcomes = result_delivery.retry_result_deliveries(workspace, event_log)

            self.assertEqual(send_calls, [], "no leader injection expected after already-notified")
            self.assertEqual(len(outcomes), 1)
            self.assertTrue(outcomes[0].get("deduped"))
            self.assertEqual(outcomes[0].get("message_id"), "msg_first")
            self.assertEqual(outcomes[0].get("dedupe_reason"), "rebind_retry")
            events = [e for e in _events(workspace) if e.get("event") == "leader_receiver.notification_dedupe_skip"]
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0]["result_id"], result_id)
            self.assertEqual(events[0]["suppressed_message_id"], "msg_first")
            self.assertEqual(events[0]["reason"], "rebind_retry")
            self.assertEqual(events[0]["team_id"], _TEAM)

    def test_gap32_multiple_watchers_one_result_one_notification(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap32-2-") as tmp:
            workspace = Path(tmp)
            store, _ = _bootstrap_workspace(workspace)
            event_log = EventLog(workspace)
            watcher_a = store.create_result_watcher("task_impl", "fake_impl", "msg_a", "leader", owner_team_id=_TEAM)
            watcher_b = store.create_result_watcher("task_impl", "fake_impl", "msg_b", "leader", owner_team_id=_TEAM)
            watcher_c = store.create_result_watcher("task_impl", "fake_impl", "msg_c", "leader", owner_team_id=_TEAM)
            result_id = store.add_result(_result_envelope("success"), owner_team_id=_TEAM)

            sends: list[dict] = []
            def fake_send(*_a, **kwargs):
                sends.append(kwargs)
                return {"ok": True, "status": "submitted", "message_id": f"msg_n{len(sends)}"}

            with patch.object(result_delivery, "deliver_stored_message", side_effect=fake_send), \
                 patch.object(result_delivery, "send_message", side_effect=fake_send):
                outcomes = result_delivery.notify_result_watchers(workspace, _result(result_id), event_log)

            self.assertEqual(len(sends), 1, f"exactly one leader injection expected, got {sends}")
            rows = {w["watcher_id"]: w for w in store.result_watchers(owner_team_id=_TEAM)}
            statuses = sorted([rows[wid]["status"] for wid in (watcher_a, watcher_b, watcher_c)])
            self.assertEqual(statuses.count("notified"), 1)
            self.assertEqual(statuses.count("superseded"), 2)
            notified_ids = {rows[wid]["notified_message_id"] for wid in (watcher_a, watcher_b, watcher_c)
                            if rows[wid]["status"] == "notified"}
            self.assertEqual(len(notified_ids), 1)

    def test_gap32_repeat_report_result_same_id_no_second_message(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap32-3-") as tmp:
            workspace = Path(tmp)
            store, _ = _bootstrap_workspace(workspace)
            event_log = EventLog(workspace)
            result_id = store.add_result(_result_envelope("success"), owner_team_id=_TEAM)

            sends: list[dict] = []
            def fake_send(*_a, **kwargs):
                sends.append(kwargs)
                return {"ok": True, "status": "submitted", "message_id": f"msg_{len(sends)}"}

            with patch.object(result_delivery, "deliver_stored_message", side_effect=fake_send), \
                 patch.object(result_delivery, "send_message", side_effect=fake_send):
                # First report_result-style submission: create watcher, notify.
                w1 = store.create_result_watcher("task_impl", "fake_impl", "msg_first", "leader", owner_team_id=_TEAM)
                first = result_delivery.notify_result_watchers(workspace, _result(result_id), event_log, watchers=[store.result_watchers(owner_team_id=_TEAM)[0]])
                self.assertEqual(len(sends), 1)
                self.assertTrue(first[0]["ok"])

                # Second report_result-style submission for same result_id: new watcher, must dedupe.
                w2 = store.create_result_watcher("task_impl", "fake_impl", "msg_second", "leader", owner_team_id=_TEAM)
                w2_row = next(w for w in store.result_watchers(owner_team_id=_TEAM) if w["watcher_id"] == w2)
                second = result_delivery.notify_result_watchers(
                    workspace, _result(result_id), event_log, watchers=[w2_row],
                    dedupe_reason="repeated_report_result",
                )

            self.assertEqual(len(sends), 1, "second submission must not produce a second leader injection")
            self.assertTrue(second[0]["deduped"])
            self.assertEqual(second[0]["dedupe_reason"], "repeated_report_result")
            dedupe_events = [e for e in _events(workspace) if e.get("event") == "leader_receiver.notification_dedupe_skip"]
            self.assertTrue(dedupe_events)
            self.assertEqual(dedupe_events[-1]["result_id"], result_id)
            self.assertEqual(dedupe_events[-1]["reason"], "repeated_report_result")

    def test_gap32_rebind_retry_same_result_exactly_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap32-4-") as tmp:
            workspace = Path(tmp)
            store, _ = _bootstrap_workspace(workspace)
            event_log = EventLog(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_orig", "leader", owner_team_id=_TEAM)
            result_id = store.add_result(_result_envelope("success"), owner_team_id=_TEAM)

            sends: list[dict] = []
            def fake_send(*_a, **kwargs):
                sends.append(kwargs)
                return {"ok": True, "status": "submitted", "message_id": "msg_pane_old"}

            with patch.object(result_delivery, "deliver_stored_message", side_effect=fake_send), \
                 patch.object(result_delivery, "send_message", side_effect=fake_send):
                # First delivery succeeds on stale pane (gets msg_pane_old).
                result_delivery.notify_result_watchers(workspace, _result(result_id), event_log)
                self.assertEqual(len(sends), 1)

                # Exhaust + requeue (mimic Phase D F1 attach-leader trigger). Watcher transitions
                # through delivery_exhausted then back to notify_failed by requeue; notified_message_id
                # MUST be preserved (Gap 32 reversal of 78055bc).
                store.mark_result_watcher(watcher_id, "delivery_exhausted", result_id=result_id)
                requeued = store.requeue_delivery_exhausted_watchers()
                self.assertEqual(requeued, [watcher_id])
                refetched = store.result_watchers(owner_team_id=_TEAM)[0]
                self.assertEqual(refetched["notified_message_id"], "msg_pane_old")
                self.assertEqual(refetched["status"], "notify_failed")

                # Retry path runs after rebind — must dedupe, NOT inject again.
                result_delivery.retry_result_deliveries(workspace, event_log)

            self.assertEqual(len(sends), 1, "rebind retry must not produce a second leader injection")
            dedupe_events = [e for e in _events(workspace) if e.get("event") == "leader_receiver.notification_dedupe_skip"]
            self.assertTrue(dedupe_events)
            self.assertEqual(dedupe_events[-1]["reason"], "rebind_retry")
            self.assertEqual(dedupe_events[-1]["result_id"], result_id)
            self.assertEqual(dedupe_events[-1]["suppressed_message_id"], "msg_pane_old")
            self.assertEqual(dedupe_events[-1]["original_message_id"], "msg_pane_old")

    def test_gap32_dedupe_event_schema(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap32-5-") as tmp:
            workspace = Path(tmp)
            store, _ = _bootstrap_workspace(workspace)
            event_log = EventLog(workspace)
            watcher_a = store.create_result_watcher("task_impl", "fake_impl", "msg_orig", "leader", owner_team_id=_TEAM)
            result_id = store.add_result(_result_envelope("success"), owner_team_id=_TEAM)
            store.mark_result_watcher(watcher_a, "notified", result_id=result_id, notified_message_id="msg_orig_delivered")

            watcher_b = store.create_result_watcher("task_impl", "fake_impl", "msg_dup", "leader", owner_team_id=_TEAM)
            wb_row = next(w for w in store.result_watchers(owner_team_id=_TEAM) if w["watcher_id"] == watcher_b)

            with patch.object(result_delivery, "deliver_stored_message", side_effect=AssertionError("no inject")), \
                 patch.object(result_delivery, "send_message", side_effect=AssertionError("no inject")):
                result_delivery.notify_result_watchers(workspace, _result(result_id), event_log, watchers=[wb_row])

            dedupe_events = [e for e in _events(workspace) if e.get("event") == "leader_receiver.notification_dedupe_skip"]
            self.assertTrue(dedupe_events)
            evt = dedupe_events[-1]
            for required in ("result_id", "original_message_id", "suppressed_message_id", "reason", "team_id"):
                self.assertIn(required, evt, f"event missing required field {required}")
            self.assertEqual(evt["result_id"], result_id)
            self.assertEqual(evt["team_id"], _TEAM)
            self.assertEqual(evt["suppressed_message_id"], "msg_orig_delivered")
            self.assertIn(evt["reason"], {"watcher_duplicate", "repeated_report_result", "rebind_retry"})
            # Schema must not leak full envelope body — only metadata.
            for forbidden in ("envelope", "content", "tests", "next_actions"):
                self.assertNotIn(forbidden, evt, f"event must not include {forbidden}")

    def test_upsert_atomic_under_thread_race(self) -> None:
        # Stage 11.12: N concurrent notify_result_watchers calls against the same
        # (team_id, result_id) must collapse to exactly one deliver_attempt via the
        # claim_leader_notification atomic UPSERT (BEGIN IMMEDIATE serialises). N-1
        # callers must observe 'already_notified_by' and emit notification_dedupe_skip.
        import threading
        with tempfile.TemporaryDirectory(prefix="team-agent-gap32-upsert-race-") as tmp:
            workspace = Path(tmp)
            store, _ = _bootstrap_workspace(workspace)
            event_log = EventLog(workspace)
            N = 4
            watcher_ids = [
                store.create_result_watcher("task_impl", "fake_impl", f"msg_{i}", "leader", owner_team_id=_TEAM)
                for i in range(N)
            ]
            result_id = store.add_result(_result_envelope("success"), owner_team_id=_TEAM)
            watchers_by_id = {w["watcher_id"]: w for w in store.result_watchers(owner_team_id=_TEAM)}

            send_lock = threading.Lock()
            sends: list[str] = []
            def fake_send(*_a, **kwargs):
                with send_lock:
                    sends.append(kwargs.get("team") or "unknown")
                return {"ok": True, "status": "submitted", "message_id": f"msg_real_{len(sends)}"}

            barrier = threading.Barrier(N)

            def worker(wid):
                barrier.wait()  # release all N threads at once
                result_delivery.notify_result_watchers(
                    workspace, _result(result_id), event_log,
                    watchers=[watchers_by_id[wid]],
                )

            # Patch the module-level deliver hooks ONCE in the main thread; concurrent
            # patch.object inside per-thread workers would race the restore step and
            # leave the module-level attribute pointing at one thread's fake after exit,
            # contaminating subsequent tests.
            with patch.object(result_delivery, "deliver_stored_message", side_effect=fake_send), \
                 patch.object(result_delivery, "send_message", side_effect=fake_send):
                threads = [threading.Thread(target=worker, args=(wid,)) for wid in watcher_ids]
                for t in threads:
                    t.start()
                for t in threads:
                    t.join()

            self.assertEqual(len(sends), 1,
                f"exactly one deliver_attempt under N={N} concurrent UPSERT race; got {len(sends)}: {sends}")
            dedupe_skips = [e for e in _events(workspace) if e.get("event") == "leader_receiver.notification_dedupe_skip"]
            self.assertEqual(len(dedupe_skips), N - 1,
                f"N-1 callers must dedupe-skip; got {len(dedupe_skips)} events")
            rows = store.result_watchers(owner_team_id=_TEAM)
            notified_ids = {row.get("notified_message_id") for row in rows if row.get("notified_message_id")}
            self.assertEqual(len(notified_ids), 1,
                f"all watchers must converge on a single canonical notified_message_id; got {notified_ids}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
