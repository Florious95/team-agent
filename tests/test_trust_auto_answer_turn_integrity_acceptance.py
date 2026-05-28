"""Acceptance contract for Codex trust auto-answer turn integrity.

The real 0.2.4 Mac mini fixture shows a stray `1` Codex turn after trust
auto-answer and a Team Agent brief stuck in Codex's queued-message area while
Team Agent emitted send.submitted. These tests pin the external behavior.
"""
from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch

from team_agent.message_store import MessageStore


FIXTURE_DIR = Path(__file__).parent / "fixtures" / "trust_auto_answer_turn_integrity"
RAW_CAPTURE = FIXTURE_DIR / "codex-worker1-gap39-fail.raw.txt"
EVENTS_FIXTURE = FIXTURE_DIR / "gap39-dead-owner-restart.events.jsonl"
INCIDENT_MESSAGE = (
    "GAP39_PRIME_0.2.4-bundled-20260528T033300Z: reply via report_result "
    "summary GAP39_PRIME_DONE_0.2.4-bundled-20260528T033300Z"
)
INCIDENT_TOKEN = "msg_c2591760ea1a"
INCIDENT_RENDERED_TEXT = (
    "Team Agent message from leader:\n\n"
    f"{INCIDENT_MESSAGE}\n\n"
    f"[team-agent-token:{INCIDENT_TOKEN}]"
)


def _read_events() -> list[dict[str, Any]]:
    return [json.loads(line) for line in EVENTS_FIXTURE.read_text(encoding="utf-8").splitlines() if line.strip()]


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


class TrustAutoAnswerTurnIntegrityAcceptanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="trust-turn-integrity-")
        self.workspace = Path(self._tmp_ctx.name).resolve() / "0.2.4-bundled-20260528T033300Z-gap39"
        self.workspace.mkdir(parents=True, exist_ok=True)
        self.target = "team-024-gap39:worker_1"
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
        return store.create_message(None, "leader", "worker_1", INCIDENT_MESSAGE, requires_ack=False)

    def _state(self) -> dict[str, Any]:
        return {
            "session_name": "team-024-gap39",
            "agents": {
                "worker_1": {
                    "status": "running",
                    "provider": "codex",
                    "window": "worker_1",
                }
            },
        }

    def _workspace_trust_tail(self) -> str:
        full = str(self.workspace)
        return _trust_prompt_for(full[:-8])

    def _local_events(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def test_fixture_records_stray_one_turn_queued_brief_and_false_submitted_event(self) -> None:
        raw = RAW_CAPTURE.read_text(encoding="utf-8")
        events = _read_events()

        self.assertIn("› 1\n", raw)
        self.assertIn("Messages to be submitted after next tool call", raw)
        self.assertIn("Team Agent message from leader", raw)
        self.assertIn("GAP39_PRIME_0.2.4-bundled-20260528T033300Z", raw)
        trust_events = [event for event in events if event.get("event") == "leader_panes.trust_auto_answered"]
        submitted = [event for event in events if event.get("event") == "send.submitted"]
        self.assertGreaterEqual(len(trust_events), 1)
        self.assertEqual(len(submitted), 1)
        self.assertEqual(submitted[0]["message_id"], INCIDENT_TOKEN)
        self.assertEqual(submitted[0]["turn_verification"], "leader_new_turn_boundary_verified")
        self.assertEqual(submitted[0]["verification"], "capture_contains_token")

    def test_queued_message_under_stray_one_is_not_a_valid_codex_model_turn(self) -> None:
        from team_agent.messaging.tmux_io import _capture_has_leader_new_turn

        raw = RAW_CAPTURE.read_text(encoding="utf-8")

        self.assertFalse(
            _capture_has_leader_new_turn(raw, INCIDENT_RENDERED_TEXT, INCIDENT_TOKEN, "codex"),
            "queued-message text below a stray `1` turn is not the next real Team Agent model turn",
        )

    def test_live_delivery_after_trust_answer_does_not_submit_when_brief_is_only_queued(self) -> None:
        from team_agent.messaging import delivery as delivery_mod

        message_id = self._seed_message()
        queued_capture = RAW_CAPTURE.read_text(encoding="utf-8")
        prepare_results = iter(
            [
                {
                    "ok": False,
                    "status": "failed",
                    "stage": "pre-paste-pane-state",
                    "reason": "recipient_pane_in_non_input_mode",
                    "verification": "recipient_pane_in_non_input_mode",
                    "detected": "codex_trust_prompt",
                    "pane_id": self.target,
                    "pane_mode": "",
                    "pane_capture_tail": self._workspace_trust_tail(),
                },
                {"ok": True, "verification": "pane_input_ready"},
            ]
        )

        def fake_prepare(_target: str) -> dict[str, Any]:
            return next(prepare_results)

        def fake_run_cmd(_args: list[str], timeout: int = 20) -> SimpleNamespace:
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.delivery._tmux_pane_width", return_value={"ok": True, "pane_width": 120}), \
             patch("team_agent.messaging.delivery._wait_for_trust_prompt_dismissal", return_value=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value={"ok": True}), \
             patch("team_agent.messaging.tmux_io._prepare_tmux_pane_for_input", side_effect=fake_prepare), \
             patch("team_agent.messaging.tmux_io._capture_tmux_pane_text", return_value={"ok": True, "capture": queued_capture}), \
             patch("team_agent.messaging.tmux_io._tmux_set_buffer_text", return_value={"ok": True, "stage": "set-buffer", "method": "set_buffer_arg", "text_bytes": 193}), \
             patch("team_agent.messaging.tmux_io._tmux_delete_buffer", return_value={"ok": True}), \
             patch("team_agent.messaging.tmux_io.run_cmd", side_effect=fake_run_cmd), \
             patch("team_agent.messaging.tmux_io._wait_for_message_ready", return_value=(True, "capture_contains_message_fragment", queued_capture)), \
             patch("team_agent.messaging.tmux_io._submit_worker_prompt", return_value={"ok": True, "verification": "enter_sent_without_placeholder_check", "attempts": [{"attempt": 1}]}), \
             patch("team_agent.messaging.tmux_io.time.sleep", return_value=None):
            result = delivery_mod._deliver_pending_message(self.workspace, self._state(), message_id)

        self.assertFalse(result["ok"])
        self.assertNotEqual(result.get("status"), "delivered")
        self.assertNotEqual(result.get("message_status"), "submitted")
        submitted = [event for event in self._local_events() if event.get("event") == "send.submitted"]
        self.assertEqual(submitted, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
