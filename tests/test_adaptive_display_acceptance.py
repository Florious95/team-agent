from __future__ import annotations

import copy
import json
import os
import tempfile
import time
import unittest
from collections import Counter
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import display, runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.events import EventLog
from team_agent.simple_yaml import dumps
from team_agent.state import load_runtime_state, save_runtime_state


FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "adaptive_display"
ADAPTIVE_REASONS = {
    "leader_not_in_tmux",
    "split_failed",
    "window_create_failed",
    "worker_session_missing",
    "not_implemented_this_platform",
    "aggregator_rebuild_failed",
}


class AdaptiveDisplayAcceptanceTests(unittest.TestCase):
    """Gap 41 adaptive display contract.

    These tests use S0 real layout fixtures as the reference shape, then exercise
    only fake tmux/probe layers. Real terminal visibility remains Mac mini E2E.
    """

    def test_1_adaptive_open_appends_tagged_332_windows_without_touching_leader_or_launching_gui(self) -> None:
        fixture = _leader_fixture()
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-open-") as tmp:
            workspace = Path(tmp)
            tmux = FakeTmux(fixture)
            with _patched_adaptive_open(tmux, fixture) as ghostty_open:
                displays = display.open_worker_displays(
                    workspace,
                    "team-adaptive",
                    _jobs(8),
                    EventLog(workspace),
                    display_backend="adaptive",
                )

            self.assertEqual(ghostty_open.call_count, 0, "adaptive display must not use Ghostty window launch")
            self.assertEqual(sorted(_window_counts(displays).values()), _plain_332_counts())
            self.assertEqual(
                set(_window_counts(displays)),
                {
                    "team-agent:team-adaptive:overview",
                    "team-agent:team-adaptive:overview-2",
                    "team-agent:team-adaptive:overview-3",
                },
            )
            self.assertTrue(all(item.get("backend") == "adaptive" for item in displays.values()), displays)
            self.assertTrue(all(item.get("status") == "opened" for item in displays.values()), displays)
            self.assertFalse(tmux.gui_launched, tmux.calls)
            self.assertFalse(tmux.leader_pane_was_mutated, tmux.calls)

    def test_2_adaptive_teardown_removes_only_team_scoped_windows_in_shared_leader_session(self) -> None:
        fixture = _leader_fixture()
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-close-") as tmp:
            workspace = Path(tmp)
            state = _adaptive_state(workspace, fixture, _jobs(4))
            save_runtime_state(workspace, state)
            tmux = FakeTmux(fixture, adaptive_windows=["team-agent:team-adaptive:overview", "team-agent:team-adaptive:overview-2"])

            with _patched_shutdown(tmux):
                result = runtime.shutdown(workspace)

            self.assertTrue(result["ok"], result)
            killed_windows = [call[-1] for call in tmux.calls if call[:3] == ["tmux", "kill-window", "-t"]]
            self.assertEqual(
                killed_windows,
                [
                    f"{fixture['leader_receiver']['session_name']}:team-agent:team-adaptive:overview",
                    f"{fixture['leader_receiver']['session_name']}:team-agent:team-adaptive:overview-2",
                ],
            )
            self.assertNotIn(f"{fixture['leader_receiver']['session_name']}:notes", killed_windows)
            self.assertNotIn(fixture["leader_receiver"]["session_name"], killed_windows)

    def test_3_adaptive_view_mirrors_workers_without_reparenting_worker_processes(self) -> None:
        fixture = _leader_fixture()
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-topology-") as tmp:
            workspace = Path(tmp)
            tmux = FakeTmux(fixture)
            with _patched_adaptive_open(tmux, fixture):
                displays = display.open_worker_displays(
                    workspace,
                    "team-adaptive",
                    _jobs(3),
                    EventLog(workspace),
                    display_backend="adaptive",
                )

            self.assertEqual(
                {item.get("target_worker_session") for item in displays.values()},
                {"team-adaptive:worker_1", "team-adaptive:worker_2", "team-adaptive:worker_3"},
            )
            pane_commands = [call[-1] for call in tmux.calls if call[:2] in (["tmux", "new-window"], ["tmux", "split-window"])]
            self.assertTrue(pane_commands, tmux.calls)
            self.assertTrue(all("tmux attach-session -t" in command for command in pane_commands), pane_commands)
            self.assertFalse(any("sh -lc" in command for command in pane_commands), pane_commands)
            leader_session = fixture["leader_receiver"]["session_name"]
            self.assertFalse(
                any(call[:2] == ["tmux", "new-window"] and call[call.index("-n") + 1] in {"worker_1", "worker_2", "worker_3"} for call in tmux.calls if "-n" in call),
                f"worker windows must not be created directly in leader session {leader_session}: {tmux.calls}",
            )

    def test_4_leader_not_in_tmux_degrades_to_headless_hint_and_structured_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-no-tmux-") as tmp:
            workspace = Path(tmp)
            tmux = FakeTmux(_leader_fixture())
            with _patched_adaptive_open(tmux, _leader_fixture(), in_tmux=False) as ghostty_open:
                displays = display.open_worker_displays(
                    workspace,
                    "team-adaptive",
                    _jobs(2),
                    EventLog(workspace),
                    display_backend="adaptive",
                )

            self.assertEqual(ghostty_open.call_count, 0, "non-tmux leader must not silently fall back to Ghostty")
            self.assertEqual(set(displays), {"worker_1", "worker_2"})
            for item in displays.values():
                self.assertEqual(item.get("backend"), "adaptive")
                self.assertEqual(item.get("status"), "blocked")
                self.assertEqual(item.get("reason"), "leader_not_in_tmux")
                self.assertEqual(item.get("fallback"), "tmux_headless")
                self.assertIn("tmux", str(item.get("hint", "")).lower())
            blocked = _events_named(workspace, "display.adaptive_blocked")
            self.assertEqual(len(blocked), 2, _events(workspace))
            self.assertTrue(all(event.get("reason") == "leader_not_in_tmux" for event in blocked))
            self.assertFalse(tmux.gui_launched, tmux.calls)

    def test_5_omitted_display_backend_launch_resolves_to_adaptive_with_audit_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-default-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_spec(workspace, worker_count=2, display_backend=None)
            opened: dict[str, Any] = {}

            def fake_open(workspace_arg, session_name, jobs, event_log, display_backend):
                opened["display_backend"] = display_backend
                return {agent_id: {"backend": display_backend, "status": "opened"} for agent_id, _agent in jobs}

            with _patched_launch(FakeTmux(_leader_fixture()), open_worker_displays=fake_open):
                result = _result_or_error(runtime.launch, spec_path, auto_approve=True)

            self.assertTrue(result.get("ok"), result)
            state = load_runtime_state(workspace)
            self.assertEqual(state.get("display_backend"), "adaptive")
            self.assertEqual(opened.get("display_backend"), "adaptive")
            resolved = _events_named(workspace, "display.backend_resolved")
            self.assertEqual(len(resolved), 1, _events(workspace))
            self.assertIsNone(resolved[0].get("requested"))
            self.assertEqual(resolved[0].get("resolved"), "adaptive")
            self.assertEqual(resolved[0].get("reason"), "default")

    def test_6_running_team_status_does_not_hot_swap_recorded_backend_when_default_changes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-no-hotswap-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-existing",
                "display_backend": "ghostty_workspace",
                "agents": {
                    "worker_1": {
                        "status": "running",
                        "provider": "fake",
                        "window": "worker_1",
                        "session_id": "session-worker_1",
                        "display": {"backend": "ghostty_workspace", "status": "opened"},
                    }
                },
                "tasks": [],
            }
            save_runtime_state(workspace, state)

            with patch("team_agent.runtime._tmux_session_exists", return_value=True):
                status = runtime.status(workspace, as_json=True)

            self.assertEqual(status["agents"]["worker_1"]["display"]["backend"], "ghostty_workspace")
            self.assertEqual(load_runtime_state(workspace)["display_backend"], "ghostty_workspace")
            self.assertEqual(_events_named(workspace, "display.backend_resolved"), [])

    def test_7_shutdown_dispatches_on_recorded_ghostty_backend_not_current_adaptive_default(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-close-recorded-") as tmp:
            workspace = Path(tmp)
            session_name = "team-recorded-ghostty"
            aggregator = display.ghostty_workspace_aggregator_name(session_name)
            linked = display.ghostty_display_session_name(session_name, "worker_1")
            save_runtime_state(
                workspace,
                {
                    "session_name": session_name,
                    "display_backend": "ghostty_workspace",
                    "agents": {
                        "worker_1": {
                            "status": "running",
                            "provider": "fake",
                            "window": "worker_1",
                            "session_id": "session-worker_1",
                            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / "worker_1.json"),
                            "display": {
                                "backend": "ghostty_workspace",
                                "aggregator_session": aggregator,
                                "display_session": aggregator,
                                "linked_session": linked,
                                "title": "team-agent:team-recorded-ghostty:workspace",
                                "pids": [],
                            },
                        }
                    },
                    "tasks": [],
                },
            )
            tmux = FakeTmux(_leader_fixture(), sessions={session_name, aggregator, linked})

            with _patched_shutdown(tmux):
                result = runtime.shutdown(workspace)

            self.assertTrue(result["ok"], result)
            kill_sessions = [call[-1] for call in tmux.calls if call[:3] == ["tmux", "kill-session", "-t"]]
            self.assertIn(aggregator, kill_sessions)
            self.assertIn(linked, kill_sessions)
            self.assertIn(session_name, kill_sessions)
            self.assertEqual(len(_events_named(workspace, "display.ghostty_workspace_closed")), 1)

    def test_8_332_tiling_parity_matches_real_plain_tmux_fixture_for_ghostty_workspace_and_adaptive(self) -> None:
        expected = _plain_332_counts()
        self.assertEqual(expected, [2, 3, 3], "S0 plain-tmux fixture must anchor the 8-worker 3+3+2 shape")
        for count in range(1, 9):
            with self.subTest(worker_count=count), tempfile.TemporaryDirectory(prefix="team-agent-adaptive-parity-") as tmp:
                workspace = Path(tmp)
                ghostty_counts = _open_backend_counts(workspace, "ghostty_workspace", count)
                adaptive_counts = _open_backend_counts(workspace, "adaptive", count)
                self.assertEqual(adaptive_counts, ghostty_counts)
                if count == 8:
                    self.assertEqual(adaptive_counts, expected)

    def test_9_restart_rebuilds_adaptive_view_after_leader_claim_rebind(self) -> None:
        fixture = _leader_fixture()
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-restart-") as tmp:
            workspace = Path(tmp)
            spec = _write_restart_workspace(workspace, worker_count=3)
            tmux = FakeTmux(fixture, stale_adaptive=True)

            def fake_rebind(workspace_arg, leader_provider, source):
                EventLog(workspace_arg).write(
                    "leader_receiver.rebind_applied",
                    source=source,
                    provider=leader_provider,
                    new_session_name=fixture["leader_receiver"]["session_name"],
                    new_pane_id=fixture["leader_receiver"]["pane_id"],
                )
                return {"ok": True}

            with _patched_restart(tmux, spec), patch("team_agent.leader.autobind_leader_receiver_from_env", side_effect=fake_rebind):
                result = _result_or_error(runtime.restart, workspace)

            self.assertTrue(result.get("ok"), result)
            events = _events(workspace)
            names = [event["event"] for event in events]
            self.assertIn("leader_receiver.rebind_applied", names, events)
            self.assertIn("display.adaptive_rebuilt", names, events)
            self.assertLess(names.index("leader_receiver.rebind_applied"), names.index("display.adaptive_rebuilt"), events)
            rebuilt = _events_named(workspace, "display.adaptive_rebuilt")[-1]
            self.assertEqual(rebuilt.get("leader_session"), fixture["leader_receiver"]["session_name"])
            self.assertTrue(rebuilt.get("stale_windows_recreated"), rebuilt)

    def test_10_capability_probe_drives_platform_fallback_without_darwin_hardcode(self) -> None:
        probe_fn = getattr(display, "probe_display_capabilities", None)
        self.assertTrue(callable(probe_fn), "display.probe_display_capabilities(env, platform, tmux) is the public probe contract")
        probe = probe_fn(env={}, platform="win32", tmux=FakeTmux(_leader_fixture()))
        self.assertFalse(probe["in_tmux"])
        self.assertEqual(probe["platform"], "win32")
        self.assertEqual(probe["adaptive_status"], "not_implemented_this_platform")

        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-platform-") as tmp:
            workspace = Path(tmp)
            displays = _result_or_error(
                display.open_worker_displays,
                workspace,
                "team-adaptive",
                _jobs(1),
                EventLog(workspace),
                display_backend="adaptive",
                capability_probe=probe,
            )
        self.assertNotIn("exception_type", displays, displays)
        only = displays["worker_1"]
        self.assertEqual(only.get("status"), "blocked")
        self.assertEqual(only.get("reason"), "not_implemented_this_platform")
        self.assertEqual(only.get("fallback"), "tmux_headless")

    def test_11_adaptive_failures_emit_closed_reason_enum_and_never_degrade_silently(self) -> None:
        for reason in sorted(ADAPTIVE_REASONS - {"leader_not_in_tmux", "not_implemented_this_platform"}):
            with self.subTest(reason=reason), tempfile.TemporaryDirectory(prefix=f"team-agent-adaptive-{reason}-") as tmp:
                workspace = Path(tmp)
                fixture = _leader_fixture()
                tmux = FakeTmux(fixture, fail_reason=reason)
                with _patched_adaptive_open(tmux, fixture):
                    displays = display.open_worker_displays(
                        workspace,
                        "team-adaptive",
                        _jobs(2),
                        EventLog(workspace),
                        display_backend="adaptive",
                    )

                self.assertTrue(displays, reason)
                self.assertTrue(all(item.get("status") == "blocked" for item in displays.values()), displays)
                self.assertTrue(all(item.get("reason") == reason for item in displays.values()), displays)
                self.assertTrue(all(item.get("fallback") == "tmux_headless" for item in displays.values()), displays)
                blocked = _events_named(workspace, "display.adaptive_blocked")
                self.assertGreaterEqual(len(blocked), 1, _events(workspace))
                self.assertTrue(all(event.get("reason") in ADAPTIVE_REASONS for event in blocked), blocked)

    def test_12_display_failure_does_not_block_team_readiness_or_two_second_startup_path(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-adaptive-readiness-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_spec(workspace, worker_count=2, display_backend="adaptive")
            blocked_display = {
                "backend": "adaptive",
                "status": "blocked",
                "reason": "split_failed",
                "fallback": "tmux_headless",
                "hint": "Team is running headless; attach to tmux manually.",
            }

            def fake_open(workspace_arg, session_name, jobs, event_log, display_backend):
                for agent_id, _agent in jobs:
                    event_log.write("display.adaptive_blocked", worker_id=agent_id, **blocked_display)
                return {agent_id: dict(blocked_display) for agent_id, _agent in jobs}

            start = time.monotonic()
            with _patched_launch(FakeTmux(_leader_fixture()), open_worker_displays=fake_open):
                result = _result_or_error(runtime.launch, spec_path, auto_approve=True)
            elapsed = time.monotonic() - start

            self.assertTrue(result.get("ok"), result)
            self.assertLess(elapsed, 2.0)
            state = load_runtime_state(workspace)
            self.assertEqual(sorted(state["agents"]), ["worker_1", "worker_2"])
            self.assertTrue(all(agent["status"] == "running" for agent in state["agents"].values()))
            self.assertTrue(all(agent.get("display", {}).get("reason") == "split_failed" for agent in state["agents"].values()))
            self.assertEqual(len(_events_named(workspace, "display.adaptive_blocked")), 2, _events(workspace))


class FakeTmux:
    def __init__(
        self,
        leader_fixture: dict[str, Any],
        *,
        adaptive_windows: list[str] | None = None,
        sessions: set[str] | None = None,
        stale_adaptive: bool = False,
        fail_reason: str | None = None,
    ) -> None:
        leader = leader_fixture["leader_receiver"]
        self.leader_session = leader["session_name"]
        self.leader_window = leader["window_name"]
        self.leader_pane = leader["pane_id"]
        self.fail_reason = fail_reason
        self.stale_adaptive = stale_adaptive
        self.calls: list[list[str]] = []
        self.pane_counter = 9000
        self.sessions = set(sessions or {self.leader_session})
        self.windows: dict[str, list[str]] = {
            self.leader_session: [self.leader_window, "notes"] + list(adaptive_windows or [])
        }
        self.gui_launched = False
        self.leader_pane_was_mutated = False

    def run_cmd(self, args: list[str], timeout: int = 20) -> Mock:
        _ = timeout
        self.calls.append(args)
        self._record_leader_pane_mutation(args)
        proc = Mock(returncode=0, stdout="", stderr="")
        if args and args[0] == "open":
            self.gui_launched = True
        if self.fail_reason == "worker_session_missing" and any("worker_2" in str(arg) for arg in args):
            proc.returncode = 1
            proc.stderr = "worker session missing"
            return proc
        if args[:2] == ["tmux", "display-message"]:
            proc.stdout = f"{self.leader_pane}\t{self.leader_session}\t{self.leader_window}\t1\n"
        elif args[:2] == ["tmux", "has-session"]:
            proc.returncode = 0 if args[-1] in self.sessions else 1
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            session = args[3]
            if session not in self.sessions:
                proc.returncode = 1
                proc.stderr = f"can't find session: {session}"
            else:
                proc.stdout = "\n".join(self.windows.get(session, []))
        elif args[:3] == ["tmux", "capture-pane", "-p"]:
            proc.stdout = ""
        elif args[:3] == ["tmux", "new-session", "-d"]:
            session = args[args.index("-s") + 1]
            window = args[args.index("-n") + 1] if "-n" in args else "0"
            self.sessions.add(session)
            self.windows.setdefault(session, [])
            if window not in self.windows[session]:
                self.windows[session].append(window)
            if "-P" in args:
                proc.stdout = self._pane_id()
        elif args[:2] == ["tmux", "new-window"]:
            if self.fail_reason in {"window_create_failed", "aggregator_rebuild_failed"}:
                proc.returncode = 1
                proc.stderr = self.fail_reason
                return proc
            session = args[args.index("-t") + 1]
            window = args[args.index("-n") + 1]
            self.sessions.add(session)
            self.windows.setdefault(session, [])
            if window not in self.windows[session]:
                self.windows[session].append(window)
            if "-P" in args:
                proc.stdout = self._pane_id()
        elif args[:2] == ["tmux", "split-window"]:
            if self.fail_reason == "split_failed":
                proc.returncode = 1
                proc.stderr = "split failed"
            else:
                proc.stdout = self._pane_id()
        elif args[:3] == ["tmux", "kill-window", "-t"]:
            target = args[3]
            session, _, window = target.partition(":")
            if window in self.windows.get(session, []):
                self.windows[session].remove(window)
        elif args[:3] == ["tmux", "kill-session", "-t"]:
            self.sessions.discard(args[3])
            self.windows.pop(args[3], None)
        return proc

    def _pane_id(self) -> str:
        self.pane_counter += 1
        return f"%{self.pane_counter}\n"

    def _record_leader_pane_mutation(self, args: list[str]) -> None:
        if self.leader_pane not in args:
            return
        if args[:2] in (["tmux", "split-window"], ["tmux", "resize-pane"], ["tmux", "kill-pane"], ["tmux", "respawn-pane"]):
            self.leader_pane_was_mutated = True


class RecordingAdapter:
    provider = "fake"
    command_name = "fake"

    def is_installed(self) -> bool:
        return True

    def mcp_config(self, workspace: Path, agent_id: str) -> dict[str, Any]:
        return {"team_orchestrator": {"command": "fake", "args": [], "env": {"TEAM_AGENT_ID": agent_id}}}

    def install_mcp(self, workspace: Path, agent_id: str, config: dict[str, Any]) -> Path:
        path = workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"mcpServers": config}), encoding="utf-8")
        return path

    def cleanup_mcp(self, workspace: Path, agent_id: str, mcp_path: Path | None = None) -> None:
        _ = workspace, agent_id, mcp_path

    def session_is_resumable(self, agent_state: dict[str, Any], workspace: Path) -> bool:
        _ = workspace
        return bool(agent_state.get("session_id"))

    def recover_session_id(
        self,
        agent_id: str,
        agent_state: dict[str, Any],
        workspace: Path,
        exclude_session_ids: set[str] | None = None,
    ) -> dict[str, Any] | None:
        _ = agent_id, agent_state, workspace, exclude_session_ids
        return None


