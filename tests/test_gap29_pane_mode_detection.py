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


class Gap29PaneModeDetectionTests(unittest.TestCase):
    def test_pane_mode_cancel_uses_mode_specific_exit_key(self) -> None:
        cases = [
            ("copy-mode", ["tmux", "send-keys", "-t", "%7", "-X", "cancel"], None),
            ("tree-mode", ["tmux", "send-keys", "-t", "%7", "q"], None),
            ("view-mode", ["tmux", "send-keys", "-t", "%7", "q"], None),
            ("client-mode", ["tmux", "send-keys", "-t", "%7", "d"], None),
            ("choose-mode", ["tmux", "send-keys", "-t", "%7", "-X", "cancel"], "pane_mode_unknown_cancel_attempted"),
        ]
        for pane_mode, expected_send, expected_warning in cases:
            with self.subTest(pane_mode=pane_mode):
                send_calls: list[list[str]] = []
                display_calls = 0

                def fake_run_cmd(args: list[str], timeout: int = 20):
                    nonlocal display_calls
                    proc = Mock(returncode=0, stdout="", stderr="")
                    if args[:4] == ["tmux", "display-message", "-p", "-t"]:
                        display_calls += 1
                        proc.stdout = f"{pane_mode}\n" if display_calls == 1 else "\n"
                    elif args[:3] == ["tmux", "capture-pane", "-p"]:
                        proc.stdout = "provider prompt\n"
                    elif args[:3] == ["tmux", "send-keys", "-t"]:
                        send_calls.append(args)
                    return proc

                with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                    result = runtime._prepare_tmux_pane_for_input("%7")

                self.assertTrue(result["ok"], result)
                self.assertEqual(send_calls, [expected_send])
                self.assertEqual(result.get("warning_event"), expected_warning)

    def test_scrollback_non_input_patterns_refuse_before_paste(self) -> None:
        cases = [
            ("Do you trust the contents of this directory?\n", "codex_trust_prompt"),
            ("Press enter to log in\n", "codex_first_run_auth"),
            ("Your capability may degrade after compaction\n", "codex_compaction_warning"),
            ("Press enter to continue\n", "generic_press_enter"),
            ("Press any key to continue\n", "generic_press_enter"),
            ("Continue? (y/n)\n", "y_n_confirm"),
            ("1. Keep going\n2. Stop\n", "numbered_menu"),
            ("alauda@host ~/repo $ ", "shell_prompt_cli_dead"),
        ]
        for tail, detected in cases:
            with self.subTest(detected=detected):
                calls: list[list[str]] = []

                def fake_run_cmd(args: list[str], timeout: int = 20):
                    calls.append(args)
                    proc = Mock(returncode=0, stdout="", stderr="")
                    if args[:4] == ["tmux", "display-message", "-p", "-t"]:
                        proc.stdout = "\n"
                    elif args[:3] == ["tmux", "capture-pane", "-p"]:
                        proc.stdout = tail
                    return proc

                with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                    result = runtime._tmux_inject_text("%8", "payload", "Enter", "team-agent-gap29")

                self.assertFalse(result["ok"], result)
                self.assertEqual(result["reason"], "recipient_pane_in_non_input_mode")
                self.assertEqual(result["detected"], detected)
                self.assertFalse(any(call[:3] == ["tmux", "paste-buffer", "-t"] for call in calls))

    def test_bypass_non_input_gate_allows_auto_answer_paste(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "Do you trust the contents of this directory?\nPress enter to continue\n"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%8",
                "1",
                "Enter",
                "team-agent-gap29-auto-answer",
                attempts=1,
                provider="fake",
                bypass_non_input_gate=True,
            )

        self.assertTrue(result["ok"], result)
        self.assertTrue(any(call[:2] == ["tmux", "set-buffer"] for call in calls))
        self.assertTrue(any(call[:3] == ["tmux", "paste-buffer", "-t"] for call in calls))
        self.assertFalse(any(call[:4] == ["tmux", "display-message", "-p", "-t"] for call in calls))

    def test_send_returns_structured_non_input_envelope(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap29-send-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-gap29",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:4] == ["tmux", "display-message", "-p", "-t"]:
                    proc.stdout = "\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "Do you trust the contents of this directory?\n"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.send_message(workspace, "fake_impl", "hello")

            self.assertFalse(result["ok"], result)
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["reason"], "recipient_pane_in_non_input_mode")
            self.assertEqual(result["detected"], "codex_trust_prompt")
            self.assertEqual(result["pane_id"], "team-gap29:fake_impl")
            self.assertEqual(result["pane_mode"], "")
            self.assertIn("Do you trust", result["pane_capture_tail"])

    def test_empty_mode_and_empty_scrollback_allows_normal_paste(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:4] == ["tmux", "display-message", "-p", "-t"]:
                proc.stdout = "\n"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text("%9", "payload", "Enter", "team-agent-gap29", provider="fake")

        self.assertTrue(result["ok"], result)
        self.assertTrue(any(call[:3] == ["tmux", "paste-buffer", "-t"] for call in calls))
        self.assertTrue(any(call[:3] == ["tmux", "send-keys", "-t"] and call[-1] == "Enter" for call in calls))


if __name__ == "__main__":
    unittest.main(verbosity=2)
