"""Gap 29 trust auto-answer (Stage 2 Lane B opt-in).

Tests for src/team_agent/messaging/leader_panes.attempt_trust_auto_answer and
the delivery.py wrap that consumes developer's structured codex_trust_prompt
envelope.

Hard contract:
  - Default opt-out: answered=False with reason='not_opted_in'; pane untouched.
  - Opt-in via spec.runtime.auto_trust_own_workspace=True: answers AND emits.
  - Opt-in via env TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE=1: also answers.
  - Workspace mismatch: even opted in, returns 'workspace_dir_mismatch' and
    does NOT touch the pane. (Security boundary: never trust an arbitrary dir.)
  - tmux send-keys failure: returns ok=False reason='tmux_send_keys_failed'.
  - delivery.py wrap: when injection envelope has detected='codex_trust_prompt'
    and helper answers, the inject is retried once; the second inject's success
    bubbles up as the final result.
"""
from __future__ import annotations

import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.messaging.leader_panes import attempt_trust_auto_answer


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base_gap29trust", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _ok_proc() -> SimpleNamespace:
    return SimpleNamespace(returncode=0, stdout="", stderr="")


def _fail_proc(stderr: str = "tmux refused") -> SimpleNamespace:
    return SimpleNamespace(returncode=1, stdout="", stderr=stderr)


def _trust_capture_tail(workspace: Path) -> str:
    return (
        "Do you trust the contents of this directory and want to allow execution of source files?\n"
        f"\n  ▌ {workspace.resolve()}\n"
        "\n  ▌ 1. Yes, proceed\n  ▌ 2. No, exit\n"
    )


class Gap29TrustAutoAnswerTests(unittest.TestCase):

    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="gap29-trust-")
        self.workspace = Path(self._tmp_ctx.name).resolve()
        (self.workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
        self.event_log = EventLog(self.workspace)
        # Stash env to restore.
        self._env_backup = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)

    def tearDown(self) -> None:
        if self._env_backup is None:
            os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        else:
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = self._env_backup
        self._tmp_ctx.cleanup()

    def _emitted(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def test_default_opt_out_does_not_touch_pane_but_emits_skipped_event(self) -> None:
        """Spark LOW #6: every refusal branch is observable via a structured event.
        Opt-out emits trust_auto_answer_skipped with reason=not_opted_in."""
        with patch("team_agent.messaging.leader_panes.run_cmd") as mock_run:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec={},
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "not_opted_in")
        mock_run.assert_not_called()
        skipped = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_skipped"]
        self.assertEqual(len(skipped), 1)
        self.assertEqual(skipped[0]["reason"], "not_opted_in")
        self.assertEqual(skipped[0]["pane_id"], "%worker")

    def test_opt_in_via_spec_answers_and_emits(self) -> None:
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        with patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()) as mock_run:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")
        mock_run.assert_called_once()
        args = mock_run.call_args[0][0]
        self.assertEqual(args[:2], ["tmux", "send-keys"])
        self.assertIn("%worker", args)
        self.assertIn("1", args)
        self.assertIn("Enter", args)
        emitted = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answered"]
        self.assertEqual(len(emitted), 1)
        self.assertEqual(emitted[0]["pane_id"], "%worker")

    def test_opt_in_via_env_var_answers(self) -> None:
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        with patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec={},
            )
        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")

    def test_workspace_mismatch_refuses_even_when_opted_in(self) -> None:
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        unrelated_dir_tail = (
            "Do you trust the contents of this directory and want to allow execution of source files?\n"
            "\n  ▌ /completely/different/path\n  ▌ 1. Yes  ▌ 2. No\n"
        )
        with patch("team_agent.messaging.leader_panes.run_cmd") as mock_run:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", unrelated_dir_tail,
                self.event_log, spec=spec,
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_run.assert_not_called()
        refused = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_refused"]
        self.assertEqual(len(refused), 1)

    def test_tmux_send_keys_failure_surfaces_reason(self) -> None:
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        with patch("team_agent.messaging.leader_panes.run_cmd", return_value=_fail_proc("no server")):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "tmux_send_keys_failed")
        self.assertEqual(result["error"], "no server")
        failed = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_failed"]
        self.assertEqual(len(failed), 1)

    def test_missing_pane_id_refuses_and_emits_skipped_event(self) -> None:
        """Spark LOW #6: pane_id_missing branch emits trust_auto_answer_skipped."""
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        with patch("team_agent.messaging.leader_panes.run_cmd") as mock_run:
            result = attempt_trust_auto_answer(
                self.workspace, None, _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "pane_id_missing")
        mock_run.assert_not_called()
        skipped = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_skipped"]
        self.assertEqual(len(skipped), 1)
        self.assertEqual(skipped[0]["reason"], "pane_id_missing")


