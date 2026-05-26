from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.cli.commands import cmd_status
from team_agent.simple_yaml import dumps
from team_agent.state import load_runtime_state, save_runtime_state


FIRST_SEND_AT = "2026-05-27T10:23:00+00:00"


class C1WorkerFirstInteractionAuditTests(unittest.TestCase):
    def test_1_first_leader_to_worker_send_emits_worker_first_interaction(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c1-first-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace, ["worker_a"], status="running")

            with _patched_send_success():
                result = runtime.send_message(workspace, "worker_a", "first task", timeout=0.01)

            self.assertTrue(result["ok"], result)
            state = load_runtime_state(workspace)
            first_send_at = state["agents"]["worker_a"].get("first_send_at")
            self.assertIsNotNone(first_send_at)
            events = _events_named(workspace, "worker.first_interaction")
            self.assertEqual(len(events), 1, _events(workspace))
            self.assertEqual(events[0]["worker_id"], "worker_a")
            self.assertEqual(events[0]["first_send_at"], first_send_at)
            self.assertEqual(events[0]["message_id"], result["message_id"])

    def test_2_subsequent_sends_do_not_re_emit_worker_first_interaction(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c1-idempotent-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace, ["worker_a"], status="running")

            with _patched_send_success():
                first = runtime.send_message(workspace, "worker_a", "first task", timeout=0.01)
                second = runtime.send_message(workspace, "worker_a", "follow-up task", timeout=0.01)

            self.assertTrue(first["ok"], first)
            self.assertTrue(second["ok"], second)
            events = _events_named(workspace, "worker.first_interaction")
            self.assertEqual(len(events), 1, _events(workspace))
            self.assertEqual(events[0]["worker_id"], "worker_a")
            self.assertEqual(events[0]["message_id"], first["message_id"])

    def test_3_worker_to_worker_peer_send_does_not_emit_worker_first_interaction(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c1-peer-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace, ["worker_a", "worker_b"], status="running")

            with _patched_send_success(), patch("team_agent.runtime._send_to_leader_receiver", return_value={"ok": True}):
                result = runtime.send_message(workspace, "worker_b", "peer note", sender="worker_a", timeout=0.01)

            self.assertTrue(result["ok"], result)
            state = load_runtime_state(workspace)
            self.assertIsNone(state["agents"]["worker_b"].get("first_send_at"))
            self.assertEqual(_events_named(workspace, "worker.first_interaction"), [])


