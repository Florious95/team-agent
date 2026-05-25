from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.simple_yaml import dumps
from team_agent.state import load_runtime_state, save_runtime_state


def _spec_with_agents(workspace: Path, ids: list[str], *, display_backend: str = "none") -> dict[str, Any]:
    spec = cli_fake_spec(workspace)
    base = copy.deepcopy(spec["agents"][0])
    spec["agents"] = []
    for agent_id in ids:
        agent = copy.deepcopy(base)
        agent["id"] = agent_id
        agent["role"] = f"Worker {agent_id}"
        spec["agents"].append(agent)
    spec["runtime"]["session_name"] = "team-lifecycle-failure"
    spec["runtime"]["startup_order"] = ids
    spec["runtime"]["max_active_agents"] = len(ids)
    spec["runtime"]["display_backend"] = display_backend
    spec["routing"]["default_assignee"] = ids[0]
    spec["routing"]["rules"] = []
    return spec


def _write_restart_workspace(workspace: Path, ids: list[str], *, display_backend: str = "none") -> dict[str, Any]:
    spec = _spec_with_agents(workspace, ids, display_backend=display_backend)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "workspace": str(workspace),
            "session_name": spec["runtime"]["session_name"],
            "agents": {},
            "tasks": spec["tasks"],
            "display_backend": display_backend,
        },
    )
    return spec


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


class FakeTmux:
    def __init__(self) -> None:
        self.sessions: dict[str, set[str]] = {}
        self.calls: list[list[str]] = []
        self.fail_new_window_names: set[str] = set()
        self.fail_kill_sessions: set[str] = set()
        self.fail_open = False
        self._pane_index = 0

    def run_cmd(self, args: list[str], timeout: int = 20) -> Mock:
        self.calls.append(args)
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:2] == ["tmux", "has-session"]:
            proc.returncode = 0 if args[-1] in self.sessions else 1
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            session = args[3]
            proc.returncode = 0 if session in self.sessions else 1
            proc.stdout = "\n".join(sorted(self.sessions.get(session, set())))
        elif args[:3] == ["tmux", "new-session", "-d"]:
            session = args[args.index("-s") + 1]
            base = args[args.index("-t") + 1] if "-t" in args else None
            window = args[args.index("-n") + 1] if "-n" in args else None
            self.sessions[session] = set(self.sessions.get(base, set())) if base else set()
            if window:
                self.sessions[session].add(window)
            if "-P" in args:
                self._pane_index += 1
                proc.stdout = f"%{self._pane_index}\n"
        elif args[:2] == ["tmux", "new-window"]:
            session = args[args.index("-t") + 1]
            window = args[args.index("-n") + 1]
            if window in self.fail_new_window_names:
                proc.returncode = 1
                proc.stderr = f"startup failed for {window}"
            else:
                self.sessions.setdefault(session, set()).add(window)
                if "-P" in args:
                    self._pane_index += 1
                    proc.stdout = f"%{self._pane_index}\n"
        elif args[:3] == ["tmux", "kill-session", "-t"]:
            session = args[3]
            if session in self.fail_kill_sessions:
                proc.returncode = 1
                proc.stderr = f"kill failed for {session}"
            else:
                self.sessions.pop(session, None)
        elif args[:2] == ["tmux", "split-window"]:
            self._pane_index += 1
            proc.stdout = f"%{self._pane_index}\n"
        elif args[:3] == ["tmux", "select-window", "-t"]:
            session = args[3].split(":", 1)[0]
            proc.returncode = 0 if session in self.sessions else 1
            if proc.returncode:
                proc.stderr = f"can't find session: {session}"
        elif args and args[0] == "open":
            if self.fail_open:
                proc.returncode = 1
                proc.stderr = "Ghostty failed to open"
        return proc