class Gap29PathCanonicalizationTests(unittest.TestCase):
    """Spark MEDIUM #5: workspace-path comparison must be boundary-safe (so
    /repo never matches /repo-backup), tolerant of symlinks / ~ / trailing
    slashes, and must reject any directory that is not the workspace itself."""

    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="gap29-path-")
        self.workspace = Path(self._tmp_ctx.name).resolve()
        (self.workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
        self.event_log = EventLog(self.workspace)

    def tearDown(self) -> None:
        self._tmp_ctx.cleanup()

    def _spec_opt_in(self) -> dict[str, Any]:
        return {"runtime": {"auto_trust_own_workspace": True}}

    def _capture_with_dir(self, directory: str) -> str:
        return (
            "Do you trust the contents of this directory and want to allow execution of source files?\n"
            f"\n  ▌ {directory}\n  ▌\n  ▌ 1. Yes  ▌ 2. No\n"
        )

    def test_prefix_lookalike_is_refused(self) -> None:
        """/repo must NOT match /repo-backup. Boundary-safe equality, not substring."""
        evil = str(self.workspace.parent / (self.workspace.name + "-backup"))
        with patch("team_agent.messaging.leader_panes.run_cmd") as mock_run:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", self._capture_with_dir(evil),
                self.event_log, spec=self._spec_opt_in(),
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_run.assert_not_called()

    def test_trailing_slash_variant_is_accepted(self) -> None:
        """Trailing slash on the prompt path must still resolve to the workspace."""
        with patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", self._capture_with_dir(str(self.workspace) + "/"),
                self.event_log, spec=self._spec_opt_in(),
            )
        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")

    def test_unrelated_directory_is_refused(self) -> None:
        with patch("team_agent.messaging.leader_panes.run_cmd") as mock_run:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", self._capture_with_dir("/tmp/some-other-workspace"),
                self.event_log, spec=self._spec_opt_in(),
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_run.assert_not_called()

    def test_symlink_to_workspace_is_accepted(self) -> None:
        """Symlinked spelling of the same directory resolves to the canonical
        path and must auto-answer."""
        link = self.workspace.parent / (self.workspace.name + "-link")
        if link.exists() or link.is_symlink():
            link.unlink()
        try:
            link.symlink_to(self.workspace)
        except OSError:
            self.skipTest("filesystem does not support symlinks")
        try:
            with patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()):
                result = attempt_trust_auto_answer(
                    self.workspace, "%worker", self._capture_with_dir(str(link)),
                    self.event_log, spec=self._spec_opt_in(),
                )
            self.assertTrue(result["answered"])
            self.assertEqual(result["reason"], "trust_auto_answered")
        finally:
            try:
                link.unlink()
            except FileNotFoundError:
                pass


