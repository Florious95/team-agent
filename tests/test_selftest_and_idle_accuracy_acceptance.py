from __future__ import annotations

import contextlib
import importlib
import io
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from team_agent.approvals import status as approvals_status
from team_agent.cli import parser as cli_parser
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging import scheduler
from team_agent.messaging.activity_detector import classify_agent_activity


SELFTEST_PREFIX = "ta-selftest-comms-"


class SelftestAndIdleAccuracyAcceptanceTests(unittest.TestCase):
    def test_c1_doctor_comms_extends_doctor_without_new_top_level_selftest(self) -> None:
        top_help = _cli_stdout(["--help"])
        self.assertNotIn("selftest", _visible_command_words(top_help))

        doctor_help = _cli_stdout(["doctor", "--help"])
        self.assertIn("--comms", doctor_help)

    def test_c2_doctor_comms_and_gate_comms_route_to_same_json_helper(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c2-") as tmp:
            workspace = Path(tmp)
            direct = _cli_json(["doctor", "--comms", "--workspace", str(workspace), "--json"])
            gate = _cli_json(["doctor", "--gate", "comms", "--workspace", str(workspace), "--json"])

        self.assertEqual(_canonical_selftest_json(direct), _canonical_selftest_json(gate))

    def test_c3_worker_to_leader_probe_rejects_result_id_prefix_before_dedupe(self) -> None:
        result = _run_comms_selftest(probe_content="Result id: res_fake\nSELFTEST token")
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(result.get("error"), "probe_content_uses_result_prefix")
        self.assertFalse(result.get("dedupe_checked"), result)

    def test_c4_fallback_log_is_worker_to_leader_failure_not_green_ok(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(worker_to_leader={"ok": True, "status": "fallback_log"}))
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(result["checks"]["worker_to_leader"]["status"], "fail")
        self.assertEqual(result["checks"]["worker_to_leader"]["reason"], "fallback_log")

    def test_c5_deduped_worker_to_leader_notification_is_failure(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(worker_to_leader={"ok": True, "status": "submitted", "deduped": True}))
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(result["checks"]["worker_to_leader"]["status"], "fail")
        self.assertEqual(result["checks"]["worker_to_leader"]["reason"], "deduped")

    def test_c6_db_submitted_without_capture_token_is_failure(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(capture_text="capture has no probe token"))
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(result["checks"]["worker_to_leader"]["status"], "fail")
        self.assertEqual(result["checks"]["worker_to_leader"]["reason"], "token_missing_from_capture")

    def test_c7_c10_disposable_session_prefix_cleanup_and_sweep_are_first_class_checks(self) -> None:
        driver = FakeSelftestDriver(stale_sessions=[f"{SELFTEST_PREFIX}stale-1"], kill_ok=False)
        result = _run_comms_selftest(driver=driver)
        self.assertFalse(result.get("ok"), result)
        cleanup = result["checks"]["cleanup"]
        self.assertEqual(cleanup["status"], "fail")
        self.assertIn(f"{SELFTEST_PREFIX}stale-1", cleanup["killed_sessions"])
        self.assertTrue(all(name.startswith(SELFTEST_PREFIX) for name in cleanup["created_sessions"]), cleanup)
        self.assertIn("selftest.swept_stale", result.get("events", []))

    def test_c8_created_disposable_session_is_killed_when_probe_raises(self) -> None:
        driver = FakeSelftestDriver(raise_after_create=True)
        result = _run_comms_selftest(driver=driver)
        cleanup = result["checks"]["cleanup"]
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(cleanup["status"], "killed")
        self.assertEqual(driver.remaining_sessions(), [])

    def test_c11_c13_external_version_command_no_uuid_and_state_read_only(self) -> None:
        before = {
            "team_owner": {"pane_id": "%100", "provider": "claude_code", "owner_epoch": 1},
            "leader_receiver": {"mode": "direct_tmux", "pane_id": "%100", "provider": "claude_code", "owner_epoch": 1},
        }
        driver = FakeSelftestDriver(
            pane_current_command="2.1.154",
            env={},
            state_before=json.loads(json.dumps(before, sort_keys=True)),
        )
        result = _run_comms_selftest(driver=driver)
        self.assertTrue(result.get("ok"), result)
        self.assertEqual(result["checks"]["receiver_binding"]["status"], "pass")
        self.assertFalse(result.get("used_uuid_gate"), result)
        self.assertEqual(driver.state_after, before)

    def test_matrix_a1_idle_leader_to_worker_reports_all_four_acks(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(matrix_case="A1_IDLE_LEADER_TO_WORKER"))
        self.assertTrue(result.get("ok"), result)
        matrix = result["checks"]["matrix"]["A1"]
        self.assertEqual(_ack_statuses(matrix), {
            "enqueue_ack": "pass",
            "delivery_ack": "pass",
            "execution_ack": "pass",
            "leader_notification_ack": "pass",
        })

    def test_matrix_a2_busy_leader_to_worker_is_fifo_defer_then_deliver_not_preempt(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(matrix_case="A2_BUSY_LEADER_TO_WORKER"))
        self.assertTrue(result.get("ok"), result)
        matrix = result["checks"]["matrix"]["A2"]
        self.assertEqual(matrix["enqueue_ack"]["status"], "pass")
        self.assertEqual(matrix["busy_defer_ack"]["event"], "send.deferred_busy")
        self.assertEqual(matrix["delivery_ack"]["event"], "send.pending_delivered")
        self.assertFalse(matrix.get("preempt_attempted"), matrix)

    def test_matrix_b1_b2_worker_to_leader_renders_only_in_disposable_capture(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(matrix_case="B1_B2_WORKER_TO_LEADER"))
        self.assertTrue(result.get("ok"), result)
        for cell in ("B1", "B2"):
            with self.subTest(cell=cell):
                matrix = result["checks"]["matrix"][cell]
                self.assertEqual(matrix["leader_notification_ack"]["status"], "pass")
                self.assertTrue(matrix["capture_contains_token"], matrix)
                self.assertFalse(matrix["live_leader_contains_token"], matrix)

    def test_idle_behavior_challenge_times_out_when_worker_claimed_idle_but_busy(self) -> None:
        result = _evaluate_idle_behavior(
            agent_id="worker_1",
            claimed_status="IDLE",
            driver=FakeSelftestDriver(idle_execution={"status": "timeout"}),
        )
        self.assertEqual(result["execution_ack"], "timeout")
        self.assertEqual(result["classification_accuracy"], "fail")

    def test_c14_latest_idle_prompt_still_wins_over_pane_delta_and_old_working(self) -> None:
        activity = classify_agent_activity(
            "worker_1",
            "codex",
            datetime.now(timezone.utc).isoformat(),
            {"pane_current_command": "node", "pane_in_mode": "0"},
            "old output\n✱ Working (40s) ⠋\nfinished\n\n› Use /skills to list available skills\n",
        )
        self.assertEqual(activity["status"], "idle", activity)
        self.assertGreaterEqual(activity["confidence"], 0.85, activity)

    def test_c15_active_task_with_recent_pane_delta_reports_working_not_idle(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c15-active-") as tmp:
            workspace = Path(tmp)
            state = _health_state(workspace, tasks=[{"id": "task_1", "assignee": "worker_1", "status": "running"}])
            store = MessageStore(workspace)
            _sync_health_with_capture(workspace, state, store, "compile output TOKEN-1\n")
            health = store.agent_health(owner_team_id="current")["worker_1"]

        self.assertEqual(health["status"], "WORKING", health)

    def test_c15_no_active_task_with_pane_delta_may_remain_idle(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c15-no-active-") as tmp:
            workspace = Path(tmp)
            state = _health_state(workspace, tasks=[])
            store = MessageStore(workspace)
            _sync_health_with_capture(workspace, state, store, "user shell output TOKEN-2\n")
            health = store.agent_health(owner_team_id="current")["worker_1"]

        self.assertEqual(health["status"], "IDLE", health)

    def test_c16_idle_takeover_wiring_does_not_import_agent_health_or_status(self) -> None:
        source = (Path(__file__).resolve().parents[1] / "src/team_agent/idle_takeover_wiring.py").read_text()
        self.assertNotIn("agent_health", source)
        self.assertNotIn("approvals.status", source)
        self.assertNotIn("activity_output_hash", source)
        self.assertNotIn("last_output_at", source)

    def test_c17_working_status_is_included_in_stuck_detection(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c17-stuck-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            old = (datetime.now(timezone.utc) - timedelta(hours=1)).isoformat()
            store.upsert_agent_health("worker_1", "WORKING", last_output_at=old, current_task_id="task_1", owner_team_id="current")
            state = _health_state(workspace, tasks=[{"id": "task_1", "assignee": "worker_1", "status": "running"}])
            event_log = EventLog(workspace)
            with patch("team_agent.messaging.scheduler.send_message", return_value={"ok": True}):
                stuck = scheduler._detect_stuck_agents(workspace, state, store, event_log)

        self.assertIn("worker_1", stuck)


class FakeSelftestDriver:
    def __init__(
        self,
        *,
        worker_to_leader: dict | None = None,
        capture_text: str | None = None,
        stale_sessions: list[str] | None = None,
        kill_ok: bool = True,
        raise_after_create: bool = False,
        pane_current_command: str = "2.1.154",
        env: dict | None = None,
        state_before: dict | None = None,
        matrix_case: str | None = None,
        idle_execution: dict | None = None,
    ) -> None:
        self.worker_to_leader = worker_to_leader or {"ok": True, "status": "submitted", "visible": True, "submitted": True}
        self.capture_text = capture_text if capture_text is not None else "SELFTEST_TOKEN rendered"
        self.stale_sessions = list(stale_sessions or [])
        self.kill_ok = kill_ok
        self.raise_after_create = raise_after_create
        self.pane_current_command = pane_current_command
        self.env = env or {}
        self.state_before = state_before or {}
        self.state_after = json.loads(json.dumps(self.state_before, sort_keys=True))
        self.matrix_case = matrix_case
        self.idle_execution = idle_execution or {"status": "pass"}
        self._sessions = list(self.stale_sessions)

    def remaining_sessions(self) -> list[str]:
        return list(self._sessions)


def _run_comms_selftest(**kwargs) -> dict:
    with tempfile.TemporaryDirectory(prefix="ta-selftest-contract-") as tmp:
        workspace = Path(tmp)
        try:
            module = importlib.import_module("team_agent.diagnose.comms")
        except ModuleNotFoundError:
            module = importlib.import_module("_contract_stubs.selftest_and_idle")
        try:
            return module.run_comms_selftest(workspace, **kwargs)
        except NotImplementedError as exc:
            raise AssertionError(str(exc)) from exc


def _evaluate_idle_behavior(**kwargs) -> dict:
    with tempfile.TemporaryDirectory(prefix="ta-selftest-idle-contract-") as tmp:
        workspace = Path(tmp)
        try:
            module = importlib.import_module("team_agent.diagnose.comms")
        except ModuleNotFoundError:
            module = importlib.import_module("_contract_stubs.selftest_and_idle")
        try:
            return module.evaluate_idle_behavior(workspace, **kwargs)
        except NotImplementedError as exc:
            raise AssertionError(str(exc)) from exc


def _cli_stdout(argv: list[str]) -> str:
    out = io.StringIO()
    err = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            cli_parser.main(argv)
        except SystemExit as exc:
            if exc.code not in (0, None):
                raise AssertionError(f"CLI {argv!r} exited {exc.code}: {err.getvalue()}") from exc
    return out.getvalue()


def _cli_json(argv: list[str]) -> dict:
    raw = _cli_stdout(argv)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise AssertionError(f"CLI did not emit JSON for {argv!r}: {raw!r}") from exc


def _canonical_selftest_json(data: dict) -> dict:
    scrub = json.loads(json.dumps(data, sort_keys=True))
    for key in ("timestamp", "run_id", "started_at", "finished_at"):
        scrub.pop(key, None)
    return scrub


def _visible_command_words(help_text: str) -> set[str]:
    words: set[str] = set()
    for token in help_text.replace("{", " ").replace("}", " ").replace(",", " ").split():
        words.add(token.strip())
    return words


def _ack_statuses(matrix_cell: dict) -> dict[str, str]:
    return {key: value.get("status") for key, value in matrix_cell.items() if key.endswith("_ack")}


def _health_state(workspace: Path, *, tasks: list[dict]) -> dict:
    return {
        "workspace": str(workspace),
        "team_dir": str(workspace / ".team" / "current"),
        "session_name": "ta-selftest-health",
        "agents": {
            "worker_1": {
                "status": "running",
                "provider": "codex",
                "window": "worker_1",
            }
        },
        "tasks": tasks,
    }


def _sync_health_with_capture(workspace: Path, state: dict, store: MessageStore, capture: str) -> None:
    proc = SimpleNamespace(returncode=0, stdout=capture, stderr="")
    with patch("team_agent.runtime._tmux_window_exists", return_value=True), patch(
        "team_agent.runtime.run_cmd",
        return_value=proc,
    ), patch("team_agent.runtime._tmux_pane_info", return_value={"pane_current_command": "node", "pane_in_mode": "0"}):
        approvals_status.sync_agent_health(workspace, state, store)


if __name__ == "__main__":
    unittest.main()