class LifecycleFailureInjectionTests(unittest.TestCase):
    @unittest.skip(
        "Gap 15 base slice required: restart midway must clean up newly-created sessions; "
        "see team-agent-unattended-runtime-gaps.md Gap 15"
    )
    def test_restart_midway_worker_failure_rolls_back_new_session_only(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-midway-") as tmp:
            workspace = Path(tmp)
            _write_restart_workspace(workspace, ["fake_a", "fake_b"])
            tmux = FakeTmux()
            tmux.sessions["pre_existing_team"] = {"kept_worker"}
            tmux.fail_new_window_names.add("fake_b")

            with (
                patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
                patch("team_agent.runtime._ensure_agent_start_requirements", return_value=None),
                patch("team_agent.runtime._handle_startup_prompts_and_verify_window", return_value=True),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime.restart(workspace)

            self.assertNotIn("team-lifecycle-failure", tmux.sessions)
            self.assertEqual(tmux.sessions.get("pre_existing_team"), {"kept_worker"})
            self.assertIn(["tmux", "kill-session", "-t", "team-lifecycle-failure"], tmux.calls)

    def test_display_setup_failure_rolls_back_display_sessions_after_workers_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-display-rollback-") as tmp:
            workspace = Path(tmp)
            tmux = FakeTmux()
            session_name = "team-display-rollback"
            tmux.sessions[session_name] = {"fake_a"}
            tmux.fail_open = True
            agent = {"id": "fake_a", "role": "Worker fake_a"}
            aggregator = runtime._ghostty_workspace_aggregator_name(session_name)
            linked = runtime._ghostty_display_session_name(session_name, "fake_a")

            with (
                patch("team_agent.runtime._ghostty_app_exists", return_value=True), patch("team_agent.display.workspace.ghostty_app_exists", return_value=True), patch("team_agent.display.worker_window.ghostty_app_exists", return_value=True),
                patch("team_agent.display.worker_window.ghostty_pids_by_title", return_value=[]),
                patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
            ):
                displays = runtime._open_worker_displays(
                    workspace,
                    session_name,
                    [("fake_a", agent)],
                    runtime.EventLog(workspace),
                    "ghostty_workspace",
                )

            self.assertEqual(displays["fake_a"]["status"], "blocked")
            self.assertNotIn(aggregator, tmux.sessions)
            self.assertNotIn(linked, tmux.sessions)
            self.assertIn(session_name, tmux.sessions)
            self.assertTrue(any(e["event"] == "display.ghostty_workspace_blocked" for e in _events(workspace)))

    @unittest.skip(
        "Gap 15 base slice required: coordinator setup midway failure must clean pid/meta; "
        "see team-agent-unattended-runtime-gaps.md Gap 15"
    )
    def test_coordinator_setup_failure_after_restart_cleans_dead_pid_metadata(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coord-rollback-") as tmp:
            workspace = Path(tmp)
            _write_restart_workspace(workspace, ["fake_a"])
            tmux = FakeTmux()
            killed: list[int] = []

            def failing_coordinator(ws: Path) -> dict[str, Any]:
                runtime.coordinator_pid_path(ws).write_text("77777", encoding="utf-8")
                runtime.coordinator_meta_path(ws).write_text('{"pid": 77777}', encoding="utf-8")
                raise OSError("coordinator metadata write failed")

            with (
                patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
                patch("team_agent.runtime._ensure_agent_start_requirements", return_value=None),
                patch("team_agent.runtime._handle_startup_prompts_and_verify_window", return_value=True),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
                patch("team_agent.runtime.start_coordinator", side_effect=failing_coordinator),
                patch("team_agent.runtime.os.kill", side_effect=lambda pid, _sig: killed.append(pid)),
            ):
                with self.assertRaises((TeamAgentRuntimeError, OSError)):
                    runtime.restart(workspace)

            self.assertFalse(runtime.coordinator_pid_path(workspace).exists())
            self.assertFalse(runtime.coordinator_meta_path(workspace).exists())
            self.assertIn(77777, killed)
            self.assertNotIn("coordinator", load_runtime_state(workspace))

    @unittest.skip(
        "Gap 15 base slice required: rollback cleanup must emit rollback_failed event with leaked resource id; "
        "see team-agent-unattended-runtime-gaps.md Gap 15"
    )
    def test_cleanup_failure_during_restart_rollback_reports_leaked_resource_id(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rollback-fail-") as tmp:
            workspace = Path(tmp)
            _write_restart_workspace(workspace, ["fake_a", "fake_b"])
            tmux = FakeTmux()
            tmux.fail_new_window_names.add("fake_b")
            tmux.fail_kill_sessions.add("team-lifecycle-failure")

            with (
                patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
                patch("team_agent.runtime._ensure_agent_start_requirements", return_value=None),
                patch("team_agent.runtime._handle_startup_prompts_and_verify_window", return_value=True),
                patch("team_agent.runtime._capture_agent_session", return_value=None),
            ):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime.restart(workspace)

            self.assertIn("rollback_failed", str(ctx.exception))
            self.assertIn("team-lifecycle-failure", str(ctx.exception))
            self.assertTrue(
                any(
                    e["event"] == "restart.rollback_failed"
                    and e.get("resource_id") == "team-lifecycle-failure"
                    for e in _events(workspace)
                )
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
