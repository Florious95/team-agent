from __future__ import annotations

import json
import tempfile
import unittest
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli import _fake_spec
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


BUSY_CAPTURE = "\n".join(
    [
        "Compacting conversation history...",
        "Summarizing prior turns for context budget.",
        "Still working; no prompt marker is visible yet.",
    ]
)


class SendBusyRecipientAcceptanceTests(unittest.TestCase):
    """Gap 42 contract: busy recipients are not delivery failures.

    The diagnosis showed a real `send.failed` event with
    `stage=turn-boundary-verification`,
    `turn_verification=leader_new_turn_boundary_missing`, and
    `submit_attempts[-1].submitted=true`. These tests preserve that evidence
    shape while using only a fake tmux layer.
    """

    def test_1_submit_success_without_turn_marker_returns_success_not_yet_observed(self) -> None:
        text = _message("msg_busy", "deliver while the recipient is compacting")
        harness = FakeTmuxHarness()
        submit_attempts = [{"attempt": 1, "submitted": True, "verification": "not_required"}]

        with _patched_injection_path(harness, turn_capture=BUSY_CAPTURE, submit_attempts=submit_attempts, turn_observed=False):
            result = runtime._tmux_inject_text(
                "%42",
                text,
                "Enter",
                "team-agent-send-msg_busy",
                attempts=1,
                provider="codex",
                bypass_non_input_gate=True,
            )

        self.assertTrue(result.get("ok"), result)
        self.assertEqual(result.get("stage"), "submitted")
        self.assertIs(result.get("submitted"), True)
        self.assertEqual(result.get("turn_verification"), "not_yet_observed")
        self.assertNotEqual(result.get("turn_verification"), "leader_new_turn_boundary_missing")
        self.assertNotEqual(result.get("stage"), "turn-boundary-verification")
        self.assertEqual(result.get("submit_attempts"), submit_attempts)
        self.assertTrue(result["submit_attempts"][-1]["submitted"])
        self.assertIn(["tmux", "paste-buffer", "-t", "%42", "-b", "team-agent-send-msg_busy", "-p"], harness.calls)
        self.assertIn(["tmux", "send-keys", "-t", "%42", "Enter"], harness.calls)

    def test_2_idle_recipient_with_turn_marker_returns_verified(self) -> None:
        text = _message("msg_idle", "idle recipient shows a new prompt marker")
        idle_capture = f"❯ Team Agent message from leader:\n\nidle recipient shows a new prompt marker\n\n[team-agent-token:msg_idle]"
        harness = FakeTmuxHarness()

        with _patched_injection_path(harness, turn_capture=idle_capture, turn_observed=True):
            result = runtime._tmux_inject_text(
                "%42",
                text,
                "Enter",
                "team-agent-send-msg_idle",
                attempts=1,
                provider="codex",
                bypass_non_input_gate=True,
            )

        self.assertTrue(result.get("ok"), result)
        self.assertEqual(result.get("stage"), "submitted")
        self.assertEqual(result.get("turn_verification"), "leader_new_turn_boundary_verified")
        self.assertTrue(result["submit_attempts"][-1]["submitted"])

    def test_3_paste_failure_still_returns_failed(self) -> None:
        text = _message("msg_paste_fail", "paste should still be authoritative failure")
        harness = FakeTmuxHarness(paste_returncode=1, paste_stderr="paste failed")

        with _patched_capture_only():
            with patch("team_agent.runtime.run_cmd", side_effect=harness.run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                result = runtime._tmux_inject_text(
                    "%42",
                    text,
                    "Enter",
                    "team-agent-send-msg_paste_fail",
                    attempts=1,
                    provider="codex",
                    bypass_non_input_gate=True,
                )

        self.assertFalse(result.get("ok"), result)
        self.assertEqual(result.get("stage"), "paste-buffer")
        self.assertEqual(result.get("error"), "paste failed")
        self.assertNotIn("turn_verification", result)

    def test_4_submit_failure_still_returns_failed(self) -> None:
        text = _message("msg_submit_fail", "submit should still be authoritative failure")
        harness = FakeTmuxHarness()
        submit_attempts = [{"attempt": 1, "submitted": False, "verification": "pasted_content_prompt_still_present"}]
        submit_result = {
            "ok": False,
            "stage": "submit-verification",
            "error": "submit did not clear pasted content prompt",
            "verification": "pasted_content_prompt_still_present_after_retries",
            "attempts": submit_attempts,
        }

        with _patched_injection_path(harness, submit_result=submit_result, turn_capture=BUSY_CAPTURE, turn_observed=False):
            result = runtime._tmux_inject_text(
                "%42",
                text,
                "Enter",
                "team-agent-send-msg_submit_fail",
                attempts=1,
                provider="codex",
                bypass_non_input_gate=True,
            )

        self.assertFalse(result.get("ok"), result)
        self.assertEqual(result.get("stage"), "submit-verification")
        self.assertEqual(result.get("submit_verification"), "pasted_content_prompt_still_present_after_retries")
        self.assertEqual(result.get("submit_attempts"), submit_attempts)
        self.assertNotEqual(result.get("turn_verification"), "not_yet_observed")

    def test_5_send_message_busy_recipient_submitted_without_marker_is_delivered_not_failed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap42-send-busy-") as tmp:
            workspace = Path(tmp)
            _write_busy_workspace(workspace)
            harness = FakeTmuxHarness(list_windows=["worker"])
            submit_attempts = [{"attempt": 1, "submitted": True, "verification": "not_required"}]

            with _patched_injection_path(harness, turn_capture=BUSY_CAPTURE, submit_attempts=submit_attempts, turn_observed=False):
                sent = runtime.send_message(workspace, "worker", "deliver to busy worker", timeout=0.01)

            self.assertTrue(sent.get("ok"), sent)
            self.assertEqual(sent.get("status"), "delivered")
            self.assertEqual(sent.get("message_status"), "submitted")
            self.assertEqual(sent.get("turn_verification"), "not_yet_observed")
            self.assertTrue(sent["submit_attempts"][-1]["submitted"])
            events = _events(workspace)
            self.assertTrue(any(event["event"] == "send.submitted" for event in events), events)
            self.assertFalse(any(event["event"] == "send.failed" for event in events), events)
            submitted = next(event for event in events if event["event"] == "send.submitted")
            self.assertEqual(submitted.get("turn_verification"), "not_yet_observed")


class FakeTmuxHarness:
    def __init__(self, *, paste_returncode: int = 0, paste_stderr: str = "", list_windows: list[str] | None = None) -> None:
        self.paste_returncode = paste_returncode
        self.paste_stderr = paste_stderr
        self.list_windows = list_windows or []
        self.calls: list[list[str]] = []

    def run_cmd(self, args: list[str], timeout: int = 20) -> Mock:
        _ = timeout
        self.calls.append(args)
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:3] == ["tmux", "list-windows", "-t"]:
            proc.stdout = "\n".join(self.list_windows)
        elif args[:3] == ["tmux", "display-message", "-p"]:
            proc.stdout = "0\n"
        elif args[:3] == ["tmux", "capture-pane", "-p"]:
            proc.stdout = "codex>"
        elif args[:2] == ["tmux", "paste-buffer"]:
            proc.returncode = self.paste_returncode
            proc.stderr = self.paste_stderr
        return proc