@contextmanager
def _patched_adaptive_open(tmux: FakeTmux, fixture: dict[str, Any], *, in_tmux: bool = True):
    ghostty_open = Mock(return_value={"backend": "ghostty_window", "status": "opened"})
    env = {"TMUX_PANE": fixture["leader_receiver"]["pane_id"]} if in_tmux else {}
    with (
        patch.dict(os.environ, env, clear=not in_tmux),
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
        patch("team_agent.display.worker_window.open_ghostty_worker_window", ghostty_open),
        patch("team_agent.display.worker_window.ghostty_app_exists", return_value=False),
        patch("team_agent.display.workspace.ghostty_app_exists", return_value=False),
    ):
        yield ghostty_open


@contextmanager
def _patched_launch(tmux: FakeTmux, *, open_worker_displays):
    adapter = RecordingAdapter()
    patches = [
        patch("team_agent.runtime.get_adapter", return_value=adapter),
        patch("team_agent.runtime.get_adapter_or_raise", return_value=adapter),
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
        patch("team_agent.runtime._tmux_session_exists", side_effect=lambda name: name in tmux.sessions),
        patch("team_agent.runtime._ensure_agent_start_requirements", return_value=None),
        patch("team_agent.runtime._capture_agent_session", return_value=None),
        patch("team_agent.runtime._open_worker_displays", side_effect=open_worker_displays),
        patch("team_agent.runtime.shell_command_for_agent", side_effect=lambda agent, workspace, mcp: f"fresh:{agent['id']}"),
        patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
    ]
    with ExitStack() as stack:
        for item in patches:
            stack.enter_context(item)
        yield


