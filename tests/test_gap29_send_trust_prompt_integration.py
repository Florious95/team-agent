from __future__ import annotations

import importlib.util
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base_gap29_send", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


# Stage 15 CI hotfix (2026-05-27): the Mac mini run that captured these
# fixtures embedded its real workspace path into the prompt text. Linux CI
# (/opt/hostedtoolcache/Python/3.12) has no /private/tmp/, so any code that
# tries to mkdir that absolute path fails FileNotFoundError before the test
# can mock the inject. The fixtures keep a {WORKSPACE} placeholder; each
# test substitutes its OWN tempfile.mkdtemp() path, so the prompt text and
# the actual Path operations refer to the same real directory on every OS.
_WORKSPACE_PLACEHOLDER = "{WORKSPACE}"

_MACMINI_CODEX_TRUST_PROMPT_TEMPLATE = """You are in {WORKSPACE}

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue
"""

_MACMINI_CODEX_TRUST_PROMPT_BLANK_TAIL_TEMPLATE = _MACMINI_CODEX_TRUST_PROMPT_TEMPLATE + ("\n" * 12)

_MACMINI_CODEX_TRUST_PROMPT_ANSI_TEMPLATE = _MACMINI_CODEX_TRUST_PROMPT_TEMPLATE.replace(
    "Do you trust the contents of this directory?",
    "\x1b[1mDo you trust the contents of this directory?\x1b[0m",
)

_MACMINI_CODEX_TRUST_PROMPT_WRAPPED_TEMPLATE = """You are in {WORKSPACE}

  Do you trust the contents of this
  directory? Working with untrusted contents comes with higher risk of prompt
  injection. Trusting the directory allows project-local config, hooks, and
  exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to
  continue
""" + ("\n" * 12)


def _materialize_workspace_and_fixture(prompt_template: str) -> tuple[Path, str]:
    """Create a real temp dir for this test and inject its absolute path into
    the prompt template so the workspace-dir check in attempt_trust_auto_answer
    sees a real on-disk path that matches the prompt's quoted directory. Caller
    is responsible for shutil.rmtree on teardown."""
    workspace = Path(tempfile.mkdtemp(prefix="team-agent-gap29-trust-real-")).resolve()
    return workspace, prompt_template.replace(_WORKSPACE_PLACEHOLDER, str(workspace))


def _ok_proc(stdout: str = "") -> SimpleNamespace:
    return SimpleNamespace(returncode=0, stdout=stdout, stderr="")


