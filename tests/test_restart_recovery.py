from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.providers import get_adapter
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


def _fake_spec(workspace: Path) -> dict[str, Any]:
    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-agent-test"
    return spec


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


class RestartRecoveryTests(unittest.TestCase):
    def test_codex_startup_prompt_waits_after_trust_until_ready(self) -> None:
        adapter = get_adapter("codex")
        captures = [
            Mock(returncode=0, stdout="Do you trust the contents of this directory?\nPress enter to continue\n", stderr=""),
            Mock(returncode=0, stdout="OpenAI Codex\n› ", stderr=""),
        ]
        sent: list[list[str]] = []

        def fake_run(args: list[str], **_: Any) -> Mock:
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                return captures.pop(0)
            if args[:2] == ["tmux", "send-keys"]:
                sent.append(args)
                return Mock(returncode=0, stdout="", stderr="")
            raise AssertionError(f"unexpected command: {args}")

        with patch("team_agent.provider_cli.codex.subprocess.run", side_effect=fake_run), patch("team_agent.provider_cli.codex.time.sleep", return_value=None):
            handled = adapter.handle_startup_prompts("team-trust", "worker", checks=3, sleep_s=0.1)

        self.assertEqual(handled, [{"prompt": "codex_workspace_trust", "action": "sent_enter"}])
        self.assertEqual(sent, [["tmux", "send-keys", "-t", "team-trust:worker", "Enter"]])

    def test_codex_startup_prompt_skips_update_prompt(self) -> None:
        adapter = get_adapter("codex")
        captures = [
            Mock(
                returncode=0,
                stdout=(
                    "Update available! 0.131.0 -> 0.133.0\n"
                    "1. Update now\n"
                    "2. Skip\n"
                    "Press enter to continue\n"
                ),
                stderr="",
            ),
            Mock(returncode=0, stdout="OpenAI Codex\n› ", stderr=""),
        ]
        sent: list[list[str]] = []

        def fake_run(args: list[str], **_: Any) -> Mock:
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                return captures.pop(0)
            if args[:2] == ["tmux", "send-keys"]:
                sent.append(args)
                return Mock(returncode=0, stdout="", stderr="")
            raise AssertionError(f"unexpected command: {args}")

        with patch("team_agent.provider_cli.codex.subprocess.run", side_effect=fake_run), patch("team_agent.provider_cli.codex.time.sleep", return_value=None):
            handled = adapter.handle_startup_prompts("team-update", "worker", checks=3, sleep_s=0.1)

        self.assertEqual(handled, [{"prompt": "codex_update_available", "action": "sent_skip"}])
        self.assertEqual(sent, [["tmux", "send-keys", "-t", "team-update:worker", "Down", "Enter"]])

    def test_codex_runtime_prompt_skips_mid_session_update_prompt(self) -> None:
        adapter = get_adapter("codex")
        capture = Mock(
            returncode=0,
            stdout=(
                "Update available! 0.131.0 -> 0.133.0\n"
                "1. Update now\n"
                "2. Skip\n"
                "Press enter to continue\n"
            ),
            stderr="",
        )
        sent: list[list[str]] = []

        def fake_run(args: list[str], **_: Any) -> Mock:
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                return capture
            if args[:2] == ["tmux", "send-keys"]:
                sent.append(args)
                return Mock(returncode=0, stdout="", stderr="")
            raise AssertionError(f"unexpected command: {args}")

        with patch("team_agent.provider_cli.codex.subprocess.run", side_effect=fake_run):
            handled = adapter.handle_runtime_prompts("team-update", "worker")

        self.assertEqual(handled, [{"prompt": "codex_update_available", "action": "sent_skip"}])
        self.assertEqual(sent, [["tmux", "send-keys", "-t", "team-update:worker", "Down", "Enter"]])

    def test_codex_startup_prompt_does_not_treat_generic_enter_as_trust(self) -> None:
        adapter = get_adapter("codex")
        capture = Mock(returncode=0, stdout="Press enter to continue\n", stderr="")

        def fake_run(args: list[str], **_: Any) -> Mock:
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                return capture
            raise AssertionError(f"unexpected command: {args}")

        with patch("team_agent.provider_cli.codex.subprocess.run", side_effect=fake_run), patch("team_agent.provider_cli.codex.time.sleep", return_value=None):
            handled = adapter.handle_startup_prompts("team-generic", "worker", checks=1, sleep_s=0.1)

        self.assertEqual(handled, [])

    def test_restart_fails_closed_when_started_agent_is_missing_before_complete(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-missing-after-start-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-restart-missing-after-start",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "fake-session-1",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            session_exists = False
            calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                nonlocal session_exists
                calls.append(args)
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "has-session"]:
                    proc.returncode = 0 if session_exists else 1
                elif args[:3] == ["tmux", "new-session", "-d"]:
                    session_exists = True
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = ""
                elif args[:3] == ["tmux", "kill-session", "-t"]:
                    session_exists = False
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._handle_startup_prompts_and_verify_window", return_value=True),
                patch("team_agent.runtime.start_coordinator") as start_coordinator,
            ):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime.restart(workspace)

            self.assertIn("exited after start", str(ctx.exception))
            start_coordinator.assert_not_called()
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "restart.agent_missing_after_start" for e in events))
            self.assertTrue(any(e["event"] == "restart.rollback_session" and e["ok"] for e in events))
            self.assertFalse(any(e["event"] == "restart.complete" for e in events))
            self.assertIn(["tmux", "kill-session", "-t", "team-restart-missing-after-start"], calls)


if __name__ == "__main__":
    unittest.main(verbosity=2)
