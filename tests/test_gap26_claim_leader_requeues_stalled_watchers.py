from __future__ import annotations

import copy
import importlib.util
import os
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from unittest.mock import patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging import result_delivery
from team_agent.simple_yaml import dumps
from team_agent.state import (
    derive_leader_session_uuid,
    load_runtime_state,
    save_runtime_state,
)


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _events(workspace: Path) -> list[dict[str, Any]]:
    import json
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def _result_envelope() -> dict[str, Any]:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": "task_impl",
        "agent_id": "fake_impl",
        "status": "success",
        "summary": "done",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


def _setup_two_candidate_workspace(tmp: str) -> tuple[Path, str, str, str, dict[str, Any]]:
    """Workspace with team_owner uuid bound to a stale pane and a recent ambiguous incident.
    Returns (workspace, owner_uuid, candidate_a_pane, candidate_b_pane, incident_event).
    """
    workspace = Path(tmp)
    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-claim"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    workspace_abspath = str(workspace.resolve())
    user = os.environ.get("USER") or os.environ.get("USERNAME") or ""
    owner_uuid = derive_leader_session_uuid("mfp-claim", workspace_abspath, user, "current")
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "team_dir": str(team_dir),
            "workspace": workspace_abspath,
            "session_name": "team-claim",
            "leader": spec["leader"],
            "team_owner": {
                "pane_id": "%stale",
                "provider": "codex",
                "machine_fingerprint": "mfp-claim",
                "leader_session_uuid": owner_uuid,
                "owner_epoch": 0,
            },
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": "%stale",
                "leader_session_uuid": owner_uuid,
                "owner_epoch": 0,
            },
            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
            "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}],
        },
    )
    candidate_a = "%cand_a"
    candidate_b = "%cand_b"
    incident_ts = (datetime.now(timezone.utc) - timedelta(seconds=10)).isoformat()
    event_log = EventLog(workspace)
    incident_event = event_log.write(
        "leader_receiver.ambiguous_candidates",
        incident_id="incident-claim-1",
        team_id="current",
        old_pane_id="%stale",
        candidates=[candidate_a, candidate_b],
        ts=incident_ts,
    )
    return workspace, owner_uuid, candidate_a, candidate_b, incident_event


def _fake_target(pane_id: str, owner_uuid: str) -> dict[str, Any]:
    return {
        "pane_id": pane_id,
        "session_name": "team-claim",
        "window_index": "0",
        "window_name": pane_id.lstrip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.lstrip('%')[:3]}",
        "pane_current_command": "claude.exe",
        "pane_active": True,
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid},
        "leader_session_uuid": owner_uuid,
    }


