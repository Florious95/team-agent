"""Gap 38 diagnostic harness — slice 2 stage 1.

Direct evidence 2026-05-26: res_c9434d9235f1 (Stage 15 CI fix report) injected into
leader pane 4 times, each wrapped in a different outer msg_id. Body byte-identical.
Gap 32's injection-boundary dedupe (commit 945948b) should have rejected attempts 2-4.

This harness replays a single result_id through every code path that could call into
the leader-pane injection layer, and asserts EXACTLY ONE leader_notification_log row
remains per (result_id, leader_session_uuid) regardless of how many times each path
fires. Subtests identify which path bypassed the gate.
"""
from __future__ import annotations

import importlib.util
import os
import tempfile
import threading
import unittest
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from unittest.mock import patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.message_store.leader_notification_log import (
    leader_notification_log_rows,
)
from team_agent.messaging import results as results_mod
from team_agent.messaging import leader as leader_mod
from team_agent.simple_yaml import dumps
from team_agent.state import (
    derive_leader_session_uuid,
    save_runtime_state,
)


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _envelope(task_id: str = "task-stage15", agent_id: str = "claude-runtime-developer") -> dict[str, Any]:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": task_id,
        "agent_id": agent_id,
        "status": "success",
        "summary": "Stage 15 CI publish fixes shipped as commit df940ab.",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


def _setup_workspace(tmp: str) -> tuple[Path, str]:
    workspace = Path(tmp)
    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-gap38"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    workspace_abspath = str(workspace.resolve())
    user = os.environ.get("USER") or os.environ.get("USERNAME") or ""
    owner_uuid = derive_leader_session_uuid("mfp-gap38", workspace_abspath, user, "current")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "team_dir": str(team_dir),
            "workspace": workspace_abspath,
            "session_name": "team-gap38",
            "leader": spec["leader"],
            "team_owner": {
                "pane_id": "%leader",
                "provider": "codex",
                "machine_fingerprint": "mfp-gap38",
                "leader_session_uuid": owner_uuid,
            },
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": "%leader",
                "session_name": "team-gap38",
                "leader_session_uuid": owner_uuid,
                "window_index": "0",
                "window_name": "leader",
                "pane_index": "0",
                "pane_tty": "/dev/ttys001",
                "pane_current_command": "codex",
            },
            "agents": {"claude-runtime-developer": {"status": "running", "provider": "fake", "window": "claude-runtime-developer"}},
            "tasks": [{"id": "task-stage15", "assignee": "claude-runtime-developer", "status": "running"}],
        },
    )
    # Seed a worker→leader inbound message so _owner_team_id_for_report can resolve owner.
    return workspace, owner_uuid


def _capture_injects():
    """Returns (fake_inject, calls). fake_inject mimics a successful tmux injection and
    records target+text per call so the test can extract result_id and count duplicates."""
    calls: list[dict[str, Any]] = []
    lock = threading.Lock()

    def fake_inject(target, text, submit_key, buffer_name, **kwargs):
        with lock:
            calls.append({"target": target, "text": text, "buffer": buffer_name})
        return {"ok": True, "verification": {"visible": True}, "submit_verification": {"submitted": True}, "turn_verification": {"turn": True}, "attempts": [{}], "submit_attempts": [{}]}

    return fake_inject, calls


def _patches_for_leader_pane(fake_inject):
    """Stack of patches that make _send_to_leader_receiver believe the leader pane is alive
    so the gate runs and inject gets called. Mocks tmux at the validation + inject layer."""
    from unittest.mock import MagicMock
    pane_info = {
        "pane_id": "%leader",
        "session_name": "team-gap38",
        "window_index": "0",
        "window_name": "leader",
        "pane_index": "0",
        "pane_tty": "/dev/ttys001",
        "pane_current_command": "codex",
        "pane_active": "1",
    }
    return [
        patch("team_agent.runtime._tmux_pane_info", return_value=pane_info),
        patch("team_agent.runtime.run_cmd", return_value=MagicMock(returncode=0, stdout="› idle", stderr="")),
        patch("team_agent.runtime._tmux_inject_text", side_effect=fake_inject),
        patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 0, "status": "started"}),
    ]


def _result_id_from_text(text: str) -> str | None:
    for line in text.splitlines():
        if line.startswith("Result id: "):
            return line.removeprefix("Result id: ").strip() or None
    return None


