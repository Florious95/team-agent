from __future__ import annotations

import importlib.util
import json
import os
import shutil
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


_MACMINI_CODEX_TRUST_PROMPT = """You are in /private/tmp/teamA-slice2-env-slice-2-20260526T103705Z

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue
"""


def _ok_proc(stdout: str = "") -> SimpleNamespace:
    return SimpleNamespace(returncode=0, stdout=stdout, stderr="")


class Gap29SendTrustPromptIntegrationTests(unittest.TestCase):
    def test_send_path_detects_real_codex_trust_prompt_answers_then_re_pastes(self) -> None:
        from team_agent import runtime
        from team_agent.state import save_runtime_state

        workspace = Path("/private/tmp/teamA-slice2-env-slice-2-20260526T103705Z")
        shutil.rmtree(workspace, ignore_errors=True)
        workspace.mkdir(parents=True)
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
                        return _ok_proc(_MACMINI_CODEX_TRUST_PROMPT)
                    if state["submitted"] and state["message_token"]:
                        return _ok_proc(f"› [team-agent-token:{state['message_token']}] accepted\n")
                    return _ok_proc(state["pasted_text"] or "› idle prompt\n")
                if args[:2] == ["tmux", "set-buffer"]:
                    actions.append("set-buffer")
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
                    if args[-2:] == ["1", "Enter"]:
                        actions.append("trust-answer-1-enter")
                        state["trust_answered"] = True
                    elif args[-1] == "Enter":
                        actions.append("submit-enter")
                        state["submitted"] = True
                return _ok_proc()

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), \
                 patch("team_agent.runtime.time.sleep", return_value=None):
                result = runtime.send_message(workspace, "developer", "hello after trust", wait_visible=True)

            self.assertTrue(result["ok"], result)
            self.assertIn("detect-codex-trust-prompt", actions)
            self.assertIn("trust-answer-1-enter", actions)
            self.assertIn("set-buffer", actions)
            self.assertLess(actions.index("trust-answer-1-enter"), actions.index("set-buffer"), actions)
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


def _read_events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