@contextmanager
def _patched_restart(tmux: FakeTmux, spec: dict[str, Any]):
    adapter = RecordingAdapter()
    patches = [
        patch("team_agent.restart.orchestration.load_spec", return_value=spec),
        patch("team_agent.runtime.get_adapter", return_value=adapter),
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
        patch("team_agent.runtime._ensure_agent_start_requirements", return_value=None),
        patch("team_agent.runtime._handle_startup_prompts_and_verify_window", return_value=True),
        patch("team_agent.runtime._capture_agent_session", return_value=None),
        patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}),
        patch("team_agent.runtime.shell_resume_command_for_agent", side_effect=lambda agent, previous, workspace, mcp: f"resume:{agent['id']}"),
        patch("team_agent.runtime.shell_command_for_agent", side_effect=lambda agent, workspace, mcp: f"fresh:{agent['id']}"),
    ]
    with ExitStack() as stack:
        for item in patches:
            stack.enter_context(item)
        yield


@contextmanager
def _patched_shutdown(tmux: FakeTmux):
    adapter = RecordingAdapter()
    patches = [
        patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
        patch("team_agent.runtime.stop_coordinator", return_value={"ok": True, "status": "stopped"}),
        patch("team_agent.runtime.get_adapter", return_value=adapter),
        patch("team_agent.runtime._tmux_session_exists", side_effect=lambda name: name in tmux.sessions),
        patch("team_agent.runtime._tmux_window_exists", return_value=True),
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
    ]
    with ExitStack() as stack:
        for item in patches:
            stack.enter_context(item)
        yield


