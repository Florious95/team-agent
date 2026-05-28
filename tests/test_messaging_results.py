from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

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

class MessagingResultsTests(unittest.TestCase):
    def test_collect_invalid_stored_result_does_not_break_team_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-invalid-stored-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})
            store = MessageStore(workspace)
            result_id = "res_bad_nested"
            bad = _result_envelope("success")
            bad["artifacts"] = ["bad-artifact.md"]
            conn = store.connect()
            try:
                conn.execute(
                    "insert into results(result_id, task_id, agent_id, envelope, status, created_at) values (?, ?, ?, ?, ?, ?)",
                    (result_id, "task_impl", "fake_impl", json.dumps(bad), "success", "now"),
                )
                conn.commit()
            finally:
                conn.close()
            result = runtime.collect(workspace)
            self.assertFalse(result["ok"])
            self.assertEqual(result["invalid_results"][0]["result_id"], result_id)
            self.assertIn("/artifacts/0", result["invalid_results"][0]["error"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "pending")
            state_text = (workspace / "team_state.md").read_text(encoding="utf-8")
            self.assertIn("task_impl [pending]", state_text)

    def test_status_and_collect_expose_uncollected_report_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-status-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})

            accepted = runtime.report_result(workspace, _result_envelope("success"))
            self.assertTrue(accepted["ok"])
            status = runtime.status(workspace, as_json=True)
            self.assertEqual(status["results"]["uncollected"], 1)
            self.assertEqual(status["results"]["by_status"]["success"], 1)
            self.assertIsNone(status["messages"].get("failed"))
            self.assertIn("uncollected 1", runtime.format_status(workspace))
            self.assertIn("1 uncollected result(s) pending", runtime.format_inbox(workspace, "fake_impl"))

            collected = runtime.collect(workspace)
            self.assertEqual(collected["results"]["uncollected"], 0)
            self.assertEqual(collected["results"]["collected"], 1)
            status = runtime.status(workspace, as_json=True)
            self.assertEqual(status["tasks"][0]["status"], "done")
            self.assertEqual(status["tasks"][0]["accepted_result_id"], accepted["result_id"])

    def test_collect_accepts_message_scoped_report_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-message-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake"}},
                    "tasks": spec["tasks"],
                },
            )
            message_id = MessageStore(workspace).create_message(
                None,
                "leader",
                "fake_impl",
                "do one visible action",
                requires_ack=True,
            )
            envelope = _result_envelope("success")
            envelope["task_id"] = message_id

            reported = runtime.report_result(workspace, envelope)
            collected = runtime.collect(workspace)

        self.assertTrue(reported["ok"])
        self.assertEqual(reported["acknowledged_messages"], [message_id])
        self.assertTrue(collected["ok"])
        self.assertEqual(collected["invalid_results"], [])
        self.assertEqual(collected["collected_results"][0]["scope"], "message")

    def test_report_result_queues_leader_notification_without_blocking_mcp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-report-notify-") as tmp:
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
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                accepted = runtime.report_result(workspace, _result_envelope("success"))
            self.assertTrue(accepted["ok"])
            self.assertFalse(accepted["leader_notified"])
            self.assertEqual(accepted["notification_status"], "queued")
            self.assertEqual(accepted["notification_channel"], "coordinator")
            self.assertIsNotNone(accepted["notification_event_id"])

            with patch(
                "team_agent.runtime._send_to_leader_receiver",
                return_value={"ok": True, "message_id": "msg_notice", "status": "submitted", "channel": "direct_tmux"},
            ) as notify:
                tick = runtime.coordinator_tick(workspace)
            self.assertTrue(tick["ok"])
            args, _ = notify.call_args
            self.assertEqual(args[2], "leader")
            self.assertIn("Task task_impl reported success from fake_impl", args[3])
            self.assertEqual(args[4], "task_impl")
            self.assertEqual(args[5], "fake_impl")
            self.assertFalse(args[6])

    def test_coordinator_collects_watched_result_and_notifies_leader(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watch-notify-") as tmp:
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
                    "agents": {},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            notifications: list[dict[str, Any]] = []

            def fake_send_message(*args, **kwargs):
                notifications.append({"args": args, "kwargs": kwargs})
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with patch("team_agent.runtime.send_message", side_effect=fake_send_message):
                tick = runtime.coordinator_tick(workspace)

            self.assertTrue(tick["ok"])
            self.assertEqual(tick["results"]["collected"], 1)
            self.assertEqual(tick["results"]["notified"][0]["watcher_id"], watcher_id)
            self.assertEqual(MessageStore(workspace).results()[0]["status"], "collected")
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["status"], "notified")
            self.assertEqual(watcher["result_id"], result_id)
            self.assertEqual(watcher["notified_message_id"], "msg_notice")
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "done")
            self.assertEqual(state["tasks"][0]["accepted_result_id"], result_id)
            self.assertEqual(notifications[0]["args"][1], "leader")
            self.assertIn("No manual polling is needed", notifications[0]["args"][2])

    def test_mcp_report_result_accepts_summary_and_fills_envelope(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-thin-report-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            store.create_message("task_impl", "leader", "fake_impl", "do work", requires_ack=True)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                result = TeamOrchestratorTools(workspace).report_result(
                    summary="done",
                    tests=[{"command": "unit", "status": "passed"}],
                )
            self.assertTrue(result["ok"])
            self.assertEqual(result["task_id"], "task_impl")
            self.assertEqual(result["agent_id"], "fake_impl")
            self.assertEqual(result["acknowledged_count"], 1)
            self.assertNotIn("acknowledged_messages", result)
            stored = MessageStore(workspace).results()[0]
            envelope = json.loads(stored["envelope"])
            self.assertEqual(envelope["schema_version"], "result_envelope_v1")
            self.assertEqual(envelope["task_id"], "task_impl")
            self.assertEqual(envelope["agent_id"], "fake_impl")
            self.assertEqual(envelope["summary"], "done")
            self.assertEqual(envelope["changes"], [])
            self.assertEqual(envelope["tests"], [{"command": "unit", "status": "passed"}])

    def test_mcp_report_result_without_env_uses_manual_unknown_without_candidate_scan(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-missing-id-report-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
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
            MessageStore(workspace).create_message("task_impl", "leader", "fake_impl", "do work", requires_ack=True)
            old_agent = os.environ.pop("TEAM_AGENT_ID", None)
            try:
                with patch(
                    "team_agent.runtime._notify_leader_of_report_result",
                    return_value={"ok": True, "message_id": "msg_notice", "status": "submitted", "channel": "direct_tmux"},
                ):
                    result = TeamOrchestratorTools(workspace).report_result(summary="done")
            finally:
                if old_agent is not None:
                    os.environ["TEAM_AGENT_ID"] = old_agent
            self.assertTrue(result["ok"])
            self.assertEqual(result["task_id"], "manual")
            self.assertEqual(result["agent_id"], "unknown")
            self.assertTrue(result["leader_notified"])
            envelope = json.loads(MessageStore(workspace).results()[0]["envelope"])
            self.assertEqual(envelope["task_id"], "manual")
            self.assertEqual(envelope["agent_id"], "unknown")

    def test_mcp_report_result_normalizes_common_loose_shapes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-lenient-report-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            store.create_message("task_impl", "leader", "fake_impl", "do work", requires_ack=True)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                first = TeamOrchestratorTools(workspace).report_result(
                    summary="Created hello.py in the current workspace and verified it prints Hello, World!",
                    status="success",
                    changes=[{"path": "hello.py", "description": "Added minimal Python Hello World script."}],
                    tests=[{"command": "python3 hello.py", "status": "passed", "output": "Hello, World!"}],
                )
                second = TeamOrchestratorTools(workspace).report_result(
                    summary="Created hello.py in the current workspace and verified it prints Hello, World!",
                    status="success",
                    changes=[{"kind": "created", "path": "hello.py", "summary": "Added minimal Python Hello World script."}],
                    tests=[{"command": "python3 hello.py", "status": "passed"}],
                )
            self.assertTrue(first["ok"])
            self.assertTrue(second["ok"])
            rows = MessageStore(workspace).results()
            first_envelope = json.loads(rows[0]["envelope"])
            second_envelope = json.loads(rows[1]["envelope"])
            self.assertEqual(
                first_envelope["changes"],
                [{"path": "hello.py", "kind": "created", "description": "Added minimal Python Hello World script."}],
            )
            self.assertEqual(
                first_envelope["tests"],
                [{"command": "python3 hello.py", "status": "passed", "detail": "Hello, World!"}],
            )
            self.assertEqual(
                second_envelope["changes"],
                [{"path": "hello.py", "kind": "created", "description": "Added minimal Python Hello World script."}],
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
