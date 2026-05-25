from __future__ import annotations

import inspect
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import display, runtime
from team_agent.events import EventLog


class DisplayBoundaryTests(unittest.TestCase):
    """Pin runtime.py <-> display/ contract by exercising actual behavior, not
    just symbol presence. The 0a36ad9 incident proved that identity-equal
    aliases can stay green while the implementation module is missing on
    disk; these tests call the helpers so a half-landed extraction fails."""

    def test_runtime_aliases_resolve_to_display_module(self) -> None:
        pairs = [
            (runtime._open_worker_displays, display.open_worker_displays),
            (runtime._open_ghostty_worker_window, display.open_ghostty_worker_window),
            (runtime._open_ghostty_workspace, display.open_ghostty_workspace),
            (runtime._open_ghostty_workspace_agent_display, display.open_ghostty_workspace_agent_display),
            (runtime._close_ghostty_display, display.close_ghostty_display),
            (runtime._close_ghostty_workspace, display.close_ghostty_workspace),
            (runtime._ghostty_command, display.ghostty_command),
            (runtime._ghostty_app_exists, display.ghostty_app_exists),
            (runtime._ghostty_pids_by_title, display.ghostty_pids_by_title),
            (runtime._ghostty_attach_args, display.ghostty_attach_args),
            (runtime._ghostty_display_session_name, display.ghostty_display_session_name),
            (runtime._prepare_ghostty_display_session, display.prepare_ghostty_display_session),
            (runtime._ghostty_workspace_aggregator_name, display.ghostty_workspace_aggregator_name),
            (runtime._ghostty_workspace_window_name, display.ghostty_workspace_window_name),
            (runtime._ghostty_workspace_pane_command, display.ghostty_workspace_pane_command),
            (runtime._ghostty_workspace_pane_title, display.ghostty_workspace_pane_title),
            (runtime._ghostty_workspace_blocked, display.ghostty_workspace_blocked),
            (runtime._ghostty_workspace_partial_update_display, display.ghostty_workspace_partial_update_display),
            (runtime._prepare_ghostty_workspace_linked_sessions, display.prepare_ghostty_workspace_linked_sessions),
            (runtime._prepare_ghostty_workspace_aggregator, display.prepare_ghostty_workspace_aggregator),
            (runtime._set_ghostty_workspace_pane_title, display.set_ghostty_workspace_pane_title),
            (runtime._kill_ghostty_workspace_linked_sessions, display.kill_ghostty_workspace_linked_sessions),
        ]
        for rt_attr, disp_attr in pairs:
            self.assertIs(rt_attr, disp_attr, f"{rt_attr.__name__} alias drift")
        self.assertEqual(runtime.GHOSTTY_WORKSPACE_PANES_PER_WINDOW, display.GHOSTTY_WORKSPACE_PANES_PER_WINDOW)

    def test_display_helpers_have_explicit_signatures(self) -> None:
        for fn in (
            display.open_worker_displays,
            display.open_ghostty_worker_window,
            display.open_ghostty_workspace,
            display.open_ghostty_workspace_agent_display,
            display.close_ghostty_display,
            display.close_ghostty_workspace,
            display.prepare_ghostty_workspace_aggregator,
            display.prepare_ghostty_workspace_linked_sessions,
            display.kill_ghostty_workspace_linked_sessions,
        ):
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{fn.__name__} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{fn.__name__} uses **kwargs")

    def test_display_modules_do_not_top_level_import_runtime(self) -> None:
        # Anti-cycle rule: top-level `from team_agent.runtime` or `import
        # team_agent.runtime` would deadlock the module-load graph. Lazy
        # imports inside function bodies are fine; they only resolve at call
        # time when runtime is already loaded.
        for module_name in (
            "team_agent.display.ghostty",
            "team_agent.display.worker_window",
            "team_agent.display.workspace",
            "team_agent.display.close",
            "team_agent.display",
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


class GhosttyDisplaySessionNameTests(unittest.TestCase):
    def test_session_name_is_stable_and_collision_resistant(self) -> None:
        a = display.ghostty_display_session_name("team-foo", "worker_a")
        b = display.ghostty_display_session_name("team-foo", "worker_a")
        c = display.ghostty_display_session_name("team-foo", "worker_b")
        self.assertEqual(a, b)
        self.assertNotEqual(a, c)
        self.assertIn("display", a)
        self.assertTrue(a.startswith("team-foo__display__worker_a__"))

    def test_session_name_sanitizes_unsafe_characters(self) -> None:
        name = display.ghostty_display_session_name("team/with bad:chars", "agent$#")
        self.assertNotIn(":", name)
        self.assertNotIn(" ", name)
        self.assertNotIn("/", name)


class GhosttyWorkspaceWindowNameTests(unittest.TestCase):
    def test_first_window_is_overview_then_indexed(self) -> None:
        self.assertEqual(display.ghostty_workspace_window_name(0), "overview")
        self.assertEqual(display.ghostty_workspace_window_name(1), "overview-2")
        self.assertEqual(display.ghostty_workspace_window_name(2), "overview-3")


class GhosttyWorkspacePaneTitleTests(unittest.TestCase):
    def test_pane_title_uses_team_agent_prefix(self) -> None:
        title = display.ghostty_workspace_pane_title({"id": "alpha", "role": "engineer"})
        self.assertEqual(title, "team-agent:alpha:engineer")

    def test_pane_title_tolerates_missing_role(self) -> None:
        title = display.ghostty_workspace_pane_title({"id": "alpha"})
        self.assertEqual(title, "team-agent:alpha:")


class GhosttyAttachArgsTests(unittest.TestCase):
    def test_attach_args_invoke_ghostty_open_with_tmux_attach(self) -> None:
        args = display.ghostty_attach_args("display-session", "team-agent:alpha")
        self.assertEqual(args[0], "open")
        self.assertIn("Ghostty.app", args)
        self.assertEqual(args[-3:], ["attach-session", "-t", "display-session"])
        self.assertIn("--title=team-agent:alpha", args)


class GhosttyWorkspaceBlockedEventTests(unittest.TestCase):
    def test_blocked_emits_event_per_agent_and_returns_blocked_status(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-display-blocked-") as tmp:
            workspace = Path(tmp)
            event_log = EventLog(workspace)
            displays = display.ghostty_workspace_blocked(
                [("alpha", {"id": "alpha"}), ("beta", {"id": "beta"})],
                event_log,
                "ghostty_app_missing",
                target="team-test:alpha",
            )
        self.assertEqual(set(displays), {"alpha", "beta"})
        for agent_id, agent_display in displays.items():
            self.assertEqual(agent_display["backend"], "ghostty_workspace")
            self.assertEqual(agent_display["status"], "blocked")
            self.assertEqual(agent_display["reason"], "ghostty_app_missing")
            self.assertEqual(agent_display["fallback"], "tmux_headless")


class OpenWorkerDisplaysOrchestrationTests(unittest.TestCase):
    def test_empty_job_list_returns_empty_mapping(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-display-empty-") as tmp:
            result = display.open_worker_displays(Path(tmp), "team-x", [], EventLog(Path(tmp)))
        self.assertEqual(result, {})

    def test_workspace_backend_routes_to_open_ghostty_workspace(self) -> None:
        captured: dict = {}

        def fake_workspace(workspace, session_name, jobs, event_log):
            captured["jobs"] = jobs
            captured["session_name"] = session_name
            return {agent_id: {"backend": "ghostty_workspace", "status": "fake"} for agent_id, _ in jobs}

        with tempfile.TemporaryDirectory(prefix="team-agent-display-ws-route-") as tmp:
            with patch("team_agent.display.worker_window.open_ghostty_workspace", side_effect=fake_workspace):
                result = display.open_worker_displays(
                    Path(tmp),
                    "team-x",
                    [("alpha", {"id": "alpha"}), ("beta", {"id": "beta"})],
                    EventLog(Path(tmp)),
                    display_backend="ghostty_workspace",
                )
        self.assertEqual(set(result), {"alpha", "beta"})
        self.assertEqual(captured["session_name"], "team-x")
        self.assertEqual({agent_id for agent_id, _ in captured["jobs"]}, {"alpha", "beta"})

    def test_window_backend_single_job_calls_worker_window_path(self) -> None:
        marker = {"called": False}

        def fake_open(workspace, session_name, window_name, agent, event_log):
            marker["called"] = True
            marker["window_name"] = window_name
            return {"backend": "ghostty_window", "status": "opened"}

        with tempfile.TemporaryDirectory(prefix="team-agent-display-window-route-") as tmp:
            with patch("team_agent.display.worker_window.open_ghostty_worker_window", side_effect=fake_open):
                result = display.open_worker_displays(
                    Path(tmp),
                    "team-x",
                    [("alpha", {"id": "alpha"})],
                    EventLog(Path(tmp)),
                    display_backend="ghostty_window",
                )
        self.assertTrue(marker["called"])
        self.assertEqual(marker["window_name"], "alpha")
        self.assertEqual(result["alpha"]["backend"], "ghostty_window")


class OpenGhosttyWorkerWindowGuardTests(unittest.TestCase):
    def test_missing_ghostty_app_returns_blocked_without_running_open(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-display-guard-") as tmp:
            with patch("team_agent.display.worker_window.ghostty_app_exists", return_value=False), \
                 patch("team_agent.runtime.run_cmd") as mock_run:
                result = display.open_ghostty_worker_window(
                    Path(tmp),
                    "team-x",
                    "alpha",
                    {"id": "alpha", "role": "engineer"},
                    EventLog(Path(tmp)),
                )
            mock_run.assert_not_called()
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["reason"], "ghostty_app_missing")
        self.assertEqual(result["fallback"], "tmux_headless")


class CloseGhosttyDisplayContractTests(unittest.TestCase):
    def test_non_ghostty_window_backend_is_noop(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-display-close-noop-") as tmp:
            with patch("team_agent.runtime.run_cmd") as mock_run, \
                 patch("team_agent.runtime._tmux_session_exists", return_value=False):
                display.close_ghostty_display(
                    "alpha",
                    {"display": {"backend": "ghostty_workspace"}},
                    EventLog(Path(tmp)),
                )
            mock_run.assert_not_called()

    def test_close_workspace_skips_when_no_workspace_displays(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-display-close-empty-") as tmp:
            with patch("team_agent.runtime.run_cmd") as mock_run:
                display.close_ghostty_workspace({"agents": {}}, EventLog(Path(tmp)))
            mock_run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