class Gap29DeliveryWrapTests(unittest.TestCase):
    """The delivery.py wrap that consumes developer's structured envelope and
    invokes the trust auto-answer helper, then retries the inject."""

    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="gap29-delivery-")
        self.workspace = Path(self._tmp_ctx.name).resolve()
        (self.workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
        self.event_log = EventLog(self.workspace)
        self._env_backup = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)

    def tearDown(self) -> None:
        if self._env_backup is None:
            os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        else:
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = self._env_backup
        self._tmp_ctx.cleanup()

    def _seed_message(self) -> str:
        from team_agent.message_store import MessageStore
        store = MessageStore(self.workspace)
        return store.create_message(None, "leader", "developer", "task body", requires_ack=False)

    def _state(self) -> dict[str, Any]:
        return {
            "session_name": "team-gap29",
            "agents": {"developer": {"status": "running", "provider": "codex", "window": "developer"}},
            "spec_path": str(self.workspace / "team.spec.yaml"),
        }

    def _trust_envelope(self) -> dict[str, Any]:
        return {
            "ok": False,
            "status": "failed",
            "stage": "pre-paste-pane-state",
            "reason": "recipient_pane_in_non_input_mode",
            "verification": "recipient_pane_in_non_input_mode",
            "detected": "codex_trust_prompt",
            "pane_id": "team-gap29:developer",
            "pane_mode": "",
            "pane_capture_tail": _trust_capture_tail(self.workspace),
        }

    def _ok_envelope(self) -> dict[str, Any]:
        return {
            "ok": True,
            "verification": "capture_contains_new_pasted_content_prompt",
            "submit_verification": "pasted_content_prompt_absent_after_submit",
            "turn_verification": "leader_new_turn_boundary_verified",
            "attempts": [{}],
            "submit_attempts": [{}],
        }

    def test_opt_in_delivery_wrap_answers_and_retries_inject(self) -> None:
        from team_agent.messaging import delivery as delivery_mod
        # spec opted in via env to avoid writing a spec file in the temp workspace
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        message_id = self._seed_message()
        state = self._state()
        inject_calls: list[Any] = []
        # First inject returns trust prompt envelope; second (retry) returns ok.
        responses = iter([self._trust_envelope(), self._ok_envelope()])

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            inject_calls.append({"target": target, "buffer": buffer_name})
            return next(responses)

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()):
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertTrue(result["ok"])
        self.assertEqual(len(inject_calls), 2,
            f"trust auto-answer must trigger a retry inject; got {len(inject_calls)} calls")
        self.assertTrue(any("trust-retry" in call["buffer"] for call in inject_calls),
            "second inject's buffer name should mark it as the trust retry")
        emitted = [ev for ev in self._read_events() if ev.get("event") == "leader_panes.trust_auto_answered"]
        self.assertEqual(len(emitted), 1)

    def test_opt_in_delivery_wrap_returns_retry_needed_when_prompt_does_not_dismiss(self) -> None:
        """Spark MEDIUM #4: if the bounded poll never sees the trust prompt
        clear, return a retry_needed envelope instead of attempting the second
        inject blindly. Helper still answered (trust_auto_answered emitted) and
        the message_id is marked failed so it does not stay claimed.
        """
        from team_agent.messaging import delivery as delivery_mod
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        message_id = self._seed_message()
        state = self._state()
        inject_calls: list[Any] = []

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            inject_calls.append({"target": target, "buffer": buffer_name})
            return self._trust_envelope()

        # Pane capture always reports the trust prompt is still present, so the
        # bounded poll exhausts and we hit the retry_needed branch.
        def stuck_capture(target):
            return "Do you trust the contents of this directory and want to allow execution of source files?\n"

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()), \
             patch("team_agent.messaging.delivery._capture_pane_tail", side_effect=stuck_capture), \
             patch("time.sleep", return_value=None), \
             patch("time.monotonic", side_effect=[0.0, 0.0, 100.0, 100.0]):
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertFalse(result["ok"])
        self.assertEqual(result["status"], "retry_needed")
        self.assertEqual(result["reason"], "trust_prompt_not_dismissed_after_answer")
        # Only the original inject ran; the retry was suppressed because the
        # prompt never cleared.
        self.assertEqual(len(inject_calls), 1)
        emitted_events = [ev for ev in self._read_events()
                          if ev.get("event") == "leader_panes.trust_auto_answer_retry_needed"]
        self.assertEqual(len(emitted_events), 1)
        self.assertEqual(emitted_events[0]["reason"], "trust_prompt_not_dismissed_after_answer")

    def test_opt_in_delivery_wrap_proceeds_when_poll_observes_dismissal(self) -> None:
        """Bounded poll succeeds quickly when the pane returns to input mode."""
        from team_agent.messaging import delivery as delivery_mod
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        message_id = self._seed_message()
        state = self._state()
        responses = iter([self._trust_envelope(), self._ok_envelope()])

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            return next(responses)

        def cleared_capture(target):
            return "> idle prompt\n"

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes.run_cmd", return_value=_ok_proc()), \
             patch("team_agent.messaging.delivery._capture_pane_tail", side_effect=cleared_capture):
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertTrue(result["ok"])
        emitted_skipped = [ev for ev in self._read_events()
                           if ev.get("event") == "leader_panes.trust_auto_answer_retry_needed"]
        self.assertEqual(emitted_skipped, [])

    def test_opt_out_delivery_wrap_does_not_retry(self) -> None:
        from team_agent.messaging import delivery as delivery_mod
        message_id = self._seed_message()
        state = self._state()
        inject_calls: list[Any] = []

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            inject_calls.append({"target": target, "buffer": buffer_name})
            return self._trust_envelope()

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes.run_cmd") as mock_run:
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertFalse(result["ok"])
        self.assertEqual(len(inject_calls), 1,
            "opt-out must not retry; single inject only")
        mock_run.assert_not_called()
        self.assertEqual(
            [ev for ev in self._read_events() if ev.get("event") == "leader_panes.trust_auto_answered"],
            [],
        )

    def _read_events(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
