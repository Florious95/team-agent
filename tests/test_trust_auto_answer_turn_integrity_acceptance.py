"""Acceptance contract for minimal Codex trust auto-answer delivery behavior."""
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.message_store import MessageStore


BRIEF = "GAP43 minimal trust delivery contract"


def _trust_prompt_for(path_text: str) -> str:
    return (
        f"> You are in {path_text}\n\n"
        "  Do you trust the contents of this directory? Working with untrusted contents\n"
        "  comes with higher risk of prompt injection. Trusting the directory allows\n"
        "  project-local config, hooks, and exec policies to load.\n\n"
        "› 1. Yes, continue\n"
        "  2. No, quit\n\n"
        "  Press enter to continue\n"
    )


def _flatten_commands(commands: list[list[str]]) -> list[str]:
    return [part for command in commands for part in command]


class TrustAutoAnswerTurnIntegrityAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="trust-minimal-")
        self.workspace = Path(self._tmp_ctx.name).resolve() / "workspace"
        self.workspace.mkdir(parents=True, exist_ok=True)
        self.target = "team-gap43:worker_1"
        self._env_backup = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"

    def tearDown(self) -> None:
        if self._env_backup is None:
            os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        else:
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = self._env_backup
        self._tmp_ctx.cleanup()

    def _seed_message(self) -> str:
        store = MessageStore(self.workspace)
        return store.create_message(None, "leader", "worker_1", BRIEF, requires_ack=False)

    def _state(self) -> dict[str, Any]:
        return {
            "session_name": "team-gap43",
            "agents": {
                "worker_1": {
                    "status": "running",
                    "provider": "codex",
                    "window": "worker_1",
                }
            },
        }

    def _events(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        import json
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def test_trust_auto_answer_sends_enter_only_without_choice_digit(self) -> None:
        from team_agent.messaging.leader_panes import attempt_trust_auto_answer

        injected: list[tuple[Any, ...]] = []

        def fake_inject(*args: Any, **_kwargs: Any) -> dict[str, Any]:
            injected.append(args)
            return {"ok": True}

        with patch("team_agent.messaging.leader_panes._wait_for_codex_trust_input_ready", return_value={"ok": True}, create=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", side_effect=fake_inject):
            result = attempt_trust_auto_answer(
                self.workspace,
                self.target,
                _trust_prompt_for(str(self.workspace)),
                EventLog(self.workspace),
                state={"pane_width": 160},
            )

        self.assertTrue(result["ok"])
        self.assertTrue(result["answered"])
        self.assertEqual(len(injected), 1)
        self.assertEqual(injected[0][0], self.target)
        self.assertEqual(injected[0][1], "")
        self.assertEqual(injected[0][2], "Enter")

    def test_post_trust_paste_and_enter_success_is_delivered_regardless_of_pane_content(self) -> None:
        from team_agent.messaging import delivery as delivery_mod

        message_id = self._seed_message()
        arbitrary_codex_content = (
            "› 1\n\n"
            "• Working (12s • esc to interrupt)\n\n"
            "• Messages to be submitted after next tool call\n"
        )
        delivery_calls: list[tuple[Any, ...]] = []

        def fake_delivery_inject(*args: Any, **_kwargs: Any) -> dict[str, Any]:
            delivery_calls.append(args)
            if len(delivery_calls) == 1:
                return {
                    "ok": False,
                    "status": "failed",
                    "stage": "pre-paste-pane-state",
                    "reason": "recipient_pane_in_non_input_mode",
                    "verification": "recipient_pane_in_non_input_mode",
                    "detected": "codex_trust_prompt",
                    "pane_id": self.target,
                    "pane_capture_tail": _trust_prompt_for(str(self.workspace)),
                }
            return {
                "ok": True,
                "verification": "paste_and_enter_succeeded",
                "submit_verification": "Enter_sent",
                "turn_verification": "not_checked",
                "attempts": [{"attempt": 1}],
                "submit_attempts": [{"attempt": 1}],
            }

        with patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.delivery._tmux_pane_width", return_value={"ok": True, "pane_width": 160}), \
             patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_delivery_inject), \
             patch("team_agent.messaging.delivery._wait_for_trust_prompt_dismissal", return_value=True), \
             patch("team_agent.messaging.delivery._wait_for_codex_idle_after_trust_dismissal", return_value=False, create=True), \
             patch("team_agent.messaging.delivery._capture_pane_tail", return_value=arbitrary_codex_content, create=True), \
             patch("team_agent.messaging.leader_panes._wait_for_codex_trust_input_ready", return_value={"ok": True}, create=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value={"ok": True}):
            result = delivery_mod._deliver_pending_message(self.workspace, self._state(), message_id)

        self.assertTrue(result["ok"])
        self.assertEqual(result["status"], "delivered")
        self.assertEqual(len(delivery_calls), 2)
        self.assertEqual(delivery_calls[1][0], self.target)
        self.assertIn(BRIEF, delivery_calls[1][1])
        self.assertEqual(delivery_calls[1][2], "Enter")
        submitted = [event for event in self._events() if event.get("event") == "send.submitted"]
        self.assertEqual(len(submitted), 1)

    def test_empty_text_enter_path_uses_send_keys_without_empty_tmux_buffer(self) -> None:
        from team_agent.messaging.tmux_io import _tmux_inject_text

        commands: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20) -> SimpleNamespace:
            commands.append(args)
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd), \
             patch("team_agent.messaging.tmux_prompt.run_cmd", side_effect=fake_run_cmd), \
             patch("team_agent.messaging.tmux_io._capture_tmux_pane_text", return_value={"ok": True, "capture": ""}), \
             patch("team_agent.messaging.tmux_io.time.sleep", return_value=None):
            result = _tmux_inject_text(
                self.target,
                "",
                "Enter",
                "team-agent-empty-trust-answer",
                attempts=1,
                provider="fake",
                bypass_non_input_gate=True,
            )

        self.assertTrue(result["ok"])
        self.assertNotIn("set-buffer", _flatten_commands(commands))
        self.assertNotIn("load-buffer", _flatten_commands(commands))
        self.assertNotIn("paste-buffer", _flatten_commands(commands))
        self.assertIn(["tmux", "send-keys", "-t", self.target, "Enter"], commands)

    def test_buffer_paste_path_is_only_used_for_non_empty_text(self) -> None:
        from team_agent.messaging.tmux_io import _tmux_inject_text

        commands: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20) -> SimpleNamespace:
            commands.append(args)
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd), \
             patch("team_agent.messaging.tmux_prompt.run_cmd", side_effect=fake_run_cmd), \
             patch("team_agent.messaging.tmux_io._capture_tmux_pane_text", return_value={"ok": True, "capture": ""}), \
             patch("team_agent.messaging.tmux_io.time.sleep", return_value=None):
            result = _tmux_inject_text(
                self.target,
                "non-empty message",
                "Enter",
                "team-agent-non-empty-message",
                attempts=1,
                provider="fake",
                bypass_non_input_gate=True,
            )

        self.assertTrue(result["ok"])
        set_buffer_commands = [command for command in commands if len(command) >= 3 and command[1] == "set-buffer"]
        paste_buffer_commands = [command for command in commands if len(command) >= 2 and command[1] == "paste-buffer"]
        self.assertEqual(len(set_buffer_commands), 1)
        self.assertEqual(set_buffer_commands[0][-1], "non-empty message")
        self.assertNotEqual(set_buffer_commands[0][-1], "")
        self.assertEqual(len(paste_buffer_commands), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
