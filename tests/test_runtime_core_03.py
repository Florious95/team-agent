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

class RuntimeTests03(unittest.TestCase):
    def test_launch_blocks_on_compatible_api_smoke_failure_before_worker_windows(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-smoke-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0].update(
                {
                    "provider": "claude_code",
                    "model": "deepseek-v4-pro-bad",
                    "auth_mode": "compatible_api",
                    "profile": "deepseek",
                    "credential_ref": "profile:deepseek",
                }
            )
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro-bad",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            adapter = Mock()
            adapter.command_name = "claude"
            adapter.is_installed.return_value = True
            with (
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.profiles.urllib.request.urlopen", side_effect=Exception("model rejected sk-do-not-print")),
                self.assertRaises(TeamAgentRuntimeError) as ctx,
            ):
                runtime.launch(spec_path, auto_approve=True)
            message = str(ctx.exception)
            self.assertIn("provider profile smoke check failed", message)
            self.assertNotIn("sk-do-not-print", message)

    def test_quick_start_reports_proxy_connectivity_profile_smoke_blocker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-proxy-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir()
            (team / "TEAM.md").write_text(
                """---
name: proxy-team
objective: Proxy smoke failure.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "HTTPS_PROXY=http://user:secret@proxy.local:8443",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            (team / "agents" / "coder.md").write_text(
                """---
name: coder
role: Implementation Worker
provider: claude_code
auth_mode: compatible_api
profile: deepseek
tools:
  - fs_read
  - mcp_team
---

Work.
""",
                encoding="utf-8",
            )
            with (
                patch.dict(
                    os.environ,
                    {
                        "HTTPS_PROXY": "http://user:secret@proxy.local:8443",
                        "https_proxy": "http://user:secret@proxy.local:8443",
                    },
                ),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.profiles.urllib.request.urlopen", side_effect=Exception("Connection reset by peer")),
            ):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(team)
                finally:
                    os.chdir(cwd)
            self.assertFalse(result["ok"])
            self.assertEqual(result["step"], "preflight")
            self.assertTrue(any("proxy_connectivity_failed" in item for item in result["blockers"]))
            rendered = json.dumps(result, ensure_ascii=False)
            self.assertIn("proxy.local:8443", rendered)
            self.assertNotIn("secret", rendered)
            self.assertNotIn("sk-do-not-print", rendered)

    def test_compatible_api_smoke_reports_ambient_proxy_choice_by_default(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-ambient-proxy-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir()
            (team / "TEAM.md").write_text(
                """---
name: ambient-proxy-team
objective: Ambient proxy reported.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            (team / "agents" / "coder.md").write_text(
                """---
name: coder
role: Implementation Worker
provider: claude_code
auth_mode: compatible_api
profile: deepseek
tools:
  - fs_read
  - mcp_team
---

Work.
""",
                encoding="utf-8",
            )
            with (
                patch.dict(os.environ, {"HTTPS_PROXY": "http://user:ambient-secret@proxy.local:8443"}),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.profiles.urllib.request.urlopen", side_effect=Exception("Connection reset by peer")),
            ):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(team)
                finally:
                    os.chdir(cwd)
            self.assertFalse(result["ok"])
            self.assertTrue(any("proxy_source=ambient" in item for item in result["blockers"]))
            self.assertTrue(any("PROXY_MODE=direct" in item for item in result["next_actions"]))
            rendered = json.dumps(result, ensure_ascii=False)
            self.assertIn("proxy.local:8443", rendered)
            self.assertNotIn("ambient-secret", rendered)
            self.assertNotIn("sk-do-not-print", rendered)

    def test_preflight_reports_invalid_codex_model_before_quick_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-preflight-model-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8").replace("model: gpt-5.5\n", "model: GPT-5.3-Codex-Spark\n"),
                encoding="utf-8",
            )
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            adapter.validate_model.return_value = {
                "ok": False,
                "provider": "codex",
                "model": "GPT-5.3-Codex-Spark",
                "reason": "model_id_not_exact",
                "suggested_model": "gpt-5.3-codex-spark",
            }
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                preflight = runtime.preflight(team)
            self.assertFalse(preflight["ok"])
            models = next(check for check in preflight["checks"] if check["name"] == "models")
            self.assertFalse(models["ok"])
            self.assertEqual(models["checks"][0]["suggested_model"], "gpt-5.3-codex-spark")

    def test_worker_delivery_submits_new_pasted_text_prompt_once_with_adaptive_wait(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-new-paste-prompt-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []
            wait_timeouts: list[float] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "codex>"
                return proc

            def fake_wait(target: str, message_id: str, timeout: float, expected_text: str = ""):
                wait_timeouts.append(timeout)
                return True, "capture_contains_pasted_content_prompt", "› [Pasted text #1 +67 lines]"

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._wait_for_worker_message_ready", side_effect=fake_wait),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                sent = runtime.send_message(workspace, "fake_impl", "x" * 6000, timeout=30)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted")
            self.assertEqual(len(paste_calls), 1)
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])
            self.assertEqual(sent["paste_attempts"][0]["verification"], "capture_contains_pasted_content_prompt")
            self.assertLess(wait_timeouts[0], 30)
            self.assertEqual(wait_timeouts[0], runtime.TMUX_PASTE_MIN_READY_TIMEOUT)

    def test_worker_pasted_content_prompt_retries_enter_until_submitted(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-enter-retry-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "codex>" if len(send_calls) >= 2 else "› [Pasted Content 1093 chars]"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "long payload", timeout=0.01)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted")
            self.assertEqual(sent["submit_verification"], "pasted_content_prompt_absent_after_submit")
            self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter"])
            self.assertEqual(sent["submit_attempts"][0]["verification"], "pasted_content_prompt_still_present")
            self.assertEqual(sent["submit_attempts"][1]["verification"], "pasted_content_prompt_absent")

    def test_worker_pasted_content_prompt_reports_unverified_when_enter_does_not_submit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-enter-stuck-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "› [Pasted Content 1093 chars]"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "long payload", timeout=0.01)
            self.assertFalse(sent["ok"])
            self.assertEqual(sent["status"], "injected_unverified")
            self.assertEqual(sent["submit_verification"], "pasted_content_prompt_still_present_after_retries")
            self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter", "Enter"])
            events = _events(workspace)
            unverified = next(e for e in events if e["event"] == "send.unverified")
            self.assertEqual(unverified["submit_verification"], "pasted_content_prompt_still_present_after_retries")

    def test_delivery_claim_prevents_duplicate_worker_injection(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-duplicate-delivery-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "session",
                "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            message_id = MessageStore(workspace).create_message(None, "gpt", "fake_impl", "hello")
            paste_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                if args[:2] == ["tmux", "paste-buffer"]:
                    paste_calls.append(args)
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    return Mock(returncode=0, stdout=message_id, stderr="")
                return Mock(returncode=0, stdout="", stderr="")

            with (
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                first = runtime._deliver_pending_message(workspace, state, message_id)
                second = runtime._deliver_pending_message(workspace, state, message_id)

            self.assertTrue(first["ok"])
            self.assertEqual(first["status"], "visible")
            self.assertTrue(second["ok"])
            self.assertEqual(second["status"], "visible")
            self.assertEqual(second["reason"], "message_already_claimed")
            self.assertEqual(len(paste_calls), 1)
            attempts = [e for e in _events(workspace) if e["event"] == "send.deliver_attempt" and e["message_id"] == message_id]
            self.assertEqual(len(attempts), 1)

    def test_ghostty_attach_args_split_tmux_command(self) -> None:
        display_session = runtime._ghostty_display_session_name("team-hello-world-team", "coder")
        args = runtime._ghostty_attach_args(display_session, "team-agent:coder:前沿")
        self.assertEqual(
            args,
            [
                "open",
                "-na",
                "Ghostty.app",
                "--args",
                "--title=team-agent:coder:前沿",
                "-e",
                "tmux",
                "attach-session",
                "-t",
                display_session,
            ],
        )
        self.assertIn("__display__coder__", display_session)
        self.assertNotIn(":", display_session)
        self.assertNotIn("sh", args)
        self.assertNotIn("-lc", args)
        self.assertNotIn("\\u", " ".join(args))


if __name__ == "__main__":
    unittest.main(verbosity=2)