class ClaimLeaderRequeueTests(unittest.TestCase):

    def test_claim_leader_requeues_stalled_result_watchers_after_ambiguous(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-requeue-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, incident = _setup_two_candidate_workspace(tmp)
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope())
            store.mark_result_watcher(
                watcher_id, "delivery_exhausted",
                result_id=result_id,
                error="ambiguous_candidates",
            )

            redelivery_calls: list[dict[str, Any]] = []

            def fake_redeliver(*args, **kwargs):
                redelivery_calls.append({"args": args, "kwargs": kwargs})
                return {"ok": True, "status": "submitted", "message_id": "msg_fresh"}

            env_patch = {
                "TEAM_AGENT_LEADER_PANE_ID": candidate_a,
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-claim",
                "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
                "TEAM_AGENT_ID": "leader",
            }
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, env_patch, clear=False),
                patch.object(runtime, "core_list_targets", return_value=targets),
                patch.object(result_delivery, "deliver_stored_message", side_effect=fake_redeliver),
                patch.object(result_delivery, "send_message", side_effect=fake_redeliver),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["status"], "claimed")
            self.assertEqual(result["leader_receiver"]["pane_id"], candidate_a)
            self.assertEqual(result["requeued_watchers"], [watcher_id])
            after = next(w for w in store.result_watchers() if w["watcher_id"] == watcher_id)
            self.assertEqual(after["status"], "notified")
            self.assertEqual(after["notified_message_id"], "msg_fresh")
            self.assertEqual(len(redelivery_calls), 1, "claim must trigger immediate redelivery against the new pane")

    def test_claim_leader_emits_claim_requeue_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-event-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, _ = _setup_two_candidate_workspace(tmp)
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope())
            store.mark_result_watcher(watcher_id, "delivery_exhausted", result_id=result_id)

            env_patch = {
                "TEAM_AGENT_LEADER_PANE_ID": candidate_a,
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-claim",
                "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
                "TEAM_AGENT_ID": "leader",
            }
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, env_patch, clear=False),
                patch.object(runtime, "core_list_targets", return_value=targets),
                patch.object(result_delivery, "deliver_stored_message", return_value={"ok": True, "status": "submitted", "message_id": "msg_fresh"}),
                patch.object(result_delivery, "send_message", return_value={"ok": True, "status": "submitted", "message_id": "msg_fresh"}),
            ):
                runtime.claim_leader(workspace, confirm=True)

            events = [e for e in _events(workspace) if e.get("event") == "leader_receiver.claim_requeue"]
            self.assertEqual(len(events), 1, f"expected one claim_requeue event; got {events}")
            evt = events[0]
            self.assertEqual(evt.get("result_id"), result_id)
            self.assertEqual(evt.get("watcher_id"), watcher_id)
            self.assertEqual(evt.get("prior_state"), "delivery_exhausted")
            self.assertEqual(evt.get("claimed_pane_id"), candidate_a)
            self.assertIsNotNone(evt.get("requeued_at"))

    def test_claim_leader_handles_no_stalled_watchers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-empty-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, _ = _setup_two_candidate_workspace(tmp)
            # no watchers at all
            env_patch = {
                "TEAM_AGENT_LEADER_PANE_ID": candidate_a,
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-claim",
                "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
                "TEAM_AGENT_ID": "leader",
            }
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, env_patch, clear=False),
                patch.object(runtime, "core_list_targets", return_value=targets),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["status"], "claimed")
            self.assertEqual(result["requeued_watchers"], [])
            requeue_events = [e for e in _events(workspace) if e.get("event") == "leader_receiver.claim_requeue"]
            self.assertEqual(requeue_events, [], "must not emit claim_requeue events when no stalled watchers")

    def test_claim_leader_failed_redelivery_does_not_break_claim(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-redelivery-fail-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, _ = _setup_two_candidate_workspace(tmp)
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope())
            store.mark_result_watcher(watcher_id, "delivery_exhausted", result_id=result_id)

            def failing_deliver(*_a, **_kw):
                return {"ok": False, "status": "failed", "reason": "pane_died", "message_id": None}

            env_patch = {
                "TEAM_AGENT_LEADER_PANE_ID": candidate_a,
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-claim",
                "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
                "TEAM_AGENT_ID": "leader",
            }
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, env_patch, clear=False),
                patch.object(runtime, "core_list_targets", return_value=targets),
                patch.object(result_delivery, "deliver_stored_message", side_effect=failing_deliver),
                patch.object(result_delivery, "send_message", side_effect=failing_deliver),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], "claim must succeed even if requeued redelivery fails")
            self.assertEqual(result["status"], "claimed")
            self.assertEqual(result["requeued_watchers"], [watcher_id])
            after = next(w for w in store.result_watchers() if w["watcher_id"] == watcher_id)
            self.assertIn(after["status"], {"notify_failed", "delivery_exhausted"},
                          "failed redelivery must leave watcher in a retry-able terminal state")


    def test_claim_leader_requeues_pending_watchers(self) -> None:
        # Stage 11.10: a watcher still in 'pending' status (no exhausted retries yet) must
        # also be re-routed by claim-leader. Old filter only matched delivery_exhausted /
        # notify_failed and silently dropped pending watchers; Mac mini Scenario 3 evidence
        # showed this caused the actual worker result to never reach the claimed pane.
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-pending-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, _ = _setup_two_candidate_workspace(tmp)
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope())
            # Bind the result_id to the watcher row WITHOUT moving status off 'pending'.
            with closing_helper(store) as conn:
                conn.execute(
                    "update result_watchers set result_id = ? where watcher_id = ?",
                    (result_id, watcher_id),
                )
            # Verify precondition: watcher is 'pending' with no notified_message_id.
            row = next(w for w in store.result_watchers() if w["watcher_id"] == watcher_id)
            self.assertEqual(row["status"], "pending")
            self.assertIsNone(row.get("notified_message_id"))

            redelivery_calls: list[dict[str, Any]] = []

            def fake_redeliver(*args, **kwargs):
                redelivery_calls.append({"args": args, "kwargs": kwargs})
                return {"ok": True, "status": "submitted", "message_id": "msg_fresh_pending"}

            env_patch = {
                "TEAM_AGENT_LEADER_PANE_ID": candidate_a,
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-claim",
                "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
                "TEAM_AGENT_ID": "leader",
            }
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, env_patch, clear=False),
                patch.object(runtime, "core_list_targets", return_value=targets),
                patch.object(result_delivery, "deliver_stored_message", side_effect=fake_redeliver),
                patch.object(result_delivery, "send_message", side_effect=fake_redeliver),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["requeued_watchers"], [watcher_id])
            after = next(w for w in store.result_watchers() if w["watcher_id"] == watcher_id)
            self.assertEqual(after["status"], "notified")
            self.assertEqual(after["notified_message_id"], "msg_fresh_pending")
            self.assertEqual(len(redelivery_calls), 1)
            events = _events(workspace)
            requeue_events = [e for e in events if e.get("event") == "leader_receiver.claim_requeue"]
            self.assertEqual(len(requeue_events), 1)
            self.assertEqual(requeue_events[0].get("prior_state"), "pending",
                             "prior_state must reflect actual watcher status pre-requeue")

    def test_claim_leader_atomic_against_scheduled_retry(self) -> None:
        # Stage 11.12 reframe: the per-watcher CAS re-fetch + claim_requeue_already_in_flight
        # event from 9f52048 are RETIRED. The atomic UPSERT in claim_leader_notification
        # (notify_result_watchers gate) is now the single race contract. If the scheduled
        # retry already committed notified_message_id, the requeue scan sees it in the
        # snapshot and skips silently. Concurrent races (claim + retry firing simultaneously
        # against pending watcher) are covered by test_concurrent_claim_requeue_and_scheduled_retry_*
        # via the UPSERT serialisation, not by per-watcher CAS here.
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-atomic-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, _ = _setup_two_candidate_workspace(tmp)
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope())
            # Simulate the scheduled-retry having already committed notified_message_id BEFORE
            # claim-leader runs. The snapshot scan in requeue_after_claim_leader sees the
            # notified_message_id != null and skips this watcher.
            store.mark_result_watcher(
                watcher_id, "notified",
                result_id=result_id,
                notified_message_id="msg_scheduled_won",
            )

            redelivery_calls: list[dict[str, Any]] = []

            def fake_redeliver(*args, **kwargs):
                redelivery_calls.append({"args": args, "kwargs": kwargs})
                return {"ok": True, "status": "submitted", "message_id": "msg_should_not_fire"}

            env_patch = {
                "TEAM_AGENT_LEADER_PANE_ID": candidate_a,
                "TEAM_AGENT_LEADER_PROVIDER": "codex",
                "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-claim",
                "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
                "TEAM_AGENT_ID": "leader",
            }
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, env_patch, clear=False),
                patch.object(runtime, "core_list_targets", return_value=targets),
                patch.object(result_delivery, "deliver_stored_message", side_effect=fake_redeliver),
                patch.object(result_delivery, "send_message", side_effect=fake_redeliver),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["requeued_watchers"], [],
                             "watcher already notified by scheduled retry must not be re-requeued")
            events = _events(workspace)
            requeue_events = [e for e in events if e.get("event") == "leader_receiver.claim_requeue"]
            self.assertEqual(requeue_events, [], "no claim_requeue when snapshot sees notified_message_id set")
            self.assertEqual(redelivery_calls, [], "no redelivery should fire when watcher already notified")
            # Stage 11.12: claim_requeue_already_in_flight is RETIRED; verify it does NOT appear.
            retired_events = [e for e in events if e.get("event") == "leader_receiver.claim_requeue_already_in_flight"]
            self.assertEqual(retired_events, [], "claim_requeue_already_in_flight event was retired in Stage 11.12")


    def test_concurrent_claim_requeue_and_scheduled_retry_same_result_exactly_once(self) -> None:
        # Stage 11.12: the two delivery paths (claim_leader requeue and coordinator's
        # scheduled retry) must collapse to exactly one deliver_attempt for a given
        # (team_id, result_id) when fired simultaneously. The atomic UPSERT in
        # claim_leader_notification (BEGIN IMMEDIATE) is the serialisation point — both
        # paths route through notify_result_watchers which calls the UPSERT.
        import threading
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-concurrent-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b, _ = _setup_two_candidate_workspace(tmp)
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope())
            with closing_helper(store) as conn:
                conn.execute(
                    "update result_watchers set result_id = ? where watcher_id = ?",
                    (result_id, watcher_id),
                )

            # Stage 12 reframe: the two delivery paths (claim_leader requeue +
            # coordinator scheduled retry) both terminate at _send_to_leader_receiver
            # which consults claim_leader_notification_delivery atomically. INSERT OR
            # IGNORE on the leader_notification_log table guarantees exactly one row per
            # (result_id, uuid). This test exercises the atomic primitive directly with
            # the two-path scenario.
            from team_agent.message_store.leader_notification_log import (
                claim_leader_notification_delivery,
                leader_notification_log_rows,
            )
            two_path_result_id = "res_two_path_x"
            barrier = threading.Barrier(2)
            outcomes: list[tuple[str, dict]] = []
            outcomes_lock = threading.Lock()

            def path_claim_requeue():
                barrier.wait()
                r = claim_leader_notification_delivery(
                    store, result_id=two_path_result_id, leader_session_uuid=owner_uuid,
                    proposed_message_id="msg_from_claim_requeue", envelope_hash="hash_eq",
                    owner_team_id="current", pane_id=candidate_a,
                )
                with outcomes_lock:
                    outcomes.append(("claim", r))

            def path_scheduled_retry():
                barrier.wait()
                r = claim_leader_notification_delivery(
                    store, result_id=two_path_result_id, leader_session_uuid=owner_uuid,
                    proposed_message_id="msg_from_scheduled_retry", envelope_hash="hash_eq",
                    owner_team_id="current", pane_id=candidate_a,
                )
                with outcomes_lock:
                    outcomes.append(("retry", r))

            t1 = threading.Thread(target=path_claim_requeue)
            t2 = threading.Thread(target=path_scheduled_retry)
            t1.start(); t2.start(); t1.join(); t2.join()

            claimed = [(name, r) for name, r in outcomes if r["status"] == "claimed_by_you"]
            already = [(name, r) for name, r in outcomes if r["status"] == "already_notified_by"]
            self.assertEqual(len(claimed), 1, f"exactly one path wins; got {claimed}")
            self.assertEqual(len(already), 1, f"the other path sees already_notified_by; got {already}")
            self.assertEqual(already[0][1]["notified_message_id"], claimed[0][1]["notified_message_id"],
                "loser observes the winner's message_id")
            log = leader_notification_log_rows(store)
            log_rows_for_result = [r for r in log if r["result_id"] == two_path_result_id]
            self.assertEqual(len(log_rows_for_result), 1, "exactly one log row for the (result_id, uuid)")


def closing_helper(store: "MessageStore"):
    """Tiny shim to write directly to the SQLite store inside tests."""
    from contextlib import closing as _closing
    return _closing(store.connect())


if __name__ == "__main__":
    unittest.main(verbosity=2)
