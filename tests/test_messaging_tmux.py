from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})

class MessagingTmuxTests(unittest.TestCase):
    def test_message_fragment_recognizes_literal_visible_long_paste(self) -> None:
        expected = (
            "Team Agent message from reviewer:\n\n"
            "### 总体判断\n\n"
            "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
            "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。\n\n"
            "[team-agent-token:msg_long]"
        )
        capture = (
            "### 总体判断\n\n"
            "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
            "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。"
        )
        self.assertTrue(runtime._capture_contains_message_fragment(capture, expected))

    def test_message_fragment_matching_ignores_generic_header(self) -> None:
        expected = "Team Agent message from player_d:\n\n投A：时间感太泛\n\n[team-agent-token:msg_aaa046530604]"

        self.assertFalse(runtime._capture_contains_message_fragment("Team Agent message from player_d:", expected))
        self.assertTrue(runtime._capture_contains_message_fragment("投A：时间感太泛", expected))

    def test_wait_for_message_ready_does_not_accept_old_header(self) -> None:
        expected = "Team Agent message from player_d:\n\n投A：时间感太泛\n\n[team-agent-token:msg_aaa046530604]"

        def fake_run_cmd(args: list[str], timeout: int = 20):
            self.assertEqual(args[:3], ["tmux", "capture-pane", "-p"])
            return Mock(returncode=0, stdout="Team Agent message from player_d:\n\n旧消息", stderr="")

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            visible, verification, _ = runtime._wait_for_message_ready(
                "%1",
                "msg_aaa046530604",
                0.0,
                expected_text=expected,
                allow_pasted_prompt=False,
            )

        self.assertFalse(visible)
        self.assertEqual(verification, "capture_missing_token")

    def test_wait_for_message_ready_accepts_only_new_pasted_prompt(self) -> None:
        pasted = "› [Pasted Content 123 chars]"

        def fake_run_cmd(args: list[str], timeout: int = 20):
            self.assertEqual(args[:3], ["tmux", "capture-pane", "-p"])
            return Mock(returncode=0, stdout=pasted, stderr="")

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            visible, verification, _ = runtime._wait_for_message_ready("%1", "msg_new", 0.0, baseline_capture="")
            old_visible, old_verification, _ = runtime._wait_for_message_ready(
                "%1",
                "msg_new",
                0.0,
                baseline_capture=pasted,
            )

        self.assertTrue(visible)
        self.assertEqual(verification, "capture_contains_new_pasted_content_prompt")
        self.assertFalse(old_visible)
        self.assertEqual(old_verification, "capture_missing_token")

    def test_leader_tmux_injection_retries_until_visible_then_submits_enter(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
            "team_agent.runtime._wait_for_message_ready",
            side_effect=[
                (False, "capture_missing_token", ""),
                (False, "capture_missing_token", ""),
                (False, "capture_missing_token", ""),
                (True, "capture_contains_token", "[team-agent-token:msg_retry]"),
            ],
        ):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_retry]",
                "Enter",
                "team-agent-test",
                attempts=2,
            )
        self.assertTrue(result["ok"])
        self.assertTrue(result["submitted"])
        self.assertEqual(result["attempts"][0]["verification"], "capture_missing_token")
        self.assertEqual(result["attempts"][1]["verification"], "capture_contains_token")
        self.assertEqual(sum(1 for call in calls if call[:3] == ["tmux", "send-keys", "-t"]), 1)
        self.assertIn("Enter", calls[-1])

    def test_leader_tmux_injection_submits_pasted_content_prompt_until_cleared(self) -> None:
        calls: list[list[str]] = []
        paste_calls: list[list[str]] = []
        send_calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                paste_calls.append(args)
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› [Pasted Content 1093 chars]" if paste_calls and len(send_calls) < 2 else "claude>"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_pasted]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_new_pasted_content_prompt")
        self.assertEqual(result["submit_verification"], "pasted_content_prompt_absent_after_submit")
        self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter"])

    def test_leader_tmux_injection_submits_new_pasted_text_prompt(self) -> None:
        paste_calls: list[list[str]] = []
        send_calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                paste_calls.append(args)
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› [Pasted text #1 +67 lines]" if paste_calls and not send_calls else "claude>"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_pasted_new]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_new_pasted_content_prompt")
        self.assertEqual(result["submit_verification"], "pasted_content_prompt_absent_after_submit")
        self.assertEqual([call[-1] for call in send_calls], ["Enter"])

    def test_leader_tmux_injection_submits_visible_message_fragment(self) -> None:
        send_calls: list[list[str]] = []
        text = (
            "Team Agent message from reviewer:\n\n"
            "### 总体判断\n\n"
            "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
            "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。\n\n"
            "[team-agent-token:msg_fragment]"
        )

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = (
                    "### 总体判断\n\n"
                    "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
                    "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。"
                )
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text("%1", text, "Enter", "team-agent-test", attempts=1)
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_message_fragment")
        self.assertEqual(result["submit_verification"], "Enter_sent_after_visible_fragment")
        self.assertEqual([call[-1] for call in send_calls], ["Enter"])

    def test_leader_tmux_injection_submits_preexisting_visible_fragment_without_repaste(self) -> None:
        calls: list[list[str]] = []
        send_calls: list[list[str]] = []
        text = (
            "Team Agent message from reviewer:\n\n"
            "### 总体判断\n\n"
            "保留称粮段，外卖备注段可以压缩，这是截图里已经进入输入框的长结果片段。\n\n"
            "[team-agent-token:msg_preexisting]"
        )

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› 保留称粮段，外卖备注段可以压缩，这是截图里已经进入输入框的长结果片段。"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text("%1", text, "Enter", "team-agent-test", attempts=1)
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_message_fragment")
        self.assertEqual(result["submit_verification"], "Enter_sent_after_visible_fragment")
        self.assertEqual(result["attempts"][0]["buffer_method"], "preexisting_prompt")
        self.assertEqual([call[-1] for call in send_calls], ["Enter"])
        self.assertFalse(any(call[:2] == ["tmux", "paste-buffer"] for call in calls))

    def test_leader_tmux_injection_exits_copy_mode_before_paste(self) -> None:
        calls: list[list[str]] = []
        mode_checks = 0

        def fake_run_cmd(args: list[str], timeout: int = 20):
            nonlocal mode_checks
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                mode_checks += 1
                proc.stdout = "1\n" if mode_checks == 1 else "0\n"
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "[team-agent-token:msg_copy_mode]"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_copy_mode]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertTrue(result["ok"])
        self.assertTrue(result["attempts"][0]["recovered_from_mode"])
        self.assertIn(["tmux", "send-keys", "-t", "%1", "-X", "cancel"], calls)

    def test_leader_tmux_injection_reports_unverified_when_pasted_prompt_stays(self) -> None:
        paste_calls: list[list[str]] = []
        send_calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                paste_calls.append(args)
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› [Pasted Content 1093 chars]" if paste_calls else "claude>"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_stuck]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertFalse(result["ok"])
        self.assertEqual(result["stage"], "submit-verification")
        self.assertEqual(result["submit_verification"], "pasted_content_prompt_still_present_after_retries")
        self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter", "Enter"])

    def test_tmux_set_buffer_uses_stdin_for_large_text(self) -> None:
        large_text = "x" * (runtime.TMUX_STDIN_BUFFER_THRESHOLD + 1)
        loaded = Mock(returncode=0, stdout="", stderr="")
        with (
            patch("team_agent.runtime._tmux_load_buffer_stdin", return_value=loaded) as load_buffer,
            patch("team_agent.runtime.run_cmd") as run_cmd,
        ):
            result = runtime._tmux_set_buffer_text("team-agent-large", large_text)
        self.assertTrue(result["ok"])
        self.assertEqual(result["method"], "stdin_load_buffer")
        self.assertEqual(result["text_bytes"], len(large_text))
        load_buffer.assert_called_once_with("team-agent-large", large_text)
        run_cmd.assert_not_called()

    def test_leader_tmux_injection_large_text_uses_stdin_bracketed_paste_and_adaptive_wait(self) -> None:
        calls: list[list[str]] = []
        wait_timeouts: list[float] = []
        large_text = "Team Agent message\n\n" + ("超长正文\n" * 12000) + "\n[team-agent-token:msg_large]"

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            if args[:2] == ["tmux", "set-buffer"]:
                raise AssertionError("large leader payload must not be passed as a command argument")
            return proc

        def fake_wait(target: str, message_id: str, timeout: float, expected_text: str = "", **kwargs):
            wait_timeouts.append(timeout)
            if timeout == 0:
                return False, "capture_missing_token", ""
            return True, "capture_contains_token", "[team-agent-token:msg_large]"

        with (
            patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            patch("team_agent.runtime._tmux_load_buffer_stdin", return_value=Mock(returncode=0, stdout="", stderr="")),
            patch("team_agent.runtime._wait_for_message_ready", side_effect=fake_wait),
            patch("team_agent.runtime.time.sleep", return_value=None),
        ):
            result = runtime._tmux_inject_text("%1", large_text, "Enter", "team-agent-large", attempts=1)

        self.assertTrue(result["ok"])
        self.assertEqual(result["attempts"][0]["buffer_method"], "stdin_load_buffer")
        self.assertGreater(result["attempts"][0]["text_bytes"], runtime.TMUX_STDIN_BUFFER_THRESHOLD)
        self.assertGreater(wait_timeouts[-1], runtime.TMUX_PASTE_MIN_READY_TIMEOUT)
        paste_call = next(call for call in calls if call[:2] == ["tmux", "paste-buffer"])
        self.assertIn("-p", paste_call)


if __name__ == "__main__":
    unittest.main(verbosity=2)
