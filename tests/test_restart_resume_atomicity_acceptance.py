from __future__ import annotations

import copy
import inspect
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
from team_agent.simple_yaml import dumps
from team_agent.state import load_runtime_state, save_runtime_state


AGENT_IDS = ["worker_a", "worker_b", "worker_c", "worker_d", "worker_e", "worker_f"]
FIRST_SEND_AT = "2026-05-26T12:00:00+00:00"


class RestartResumeAtomicityAcceptanceTests(unittest.TestCase):
    def test_1_all_resumable_succeeds(self) -> None:
        self._assert_restart_signature()
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-atomic-all-") as tmp:
            workspace = Path(tmp)
            session_name = _write_workspace(workspace, AGENT_IDS)
            tmux = FakeTmux()
            adapter = RecordingAdapter(unresumable=set())

            with _patched_restart_dependencies(adapter, tmux):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"], result)
            self.assertEqual(_restart_modes(result), ["resumed"] * 6)
            self.assertEqual(sorted(tmux.sessions[session_name]), sorted(AGENT_IDS))
            state = load_runtime_state(workspace)
            self.assertEqual(
                {agent_id: state["agents"][agent_id]["session_id"] for agent_id in AGENT_IDS},
                {agent_id: f"session-{agent_id}" for agent_id in AGENT_IDS},
            )
            self.assertEqual(_started_command_modes(tmux), ["resume"] * 6)

    def test_2_one_unresumable_without_allow_fresh_fails_and_rolls_back(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-atomic-one-bad-") as tmp:
            workspace = Path(tmp)
            session_name = _write_workspace(
                workspace,
                AGENT_IDS,
                missing_session_ids={"worker_e"},
                first_send_at_ids=set(AGENT_IDS),
            )
            tmux = FakeTmux()
            adapter = RecordingAdapter(unresumable=set())

            with _patched_restart_dependencies(adapter, tmux):
                result = _restart_result_or_exception(workspace)

            self.assertNotIn(session_name, tmux.sessions, "failed restart must roll back newly created tmux session")
            self.assertNotIn("fresh", _started_command_modes(tmux), "fresh fallback is forbidden without allow_fresh")
            self.assertEqual(result.get("ok"), False, result)
            self.assertIn("worker_e", _failure_text(result))

    def test_3_one_unresumable_with_allow_fresh_allowed_partial(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-atomic-allow-fresh-") as tmp:
            workspace = Path(tmp)
            session_name = _write_workspace(
                workspace,
                AGENT_IDS,
                missing_session_ids={"worker_e"},
                first_send_at_ids=set(AGENT_IDS),
            )
            tmux = FakeTmux()
            adapter = RecordingAdapter(unresumable=set())

            with _patched_restart_dependencies(adapter, tmux):
                result = runtime.restart(workspace, allow_fresh=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(_restart_modes(result), ["resumed", "resumed", "resumed", "resumed", "fresh", "resumed"])
            self.assertEqual(sorted(tmux.sessions[session_name]), sorted(AGENT_IDS))
            self.assertEqual(_started_command_modes(tmux), ["resume", "resume", "resume", "resume", "fresh", "resume"])
            self.assertIsNone(load_runtime_state(workspace)["agents"]["worker_e"]["session_id"])

    def test_4_all_unresumable_without_allow_fresh_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-atomic-all-bad-") as tmp:
            workspace = Path(tmp)
            session_name = _write_workspace(
                workspace,
                AGENT_IDS,
                missing_session_ids=set(AGENT_IDS),
                first_send_at_ids=set(AGENT_IDS),
            )
            tmux = FakeTmux()
            adapter = RecordingAdapter(unresumable=set())

            with _patched_restart_dependencies(adapter, tmux):
                result = _restart_result_or_exception(workspace)

            self.assertNotIn(session_name, tmux.sessions, "failed restart must leave no newly created tmux session")
            self.assertEqual(_started_command_modes(tmux), [])
            self.assertEqual(result.get("ok"), False, result)
            self.assertIn("worker_a", _failure_text(result))

    def test_5_never_interacted_workers_restart_fresh_without_violation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-atomic-never-used-") as tmp:
            workspace = Path(tmp)
            session_name = _write_workspace(
                workspace,
                AGENT_IDS,
                missing_session_ids=set(AGENT_IDS),
                first_send_at_ids=set(),
            )
            tmux = FakeTmux()
            adapter = RecordingAdapter(unresumable=set())

            with _patched_restart_dependencies(adapter, tmux):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"], result)
            self.assertEqual(_restart_modes(result), ["fresh"] * 6)
            self.assertEqual(_started_command_modes(tmux), ["fresh"] * 6)
            self.assertEqual(sorted(tmux.sessions[session_name]), sorted(AGENT_IDS))
            self.assertNotIn("restart.atomic_refusal", _event_names(workspace))

    def test_6_mixed_interacted_and_never_interacted_partial_resume(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-atomic-mixed-") as tmp:
            workspace = Path(tmp)
            interacted = set(AGENT_IDS[:3])
            never_interacted = set(AGENT_IDS[3:])
            session_name = _write_workspace(
                workspace,
                AGENT_IDS,
                missing_session_ids=never_interacted,
                first_send_at_ids=interacted,
            )
            tmux = FakeTmux()
            adapter = RecordingAdapter(unresumable=set())

            with _patched_restart_dependencies(adapter, tmux):
                result = runtime.restart(workspace)

            self.assertTrue(result["ok"], result)
            self.assertEqual(_restart_modes(result), ["resumed", "resumed", "resumed", "fresh", "fresh", "fresh"])
            self.assertEqual(_started_command_modes(tmux), ["resume", "resume", "resume", "fresh", "fresh", "fresh"])
            self.assertEqual(sorted(tmux.sessions[session_name]), sorted(AGENT_IDS))
            state = load_runtime_state(workspace)
            self.assertEqual(
                {agent_id: state["agents"][agent_id]["session_id"] for agent_id in interacted},
                {agent_id: f"session-{agent_id}" for agent_id in interacted},
            )
            self.assertEqual(
                {agent_id: state["agents"][agent_id]["session_id"] for agent_id in never_interacted},
                {agent_id: None for agent_id in never_interacted},
            )
            self.assertNotIn("restart.atomic_refusal", _event_names(workspace))

    def _assert_restart_signature(self) -> None:
        signature = inspect.signature(runtime.restart)
        self.assertEqual(list(signature.parameters), ["workspace", "allow_fresh", "team"])
        self.assertEqual(signature.parameters["allow_fresh"].default, False)
        self.assertIsNone(signature.parameters["team"].default)


class RecordingAdapter:
    provider = "fake"
    command_name = sys.executable

    def __init__(self, *, unresumable: set[str]) -> None:
        self.unresumable = set(unresumable)
        self.cleaned: list[str] = []

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
        self.cleaned.append(agent_id)

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
    missing_session_ids: set[str] | None = None,
    first_send_at_ids: set[str] | None = None,
) -> str:
    missing_session_ids = missing_session_ids or set()
    if first_send_at_ids is None:
        first_send_at_ids = {agent_id for agent_id in agent_ids if agent_id not in missing_session_ids}
    spec = cli_fake_spec(workspace)
    base_agent = copy.deepcopy(spec["agents"][0])
    spec["team"]["name"] = "restart-atomicity"
    spec["runtime"]["session_name"] = "team-restart-atomicity"
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
        agents[agent_id] = {
            "status": "stopped",
            "provider": "fake",
            "agent_id": agent_id,
            "window": agent_id,
            "session_id": None if agent_id in missing_session_ids else f"session-{agent_id}",
            "first_send_at": FIRST_SEND_AT if agent_id in first_send_at_ids else None,
            "spawn_cwd": str(workspace),
            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"),
        }
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


def _resume_command(agent: dict[str, Any], previous: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> str:
    _ = workspace, mcp_config
    return f"resume:{agent['id']}:{previous.get('session_id')}"


def _fresh_command(agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> str:
    _ = workspace, mcp_config
    return f"fresh:{agent['id']}"


def _restart_result_or_exception(workspace: Path, **kwargs: Any) -> dict[str, Any]:
    try:
        return runtime.restart(workspace, **kwargs)
    except Exception as exc:  # Contract expects a structured result, not an exception.
        return {"ok": "raised", "reason": str(exc), "exception_type": type(exc).__name__}


def _restart_modes(result: dict[str, Any]) -> list[str]:
    return [item.get("restart_mode") for item in result.get("agents", [])]


def _started_command_modes(tmux: FakeTmux) -> list[str]:
    modes = []
    for call in tmux.calls:
        if call[:3] == ["tmux", "new-session", "-d"] or call[:2] == ["tmux", "new-window"]:
            command = call[-1]
            if command.startswith("resume:"):
                modes.append("resume")
            elif command.startswith("fresh:"):
                modes.append("fresh")
    return modes


def _failure_text(result: dict[str, Any]) -> str:
    return " ".join(str(result.get(key, "")) for key in ("reason", "error", "message"))


def _event_names(workspace: Path) -> list[str]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line)["event"] for line in path.read_text(encoding="utf-8").splitlines()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
