from __future__ import annotations

import ast
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
from team_agent.state import save_runtime_state

class SelftestAndIdleAccuracyAcceptanceTests(unittest.TestCase):
    def test_c1_c_rt_8_doctor_comms_help_discloses_no_live_round_trip(self) -> None:
        top_help = _cli_stdout(["--help"])
        self.assertNotIn("selftest", _visible_command_words(top_help))

        doctor_help = _cli_stdout(["doctor", "--help"])
        self.assertIn("--comms", doctor_help)
        first_line = next(line for line in doctor_help.splitlines() if line.strip())
        self.assertIn("validates comms code correctness", first_line)
        self.assertIn("Does NOT perform live runtime message round-trip", first_line)

    def test_c_rt_8_doctor_comms_non_json_banner_discloses_no_live_round_trip(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c2-") as tmp:
            workspace = Path(tmp)
            output = _cli_stdout(["doctor", "--comms", "--workspace", str(workspace)])

        first_line = next(line for line in output.splitlines() if line.strip())
        self.assertIn("Does NOT perform live runtime message round-trip", first_line)
        self.assertIn("zero token", first_line)
        self.assertIn("zero pollution", first_line)

    def test_c2_doctor_comms_and_gate_comms_route_to_same_json_helper(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c2-") as tmp:
            workspace = Path(tmp)
            direct = _cli_json(["doctor", "--comms", "--workspace", str(workspace), "--json"])
            gate = _cli_json(["doctor", "--gate", "comms", "--workspace", str(workspace), "--json"])

        self.assertEqual(_canonical_selftest_json(direct), _canonical_selftest_json(gate))

    def test_receiver_binding_is_state_read_only_and_labeled_binding_consistency(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-selftest-c18-live-") as tmp:
            workspace = Path(tmp)
            state = _health_state(workspace, tasks=[])
            state["team_owner"] = {"pane_id": "%100", "provider": "claude_code", "owner_epoch": 7}
            state["leader_receiver"] = {"mode": "direct_tmux", "pane_id": "%100", "provider": "claude_code", "owner_epoch": 7}
            save_runtime_state(workspace, state)
            before = json.loads(json.dumps(state, sort_keys=True))

            driver = FakeSelftestDriver(state_before=state, current_pane_id="%100")
            result = _run_comms_selftest(workspace=workspace, driver=driver)
            after = driver.state_after

        self.assertTrue(result.get("ok"), result)
        self.assertEqual(after, before)
        binding = result["checks"]["receiver_binding"]
        self.assertEqual(binding["status"], "pass", binding)
        self.assertIn("verifies", binding)
        self.assertEqual(binding["verifies"], "binding_consistency", binding)
        self.assertEqual(binding["proof"], "state_read", binding)
        self.assertTrue(binding["state_read_observed"], binding)
        self.assertEqual(binding["pane_id"], "%100")

    def test_contract_suite_reports_installed_code_env_allowlist_and_scope(self) -> None:
        result = _run_comms_selftest(driver=FakeSelftestDriver(contract_suite=_passing_contract_suite()))

        self.assertTrue(result.get("ok"), result)
        self.assertIn("scope", result)
        self.assertEqual(result["scope"], "code_correctness_and_binding", result)
        self.assertNotEqual(result["scope"], "live_link_runtime_end_to_end", result)
        self.assertIn("contract_suite", result["checks"], result)
        suite = result["checks"]["contract_suite"]
        self.assertEqual(suite["status"], "pass", suite)
        self.assertEqual(suite["verifies"], "code_correctness", suite)
        self.assertTrue(suite["pytest_executed"], suite)
        self.assertEqual(suite["pytest"]["exit_code"], 0, suite)
        self.assertGreater(len(suite["pytest"]["tests_run"]), 0, suite)
        self.assertGreaterEqual(suite["pytest"]["counts"]["passed"], 1, suite)
        self.assertEqual(suite["pytest_env"]["python_path"], suite["live_env"]["python_path"], suite)
        self.assertEqual(suite["pytest_env"]["team_agent_version"], suite["live_env"]["team_agent_version"], suite)
        self.assertEqual(suite["pytest_env"]["site_packages_path"], suite["live_env"]["site_packages_path"], suite)
        self.assertIn("tests.test_messaging_tmux", suite["allowlist"])
        self.assertIn("tests.test_selftest_and_idle_accuracy_acceptance", suite["allowlist"])

    def test_contract_suite_install_mismatch_fails_with_explicit_error(self) -> None:
        suite = _passing_contract_suite()
        suite["pytest_env"]["site_packages_path"] = "/tmp/wrong-install"
        result = _run_comms_selftest(driver=FakeSelftestDriver(contract_suite=suite))

        self.assertFalse(result.get("ok"), result)
        suite_check = result["checks"]["contract_suite"]
        self.assertEqual(suite_check["status"], "fail", suite_check)
        self.assertEqual(suite_check["error"], "install_mismatch", suite_check)
        self.assertIn("site_packages_path", suite_check["mismatched_fields"], suite_check)

    def test_contract_suite_empty_or_all_skip_is_failure_not_default_pass(self) -> None:
        cases = [
            ("empty", {"tests_run": [], "counts": {"passed": 0, "failed": 0, "skipped": 0, "errors": 0}}, "no_tests_run"),
            ("all_skip", {"tests_run": ["tests.test_messaging_tmux"], "counts": {"passed": 0, "failed": 0, "skipped": 1, "errors": 0}}, "all_relevant_tests_skipped"),
        ]
        for _name, override, reason in cases:
            with self.subTest(reason=reason):
                suite = _passing_contract_suite()
                suite["pytest"].update(override)
                result = _run_comms_selftest(driver=FakeSelftestDriver(contract_suite=suite))

                self.assertFalse(result.get("ok"), result)
                suite_check = result["checks"]["contract_suite"]
                self.assertEqual(suite_check["status"], "fail", suite_check)
                self.assertEqual(suite_check["reason"], reason, suite_check)

    def test_pass_checks_must_include_physical_evidence_fields_not_default_pass(self) -> None:
        suite = _passing_contract_suite()
        suite.pop("pytest_executed")
        result = _run_comms_selftest(driver=FakeSelftestDriver(contract_suite=suite))

        self.assertFalse(result.get("ok"), result)
        suite_check = result["checks"]["contract_suite"]
        self.assertEqual(suite_check["status"], "fail", suite_check)
        self.assertEqual(suite_check["reason"], "missing_pytest_evidence", suite_check)

    def test_doctor_comms_does_not_create_throwaway_sessions_or_run_message_probes(self) -> None:
        driver = FakeSelftestDriver(contract_suite=_passing_contract_suite())
        result = _run_comms_selftest(driver=driver)

        self.assertTrue(result.get("ok"), result)
        self.assertEqual(driver.old_probe_calls, [], driver.old_probe_calls)
        self.assertNotIn("leader_to_worker", result["checks"], result)
        self.assertNotIn("worker_to_leader", result["checks"], result)
        self.assertNotIn("matrix", result["checks"], result)
        self.assertNotIn("cleanup", result["checks"], result)

    def test_provider_sdk_calls_are_forbidden_even_when_contract_suite_passes(self) -> None:
        result = _run_comms_selftest(
            driver=FakeSelftestDriver(
                contract_suite=_passing_contract_suite(),
                provider_sdk_calls={"anthropic": 1, "openai": 0, "httpx": 0},
            )
        )

        self.assertFalse(result.get("ok"), result)
        provider = result["checks"]["provider_sdk_calls"]
        self.assertEqual(provider["status"], "fail", provider)
        self.assertEqual(provider["verifies"], "no_provider_sdk_calls", provider)
        self.assertEqual(provider["calls"]["anthropic"], 1, provider)

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
        contract_suite: dict | None = None,
        provider_sdk_calls: dict | None = None,
        current_pane_id: str | None = None,
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
        self.contract_suite = json.loads(json.dumps(contract_suite or _passing_contract_suite(), sort_keys=True))
        self.provider_sdk_calls = dict(provider_sdk_calls or {"anthropic": 0, "openai": 0, "httpx": 0})
        self._current_pane_id = current_pane_id
        self.old_probe_calls: list[str] = []
        self._sessions = list(self.stale_sessions)
        for key, value in kwargs.items():
            setattr(self, key, value)

    def remaining_sessions(self) -> list[str]:
        return list(self._sessions)

    def current_pane_id(self) -> str | None:
        return self._current_pane_id

    def select_runtime_state(self, _workspace: Path, *, team: str | None = None) -> dict:
        return json.loads(json.dumps(self.state_before, sort_keys=True))

    def run_contract_suite(self, *_args, **_kwargs) -> dict:
        return json.loads(json.dumps(self.contract_suite, sort_keys=True))

    def provider_sdk_call_counts(self) -> dict:
        return dict(self.provider_sdk_calls)

    def list_selftest_sessions(self, *_args, **_kwargs) -> list[str]:
        self.old_probe_calls.append("list_selftest_sessions")
        return []

    def list_selftest_workspaces(self, *_args, **_kwargs) -> list[str]:
        self.old_probe_calls.append("list_selftest_workspaces")
        return []

    def create_disposable_receiver(self, *_args, **_kwargs) -> dict:
        self.old_probe_calls.append("create_disposable_receiver")
        return {
            "status": "pass",
            "session_name": "ta-selftest-comms-old-path",
            "pane_id": "%capture",
            "receiver": {"mode": "direct_tmux", "provider": "fake", "pane_id": "%capture"},
        }

    def leader_to_worker(self, *_args, **_kwargs) -> dict:
        self.old_probe_calls.append("leader_to_worker")
        return {"status": "pass"}

    def worker_to_leader(self, *_args, **_kwargs) -> dict:
        self.old_probe_calls.append("worker_to_leader")
        return {"status": "pass"}

    def cleanup_sessions(self, *_args, **_kwargs) -> dict:
        self.old_probe_calls.append("cleanup_sessions")
        return {"status": "pass", "killed_sessions": [], "created_sessions": [], "failed": []}

    def cleanup_throwaway_workspace(self, *_args, **_kwargs) -> dict:
        self.old_probe_calls.append("cleanup_throwaway_workspace")
        return {"status": "pass"}


def _run_comms_selftest(*, workspace: Path | None = None, **kwargs) -> dict:
    if workspace is not None:
        return _run_comms_selftest_in_workspace(workspace, **kwargs)
    with tempfile.TemporaryDirectory(prefix="ta-selftest-contract-") as tmp:
        return _run_comms_selftest_in_workspace(Path(tmp), **kwargs)


def _run_comms_selftest_in_workspace(workspace: Path, **kwargs) -> dict:
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


def _passing_contract_suite() -> dict:
    env = {
        "python_path": "/opt/team-agent/bin/python",
        "team_agent_version": "0.2.8",
        "site_packages_path": "/opt/team-agent/lib/python/site-packages/team_agent",
    }
    allowlist = [
        "tests.test_messaging_tmux",
        "tests.test_send_busy_recipient_acceptance",
        "tests.test_messaging_leader_receiver_buffer",
        "tests.test_selftest_and_idle_accuracy_acceptance",
        "tests.test_messaging_leader",
        "tests.test_messaging_mcp",
        "tests.test_worker_peer_delivery_scheduling",
        "tests.test_result_delivery_contract",
    ]
    return {
        "status": "pass",
        "verifies": "code_correctness",
        "pytest_executed": True,
        "pytest": {
            "exit_code": 0,
            "tests_run": list(allowlist),
            "counts": {"passed": 57, "failed": 0, "skipped": 0, "errors": 0},
            "duration_seconds": 9.1,
            "warnings": [],
        },
        "allowlist": allowlist,
        "pytest_env": dict(env),
        "live_env": dict(env),
    }


def _visible_command_words(help_text: str) -> set[str]:
    words: set[str] = set()
    for token in help_text.replace("{", " ").replace("}", " ").replace(",", " ").split():
        words.add(token.strip())
    return words


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