def _open_backend_counts(workspace: Path, backend: str, count: int) -> list[int]:
    jobs = _jobs(count)
    fixture = _leader_fixture()
    tmux = FakeTmux(fixture)
    event_log = EventLog(workspace)
    linked = {
        agent_id: {"ok": True, "linked_session": display.ghostty_display_session_name("team-parity", agent_id)}
        for agent_id, _agent in jobs
    }
    patches = [
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
        patch("team_agent.runtime._tmux_session_exists", side_effect=lambda name: name in tmux.sessions),
        patch("team_agent.runtime._tmux_window_exists", return_value=True),
        patch("team_agent.display.workspace.ghostty_app_exists", return_value=True),
        patch("team_agent.display.workspace.ghostty_pids_by_title", return_value=[]),
        patch("team_agent.display.workspace.prepare_ghostty_workspace_linked_sessions", return_value=linked),
    ]
    if backend == "adaptive":
        patches.append(patch("team_agent.display.worker_window.open_ghostty_worker_window", return_value={"backend": "ghostty_window", "status": "opened"}))
    with ExitStack() as stack:
        stack.enter_context(patch.dict(os.environ, {"TMUX_PANE": fixture["leader_receiver"]["pane_id"]}, clear=False))
        for item in patches:
            stack.enter_context(item)
        displays = display.open_worker_displays(workspace, "team-parity", jobs, event_log, display_backend=backend)
    return sorted(_window_counts(displays).values())