class Gap29SendTrustPromptIntegrationTests(unittest.TestCase):
    def test_send_path_detects_real_codex_trust_prompt_answers_then_re_pastes(self) -> None:
        self._run_send_path_trust_prompt_fixture(_MACMINI_CODEX_TRUST_PROMPT_TEMPLATE)

    def test_send_path_detects_trust_prompt_variants_with_blank_tail_ansi_and_wrapping(self) -> None:
        cases = {
            "blank_tail": _MACMINI_CODEX_TRUST_PROMPT_BLANK_TAIL_TEMPLATE,
            "ansi": _MACMINI_CODEX_TRUST_PROMPT_ANSI_TEMPLATE,
            "wrapped": _MACMINI_CODEX_TRUST_PROMPT_WRAPPED_TEMPLATE,
        }
        for name, fixture in cases.items():
            with self.subTest(name=name):
                self._run_send_path_trust_prompt_fixture(fixture)

    def _run_send_path_trust_prompt_fixture(self, prompt_template: str) -> None:
        from team_agent import runtime
        from team_agent.state import save_runtime_state

        workspace, prompt_fixture = _materialize_workspace_and_fixture(prompt_template)
        old_env = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        try:
            spec = _base._fake_spec(workspace)
            spec["agents"] = [{**spec["agents"][0], "id": "developer"}]
            spec["runtime"]["startup_order"] = ["developer"]
            spec["routing"]["default_assignee"] = "developer"
            spec["routing"]["rules"] = []
            spec["tasks"][0]["assignee"] = "developer"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(json.dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-gap29-real",
                    "leader": spec["leader"],
                    "agents": {
                        "developer": {
                            "status": "running",
                            "provider": "codex",
                            "window": "developer",
                        },
                    },
                    "tasks": spec["tasks"],
                },
            )

            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
            actions: list[str] = []
            buffers: dict[str, str] = {}
            state: dict[str, Any] = {
                "message_token": "",
                "pasted_text": "",
                "trust_answered": False,
                "submitted": False,
            }

            def fake_run_cmd(args: list[str], timeout: int = 20):
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    return _ok_proc("developer\n")
                if args[:4] == ["tmux", "display-message", "-p", "-t"]:
                    return _ok_proc("\n")
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    if not state["trust_answered"]:
                        actions.append("detect-codex-trust-prompt")
                        return _ok_proc(prompt_fixture)
                    if state["submitted"] and state["message_token"]:
                        return _ok_proc(f"› [team-agent-token:{state['message_token']}] accepted\n")
                    return _ok_proc(state["pasted_text"] or "› idle prompt\n")
                if args[:2] == ["tmux", "set-buffer"]:
                    actions.append("set-buffer-answer" if "trust-auto-answer" in args[3] else "set-buffer-message")
                    buffers[args[3]] = args[4]
                    marker = "[team-agent-token:"
                    if marker in args[4]:
                        state["message_token"] = args[4].split(marker, 1)[1].split("]", 1)[0]
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    actions.append("paste-buffer")
                    state["pasted_text"] = buffers[args[5]]
                elif args[:2] == ["tmux", "delete-buffer"]:
                    actions.append("delete-buffer")
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    if args[-1] == "Enter" and state["pasted_text"] == "" and not state["trust_answered"]:
                        actions.append("trust-answer-1-enter")
                        state["trust_answered"] = True
                        state["pasted_text"] = ""
                    elif args[-1] == "Enter":
                        actions.append("submit-enter")
                        state["submitted"] = True
                return _ok_proc()

            with patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.messaging.leader_panes.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
                 patch("team_agent.messaging.delivery._wait_for_trust_prompt_dismissal", return_value=True), \
                 patch("team_agent.runtime.time.sleep", return_value=None):
                result = runtime.send_message(workspace, "developer", "hello after trust", wait_visible=True)

            self.assertTrue(result["ok"], result)
            self.assertIn("detect-codex-trust-prompt", actions)
            self.assertIn("trust-answer-1-enter", actions)
            self.assertIn("set-buffer-message", actions)
            self.assertLess(actions.index("trust-answer-1-enter"), actions.index("set-buffer-message"), actions)
            self.assertIn("paste-buffer", actions)
            self.assertIn("submit-enter", actions)
            emitted = [ev for ev in _read_events(workspace) if ev.get("event") == "leader_panes.trust_auto_answered"]
            self.assertEqual(len(emitted), 1)
        finally:
            if old_env is None:
                os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
            else:
                os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = old_env
            shutil.rmtree(workspace, ignore_errors=True)

    def test_leader_receiver_path_detects_trust_prompt_answers_then_re_pastes(self) -> None:
        self._run_leader_receiver_trust_prompt_fixture(_MACMINI_CODEX_TRUST_PROMPT_TEMPLATE)

    def test_leader_receiver_path_detects_trust_prompt_with_blank_tail_then_re_pastes(self) -> None:
        self._run_leader_receiver_trust_prompt_fixture(_MACMINI_CODEX_TRUST_PROMPT_BLANK_TAIL_TEMPLATE)

    def _run_leader_receiver_trust_prompt_fixture(self, prompt_template: str) -> None:
        from team_agent import runtime
        from team_agent.events import EventLog
        from team_agent.state import save_runtime_state

        workspace, prompt_fixture = _materialize_workspace_and_fixture(prompt_template)
        old_env = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        try:
            state = {
                "workspace": str(workspace),
                "leader": {"id": "leader"},
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "provider": "codex",
                    "pane_id": "%leader",
                    "session_name": "team-gap29-real",
                },
                "agents": {"worker": {"status": "running", "provider": "fake"}},
                "tasks": [{"id": "task_1", "title": "Task", "assignee": "worker", "status": "running"}],
            }
            save_runtime_state(workspace, state)
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
            actions: list[str] = []
            buffers: dict[str, str] = {}
            pane = {
                "message_token": "",
                "pasted_text": "",
                "trust_answered": False,
                "submitted": False,
            }

            def fake_run_cmd(args: list[str], timeout: int = 20):
                if args[:4] == ["tmux", "display-message", "-p", "-t"]:
                    return _ok_proc("\n")
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    lines = args[args.index("-S") + 1] if "-S" in args else ""
                    if lines == "-40":
                        return _ok_proc("› idle prompt\n")
                    if not pane["trust_answered"]:
                        actions.append("detect-codex-trust-prompt")
                        return _ok_proc(prompt_fixture)
                    if pane["submitted"] and pane["message_token"]:
                        return _ok_proc(f"› [team-agent-token:{pane['message_token']}] accepted\n")
                    return _ok_proc(pane["pasted_text"] or "› idle prompt\n")
                if args[:2] == ["tmux", "set-buffer"]:
                    if "trust-auto-answer" in args[3]:
                        actions.append("set-buffer-answer")
                    else:
                        actions.append("set-buffer-message")
                    buffers[args[3]] = args[4]
                    marker = "[team-agent-token:"
                    if marker in args[4]:
                        pane["message_token"] = args[4].split(marker, 1)[1].split("]", 1)[0]
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    actions.append("paste-buffer")
                    pane["pasted_text"] = buffers[args[5]]
                elif args[:2] == ["tmux", "delete-buffer"]:
                    actions.append("delete-buffer")
                elif args[:3] == ["tmux", "send-keys", "-t"] and args[-1] == "Enter":
                    if pane["pasted_text"] == "" and not pane["trust_answered"]:
                        actions.append("trust-answer-1-enter")
                        pane["trust_answered"] = True
                        pane["pasted_text"] = ""
                    else:
                        actions.append("submit-enter")
                        pane["submitted"] = True
                return _ok_proc()

            pane_info = {
                "pane_id": "%leader",
                "session_name": "team-gap29-real",
                "window_index": "0",
                "window_name": "leader",
                "pane_index": "0",
                "pane_tty": "/dev/ttys001",
                "pane_current_command": "codex",
                "pane_current_path": str(workspace),
                "pane_active": "1",
                "window_active": "1",
            }
            with patch("team_agent._legacy_pane_discovery._tmux_pane_info", return_value=pane_info), \
                 patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.messaging.leader_panes.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.messaging.delivery._wait_for_trust_prompt_dismissal", return_value=True), \
                 patch("team_agent.runtime.time.sleep", return_value=None):
                result = runtime._send_to_leader_receiver(
                    workspace,
                    state,
                    "leader",
                    "hello after trust",
                    "task_1",
                    "worker",
                    False,
                    EventLog(workspace),
                )

            self.assertTrue(result["ok"], result)
            self.assertIn("detect-codex-trust-prompt", actions)
            # Round-6 wiring: trust auto-answer's empty Enter goes through a
            # direct send-keys call, no set-buffer/paste-buffer for the answer.
            self.assertNotIn("set-buffer-answer", actions)
            self.assertIn("trust-answer-1-enter", actions)
            self.assertIn("set-buffer-message", actions)
            self.assertLess(actions.index("trust-answer-1-enter"), actions.index("set-buffer-message"), actions)
            emitted = [ev for ev in _read_events(workspace) if ev.get("event") == "leader_panes.trust_auto_answered"]
            self.assertEqual(len(emitted), 1)
        finally:
            if old_env is None:
                os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
            else:
                os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = old_env
            shutil.rmtree(workspace, ignore_errors=True)


def _read_events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
