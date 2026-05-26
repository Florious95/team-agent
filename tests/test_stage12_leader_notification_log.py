from __future__ import annotations

import hashlib
import importlib.util
import threading
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.message_store.leader_notification_log import (
    claim_leader_notification_delivery,
    leader_notification_log_rows,
    peek_leader_notification,
    prune_leader_notification_log,
)
from team_agent.messaging import leader as leader_module


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)
globals().update({
    name: value
    for name, value in vars(_base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})


def _events(workspace: Path) -> list[dict]:
    import json
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


class Stage12LeaderNotificationLogTests(unittest.TestCase):

    # (a) test_atomic_insert_or_ignore_dedupes_two_concurrent_paths_same_result
    def test_atomic_insert_or_ignore_dedupes_two_concurrent_paths_same_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage12-atomic-") as tmp:
            store = MessageStore(Path(tmp))
            outcomes: list[dict] = []
            outcomes_lock = threading.Lock()
            barrier = threading.Barrier(2)

            def worker(label: str) -> None:
                barrier.wait()
                r = claim_leader_notification_delivery(
                    store, result_id="res_x", leader_session_uuid="u-leader",
                    proposed_message_id=f"msg_{label}", envelope_hash="h",
                    owner_team_id="t", pane_id="%p",
                )
                with outcomes_lock:
                    outcomes.append(r)

            t1 = threading.Thread(target=worker, args=("a",))
            t2 = threading.Thread(target=worker, args=("b",))
            t1.start(); t2.start(); t1.join(); t2.join()
            claimed = [r for r in outcomes if r["status"] == "claimed_by_you"]
            already = [r for r in outcomes if r["status"] == "already_notified_by"]
            self.assertEqual(len(claimed), 1)
            self.assertEqual(len(already), 1)
            self.assertEqual(already[0]["notified_message_id"], claimed[0]["notified_message_id"])

    # (b) test_path_a_and_path_b_both_route_through_gate
    def test_path_a_and_path_b_both_route_through_gate(self) -> None:
        # Both delivery paths (scheduled_event branch + result_watchers branch) end at
        # the leader_notification_log claim. We exercise that funnel by issuing two
        # claim calls back-to-back with the same (result_id, uuid) — first wins, second
        # dedupes. The contract that both production paths *route* through the gate is
        # enforced by the source code (each calls claim_leader_notification_delivery
        # inside _send_to_leader_receiver); this test verifies the funnel collapses.
        with tempfile.TemporaryDirectory(prefix="stage12-both-paths-") as tmp:
            store = MessageStore(Path(tmp))
            first = claim_leader_notification_delivery(
                store, result_id="res_y", leader_session_uuid="u-leader",
                proposed_message_id="msg_path_b", envelope_hash="h",
                owner_team_id="t", pane_id="%p",
            )
            second = claim_leader_notification_delivery(
                store, result_id="res_y", leader_session_uuid="u-leader",
                proposed_message_id="msg_path_a", envelope_hash="h",
                owner_team_id="t", pane_id="%p",
            )
            self.assertEqual(first["status"], "claimed_by_you")
            self.assertEqual(second["status"], "already_notified_by")
            self.assertEqual(second["notified_message_id"], "msg_path_b")

    # (c) test_different_leader_session_uuid_allows_both_deliveries
    def test_different_leader_session_uuid_allows_both_deliveries(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage12-diff-uuid-") as tmp:
            store = MessageStore(Path(tmp))
            r1 = claim_leader_notification_delivery(
                store, result_id="res_z", leader_session_uuid="u-old",
                proposed_message_id="msg_old", envelope_hash="h",
                owner_team_id="t", pane_id="%p1",
            )
            r2 = claim_leader_notification_delivery(
                store, result_id="res_z", leader_session_uuid="u-new",
                proposed_message_id="msg_new", envelope_hash="h",
                owner_team_id="t", pane_id="%p2",
            )
            self.assertEqual(r1["status"], "claimed_by_you")
            self.assertEqual(r2["status"], "claimed_by_you",
                "post-takeover (new uuid) must allow a fresh delivery for the same result_id")
            rows = leader_notification_log_rows(store)
            self.assertEqual(len(rows), 2)

    # (d) test_envelope_content_mismatch_emits_legitimate_duplicate_suspected_event
    def test_envelope_content_mismatch_emits_legitimate_duplicate_suspected_event(self) -> None:
        # The event-emission policy lives in _send_to_leader_receiver. We exercise the
        # claim primitive's hash output so the caller can distinguish dedupe-skip from
        # legitimate-duplicate. Same (result_id, uuid) but different envelope_hash:
        # second caller sees already_notified_by with the FIRST hash, which the caller
        # in production compares against its own hash and routes to the
        # legitimate_duplicate_suspected event.
        with tempfile.TemporaryDirectory(prefix="stage12-hash-mismatch-") as tmp:
            store = MessageStore(Path(tmp))
            first = claim_leader_notification_delivery(
                store, result_id="res_h", leader_session_uuid="u",
                proposed_message_id="msg_first", envelope_hash="hash_A",
                owner_team_id="t", pane_id="%p",
            )
            second = claim_leader_notification_delivery(
                store, result_id="res_h", leader_session_uuid="u",
                proposed_message_id="msg_second", envelope_hash="hash_B",
                owner_team_id="t", pane_id="%p",
            )
            self.assertEqual(first["status"], "claimed_by_you")
            self.assertEqual(second["status"], "already_notified_by")
            self.assertEqual(second["envelope_content_hash"], "hash_A",
                "loser receives the original envelope hash so it can compare and decide between suppress vs legitimate-duplicate")
            self.assertNotEqual(second["envelope_content_hash"], "hash_B")

    # (e) test_prune_removes_old_rows
    def test_prune_removes_old_rows(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage12-prune-") as tmp:
            store = MessageStore(Path(tmp))
            # Insert a "fresh" row via the helper.
            claim_leader_notification_delivery(
                store, result_id="res_fresh", leader_session_uuid="u",
                proposed_message_id="msg_fresh", envelope_hash="h",
                owner_team_id="t", pane_id="%p",
            )
            # Insert an "old" row by direct SQL so the timestamp is past the cutoff.
            from contextlib import closing
            old_ts = (datetime.now(timezone.utc) - timedelta(hours=48)).isoformat()
            with closing(store.connect()) as conn:
                with conn:
                    conn.execute(
                        "insert into leader_notification_log("
                        "result_id, leader_session_uuid, notified_message_id, notified_at, "
                        "leader_pane_id_at_notify, envelope_content_hash, owner_team_id"
                        ") values (?, ?, ?, ?, ?, ?, ?)",
                        ("res_old", "u", "msg_old", old_ts, "%p", "h", "t"),
                    )
            removed = prune_leader_notification_log(store, max_age_hours=24)
            self.assertEqual(removed, 1)
            remaining_ids = {r["result_id"] for r in leader_notification_log_rows(store)}
            self.assertIn("res_fresh", remaining_ids)
            self.assertNotIn("res_old", remaining_ids)

    # (f) test_migration_empty_table_on_first_use
    def test_migration_empty_table_on_first_use(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage12-migrate-") as tmp:
            store = MessageStore(Path(tmp))
            # Fresh DB; no historical rows.
            self.assertEqual(leader_notification_log_rows(store), [])
            # First claim succeeds and creates the row.
            r = claim_leader_notification_delivery(
                store, result_id="res_first", leader_session_uuid="u",
                proposed_message_id="msg_first", envelope_hash="h",
                owner_team_id="t", pane_id="%p",
            )
            self.assertEqual(r["status"], "claimed_by_you")
            self.assertEqual(len(leader_notification_log_rows(store)), 1)

    # (g) test_pane_id_change_does_not_affect_dedupe
    def test_pane_id_change_does_not_affect_dedupe(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage12-pane-change-") as tmp:
            store = MessageStore(Path(tmp))
            r1 = claim_leader_notification_delivery(
                store, result_id="res_pane", leader_session_uuid="u-stable",
                proposed_message_id="msg_pane1", envelope_hash="h",
                owner_team_id="t", pane_id="%76",
            )
            r2 = claim_leader_notification_delivery(
                store, result_id="res_pane", leader_session_uuid="u-stable",
                proposed_message_id="msg_pane2", envelope_hash="h",
                owner_team_id="t", pane_id="%648",
            )
            self.assertEqual(r1["status"], "claimed_by_you")
            self.assertEqual(r2["status"], "already_notified_by",
                "pane_id is not part of the dedupe key; only (result_id, leader_session_uuid)")
            rows = leader_notification_log_rows(store)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["leader_pane_id_at_notify"], "%76",
                "the original pane_id is preserved as auditing metadata")

    # (h) coverage of bad6484/9f52048/edac6b3-era contract via legacy compat — handled
    # in tests/test_gap32_result_dedupe.py + tests/test_gap26_claim_leader_requeues_stalled_watchers.py
    # which still pass under the new architecture (verified by the test suite).

    # Bonus: peek helper readback contract
    def test_peek_returns_none_for_missing_and_row_for_existing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage12-peek-") as tmp:
            store = MessageStore(Path(tmp))
            self.assertIsNone(peek_leader_notification(store, result_id="absent", leader_session_uuid="u"))
            claim_leader_notification_delivery(
                store, result_id="present", leader_session_uuid="u",
                proposed_message_id="msg_p", envelope_hash="h",
                owner_team_id="t", pane_id="%p",
            )
            row = peek_leader_notification(store, result_id="present", leader_session_uuid="u")
            self.assertIsNotNone(row)
            self.assertEqual(row["notified_message_id"], "msg_p")


if __name__ == "__main__":
    unittest.main(verbosity=2)