def _jobs(count: int) -> list[tuple[str, dict[str, Any]]]:
    return [
        (
            f"worker_{index}",
            {
                "id": f"worker_{index}",
                "role": f"Worker {index}",
                "provider": "fake",
            },
        )
        for index in range(1, count + 1)
    ]


def _adaptive_state(workspace: Path, fixture: dict[str, Any], jobs: list[tuple[str, dict[str, Any]]]) -> dict[str, Any]:
    state = {
        "session_name": "team-adaptive",
        "workspace": str(workspace),
        "display_backend": "adaptive",
        "leader_receiver": fixture["leader_receiver"],
        "agents": {},
        "tasks": [],
    }
    for index, (agent_id, _agent) in enumerate(jobs):
        window = "team-agent:team-adaptive:overview" if index < 3 else "team-agent:team-adaptive:overview-2"
        state["agents"][agent_id] = {
            "status": "running",
            "provider": "fake",
            "window": agent_id,
            "session_id": f"session-{agent_id}",
            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"),
            "display": {
                "backend": "adaptive",
                "status": "opened",
                "leader_session": fixture["leader_receiver"]["session_name"],
                "window": window,
                "pane_id": f"%8{index}",
                "target_worker_session": f"team-adaptive:{agent_id}",
            },
        }
    return state