class C2RestartResumeDecisionAuditTests(unittest.TestCase):
    def test_4_restart_emits_one_resume_decision_event_per_worker_with_decision_resume(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c2-resume-") as tmp:
            workspace = Path(tmp)
            _write_workspace(
                workspace,
                ["worker_a", "worker_b"],
                status="stopped",
                session_ids={"worker_a": "session-worker_a", "worker_b": "session-worker_b"},
                first_send_at={"worker_a": FIRST_SEND_AT, "worker_b": FIRST_SEND_AT},
            )
            tmux = FakeTmux()

            with _patched_restart_dependencies(RecordingAdapter(), tmux):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"], result)
            events = _events_named(workspace, "restart.resume_decision")
            self.assertEqual(len(events), 2, _events(workspace))
            by_worker = {event["worker_id"]: event for event in events}
            self.assertEqual(set(by_worker), {"worker_a", "worker_b"})
            for worker_id, event in by_worker.items():
                self.assertIs(event["has_first_send_at"], True)
                self.assertIs(event["has_session_id"], True)
                self.assertIs(event["allow_fresh"], False)
                self.assertEqual(event["decision"], "resume")
                self.assertEqual(event["first_send_at"], FIRST_SEND_AT)
                self.assertEqual(event["session_id"], f"session-{worker_id}")

    def test_5_restart_emits_resume_decision_with_decision_refuse_when_first_send_at_present_session_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c2-refuse-") as tmp:
            workspace = Path(tmp)
            _write_workspace(
                workspace,
                ["worker_a"],
                status="stopped",
                session_ids={"worker_a": None},
                first_send_at={"worker_a": FIRST_SEND_AT},
            )

            with _patched_restart_dependencies(RecordingAdapter(), FakeTmux()):
                result = runtime.restart(workspace)

            self.assertEqual(result.get("ok"), False, result)
            decisions = _events_named(workspace, "restart.resume_decision")
            self.assertEqual(len(decisions), 1, _events(workspace))
            decision = decisions[0]
            self.assertEqual(decision["worker_id"], "worker_a")
            self.assertIs(decision["has_first_send_at"], True)
            self.assertIs(decision["has_session_id"], False)
            self.assertIs(decision["allow_fresh"], False)
            self.assertEqual(decision["decision"], "refuse")
            self.assertEqual(decision["first_send_at"], FIRST_SEND_AT)
            self.assertIsNone(decision["session_id"])
            self.assertEqual(len(_events_named(workspace, "restart.atomic_refusal")), 1)

    def test_6_restart_emits_resume_decision_with_decision_fresh_start_for_never_interacted(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c2-fresh-") as tmp:
            workspace = Path(tmp)
            _write_workspace(
                workspace,
                ["worker_a"],
                status="stopped",
                session_ids={"worker_a": None},
                first_send_at={"worker_a": None},
            )
            tmux = FakeTmux()

            with _patched_restart_dependencies(RecordingAdapter(), tmux):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"], result)
            decisions = _events_named(workspace, "restart.resume_decision")
            self.assertEqual(len(decisions), 1, _events(workspace))
            decision = decisions[0]
            self.assertEqual(decision["worker_id"], "worker_a")
            self.assertIs(decision["has_first_send_at"], False)
            self.assertIs(decision["has_session_id"], False)
            self.assertIs(decision["allow_fresh"], False)
            self.assertEqual(decision["decision"], "fresh_start")
            self.assertIsNone(decision["first_send_at"])
            self.assertIsNone(decision["session_id"])
            self.assertEqual(_events_named(workspace, "restart.atomic_refusal"), [])


class C3StatusInteractedAuditTests(unittest.TestCase):
    def test_7_status_json_includes_interacted_field_per_worker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c3-status-") as tmp:
            workspace = Path(tmp)
            _write_workspace(
                workspace,
                ["worker_a", "worker_b"],
                status="running",
                session_ids={"worker_a": "session-worker_a", "worker_b": None},
                first_send_at={"worker_a": FIRST_SEND_AT, "worker_b": None},
            )

            with _patched_status_dependencies():
                payload = runtime.status(workspace, as_json=True, compact=False)

            failures: list[str] = []
            if payload["agents"]["worker_a"].get("interacted") != FIRST_SEND_AT:
                failures.append(f"worker_a interacted={payload['agents']['worker_a'].get('interacted')!r}")
            if payload["agents"]["worker_b"].get("interacted") != "never":
                failures.append(f"worker_b interacted={payload['agents']['worker_b'].get('interacted')!r}")

            summary_data = {
                "coordinator": {"status": "running", "schema_ok": True},
                "tmux_session_present": True,
                "leader_receiver": {},
                "agents": payload["agents"],
                "agent_health": {},
                "queued_messages": [],
                "latest_results": [],
            }
            args = Mock(workspace=str(workspace), json=False, detail=False, summary=True, agent=None)
            with patch("team_agent.cli.commands.runtime.status", return_value=summary_data):
                summary = cmd_status(args)
            agents_line = summary.splitlines()[2]
            if "(1 interacted, 1 never)" not in agents_line:
                failures.append(f"summary agents line missing gated marker: {agents_line!r}")

            zero_data = dict(summary_data)
            zero_data["agents"] = {
                "worker_a": {"status": "running"},
                "worker_b": {"status": "running"},
            }
            with patch("team_agent.cli.commands.runtime.status", return_value=zero_data):
                zero_summary = cmd_status(args)
            zero_agents_line = zero_summary.splitlines()[2]
            if "interacted" in zero_agents_line or "never" in zero_agents_line:
                failures.append(f"zero-interacted summary must preserve Gap 18a shape: {zero_agents_line!r}")

            self.assertEqual([], failures)


class C4AtomicRefusalEvidenceTests(unittest.TestCase):
    def test_8_atomic_refusal_envelope_error_message_includes_first_send_at_timestamp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-b-c4-refusal-") as tmp:
            workspace = Path(tmp)
            _write_workspace(
                workspace,
                ["worker_a"],
                status="stopped",
                session_ids={"worker_a": None},
                first_send_at={"worker_a": FIRST_SEND_AT},
            )

            with _patched_restart_dependencies(RecordingAdapter(), FakeTmux()):
                result = runtime.restart(workspace)

            self.assertEqual(result.get("ok"), False, result)
            failures: list[str] = []
            unresumable = result.get("unresumable") or []
            if not unresumable or unresumable[0].get("first_send_at") != FIRST_SEND_AT:
                failures.append(f"result unresumable lacks first_send_at: {unresumable!r}")
            refusal_events = _events_named(workspace, "restart.atomic_refusal")
            if not refusal_events:
                failures.append("missing restart.atomic_refusal event")
            elif (refusal_events[0].get("unresumable") or [{}])[0].get("first_send_at") != FIRST_SEND_AT:
                failures.append(f"event unresumable lacks first_send_at: {refusal_events[0]!r}")
            error = str(result.get("error") or "")
            if "worker_a" not in error:
                failures.append(f"error message missing worker id: {error!r}")
            if FIRST_SEND_AT not in error:
                failures.append(f"error message missing first_send_at timestamp: {error!r}")
            if "--allow-fresh" not in error:
                failures.append(f"error message missing --allow-fresh action hint: {error!r}")

            self.assertEqual([], failures)


class RouteBFirstSendAtValidationTests(unittest.TestCase):
    def test_9_first_send_at_edge_values_classified_strictly(self) -> None:
        garbage_values: dict[str, Any] = {
            "empty_string": "",
            "zero": 0,
            "false": False,
            "literal_null": "null",
        }
        for label, bad_value in garbage_values.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory(prefix=f"team-agent-route-b-bad-first-send-{label}-") as tmp:
                workspace = Path(tmp)
                _write_workspace(
                    workspace,
                    ["worker_a"],
                    status="stopped",
                    session_ids={"worker_a": None},
                    first_send_at={"worker_a": bad_value},
                )

                with _patched_restart_dependencies(RecordingAdapter(), FakeTmux()):
                    result = runtime.restart(workspace)

                failures: list[str] = []
                if result.get("ok") is not False:
                    failures.append(f"restart must reject corrupt first_send_at, got {result!r}")
                if result.get("reason") != "invalid_first_send_at":
                    failures.append(f"reason must be invalid_first_send_at, got {result!r}")
                invalid_events = _events_named(workspace, "restart.first_send_at_invalid")
                if len(invalid_events) != 1:
                    failures.append(f"expected one restart.first_send_at_invalid event, got {_events(workspace)!r}")
                else:
                    event = invalid_events[0]
                    if event.get("worker_id") != "worker_a":
                        failures.append(f"invalid event missing worker_id: {event!r}")
                    if event.get("raw_first_send_at") != bad_value:
                        failures.append(f"invalid event raw_first_send_at mismatch: {event!r}")
                    if event.get("raw_first_send_at_type") != type(bad_value).__name__:
                        failures.append(f"invalid event raw_first_send_at_type mismatch: {event!r}")
                if _events_named(workspace, "restart.atomic_refusal"):
                    failures.append("corrupt first_send_at must not be misreported as resume_atomicity")
                if _events_named(workspace, "restart.fresh_spawn"):
                    failures.append("corrupt first_send_at must not silently fresh-start")

                self.assertEqual([], failures)


class RecordingAdapter:
    provider = "fake"
    command_name = sys.executable

    def __init__(self, *, unresumable: set[str] | None = None) -> None:
        self.unresumable = set(unresumable or set())

    def is_installed(self) -> bool:
        return True

    def mcp_config(self, workspace: Path, agent_id: str) -> dict[str, Any]:
        return {"team_orchestrator": {"command": sys.executable, "args": [], "env": {"TEAM_AGENT_ID": agent_id}}}

    def install_mcp(self, workspace: Path, agent_id: str, config: dict[str, Any]) -> Path:
        path = workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"mcpServers": config}), encoding="utf-8")
        return path

    def cleanup_mcp(self, workspace: Path, agent_id: str, mcp_path: Path | None = None) -> None:
        _ = workspace, agent_id, mcp_path

    def session_is_resumable(self, agent_state: dict[str, Any], workspace: Path) -> bool:
        _ = workspace
        return bool(agent_state.get("session_id")) and agent_state.get("agent_id") not in self.unresumable

    def recover_session_id(
        self,
        agent_id: str,
        agent_state: dict[str, Any],
        workspace: Path,
        exclude_session_ids: set[str] | None = None,
    ) -> dict[str, Any] | None:
        _ = agent_id, agent_state, workspace, exclude_session_ids
        return None


