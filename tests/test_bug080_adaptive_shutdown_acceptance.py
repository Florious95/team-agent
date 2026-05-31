from __future__ import annotations

import copy
import json
import tempfile
import unittest
from contextlib import ExitStack, contextmanager
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.state import save_runtime_state


FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "bug_080_adaptive_shutdown"


class Bug080AdaptiveShutdownAcceptanceTests(unittest.TestCase):
    def test_shutdown_falls_back_to_real_tmux_names_when_adaptive_display_ids_are_null(self) -> None:
        fixture = _fixture_state()
        session_name = fixture["session_name"]
        leader_session = fixture["leader_receiver"]["session_name"]
        expected_display_sessions = _real_orphan_display_sessions()
        expected_overview = f"{leader_session}:team-agent:{session_name}:overview"

        with tempfile.TemporaryDirectory(prefix="team-agent-bug080-fallback-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, _workspace_state_from_fixture(fixture))
            tmux = FixtureTmux.from_real_before_shutdown()

            with _patched_shutdown(tmux):
                result = runtime.shutdown(workspace, keep_logs=True, team="current")

            self.assertTrue(result["ok"], result)
            self.assertEqual(tmux.kill_windows, [expected_overview])
            self.assertEqual(sorted(tmux.kill_sessions), sorted([session_name, *expected_display_sessions]))
            self.assertIn(leader_session, tmux.sessions, "shutdown must never kill the leader session")
            self.assertNotIn(expected_overview.split(":", 1)[1], tmux.windows.get(leader_session, []))
            for display_session in expected_display_sessions:
                self.assertNotIn(display_session, tmux.sessions)
            closed = _events_named(workspace, "display.adaptive_closed")
            self.assertTrue(closed, _events(workspace))
            self.assertIn(expected_overview, closed[-1].get("windows", []))
            self.assertEqual(sorted(closed[-1].get("linked_sessions", [])), expected_display_sessions)

    def test_shutdown_reports_warning_when_real_named_adaptive_orphans_remain(self) -> None:
        fixture = _fixture_state()
        session_name = fixture["session_name"]
        leader_session = fixture["leader_receiver"]["session_name"]
        expected_display_sessions = _real_orphan_display_sessions()
        expected_overview = f"{leader_session}:team-agent:{session_name}:overview"

        with tempfile.TemporaryDirectory(prefix="team-agent-bug080-orphans-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, _workspace_state_from_fixture(fixture))
            tmux = FixtureTmux.from_real_before_shutdown(fail_adaptive_cleanup=True)

            with _patched_shutdown(tmux):
                result = runtime.shutdown(workspace, keep_logs=True, team="current")

            self.assertTrue(result["ok"], result)
            self.assertNotEqual(result.get("cleanup_mode"), "synchronous_committed", result)
            self.assertEqual(
                result.get("orphans_detected"),
                {
                    "adaptive_display_sessions": expected_display_sessions,
                    "adaptive_overview_windows": [expected_overview],
                },
            )
            warnings = _events_named(workspace, "shutdown.orphans_detected")
            self.assertTrue(warnings, _events(workspace))
            self.assertEqual(warnings[-1].get("adaptive_display_sessions"), expected_display_sessions)
            self.assertEqual(warnings[-1].get("adaptive_overview_windows"), [expected_overview])

    def test_shutdown_does_not_overclean_when_real_adaptive_display_objects_are_already_closed(self) -> None:
        fixture = _fixture_state()
        session_name = fixture["session_name"]
        leader_session = fixture["leader_receiver"]["session_name"]

        with tempfile.TemporaryDirectory(prefix="team-agent-bug080-clean-") as tmp:
            workspace = Path(tmp)
            clean_state = _workspace_state_from_fixture(fixture)
            for agent_id, display_state in fixture["opened_display_by_agent"].items():
                clean_state["teams"]["current"]["agents"][agent_id]["display"] = copy.deepcopy(display_state)
            save_runtime_state(workspace, clean_state)
            tmux = FixtureTmux.from_real_before_shutdown(with_adaptive_objects=False)

            with _patched_shutdown(tmux):
                result = runtime.shutdown(workspace, keep_logs=True, team="current")

            self.assertTrue(result["ok"], result)
            self.assertNotIn("orphans_detected", result)
            self.assertEqual(tmux.kill_sessions, [session_name])
            self.assertNotIn(leader_session, tmux.kill_sessions, "leader session is not display-owned")
            self.assertEqual(tmux.kill_windows, [])
            self.assertIn(leader_session, tmux.sessions)
            self.assertEqual(tmux.windows.get(leader_session), ["leader"])
            self.assertEqual(_events_named(workspace, "shutdown.orphans_detected"), [])


class FixtureTmux:
    def __init__(
        self,
        sessions: set[str],
        windows: dict[str, list[str]],
        *,
        fail_adaptive_cleanup: bool = False,
    ) -> None:
        self.sessions = set(sessions)
        self.windows = copy.deepcopy(windows)
        self.fail_adaptive_cleanup = fail_adaptive_cleanup
        self.calls: list[list[str]] = []
        self.kill_windows: list[str] = []
        self.kill_sessions: list[str] = []

    @classmethod
    def from_real_before_shutdown(
        cls,
        *,
        with_adaptive_objects: bool = True,
        fail_adaptive_cleanup: bool = False,
    ) -> "FixtureTmux":
        sessions = _parse_sessions(FIXTURE_ROOT / "r1-tmux-before-shutdown.txt")
        windows = _parse_windows(FIXTURE_ROOT / "r1-tmux-windows-before-shutdown.txt")
        if not with_adaptive_objects:
            fixture = _fixture_state()
            session_name = fixture["session_name"]
            leader_session = fixture["leader_receiver"]["session_name"]
            sessions = {session_name, leader_session}
            windows = {session_name: ["alpha", "beta"], leader_session: ["leader"]}
        return cls(sessions, windows, fail_adaptive_cleanup=fail_adaptive_cleanup)

    def run_cmd(self, args: list[str], timeout: int = 20) -> Mock:
        _ = timeout
        self.calls.append(args)
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:2] == ["tmux", "has-session"]:
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
        elif args[:3] == ["tmux", "kill-window", "-t"]:
            target = args[3]
            session, _, window = target.partition(":")
            if self.fail_adaptive_cleanup and window.startswith("team-agent:"):
                proc.returncode = 1
                proc.stderr = f"failed to kill {target}"
                return proc
            if window not in self.windows.get(session, []):
                proc.returncode = 1
                proc.stderr = f"can't find window: {target}"
                return proc
            self.windows[session].remove(window)
            self.kill_windows.append(target)
        elif args[:3] == ["tmux", "kill-session", "-t"]:
            session = args[3]
            if self.fail_adaptive_cleanup and "__display__" in session:
                proc.returncode = 1
                proc.stderr = f"failed to kill {session}"
                return proc
            if session not in self.sessions:
                proc.returncode = 1
                proc.stderr = f"can't find session: {session}"
                return proc
            self.sessions.remove(session)
            self.windows.pop(session, None)
            self.kill_sessions.append(session)
        return proc


class RecordingAdapter:
    provider = "fake"

    def cleanup_mcp(self, workspace: Path, agent_id: str, mcp_path: Path | None = None) -> None:
        _ = workspace, agent_id, mcp_path


@contextmanager
def _patched_shutdown(tmux: FixtureTmux):
    patches = [
        patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
        patch("team_agent.runtime.stop_coordinator", return_value={"ok": True, "status": "stopped"}),
        patch("team_agent.runtime.get_adapter", return_value=RecordingAdapter()),
        patch("team_agent.runtime._tmux_session_exists", side_effect=lambda name: name in tmux.sessions),
        patch("team_agent.runtime._tmux_window_exists", side_effect=lambda session, window: window in tmux.windows.get(session, [])),
        patch("team_agent.runtime.run_cmd", side_effect=tmux.run_cmd),
    ]
    with ExitStack() as stack:
        for item in patches:
            stack.enter_context(item)
        yield


def _workspace_state_from_fixture(fixture: dict) -> dict:
    team_state = {
        "session_name": fixture["session_name"],
        "display_backend": fixture["display_backend"],
        "leader_receiver": copy.deepcopy(fixture["leader_receiver"]),
        "agents": copy.deepcopy(fixture["agents"]),
        "tasks": [],
        "team_dir": ".team/current",
        "spec_path": ".team/current/team.spec.yaml",
    }
    return {
        "session_name": fixture["session_name"],
        "active_team_key": "current",
        "display_backend": fixture["display_backend"],
        "leader_receiver": copy.deepcopy(fixture["leader_receiver"]),
        "agents": copy.deepcopy(fixture["agents"]),
        "tasks": [],
        "teams": {"current": team_state},
    }


def _fixture_state() -> dict:
    return json.loads((FIXTURE_ROOT / "r1-state-selected.json").read_text(encoding="utf-8"))


def _real_orphan_display_sessions() -> list[str]:
    verdict = json.loads((FIXTURE_ROOT / "r1-verdict.json").read_text(encoding="utf-8"))
    return sorted(row.split(":", 1)[0] for row in verdict["orphan_display_sessions"])


def _parse_sessions(path: Path) -> set[str]:
    sessions: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            sessions.add(line.split(": ", 1)[0])
    return sessions


def _parse_windows(path: Path) -> dict[str, list[str]]:
    windows: dict[str, list[str]] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        session, rest = line.split(":", 1)
        window, _sep, _pane_count = rest.rpartition(":")
        windows.setdefault(session, []).append(window)
    return windows


def _events(workspace: Path) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def _events_named(workspace: Path, name: str) -> list[dict]:
    return [event for event in _events(workspace) if event.get("event") == name]


if __name__ == "__main__":
    unittest.main(verbosity=2)
