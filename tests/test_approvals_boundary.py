from __future__ import annotations

import inspect
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import approvals, runtime
from team_agent.events import EventLog


PUBLIC_NAMES = [
    "APPROVAL_CHOICE_RE",
    "INTERNAL_MCP_APPROVAL_CHOICE",
    "INTERNAL_MCP_AUTO_APPROVE_TOOLS",
    "STARTUP_PROMPT_RUNTIME_CHECK_LIMIT",
    "active_approval_choice_index",
    "active_approval_control_index",
    "age_text",
    "agent_health_status",
    "approval_choice_keys",
    "approval_prompt_fingerprint",
    "capture_has_approval_prompt",
    "capture_has_team_orchestrator_mcp_prompt",
    "choose_internal_mcp_approval_choice",
    "current_task_for_agent",
    "detect_provider_status",
    "extract_approval_choices",
    "extract_approval_prompt",
    "extract_command_approval_subject",
    "handle_internal_mcp_approval_prompt",
    "handle_provider_runtime_prompts",
    "handle_provider_startup_prompts",
    "is_approval_control_line",
    "line_is_approval_choice",
    "refresh_agent_runtime_statuses",
    "submit_internal_mcp_approval",
    "sync_agent_health",
]


class ApprovalsBoundaryTests(unittest.TestCase):
    """Calibrated convention: ONE identity smoke + ONE lightweight loop +
    per-helper behavior + e2e probes for the main orchestration symbols."""

    def test_runtime_alias_identity_smoke(self) -> None:
        self.assertIs(runtime._refresh_agent_runtime_statuses, approvals.refresh_agent_runtime_statuses)
        self.assertIs(runtime._handle_provider_runtime_prompts, approvals.handle_provider_runtime_prompts)
        self.assertIs(runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT, approvals.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT)

    def test_every_public_name_is_re_exported_on_runtime(self) -> None:
        for name in PUBLIC_NAMES:
            self.assertTrue(hasattr(approvals, name), f"team_agent.approvals missing {name}")
            public_attr = getattr(approvals, name)
            runtime_attr = getattr(runtime, name, None) or getattr(runtime, f"_{name}", None)
            self.assertIsNotNone(runtime_attr, f"runtime missing alias for {name}")
            self.assertIs(public_attr, runtime_attr, f"runtime alias for {name} drifted")

    def test_helpers_have_explicit_signatures(self) -> None:
        for name in PUBLIC_NAMES:
            attr = getattr(approvals, name)
            if not callable(attr):
                continue
            sig = inspect.signature(attr)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{name} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{name} uses **kwargs")

    def test_modules_do_not_top_level_import_runtime(self) -> None:
        for module_name in (
            "team_agent.approvals.constants",
            "team_agent.approvals.parsing",
            "team_agent.approvals.status",
            "team_agent.approvals.runtime_prompts",
            "team_agent.approvals",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            for line in source.splitlines():
                if not line or line.startswith((" ", "\t")):
                    continue
                self.assertFalse(
                    line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                    f"{module_name} top-level imports runtime: {line!r}",
                )


class AgeTextProbeTests(unittest.TestCase):
    def test_returns_dash_for_falsy(self) -> None:
        self.assertEqual(approvals.age_text(None), "-")
        self.assertEqual(approvals.age_text(""), "-")

    def test_returns_dash_for_invalid_iso(self) -> None:
        self.assertEqual(approvals.age_text("not-iso"), "-")

    def test_seconds_minutes_hours_buckets(self) -> None:
        from datetime import datetime, timezone, timedelta
        now = datetime.now(timezone.utc)
        self.assertTrue(approvals.age_text((now - timedelta(seconds=5)).isoformat()).endswith("s ago"))
        self.assertTrue(approvals.age_text((now - timedelta(minutes=5)).isoformat()).endswith("m ago"))
        self.assertTrue(approvals.age_text((now - timedelta(hours=5)).isoformat()).endswith("h ago"))


class AgentHealthStatusProbeTests(unittest.TestCase):
    def test_running_status(self) -> None:
        self.assertEqual(approvals.agent_health_status({"status": "running"}), "IDLE")
        self.assertEqual(approvals.agent_health_status({"status": "busy"}), "RUNNING")

    def test_blocked_status(self) -> None:
        self.assertEqual(approvals.agent_health_status({"status": "paused"}), "BLOCKED")
        self.assertEqual(approvals.agent_health_status({"status": "blocked"}), "BLOCKED")

    def test_error_status(self) -> None:
        for raw in ("error", "missing", "interrupted"):
            self.assertEqual(approvals.agent_health_status({"status": raw}), "ERROR")

    def test_done_status(self) -> None:
        self.assertEqual(approvals.agent_health_status({"status": "stopped"}), "DONE")
        self.assertEqual(approvals.agent_health_status({"status": "done"}), "DONE")

    def test_unknown_status_defaults_to_idle(self) -> None:
        self.assertEqual(approvals.agent_health_status({}), "IDLE")
        self.assertEqual(approvals.agent_health_status({"status": None}), "IDLE")


class CurrentTaskForAgentProbeTests(unittest.TestCase):
    def test_returns_latest_active_task_for_assignee(self) -> None:
        tasks = [
            {"id": "t1", "assignee": "alpha", "status": "pending"},
            {"id": "t2", "assignee": "beta", "status": "ready"},
            {"id": "t3", "assignee": "alpha", "status": "done"},
        ]
        self.assertEqual(approvals.current_task_for_agent(tasks, "alpha"), "t1")
        self.assertEqual(approvals.current_task_for_agent(tasks, "beta"), "t2")

    def test_returns_none_when_no_active_task(self) -> None:
        tasks = [{"id": "t1", "assignee": "alpha", "status": "done"}]
        self.assertIsNone(approvals.current_task_for_agent(tasks, "alpha"))

    def test_returns_none_for_unknown_assignee(self) -> None:
        self.assertIsNone(approvals.current_task_for_agent([{"id": "t1", "assignee": "alpha", "status": "pending"}], "ghost"))


class ApprovalParsingProbeTests(unittest.TestCase):
    def test_extract_approval_prompt_finds_team_orchestrator_mcp(self) -> None:
        text = """\
Allow the team_orchestrator MCP server to run tool "send_message"
1. Allow for this session
2. Deny
Enter to submit | Esc to cancel"""
        out = approvals.extract_approval_prompt("worker_a", text)
        self.assertIsNotNone(out)
        self.assertEqual(out["kind"], "mcp_tool")
        self.assertEqual(out["tool"], "send_message")
        self.assertIn("Allow for this session", out["choices"])

    def test_extract_approval_prompt_finds_command_approval(self) -> None:
        text = """\
Would you like to run the following command?
Bash(curl https://example.com)
1. Yes
2. No
Enter to submit | Esc to cancel"""
        out = approvals.extract_approval_prompt("worker_b", text)
        self.assertIsNotNone(out)
        self.assertEqual(out["kind"], "command")
        self.assertIn("curl", out["command"])

    def test_extract_approval_prompt_returns_none_without_control_line(self) -> None:
        text = "normal worker output\nwith no approval control"
        self.assertIsNone(approvals.extract_approval_prompt("worker_c", text))

    def test_capture_has_approval_prompt_mirrors_extract_truthiness(self) -> None:
        text = "Allow the team_orchestrator MCP server to run tool \"x\"\n1. Allow for this session\nEnter to submit | Esc to cancel"
        self.assertTrue(approvals.capture_has_approval_prompt(text))
        self.assertFalse(approvals.capture_has_approval_prompt(""))

    def test_capture_has_team_orchestrator_mcp_prompt_matches_short_form(self) -> None:
        self.assertTrue(approvals.capture_has_team_orchestrator_mcp_prompt("team_orchestrator - send_message"))
        self.assertTrue(approvals.capture_has_team_orchestrator_mcp_prompt("team_orchestrator.report_result"))
        self.assertFalse(approvals.capture_has_team_orchestrator_mcp_prompt("no mcp prompt"))

    def test_approval_prompt_fingerprint_is_stable_for_same_prompt(self) -> None:
        prompt = {"kind": "mcp_tool", "tool": "send_message", "prompt": "Allow", "choices": ["A", "B"]}
        a = approvals.approval_prompt_fingerprint(prompt)
        b = approvals.approval_prompt_fingerprint(dict(prompt))
        self.assertEqual(a, b)
        self.assertEqual(len(a), 16)

    def test_choose_internal_mcp_approval_choice_prefers_session_option(self) -> None:
        prompt = {"choices": ["Allow for this session", "Yes", "No"]}
        self.assertEqual(approvals.choose_internal_mcp_approval_choice(prompt), "Allow for this session")

    def test_choose_internal_mcp_approval_choice_falls_back_to_yes(self) -> None:
        self.assertEqual(approvals.choose_internal_mcp_approval_choice({"choices": ["Yes", "No"]}), "Yes")

    def test_approval_choice_keys_navigates_from_active_to_target(self) -> None:
        prompt = {"choices": ["A", "B", "C"]}
        capture = "❯ 1. A\n  2. B\n  3. C"
        self.assertEqual(approvals.approval_choice_keys(prompt, capture, "C"), ["Down", "Down", "Enter"])
        capture_active_3 = "  1. A\n  2. B\n❯ 3. C"
        self.assertEqual(approvals.approval_choice_keys(prompt, capture_active_3, "A"), ["Up", "Up", "Enter"])

    def test_active_approval_choice_index_returns_zero_indexed(self) -> None:
        self.assertEqual(approvals.active_approval_choice_index("❯ 2. Allow"), 1)
        self.assertEqual(approvals.active_approval_choice_index("  no marker"), None)


class HandleProviderRuntimePromptsEndToEndProbeTests(unittest.TestCase):
    """End-to-end probe for handle_provider_runtime_prompts -- one of the
    main orchestration symbols in the approvals lane."""

    def _seed_state(self, workspace: Path) -> dict:
        return {
            "session_name": "team-approvals",
            "agents": {"alpha": {"status": "running", "provider": "fake", "window": "alpha"}},
        }

    def test_missing_tmux_session_short_circuits(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-rt-miss-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            state = self._seed_state(workspace)
            with patch("team_agent.runtime._tmux_session_exists", return_value=False), \
                 patch("team_agent.runtime.run_cmd") as run_mock:
                approvals.handle_provider_runtime_prompts(workspace, state, event_log)
            run_mock.assert_not_called()

    def test_paused_agents_are_skipped(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-rt-paused-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            state = {
                "session_name": "team-approvals",
                "agents": {"alpha": {"status": "paused", "provider": "fake", "window": "alpha"}},
            }
            with patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime._tmux_window_exists", return_value=True) as win_mock:
                approvals.handle_provider_runtime_prompts(workspace, state, event_log)
            win_mock.assert_not_called()

    def test_internal_mcp_approval_short_circuits_adapter(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-rt-mcp-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            state = self._seed_state(workspace)
            adapter = Mock()
            adapter.handle_runtime_prompts = Mock(return_value=[])
            internal_result = {"ok": True, "action": "auto_approved", "tool": "send_message"}
            with patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime._tmux_window_exists", return_value=True), \
                 patch("team_agent.approvals.runtime_prompts.handle_internal_mcp_approval_prompt", return_value=internal_result), \
                 patch("team_agent.runtime.get_adapter", return_value=adapter):
                approvals.handle_provider_runtime_prompts(workspace, state, event_log)
            adapter.handle_runtime_prompts.assert_not_called()


class RefreshAgentRuntimeStatusesEndToEndProbeTests(unittest.TestCase):
    """End-to-end probe for refresh_agent_runtime_statuses -- the other
    main orchestration symbol. Exercises status-detection branches with
    runtime helpers patched."""

    def test_missing_tmux_window_marks_status_missing_and_logs_change(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-refresh-missing-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            state = {
                "session_name": "team-refresh",
                "agents": {"alpha": {"status": "running", "provider": "fake", "window": "alpha"}},
            }
            with patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime._tmux_window_exists", return_value=False):
                approvals.refresh_agent_runtime_statuses(workspace, state, event_log)
            self.assertEqual(state["agents"]["alpha"]["status"], "missing")
            self.assertFalse(state["agents"]["alpha"]["tmux_window_present"])
            log_path = workspace / ".team" / "logs" / "events.jsonl"
            self.assertTrue(log_path.exists())
            events = [json.loads(line) for line in log_path.read_text(encoding="utf-8").splitlines() if line]
            self.assertTrue(any(evt.get("event") == "runtime.status_detected" and evt.get("status") == "missing" for evt in events))

    def test_window_present_routes_to_detect_provider_status(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-refresh-detect-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            state = {
                "session_name": "team-refresh",
                "agents": {"alpha": {"status": "running", "provider": "fake", "window": "alpha"}},
            }
            with patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime._tmux_window_exists", return_value=True), \
                 patch("team_agent.approvals.status.detect_provider_status", return_value="busy"):
                approvals.refresh_agent_runtime_statuses(workspace, state, event_log)
            self.assertEqual(state["agents"]["alpha"]["status"], "busy")

    def test_paused_or_stopped_agents_are_skipped(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-refresh-skip-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            state = {
                "session_name": "team-refresh",
                "agents": {
                    "p": {"status": "paused", "provider": "fake", "window": "p"},
                    "s": {"status": "stopped", "provider": "fake", "window": "s"},
                },
            }
            with patch("team_agent.runtime._tmux_session_exists", return_value=False), \
                 patch("team_agent.runtime._tmux_window_exists", return_value=False) as win_mock:
                approvals.refresh_agent_runtime_statuses(workspace, state, event_log)
            win_mock.assert_not_called()


class SyncAgentHealthEndToEndProbeTests(unittest.TestCase):
    """End-to-end probe for sync_agent_health -- writes to the MessageStore
    agent_health table based on tmux capture + approval-prompt detection."""

    def test_sync_writes_idle_when_no_capture_available(self) -> None:
        from team_agent.message_store import MessageStore
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-sync-idle-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            store = MessageStore(workspace)
            state = {
                "session_name": "team-sync",
                "agents": {"alpha": {"status": "running", "provider": "fake", "window": "alpha"}},
                "tasks": [],
            }
            with patch("team_agent.runtime._tmux_window_exists", return_value=False):
                approvals.sync_agent_health(workspace, state, store)
            health = store.agent_health()
            self.assertIn("alpha", health)
            self.assertEqual(health["alpha"]["status"], "IDLE")

    def test_sync_upgrades_to_awaiting_approval_when_prompt_detected(self) -> None:
        from team_agent.message_store import MessageStore
        with tempfile.TemporaryDirectory(prefix="team-agent-appr-sync-prompt-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            event_log = EventLog(workspace)
            store = MessageStore(workspace)
            state = {
                "session_name": "team-sync",
                "agents": {"alpha": {"status": "running", "provider": "fake", "window": "alpha"}},
                "tasks": [],
            }
            capture = Mock(returncode=0, stdout="Allow the team_orchestrator MCP server to run tool \"x\"\n1. Allow for this session\nEnter to submit | Esc to cancel")
            with patch("team_agent.runtime._tmux_window_exists", return_value=True), \
                 patch("team_agent.runtime.run_cmd", return_value=capture):
                approvals.sync_agent_health(workspace, state, store)
            health = store.agent_health()
            self.assertEqual(health["alpha"]["status"], "AWAITING_APPROVAL")


if __name__ == "__main__":
    unittest.main()