class FakeTmux:
    def __init__(self) -> None:
        self.sessions: dict[str, set[str]] = {}
        self.calls: list[list[str]] = []

    def run_cmd(self, args: list[str], timeout: int = 20) -> Mock:
        _ = timeout
        self.calls.append(args)
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:2] == ["tmux", "has-session"]:
            proc.returncode = 0 if args[-1] in self.sessions else 1
        elif args[:3] == ["tmux", "new-session", "-d"]:
            session = args[args.index("-s") + 1]
            window = args[args.index("-n") + 1]
            self.sessions[session] = {window}
        elif args[:2] == ["tmux", "new-window"]:
            session = args[args.index("-t") + 1]
            window = args[args.index("-n") + 1]
            self.sessions.setdefault(session, set()).add(window)
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            session = args[3]
            if session not in self.sessions:
                proc.returncode = 1
                proc.stderr = f"can't find session: {session}"
            else:
                proc.stdout = "\n".join(sorted(self.sessions[session]))
        elif args[:3] == ["tmux", "kill-session", "-t"]:
            self.sessions.pop(args[3], None)
        return proc


def _write_workspace(
    workspace: Path,
    agent_ids: list[str],
    *,
    status: str,
    session_ids: dict[str, str | None] | None = None,
    first_send_at: dict[str, Any] | None = None,
) -> str:
    spec = cli_fake_spec(workspace)
    base_agent = copy.deepcopy(spec["agents"][0])
    spec["team"]["name"] = "route-b-audit"
    spec["runtime"]["session_name"] = "team-route-b-audit"
    spec["runtime"]["startup_order"] = list(agent_ids)
    spec["runtime"]["max_active_agents"] = len(agent_ids)
    spec["runtime"]["display_backend"] = "none"
    spec["routing"]["default_assignee"] = agent_ids[0]
    spec["routing"]["rules"] = []
    spec["agents"] = []
    agents: dict[str, Any] = {}
    for agent_id in agent_ids:
        agent = copy.deepcopy(base_agent)
        agent["id"] = agent_id
        agent["role"] = f"Worker {agent_id}"
        spec["agents"].append(agent)
        agent_state = {
            "status": status,
            "provider": "fake",
            "agent_id": agent_id,
            "window": agent_id,
            "session_id": (session_ids or {}).get(agent_id),
            "first_send_at": (first_send_at or {}).get(agent_id),
            "spawn_cwd": str(workspace),
            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"),
        }
        agents[agent_id] = agent_state
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "workspace": str(workspace),
            "session_name": spec["runtime"]["session_name"],
            "leader": spec["leader"],
            "agents": agents,
            "tasks": spec["tasks"],
            "display_backend": "none",
        },
    )
    return spec["runtime"]["session_name"]