def _write_spec(workspace: Path, *, worker_count: int, display_backend: str | None) -> Path:
    spec = cli_fake_spec(workspace)
    base = copy.deepcopy(spec["agents"][0])
    spec["team"]["name"] = "adaptive"
    spec["runtime"]["session_name"] = "team-adaptive"
    spec["runtime"]["max_active_agents"] = worker_count
    spec["runtime"]["startup_order"] = [f"worker_{index}" for index in range(1, worker_count + 1)]
    if display_backend is None:
        spec["runtime"].pop("display_backend", None)
    else:
        spec["runtime"]["display_backend"] = display_backend
    spec["leader"]["provider"] = "fake"
    spec["routing"]["default_assignee"] = "worker_1"
    spec["routing"]["rules"] = []
    spec["agents"] = []
    for _agent_id, agent in _jobs(worker_count):
        item = copy.deepcopy(base)
        item.update(agent)
        spec["agents"].append(item)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec_path


def _write_restart_workspace(workspace: Path, *, worker_count: int) -> dict[str, Any]:
    spec_path = _write_spec(workspace, worker_count=worker_count, display_backend="adaptive")
    spec = _spec_from_path(spec_path)
    agents = {}
    for agent_id, _agent in _jobs(worker_count):
        agents[agent_id] = {
            "status": "stopped",
            "provider": "fake",
            "agent_id": agent_id,
            "window": agent_id,
            "session_id": f"session-{agent_id}",
            "first_send_at": "2026-05-27T12:00:00+00:00",
            "spawn_cwd": str(workspace),
            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"),
            "display": {
                "backend": "adaptive",
                "status": "opened",
                "leader_session": "stale-leader-session",
                "window": "team-agent:team-adaptive:overview",
                "pane_id": "%404",
                "target_worker_session": f"team-adaptive:{agent_id}",
            },
        }
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "workspace": str(workspace),
            "session_name": "team-adaptive",
            "display_backend": "adaptive",
            "leader": spec["leader"],
            "leader_receiver": {"session_name": "stale-leader-session", "pane_id": "%404"},
            "agents": agents,
            "tasks": spec["tasks"],
        },
    )
    return spec


