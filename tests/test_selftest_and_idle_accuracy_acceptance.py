from __future__ import annotations

import ast
import contextlib
import hashlib
import importlib
import io
import json
import shutil
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
from team_agent.state import save_runtime_state


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

    def test_c9_c10_swept_stale_session_is_not_cleaned_up_twice_or_reported_created(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-stale-idempotent-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, _health_state(workspace, tasks=[]))
            sessions = {f"{SELFTEST_PREFIX}stale-1"}
            kill_calls: list[str] = []
            run_id = "freshrun0000"
            created_session = f"{SELFTEST_PREFIX}{run_id}"

            def fake_run_cmd(args: list[str], timeout: int = 10) -> SimpleNamespace:
                if args[:3] == ["tmux", "ls", "-F"]:
                    return SimpleNamespace(returncode=0, stdout="\n".join(sorted(sessions)) + ("\n" if sessions else ""), stderr="")
                if args[:2] == ["tmux", "kill-session"]:
                    target = args[args.index("-t") + 1]
                    kill_calls.append(target)
                    if target in sessions:
                        sessions.remove(target)
                        return SimpleNamespace(returncode=0, stdout="", stderr="")
                    return SimpleNamespace(returncode=1, stdout="", stderr=f"can't find session: {target}")
                if args[:2] == ["tmux", "new-session"]:
                    sessions.add(args[args.index("-s") + 1])
                    return SimpleNamespace(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "display-message", "-p"]:
                    return SimpleNamespace(returncode=0, stdout="%capture\n", stderr="")
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    return SimpleNamespace(returncode=0, stdout="Team Agent comms selftest probe selftest-comms-freshrun\n", stderr="")
                return SimpleNamespace(returncode=0, stdout="", stderr="")

            comms = importlib.import_module("team_agent.diagnose.comms")
            with patch("team_agent.diagnose.comms.uuid.uuid4", return_value=SimpleNamespace(hex=run_id)), patch.object(
                comms,
                "_check_receiver_binding",
                return_value={"status": "pass"},
            ), patch.object(
                comms,
                "_check_leader_to_worker",
                return_value={
                    "status": "pass",
                    "enqueue_ack": {"status": "pass"},
                    "delivery_ack": {"status": "pass"},
                    "execution_ack": {"status": "pass"},
                    "leader_notification_ack": {"status": "pass"},
                },
            ), patch.object(
                comms,
                "_check_worker_to_leader",
                return_value={
                    "status": "pass",
                    "enqueue_ack": {"status": "pass"},
                    "delivery_ack": {"status": "pass"},
                    "execution_ack": {"status": "pass"},
                    "leader_notification_ack": {"status": "pass"},
                },
            ), patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = comms.run_comms_selftest(workspace, team="current")

            cleanup = result["checks"]["cleanup"]

        self.assertIn("selftest.swept_stale", result.get("events", []), result)
        self.assertTrue(result.get("ok"), result)
        self.assertNotIn(f"{SELFTEST_PREFIX}stale-1", cleanup["created_sessions"], cleanup)
        self.assertEqual(kill_calls.count(f"{SELFTEST_PREFIX}stale-1"), 1, kill_calls)
        self.assertEqual(cleanup["status"], "killed", cleanup)
        self.assertEqual(cleanup["created_sessions"], [created_session], cleanup)
        self.assertNotIn(f"{SELFTEST_PREFIX}stale-1", sessions)

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

    def test_c18_c19_worker_to_leader_uses_persisted_throwaway_receiver_and_live_files_unchanged(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c18-live-") as tmp:
            workspace = Path(tmp)
            live_state = _health_state(workspace, tasks=[])
            live_state["team_owner"] = {"pane_id": "%live-fake", "provider": "fake", "owner_epoch": 1}
            live_state["leader_receiver"] = {"mode": "direct_tmux", "pane_id": "%live-fake", "provider": "fake", "owner_epoch": 1}
            save_runtime_state(workspace, live_state)
            MessageStore(workspace).create_message(None, "leader", "worker_1", "live durable row", owner_team_id="current")
            EventLog(workspace).write("live.baseline", stable=True)
            before = _persistent_hashes(workspace)
            comms = importlib.import_module("team_agent.diagnose.comms")

            result = comms.run_comms_selftest(
                workspace,
                team="current",
                driver=FakeSelftestDriver(
                    matrix_case="B1_B2_WORKER_TO_LEADER",
                    state_before=live_state,
                    worker_resolved_receiver_pane_id="%capture",
                ),
            )
            after = _persistent_hashes(workspace)

        self.assertEqual(after, before)
        self.assertIn("throwaway_state", result["checks"], result)
        throwaway = result["checks"]["throwaway_state"]
        self.assertTrue(Path(throwaway["workspace"]).is_absolute(), throwaway)
        self.assertIn("/ta-selftest-comms-", throwaway["workspace"], throwaway)
        self.assertEqual(throwaway["persisted_leader_receiver_pane_id"], "%capture")
        self.assertEqual(throwaway["worker_resolved_receiver_pane_id"], "%capture")
        self.assertNotEqual(throwaway["worker_resolved_receiver_pane_id"], "%live-fake")
        self.assertEqual(result["checks"]["live_workspace_unchanged"]["status"], "pass")

    def test_c20_live_leader_pollution_scans_pane_store_and_event_log_as_hard_failure(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c20-live-") as tmp:
            workspace = Path(tmp)
            token = "selftest-comms-pollute073"
            state = _health_state(workspace, tasks=[])
            state["team_owner"] = {"pane_id": "%live-fake", "provider": "fake", "owner_epoch": 1}
            state["leader_receiver"] = {"mode": "direct_tmux", "pane_id": "%live-fake", "provider": "fake", "owner_epoch": 1}
            save_runtime_state(workspace, state)
            MessageStore(workspace).create_message(None, "selftest_worker", "leader", f"live store pollution {token}", owner_team_id="current")
            EventLog(workspace).write("leader_receiver.submitted", target_pane_id="%live-fake", content=f"live event pollution {token}")
            comms = importlib.import_module("team_agent.diagnose.comms")

            with patch("team_agent.diagnose.comms.uuid.uuid4", return_value=SimpleNamespace(hex="pollute073")):
                result = comms.run_comms_selftest(
                    workspace,
                    team="current",
                    driver=FakeSelftestDriver(
                        matrix_case="B1_B2_WORKER_TO_LEADER",
                        capture_text=f"disposable capture also has {token}",
                        live_capture_before="no token here",
                        live_capture_after=f"live pane pollution {token}",
                    ),
                )

        self.assertIn("live_leader_pollution", result["checks"], result)
        pollution = result["checks"]["live_leader_pollution"]
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(pollution["status"], "fail", pollution)
        self.assertEqual(pollution["live_pane_id"], "%live-fake")
        self.assertEqual(pollution["token"], token)
        self.assertGreaterEqual(set(pollution["detected_in"]), {"capture_after", "message_store", "event_log"})

    def test_c21_cleanup_reports_four_subsystems_and_startup_sweeps_tmux_and_workspaces(self) -> None:
        run_id = "c21cleanup073"
        stale_tmux = f"{SELFTEST_PREFIX}stale-c21"
        stale_dir = Path(tempfile.gettempdir()) / f"{SELFTEST_PREFIX}stale-c21"
        stale_dir.mkdir(parents=True, exist_ok=True)
        sessions = {stale_tmux}
        try:
            with tempfile.TemporaryDirectory(prefix="ta-selftest-c21-live-") as tmp:
                workspace = Path(tmp)
                save_runtime_state(workspace, _health_state(workspace, tasks=[]))

                def fake_run_cmd(args: list[str], timeout: int = 10) -> SimpleNamespace:
                    if args[:3] == ["tmux", "ls", "-F"]:
                        return SimpleNamespace(returncode=0, stdout="\n".join(sorted(sessions)) + ("\n" if sessions else ""), stderr="")
                    if args[:2] == ["tmux", "kill-session"]:
                        target = args[args.index("-t") + 1]
                        sessions.discard(target)
                        return SimpleNamespace(returncode=0, stdout="", stderr="")
                    if args[:2] == ["tmux", "new-session"]:
                        sessions.add(args[args.index("-s") + 1])
                        return SimpleNamespace(returncode=0, stdout="", stderr="")
                    if args[:3] == ["tmux", "display-message", "-p"]:
                        return SimpleNamespace(returncode=0, stdout="%capture\n", stderr="")
                    if args[:3] == ["tmux", "capture-pane", "-p"]:
                        return SimpleNamespace(returncode=0, stdout=f"selftest-comms-{run_id}\n", stderr="")
                    return SimpleNamespace(returncode=0, stdout="", stderr="")

                comms = importlib.import_module("team_agent.diagnose.comms")
                with patch("team_agent.diagnose.comms.uuid.uuid4", return_value=SimpleNamespace(hex=run_id)), patch.object(
                    comms,
                    "_check_receiver_binding",
                    return_value={"status": "pass"},
                ), patch.object(
                    comms,
                    "_check_leader_to_worker",
                    return_value=_passing_ack(),
                ), patch.object(
                    comms,
                    "_check_worker_to_leader",
                    return_value=_passing_ack(),
                ), patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                    result = comms.run_comms_selftest(workspace, team="current")

            cleanup = result["checks"]["cleanup"]
            self.assertTrue(result.get("events"), result)
            swept = result["events"][0]
            self.assertIsInstance(swept, dict, result)
            self.assertEqual(swept["event"], "selftest.swept_stale", result)
            self.assertIn(stale_tmux, swept["tmux"])
            self.assertIn(str(stale_dir), swept["workspaces"])
            for key in ("tmux", "workspace", "coordinator", "worker"):
                self.assertEqual(cleanup[key]["status"], "pass", cleanup)
            self.assertTrue(result.get("ok"), result)
        finally:
            shutil.rmtree(stale_dir, ignore_errors=True)

    def test_c22_throwaway_runid_does_not_pollute_global_registries(self) -> None:
        run_id = "global073"
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c22-live-") as tmp, tempfile.TemporaryDirectory(prefix="ta-selftest-c22-home-") as home:
            workspace = Path(tmp)
            home_path = Path(home)
            registry = home_path / ".team-agent" / "teams.json"
            registry.parent.mkdir(parents=True, exist_ok=True)
            registry.write_text(json.dumps({"teams": [run_id]}), encoding="utf-8")
            save_runtime_state(workspace, _health_state(workspace, tasks=[]))
            comms = importlib.import_module("team_agent.diagnose.comms")

            with patch("team_agent.diagnose.comms.uuid.uuid4", return_value=SimpleNamespace(hex=run_id)), patch(
                "pathlib.Path.home",
                return_value=home_path,
            ):
                result = comms.run_comms_selftest(workspace, team="current", driver=FakeSelftestDriver(matrix_case="B1_B2_WORKER_TO_LEADER"))

        self.assertIn("global_registry_pollution", result["checks"], result)
        pollution = result["checks"]["global_registry_pollution"]
        self.assertFalse(result.get("ok"), result)
        self.assertEqual(pollution["status"], "fail", pollution)
        self.assertIn(str(registry), pollution["detected_paths"])

    def test_doctor_comms_does_not_deliver_preexisting_pending_messages(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-no-drain-") as tmp:
            workspace = Path(tmp)
            state = _health_state(
                workspace,
                tasks=[{"id": "task_1", "assignee": "worker_1", "status": "running"}],
            )
            state["agents"]["worker_1"]["status"] = "busy"
            state["team_owner"] = {"pane_id": "%100", "provider": "fake", "owner_epoch": 1}
            state["leader_receiver"] = {"mode": "direct_tmux", "pane_id": "%100", "provider": "fake", "owner_epoch": 1}
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            preexisting = store.create_message(
                "task_other",
                "leader",
                "worker_1",
                "preexisting user work must stay queued",
                owner_team_id="current",
            )

            def fake_deliver(patched_workspace: Path, _state: dict, message_id: str, **_kwargs) -> dict:
                MessageStore(patched_workspace).mark(message_id, "submitted")
                return {"ok": True, "status": "delivered", "message_id": message_id}

            comms = importlib.import_module("team_agent.diagnose.comms")
            with patch.object(comms, "_check_receiver_binding", return_value={"status": "pass"}), patch.object(
                comms,
                "_create_disposable_receiver",
                return_value={
                    "status": "pass",
                    "session_name": f"{SELFTEST_PREFIX}no-drain",
                    "pane_id": "%capture",
                    "receiver": {"mode": "direct_tmux", "provider": "fake", "pane_id": "%capture"},
                },
            ), patch.object(
                comms,
                "_check_worker_to_leader",
                return_value={
                    "status": "pass",
                    "enqueue_ack": {"status": "pass"},
                    "delivery_ack": {"status": "pass"},
                    "execution_ack": {"status": "pass"},
                    "leader_notification_ack": {"status": "pass"},
                },
            ), patch.object(comms, "_sweep_stale_sessions", return_value=[]), patch.object(
                comms,
                "_cleanup_sessions",
                return_value={"status": "pass", "killed_sessions": [], "created_sessions": []},
            ), patch("team_agent.messaging.delivery._deliver_pending_message", side_effect=fake_deliver):
                result = comms.run_comms_selftest(workspace, team="current")

            rows = {row["message_id"]: row for row in store.messages(owner_team_id="current")}

        self.assertTrue(result.get("ok"), result)
        self.assertEqual(rows[preexisting]["status"], "accepted", rows[preexisting])
        submitted_selftest = [
            row for message_id, row in rows.items()
            if message_id != preexisting and row["sender"] == "leader" and row["recipient"] == "worker_1"
        ]
        self.assertTrue(submitted_selftest, rows)
        self.assertTrue(all(row["status"] == "submitted" for row in submitted_selftest), submitted_selftest)

    def test_idle_behavior_challenge_times_out_when_worker_claimed_idle_but_busy(self) -> None:
        result = _evaluate_idle_behavior(
            agent_id="worker_1",
            claimed_status="IDLE",
            driver=FakeSelftestDriver(idle_execution={"status": "timeout"}),
        )
        self.assertEqual(result["execution_ack"], "timeout")
        self.assertEqual(result["classification_accuracy"], "fail")

    def test_c14_real_codex_idle_prompt_fixture_is_idle(self) -> None:
        activity = classify_agent_activity(
            "worker_1",
            "codex",
            datetime.now(timezone.utc).isoformat(),
            {"pane_current_command": "node", "pane_in_mode": "0"},
            _idle_prompt_fixture("codex_idle.txt"),
        )
        self.assertEqual(activity["status"], "idle", activity)
        self.assertGreaterEqual(activity["confidence"], 0.85, activity)

    def test_c14_real_claude_code_idle_prompt_fixture_is_idle(self) -> None:
        activity = classify_agent_activity(
            "worker_1",
            "claude_code",
            datetime.now(timezone.utc).isoformat(),
            {"pane_current_command": "node", "pane_in_mode": "0"},
            _idle_prompt_fixture("claude_code_idle.txt"),
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

    def test_c15_active_task_visible_claude_prompt_with_streaming_output_still_working(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c15-claude-stream-") as tmp:
            workspace = Path(tmp)
            state = _health_state(
                workspace,
                tasks=[{"id": "task_1", "assignee": "worker_1", "status": "running"}],
                provider="claude_code",
            )
            store = MessageStore(workspace)
            _sync_health_with_capture(
                workspace,
                state,
                store,
                "❯ python -m unittest discover -s tests\n"
                "test_alpha ... ok\n"
                "test_beta ... ok\n",
            )
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
        tree = ast.parse(source)
        imported_modules: set[str] = set()
        referenced_names: set[str] = set()
        referenced_strings: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported_modules.update(alias.name for alias in node.names)
            elif isinstance(node, ast.ImportFrom):
                if node.module:
                    imported_modules.add(node.module)
                imported_modules.update(f"{node.module}.{alias.name}" if node.module else alias.name for alias in node.names)
            elif isinstance(node, ast.Name):
                referenced_names.add(node.id)
            elif isinstance(node, ast.Attribute):
                referenced_names.add(node.attr)
            elif isinstance(node, ast.Constant) and isinstance(node.value, str):
                referenced_strings.add(node.value)

        forbidden_imports = {
            "team_agent.approvals.status",
            "team_agent.message_store.agent_health",
            "team_agent.messaging.activity_detector",
        }
        self.assertTrue(imported_modules.isdisjoint(forbidden_imports), imported_modules)
        self.assertNotIn("agent_health", referenced_names)
        self.assertNotIn("activity_output_hash", referenced_strings)
        self.assertNotIn("last_output_at", referenced_strings)

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
        **kwargs,
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
        for key, value in kwargs.items():
            setattr(self, key, value)

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


def _passing_ack() -> dict:
    return {
        "status": "pass",
        "enqueue_ack": {"status": "pass"},
        "delivery_ack": {"status": "pass"},
        "execution_ack": {"status": "pass"},
        "leader_notification_ack": {"status": "pass"},
    }


def _persistent_hashes(workspace: Path) -> dict[str, str]:
    roots = [workspace / ".team", workspace / "team.spec.yaml"]
    out: dict[str, str] = {}
    for root in roots:
        if not root.exists():
            continue
        paths = [root] if root.is_file() else [path for path in root.rglob("*") if path.is_file()]
        for path in sorted(paths):
            rel = path.relative_to(workspace).as_posix()
            out[rel] = hashlib.sha256(path.read_bytes()).hexdigest()
    return out


def _idle_prompt_fixture(name: str) -> str:
    text = (Path(__file__).resolve().parent / "fixtures" / "idle_prompts" / name).read_text()
    lines = text.splitlines(keepends=True)
    while lines and _is_fixture_metadata_line(lines[0]):
        lines.pop(0)
    return "".join(lines)


def _is_fixture_metadata_line(line: str) -> bool:
    stripped = line.strip()
    return stripped.startswith("#") or stripped.startswith(("provider=", "captured_at=", "source_pane=", "source_agent="))


def _health_state(workspace: Path, *, tasks: list[dict], provider: str = "codex") -> dict:
    return {
        "workspace": str(workspace),
        "team_dir": str(workspace / ".team" / "current"),
        "session_name": "ta-selftest-health",
        "agents": {
            "worker_1": {
                "status": "running",
                "provider": provider,
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
