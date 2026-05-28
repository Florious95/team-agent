"""Live delivery wiring for truncated Codex trust-prompt workspace paths."""
from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.message_store import MessageStore


def _ok_inject() -> dict[str, Any]:
    return {
        "ok": True,
        "verification": "capture_contains_new_pasted_content_prompt",
        "submit_verification": "pasted_content_prompt_absent_after_submit",
        "turn_verification": "leader_new_turn_boundary_verified",
        "attempts": [{}],
        "submit_attempts": [{}],
    }


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


class TrustTruncatedWorkspaceWiringAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="trust-truncated-wiring-")
        self.workspace = (
            Path(self._tmp_ctx.name).resolve()
            / "workspaces"
            / "repo-right-edge-truncated-20260528T014841Z-gap39"
        )
        self.workspace.mkdir(parents=True, exist_ok=True)
        self.target = "team-trunc:developer"
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
        return store.create_message(None, "leader", "developer", "task body", requires_ack=False)

    def _state_without_pane_width(self) -> dict[str, Any]:
        return {
            "session_name": "team-trunc",
            "agents": {
                "developer": {
                    "status": "running",
                    "provider": "codex",
                    "window": "developer",
                }
            },
        }

    def _trust_envelope(self, capture: str) -> dict[str, Any]:
        return {
            "ok": False,
            "status": "failed",
            "stage": "pre-paste-pane-state",
            "reason": "recipient_pane_in_non_input_mode",
            "verification": "recipient_pane_in_non_input_mode",
            "detected": "codex_trust_prompt",
            "pane_id": self.target,
            "pane_mode": "",
            "pane_capture_tail": capture,
        }

    def _events(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def _right_edge_capture(self) -> tuple[str, int]:
        captured_path = str(self.workspace)[:-8]
        capture = _trust_prompt_for(captured_path)
        pane_width = len(capture.splitlines()[0])
        return capture, pane_width

    def test_live_delivery_queries_pane_width_and_accepts_right_edge_truncation(self) -> None:
        from team_agent.messaging import delivery as delivery_mod

        message_id = self._seed_message()
        state = self._state_without_pane_width()
        capture, pane_width = self._right_edge_capture()
        responses = iter([self._trust_envelope(capture), _ok_inject()])
        delivery_inject_calls: list[dict[str, Any]] = []
        width_queries: list[str] = []

        def fake_delivery_inject(target, text, submit_key, buffer_name, **kwargs):
            delivery_inject_calls.append({"target": target, "buffer": buffer_name})
            return next(responses)

        def fake_pane_width(target: str) -> dict[str, Any]:
            width_queries.append(target)
            return {"ok": True, "pane_width": pane_width}

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_delivery_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.delivery._tmux_pane_width", side_effect=fake_pane_width, create=True), \
             patch("team_agent.messaging.delivery._wait_for_trust_prompt_dismissal", return_value=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value={"ok": True}) as trust_answer:
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertEqual(width_queries, [self.target])
        self.assertTrue(result["ok"])
        self.assertEqual(len(delivery_inject_calls), 2)
        self.assertTrue(any("trust-retry" in call["buffer"] for call in delivery_inject_calls))
        trust_answer.assert_called_once()
        self.assertEqual(trust_answer.call_args[0][:3], (self.target, "", "Enter"))
        refused_mismatch = [
            ev for ev in self._events()
            if ev.get("event") == "leader_panes.trust_auto_answer_refused"
            and ev.get("reason") == "workspace_dir_mismatch"
        ]
        self.assertEqual(refused_mismatch, [])

    def test_live_delivery_pane_width_query_failure_fails_safe_for_truncated_prefix(self) -> None:
        from team_agent.messaging import delivery as delivery_mod

        message_id = self._seed_message()
        state = self._state_without_pane_width()
        capture, _pane_width = self._right_edge_capture()
        width_queries: list[str] = []

        def fake_delivery_inject(target, text, submit_key, buffer_name, **kwargs):
            return self._trust_envelope(capture)

        def fake_pane_width(target: str) -> dict[str, Any]:
            width_queries.append(target)
            return {"ok": False, "error": "tmux display-message failed"}

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_delivery_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.delivery._tmux_pane_width", side_effect=fake_pane_width, create=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text") as trust_answer:
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertEqual(width_queries, [self.target])
        self.assertFalse(result["ok"])
        trust_answer.assert_not_called()
        answered = [ev for ev in self._events() if ev.get("event") == "leader_panes.trust_auto_answered"]
        self.assertEqual(answered, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