class Gap38ReproductionTests(unittest.TestCase):

    def test_two_report_result_calls_for_same_envelope_inject_exactly_once(self) -> None:
        """Path A: scheduled_event branch. report_result twice → process the scheduled
        events twice → assert leader_notification_log has exactly 1 row AND _tmux_inject_text
        called at most once for the result_id."""
        with tempfile.TemporaryDirectory(prefix="gap38-path-a-") as tmp:
            workspace, owner_uuid = _setup_workspace(tmp)
            fake_inject, inject_calls = _capture_injects()

            patches = _patches_for_leader_pane(fake_inject)
            for p in patches:
                p.start()
            try:
                # Fire report_result twice for the same envelope.
                envelope = _envelope()
                r1 = runtime.report_result(workspace, envelope)
                r2 = runtime.report_result(workspace, envelope)
                # Process the scheduled events that were enqueued.
                store = MessageStore(workspace)
                event_log = EventLog(workspace)
                from team_agent.messaging.scheduler import _fire_due_scheduled_events
                _fire_due_scheduled_events(workspace, store, event_log)
            finally:
                for p in patches:
                    p.stop()

            # The two report_result calls produce different result_ids (add_result mints
            # a fresh uuid each time). That's the upstream contract: distinct call →
            # distinct result_id → distinct dedupe key → both deliveries fire. This is
            # NOT a bypass; it's expected behaviour. Document the count for the
            # diagnostic.
            result_ids_in_injects = {_result_id_from_text(call["text"]) for call in inject_calls}
            result_ids_in_injects.discard(None)
            log = leader_notification_log_rows(store)
            print(
                "DIAGNOSTIC Path-A: inject_calls=", len(inject_calls),
                "distinct_result_ids_in_injects=", result_ids_in_injects,
                "log_rows=", [(r["result_id"], r["notified_message_id"]) for r in log],
            )
            # Hard assertion: per result_id, exactly one log row.
            from collections import Counter
            log_counts = Counter(r["result_id"] for r in log)
            for rid, count in log_counts.items():
                self.assertEqual(count, 1, f"result_id {rid} has {count} log rows; gate failed")
            # Hard assertion: number of injects equals number of distinct result_ids
            # — i.e. the gate suppressed every duplicate-result_id attempt.
            self.assertEqual(len(inject_calls), len(result_ids_in_injects),
                f"inject calls {len(inject_calls)} > distinct result_ids {result_ids_in_injects}; gate bypassed")

    def test_same_result_id_replayed_through_multiple_outer_msg_ids_dedupes(self) -> None:
        """Reproduce the EXACT incident: same result_id, multiple distinct outer msg_ids
        (each delivery via a different store.create_message call). The gate should reject
        attempts 2..N. Failure mode = each outer msg_id producing its own inject."""
        with tempfile.TemporaryDirectory(prefix="gap38-path-b-") as tmp:
            workspace, owner_uuid = _setup_workspace(tmp)
            fake_inject, inject_calls = _capture_injects()
            event_log = EventLog(workspace)

            # Compose the result-notification content the same way Path A does.
            envelope = _envelope()
            result_id = "res_c9434d9235f1"  # fixed to match the incident
            content = results_mod._format_report_result_notification(envelope, result_id)

            from team_agent.state import load_runtime_state
            state = load_runtime_state(workspace)

            patches = _patches_for_leader_pane(fake_inject)
            for p in patches:
                p.start()
            try:
                # Issue four sequential _send_to_leader_receiver calls — each will get its
                # own outer msg_id via store.create_message. The gate should suppress
                # injects 2-4.
                for attempt in range(4):
                    leader_mod._send_to_leader_receiver(
                        workspace, state, "leader", content,
                        envelope["task_id"], envelope["agent_id"], False, event_log,
                    )
            finally:
                for p in patches:
                    p.stop()

            store = MessageStore(workspace)
            log = leader_notification_log_rows(store)
            print(
                "DIAGNOSTIC Path-B: inject_calls=", len(inject_calls),
                "log_rows=", [(r["result_id"], r["notified_message_id"]) for r in log],
            )
            # Exactly one log row for the result_id.
            matching = [r for r in log if r["result_id"] == result_id]
            self.assertEqual(len(matching), 1, f"expected exactly 1 log row for {result_id}; got {len(matching)}")
            # Exactly one inject call across the four attempts.
            self.assertEqual(len(inject_calls), 1,
                f"gate must reject attempts 2-4; got {len(inject_calls)} injects")

    def test_parallel_threads_same_result_id_dedupe_to_one_inject(self) -> None:
        """Concurrency variant of the path-B test. Atomic INSERT OR IGNORE in
        claim_leader_notification_delivery must serialize four threads to exactly one
        inject."""
        with tempfile.TemporaryDirectory(prefix="gap38-parallel-") as tmp:
            workspace, owner_uuid = _setup_workspace(tmp)
            fake_inject, inject_calls = _capture_injects()
            envelope = _envelope()
            result_id = "res_parallel_replay"
            content = results_mod._format_report_result_notification(envelope, result_id)
            from team_agent.state import load_runtime_state
            state = load_runtime_state(workspace)

            barrier = threading.Barrier(4)
            errors: list[Exception] = []

            def worker():
                try:
                    barrier.wait()
                    event_log = EventLog(workspace)
                    leader_mod._send_to_leader_receiver(
                        workspace, state, "leader", content,
                        envelope["task_id"], envelope["agent_id"], False, event_log,
                    )
                except Exception as exc:
                    errors.append(exc)

            patches = _patches_for_leader_pane(fake_inject)
            for p in patches:
                p.start()
            try:
                threads = [threading.Thread(target=worker) for _ in range(4)]
                for t in threads: t.start()
                for t in threads: t.join()
            finally:
                for p in patches:
                    p.stop()

            self.assertFalse(errors, f"thread errors: {errors}")
            store = MessageStore(workspace)
            log = leader_notification_log_rows(store)
            matching = [r for r in log if r["result_id"] == result_id]
            self.assertEqual(len(matching), 1)
            self.assertEqual(len(inject_calls), 1,
                f"4 concurrent threads must collapse to 1 inject; got {len(inject_calls)}")

    def test_scheduler_retries_after_failed_inject_dedupe_to_one(self) -> None:
        """Reproduce the EXACT production incident shape: scheduler fires a result-notification
        send, inject visually succeeds but turn-boundary verification fails so the message is
        marked failed. The scheduler retry budget (max_attempts=3 in
        _notify_leader_of_report_result) then enqueues two more sends, each minting a NEW
        outer msg_id but carrying the same body (same Result id: line). With the Stage 12
        gate in place, exactly one inject (the first) reaches _tmux_inject_text; attempts
        2-N hit `claim_leader_notification_delivery` -> `already_notified_by` -> suppressed.

        This is the silent-loss arm by design (per Gap 32 roundtable): if the first
        inject claims the row but the inject itself fails verification, retries are
        silently deduped. That arm is intentional and the test does NOT assert against
        it. What this test asserts is the CONTAINMENT property: scheduler retries cannot
        produce more than one inject for one result_id, period."""
        with tempfile.TemporaryDirectory(prefix="gap38-scheduler-retries-") as tmp:
            workspace, owner_uuid = _setup_workspace(tmp)
            inject_calls: list[dict[str, Any]] = []
            inject_lock = threading.Lock()

            def fake_inject_that_fails_turn_verification(target, text, submit_key, buffer_name, **kwargs):
                """First inject 'succeeds' visually but fails turn verification — same shape
                as the production incident where leader_new_turn_boundary_missing was the
                reported error."""
                with inject_lock:
                    inject_calls.append({"target": target, "text": text})
                return {
                    "ok": False,
                    "error": "leader turn boundary not verified: leader_new_turn_boundary_missing",
                    "stage": "turn_verification",
                    "verification": "capture_contains_new_pasted_content_prompt",
                    "submit_verification": "pasted_content_prompt_absent_after_submit",
                    "turn_verification": "leader_new_turn_boundary_missing",
                    "attempts": [{}],
                    "submit_attempts": [{}],
                }

            patches = _patches_for_leader_pane(fake_inject_that_fails_turn_verification)
            for p in patches:
                p.start()
            try:
                envelope = _envelope()
                runtime.report_result(workspace, envelope)
                # Fire the scheduled event up to 4 times (matches the production retry shape).
                store = MessageStore(workspace)
                event_log = EventLog(workspace)
                from team_agent.messaging.scheduler import _fire_due_scheduled_events
                for _ in range(4):
                    _fire_due_scheduled_events(workspace, store, event_log)
            finally:
                for p in patches:
                    p.stop()

            log = leader_notification_log_rows(store)
            result_ids = {r["result_id"] for r in log}
            print(
                "DIAGNOSTIC scheduler-retries: inject_calls=", len(inject_calls),
                "log_rows=", [(r["result_id"], r["notified_message_id"]) for r in log],
            )
            # Containment: regardless of how many times the scheduler retries, exactly
            # ONE inject fires per result_id (since the gate claims atomically before
            # _tmux_inject_text is called).
            self.assertEqual(len(result_ids), 1,
                f"expected exactly 1 distinct result_id in log; got {result_ids}")
            self.assertEqual(len(inject_calls), 1,
                f"scheduler retries must collapse to 1 inject; got {len(inject_calls)}")

    def test_claim_leader_replay_for_same_result_id_dedupes(self) -> None:
        """Post-claim_leader recovery (Stage 11.10 semantics): requeue_after_claim_leader
        flips eligible watchers to notify_failed and calls retry_result_deliveries, which
        funnels through notify_result_watchers -> deliver_stored_message ->
        _send_single_message_unlocked -> _send_to_leader_receiver. The atomic gate at
        the injection boundary must still dedupe: even if a stored leader-bound message
        with the same result_id already 'won' the previous claim, a replay for the same
        result_id and same leader_session_uuid must not double-inject."""
        with tempfile.TemporaryDirectory(prefix="gap38-claim-leader-replay-") as tmp:
            workspace, owner_uuid = _setup_workspace(tmp)
            fake_inject, inject_calls = _capture_injects()
            envelope = _envelope()
            result_id = "res_claim_leader_replay"
            content = results_mod._format_report_result_notification(envelope, result_id)
            from team_agent.state import load_runtime_state
            state = load_runtime_state(workspace)

            patches = _patches_for_leader_pane(fake_inject)
            for p in patches:
                p.start()
            try:
                event_log = EventLog(workspace)
                # First delivery: claims the row.
                leader_mod._send_to_leader_receiver(
                    workspace, state, "leader", content,
                    envelope["task_id"], envelope["agent_id"], False, event_log,
                )
                # Simulate a claim-leader replay path: a second _send_to_leader_receiver
                # for the same content (which is what retry_result_deliveries ultimately
                # produces via deliver_stored_message). Must dedupe via the gate.
                for _ in range(3):
                    leader_mod._send_to_leader_receiver(
                        workspace, state, "leader", content,
                        envelope["task_id"], envelope["agent_id"], False, event_log,
                    )
            finally:
                for p in patches:
                    p.stop()

            store = MessageStore(workspace)
            log = leader_notification_log_rows(store)
            matching = [r for r in log if r["result_id"] == result_id]
            print(
                "DIAGNOSTIC claim-leader-replay: inject_calls=", len(inject_calls),
                "log_rows_for_result=", len(matching),
            )
            self.assertEqual(len(matching), 1,
                f"claim-leader replay must not create a second log row; got {len(matching)}")
            self.assertEqual(len(inject_calls), 1,
                f"claim-leader replay must not produce a second inject; got {len(inject_calls)}")

    def test_peer_mirror_path_does_not_bypass_gate_when_content_has_result_id(self) -> None:
        """The mirror_peer_message_to_leader path calls _send_to_leader_receiver. If the
        mirrored content includes a 'Result id: <id>' line, the gate must fire.
        Documents whether this is a real bypass vector."""
        with tempfile.TemporaryDirectory(prefix="gap38-peer-mirror-") as tmp:
            workspace, owner_uuid = _setup_workspace(tmp)
            fake_inject, inject_calls = _capture_injects()
            envelope = _envelope()
            result_id = "res_peer_mirror_target"
            # Content as it would appear if a worker peer-mirrored a result-bearing message.
            content_with_result = (
                f"Worker peer message containing result reference.\n"
                f"Result id: {result_id}\n"
                "Additional context."
            )

            patches = _patches_for_leader_pane(fake_inject)
            for p in patches:
                p.start()
            try:
                event_log = EventLog(workspace)
                from team_agent.state import load_runtime_state
                state = load_runtime_state(workspace)
                # Two mirror attempts — second should dedupe via the gate.
                for _ in range(2):
                    leader_mod._mirror_peer_message_to_leader(
                        workspace, state, "worker_a", "worker_b", "task-stage15", content_with_result, event_log,
                    )
            finally:
                for p in patches:
                    p.stop()

            store = MessageStore(workspace)
            log = leader_notification_log_rows(store)
            matching = [r for r in log if r["result_id"] == result_id]
            self.assertEqual(len(matching), 1, f"peer-mirror path must consult the gate; got {len(matching)} rows")
            self.assertEqual(len(inject_calls), 1, f"peer mirror duplicate must be suppressed; got {len(inject_calls)} injects")


if __name__ == "__main__":
    unittest.main(verbosity=2)