@contextmanager
def _patched_send_success():
    with ExitStack() as stack:
        stack.enter_context(patch("team_agent.runtime._tmux_window_exists", return_value=True))
        stack.enter_context(
            patch(
                "team_agent.runtime._tmux_inject_text",
                return_value={
                    "ok": True,
                    "verification": "capture_contains_new_pasted_content_prompt",
                    "submit_verification": "prompt_returned_after_submit",
                    "turn_verification": "submitted",
                    "attempts": [],
                    "submit_attempts": [],
                },
            )
        )
        stack.enter_context(patch("team_agent.runtime._capture_missing_sessions", return_value=[]))
        yield


@contextmanager
def _patched_restart_dependencies(adapter: RecordingAdapter, tmux: FakeTmux):
    patches = [
        patch("team_agent.runtime.get_adapter", return_value=adapter),
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
        patch("team_agent.runtime._ensure_agent_start_requirements", return_value=None),
        patch("team_agent.runtime._handle_startup_prompts_and_verify_window", return_value=True),
        patch("team_agent.runtime._capture_agent_session", return_value=None),
        patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}),
        patch("team_agent.runtime.shell_resume_command_for_agent", side_effect=_resume_command),
        patch("team_agent.runtime.shell_command_for_agent", side_effect=_fresh_command),
        patch("team_agent.leader.autobind_leader_receiver_from_env", return_value={"ok": True}),
    ]
    with ExitStack() as stack:
        for item in patches:
            stack.enter_context(item)
        yield


@contextmanager
def _patched_status_dependencies():
    patches = [
        patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
        patch("team_agent.runtime._refresh_agent_runtime_statuses", return_value=None),
        patch("team_agent.runtime._handle_provider_startup_prompts", return_value=None),
        patch("team_agent.runtime._sync_agent_health", return_value=None),
        patch("team_agent.runtime._tmux_session_exists", return_value=True),
        patch("team_agent.runtime.coordinator_health", return_value={"status": "running", "pid": 123, "schema_ok": True}),
    ]
    with ExitStack() as stack:
        for item in patches:
            stack.enter_context(item)
        yield


def _resume_command(agent: dict[str, Any], previous: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> str:
    _ = workspace, mcp_config
    return f"resume:{agent['id']}:{previous.get('session_id')}"


def _fresh_command(agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> str:
    _ = workspace, mcp_config
    return f"fresh:{agent['id']}"


def _events_named(workspace: Path, name: str) -> list[dict[str, Any]]:
    return [event for event in _events(workspace) if event.get("event") == name]


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