def _spec_from_path(spec_path: Path) -> dict[str, Any]:
    from team_agent.simple_yaml import loads

    return loads(spec_path.read_text(encoding="utf-8"))


def _window_counts(displays: dict[str, dict[str, Any]]) -> Counter[str]:
    counts: Counter[str] = Counter()
    for item in displays.values():
        window = item.get("window") or item.get("workspace_window") or item.get("window_name")
        if window:
            if isinstance(window, str) and window.startswith("overview"):
                window = f"team-agent:team-parity:{window}"
            counts[str(window)] += 1
    return counts


def _plain_332_counts() -> list[int]:
    rows = (FIXTURE_ROOT / "plain_tmux_332" / "windows.tsv").read_text(encoding="utf-8").splitlines()
    return sorted(int(row.split("\t")[3]) for row in rows if row.strip())


def _leader_fixture() -> dict[str, Any]:
    return json.loads((FIXTURE_ROOT / "state" / "leader_worker_relation.selected.json").read_text(encoding="utf-8"))


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def _events_named(workspace: Path, name: str) -> list[dict[str, Any]]:
    return [event for event in _events(workspace) if event.get("event") == name]


def _result_or_error(fn, *args, **kwargs) -> dict[str, Any]:
    try:
        return fn(*args, **kwargs)
    except Exception as exc:
        return {"ok": False, "exception_type": type(exc).__name__, "error": str(exc)}


if __name__ == "__main__":
    unittest.main(verbosity=2)