@contextmanager
def _patched_capture_only():
    with patch("team_agent.runtime._capture_tmux_pane_text", return_value={"ok": True, "capture": ""}):
        yield


@contextmanager
def _patched_injection_path(
    harness: FakeTmuxHarness,
    *,
    turn_capture: str,
    turn_observed: bool,
    submit_attempts: list[dict[str, Any]] | None = None,
    submit_result: dict[str, Any] | None = None,
):
    if submit_attempts is None:
        submit_attempts = [{"attempt": 1, "submitted": True, "verification": "not_required"}]
    if submit_result is None:
        submit_result = {"ok": True, "verification": "not_required", "attempts": submit_attempts}

    capture_results = [
        {"ok": True, "capture": ""},
    ]

    def capture(_target: str) -> dict[str, Any]:
        if capture_results:
            return capture_results.pop(0)
        return {"ok": True, "capture": turn_capture}

    turn_verification = "leader_new_turn_boundary_verified" if turn_observed else "leader_new_turn_boundary_missing"
    with ExitStack() as stack:
        stack.enter_context(patch("team_agent.runtime.run_cmd", side_effect=harness.run_cmd))
        stack.enter_context(patch("team_agent.runtime._capture_tmux_pane_text", side_effect=capture))
        stack.enter_context(
            patch(
                "team_agent.runtime._wait_for_message_ready",
                return_value=(True, "capture_contains_token", "[team-agent-token:msg]"),
            )
        )
        stack.enter_context(patch("team_agent.runtime._submit_worker_prompt", side_effect=_record_submit(harness, submit_result)))
        stack.enter_context(
            patch(
                "team_agent.messaging.tmux_io._wait_for_leader_new_turn",
                return_value=(turn_observed, turn_verification, turn_capture),
            )
        )
        stack.enter_context(patch("team_agent.runtime.time.sleep", return_value=None))
        yield


def _record_submit(harness: FakeTmuxHarness, result: dict[str, Any]):
    def submit(target: str, capture_text: str, *, submit_key: str, settle_timeout: float) -> dict[str, Any]:
        _ = capture_text, settle_timeout
        harness.calls.append(["tmux", "send-keys", "-t", target, submit_key])
        return result

    return submit


def _message(token: str, body: str) -> str:
    return f"Team Agent message from leader:\n\n{body}\n\n[team-agent-token:{token}]"


def _write_busy_workspace(workspace: Path) -> None:
    spec = _fake_spec(workspace)
    spec["agents"][0]["id"] = "worker"
    spec["agents"][0]["provider"] = "codex"
    spec["routing"]["default_assignee"] = "worker"
    spec["routing"]["rules"] = []
    spec["tasks"][0]["assignee"] = "worker"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": "session",
            "leader": spec["leader"],
            "agents": {"worker": {"status": "busy", "provider": "codex", "window": "worker"}},
            "tasks": spec["tasks"],
        },
    )


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
