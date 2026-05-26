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
from typing import Any
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.messaging.leader_panes import attempt_trust_auto_answer


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base_gap29trust", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _ok_inject() -> dict[str, Any]:
    return {"ok": True}


def _fail_inject(error: str = "tmux refused") -> dict[str, Any]:
    return {"ok": False, "error": error}


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
        with patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec={},
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "not_opted_in")
        mock_inject.assert_not_called()
        skipped = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_skipped"]
        self.assertEqual(len(skipped), 1)
        self.assertEqual(skipped[0]["reason"], "not_opted_in")
        self.assertEqual(skipped[0]["pane_id"], "%worker")

    def test_opt_in_via_spec_answers_and_emits(self) -> None:
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()) as mock_inject:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")
        mock_inject.assert_called_once()
        args = mock_inject.call_args[0]
        kwargs = mock_inject.call_args.kwargs
        self.assertEqual(args[:3], ("%worker", "1", "Enter"))
        self.assertTrue(kwargs["bypass_non_input_gate"])
        emitted = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answered"]
        self.assertEqual(len(emitted), 1)
        self.assertEqual(emitted[0]["pane_id"], "%worker")

    def test_opt_in_via_env_var_answers(self) -> None:
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()):
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
        with patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", unrelated_dir_tail,
                self.event_log, spec=spec,
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_inject.assert_not_called()
        refused = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_refused"]
        self.assertEqual(len(refused), 1)

    def test_tmux_send_keys_failure_surfaces_reason(self) -> None:
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_fail_inject("no server")):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "tmux_send_keys_failed")
        self.assertEqual(result["error"], "no server")
        failed = [ev for ev in self._emitted() if ev.get("event") == "leader_panes.trust_auto_answer_failed"]
        self.assertEqual(len(failed), 1)

    def test_spec_opt_in_emits_deprecation_warning_and_event(self) -> None:
        """Constitution-reviewer F3 MEDIUM: spec.runtime.auto_trust_own_workspace
        opt-in must emit a stderr deprecation warning AND a structured
        trust_auto_answer_spec_opt_in_deprecated event pointing at the env-var
        as the preferred per-session opt-in."""
        from team_agent.messaging import leader_panes as leader_panes_mod
        leader_panes_mod._reset_spec_opt_in_deprecation_state()
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        from io import StringIO
        import contextlib
        stderr_buf = StringIO()
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
             contextlib.redirect_stderr(stderr_buf):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertTrue(result["answered"])
        self.assertIn("spec.runtime.auto_trust_own_workspace is deprecated", stderr_buf.getvalue())
        self.assertIn("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", stderr_buf.getvalue())
        self.assertIn("0.3.0", stderr_buf.getvalue())
        deprecated_events = [ev for ev in self._emitted()
                             if ev.get("event") == "trust_auto_answer_spec_opt_in_deprecated"]
        self.assertEqual(len(deprecated_events), 1)
        self.assertEqual(deprecated_events[0]["deprecated_field"], "spec.runtime.auto_trust_own_workspace")
        self.assertEqual(deprecated_events[0]["removal_target_version"], "0.3.0")

    def test_env_only_opt_in_does_not_emit_deprecation_warning(self) -> None:
        """When env-var opt-in is used and the spec field is not set, no
        deprecation warning or structured event fires — env-var is the
        preferred path."""
        from team_agent.messaging import leader_panes as leader_panes_mod
        leader_panes_mod._reset_spec_opt_in_deprecation_state()
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        from io import StringIO
        import contextlib
        stderr_buf = StringIO()
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
             contextlib.redirect_stderr(stderr_buf):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", _trust_capture_tail(self.workspace),
                self.event_log, spec={},
            )
        self.assertTrue(result["answered"])
        self.assertEqual(stderr_buf.getvalue(), "")
        deprecated_events = [ev for ev in self._emitted()
                             if ev.get("event") == "trust_auto_answer_spec_opt_in_deprecated"]
        self.assertEqual(deprecated_events, [])

    def test_spec_opt_in_deprecation_warning_is_one_shot_per_process(self) -> None:
        """The stderr deprecation prints exactly once per process even when the
        helper is called repeatedly; the structured event still fires per call
        so an audit log captures every yaml-driven decision."""
        from team_agent.messaging import leader_panes as leader_panes_mod
        leader_panes_mod._reset_spec_opt_in_deprecation_state()
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        from io import StringIO
        import contextlib
        stderr_buf = StringIO()
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
             contextlib.redirect_stderr(stderr_buf):
            for _ in range(3):
                attempt_trust_auto_answer(
                    self.workspace, "%worker", _trust_capture_tail(self.workspace),
                    self.event_log, spec=spec,
                )
        stderr_text = stderr_buf.getvalue()
        # Exactly one warning line across three calls.
        self.assertEqual(stderr_text.count("spec.runtime.auto_trust_own_workspace is deprecated"), 1)
        deprecated_events = [ev for ev in self._emitted()
                             if ev.get("event") == "trust_auto_answer_spec_opt_in_deprecated"]
        # Structured events fire per call for audit completeness.
        self.assertEqual(len(deprecated_events), 3)

    def test_missing_pane_id_refuses_and_emits_skipped_event(self) -> None:
        """Spark LOW #6: pane_id_missing branch emits trust_auto_answer_skipped."""
        spec = {"runtime": {"auto_trust_own_workspace": True}}
        with patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = attempt_trust_auto_answer(
                self.workspace, None, _trust_capture_tail(self.workspace),
                self.event_log, spec=spec,
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "pane_id_missing")
        mock_inject.assert_not_called()
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
        with patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", self._capture_with_dir(evil),
                self.event_log, spec=self._spec_opt_in(),
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_inject.assert_not_called()

    def test_trailing_slash_variant_is_accepted(self) -> None:
        """Trailing slash on the prompt path must still resolve to the workspace."""
        with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", self._capture_with_dir(str(self.workspace) + "/"),
                self.event_log, spec=self._spec_opt_in(),
            )
        self.assertTrue(result["answered"])
        self.assertEqual(result["reason"], "trust_auto_answered")

    def test_unrelated_directory_is_refused(self) -> None:
        with patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", self._capture_with_dir("/tmp/some-other-workspace"),
                self.event_log, spec=self._spec_opt_in(),
            )
        self.assertFalse(result["answered"])
        self.assertEqual(result["reason"], "workspace_dir_mismatch")
        mock_inject.assert_not_called()

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
            with patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()):
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
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()):
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertTrue(result["ok"])
        self.assertEqual(len(inject_calls), 2,
            f"trust auto-answer must trigger a retry inject; got {len(inject_calls)} calls")
        self.assertTrue(any("trust-retry" in call["buffer"] for call in inject_calls),
            "second inject's buffer name should mark it as the trust retry")
        emitted = [ev for ev in self._read_events() if ev.get("event") == "leader_panes.trust_auto_answered"]
        self.assertEqual(len(emitted), 1)

    def test_opt_in_delivery_wrap_schedules_retry_when_prompt_does_not_dismiss(self) -> None:
        """Spark MEDIUM sweep #3 finding #1: when the bounded poll exhausts,
        delivery must SCHEDULE a kind='trust_retry' scheduled_event with the
        bounded backoff (attempt 2 = +5s) instead of dead-ending in 'failed'.
        Holds the message in 'failed' status so _deliver_pending_messages does
        NOT race the scheduled consumer. Emits both _retry_needed and
        _retry_scheduled events."""
        from team_agent.messaging import delivery as delivery_mod
        from team_agent.message_store import MessageStore
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        message_id = self._seed_message()
        state = self._state()
        inject_calls: list[Any] = []

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            inject_calls.append({"target": target, "buffer": buffer_name})
            return self._trust_envelope()

        def stuck_capture(target):
            return "Do you trust the contents of this directory and want to allow execution of source files?\n"

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
             patch("team_agent.messaging.delivery._capture_pane_tail", side_effect=stuck_capture), \
             patch("time.sleep", return_value=None), \
             patch("time.monotonic", side_effect=[0.0, 0.0, 100.0, 100.0]):
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertFalse(result["ok"])
        self.assertEqual(result["status"], "retry_scheduled")
        self.assertEqual(result["reason"], "trust_prompt_not_dismissed_after_answer")
        self.assertEqual(result["next_attempt"], 2)
        self.assertEqual(result["max_attempts"], delivery_mod._TRUST_RETRY_MAX_ATTEMPTS)
        self.assertIsNotNone(result.get("scheduled_event_id"))
        self.assertIsNotNone(result.get("scheduled_retry_at"))
        self.assertEqual(len(inject_calls), 1,
            "retry must be DEFERRED to scheduler; only the original inject runs in this call")
        # scheduled_events row exists with kind=trust_retry.
        store = MessageStore(self.workspace)
        with store.connect() as conn:
            rows = conn.execute(
                "select kind, payload_json, due_at, status from scheduled_events where kind='trust_retry'"
            ).fetchall()
        self.assertEqual(len(rows), 1)
        emitted = {ev.get("event") for ev in self._read_events()}
        self.assertIn("leader_panes.trust_auto_answer_retry_needed", emitted)
        self.assertIn("leader_panes.trust_auto_answer_retry_scheduled", emitted)

    def test_trust_retry_scheduled_event_fires_and_re_attempts_delivery(self) -> None:
        """Spark MEDIUM sweep #3 finding #1: the scheduled consumer must re-run
        the delivery attempt. Force the trust_retry event due immediately, fire
        the scheduler, and observe a SECOND inject call plus a
        leader_panes.trust_auto_answer_retry_attempted event."""
        from team_agent.messaging import delivery as delivery_mod
        from team_agent.messaging.scheduler import _fire_due_scheduled_events
        from team_agent.message_store import MessageStore
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        message_id = self._seed_message()
        state = self._state()
        inject_calls: list[Any] = []
        responses = iter([self._trust_envelope(), self._ok_envelope()])

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            inject_calls.append({"target": target, "buffer": buffer_name})
            return next(responses)

        def stuck_capture(target):
            return "Do you trust the contents of this directory and want to allow execution of source files?\n"

        store = MessageStore(self.workspace)
        event_log = EventLog(self.workspace)
        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
             patch("team_agent.messaging.delivery._capture_pane_tail", side_effect=stuck_capture), \
             patch("time.sleep", return_value=None), \
             patch("time.monotonic", side_effect=[0.0, 0.0, 100.0, 100.0]), \
             patch("team_agent.state.load_runtime_state", return_value=state):
            # Initial delivery → retry scheduled.
            delivery_mod._deliver_pending_message(self.workspace, state, message_id)
            # Force the scheduled retry to be due now.
            with store.connect() as conn:
                conn.execute(
                    "update scheduled_events set due_at = ? where kind = 'trust_retry'",
                    ("1970-01-01T00:00:00+00:00",),
                )
            # Fire — consumer re-attempts inject (second response is ok).
            _fire_due_scheduled_events(self.workspace, store, event_log)

        self.assertEqual(len(inject_calls), 2,
            f"trust_retry consumer must invoke a second inject; got {len(inject_calls)} total")
        emitted = {ev.get("event") for ev in self._read_events()}
        self.assertIn("leader_panes.trust_auto_answer_retry_attempted", emitted)

    def test_trust_retry_max_attempts_emits_exhausted_and_marks_failed(self) -> None:
        """Spark MEDIUM sweep #3 finding #1: after _TRUST_RETRY_MAX_ATTEMPTS
        retry_needed cycles, the consumer must emit a TERMINAL
        leader_panes.trust_auto_answer_exhausted event and stop scheduling.
        Drive the wrap with _trust_retry_attempt=MAX to simulate the final
        cycle."""
        from team_agent.messaging import delivery as delivery_mod
        from team_agent.message_store import MessageStore
        os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = "1"
        message_id = self._seed_message()
        state = self._state()
        inject_calls: list[Any] = []

        def fake_inject(target, text, submit_key, buffer_name, **kwargs):
            inject_calls.append({"target": target, "buffer": buffer_name})
            return self._trust_envelope()

        def stuck_capture(target):
            return "Do you trust the contents of this directory and want to allow execution of source files?\n"

        with patch("team_agent.messaging.delivery._tmux_inject_text", side_effect=fake_inject), \
             patch("team_agent.messaging.delivery._tmux_window_exists", return_value=True), \
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
             patch("team_agent.messaging.delivery._capture_pane_tail", side_effect=stuck_capture), \
             patch("time.sleep", return_value=None), \
             patch("time.monotonic", side_effect=[0.0, 0.0, 100.0, 100.0] * 10):
            result = delivery_mod._deliver_pending_message(
                self.workspace, state, message_id,
                _trust_retry_attempt=delivery_mod._TRUST_RETRY_MAX_ATTEMPTS,
            )

        self.assertFalse(result["ok"])
        self.assertEqual(result["status"], "trust_auto_answer_exhausted")
        self.assertEqual(result["reason"], "trust_auto_answer_exhausted")
        emitted = [ev for ev in self._read_events()
                   if ev.get("event") == "leader_panes.trust_auto_answer_exhausted"]
        self.assertEqual(len(emitted), 1)
        self.assertEqual(emitted[0]["attempts"], delivery_mod._TRUST_RETRY_MAX_ATTEMPTS)
        # No NEW scheduled retry was added — the exhausted branch is terminal.
        store = MessageStore(self.workspace)
        with store.connect() as conn:
            rows = conn.execute(
                "select count(*) from scheduled_events where kind='trust_retry'"
            ).fetchall()
        self.assertEqual(rows[0][0], 0)

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
             patch("team_agent.messaging.leader_panes._tmux_inject_text", return_value=_ok_inject()), \
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
             patch("team_agent.messaging.leader_panes._tmux_inject_text") as mock_inject:
            result = delivery_mod._deliver_pending_message(self.workspace, state, message_id)

        self.assertFalse(result["ok"])
        self.assertEqual(len(inject_calls), 1,
            "opt-out must not retry; single inject only")
        mock_inject.assert_not_called()
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
