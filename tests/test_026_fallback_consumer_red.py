from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent.cli import _fake_spec
from team_agent.cli.parser import main as cli_main
from team_agent import runtime
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


FALLBACK_STATUS = {"queued", "fallback_log"}


def _write_workspace(workspace: Path, *, agents: tuple[str, ...] = ("fake_impl",)) -> None:
    spec = _fake_spec(workspace)
    base_agent = dict(spec["agents"][0])
    spec["agents"] = []
    for agent_id in agents:
        item = dict(base_agent)
        item["id"] = agent_id
        item["role"] = agent_id
        spec["agents"].append(item)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": None,
            "leader": spec["leader"],
            "agents": {
                agent_id: {
                    "status": "running",
                    "provider": "fake",
                    "window": agent_id,
                    "session_id": f"session-{agent_id}",
                }
                for agent_id in agents
            },
            "tasks": spec["tasks"],
        },
    )


def _append_fallback_entry(workspace: Path, sender: str, title: str, body: str) -> Path:
    inbox = workspace / ".team" / "runtime" / "leader-inbox.log"
    inbox.parent.mkdir(parents=True, exist_ok=True)
    with inbox.open("a", encoding="utf-8") as handle:
        handle.write(f"\n[2026-05-28 12:00:00] fallback reason=leader_pane_wrong_command error=-\n")
        handle.write(f"Team Agent message from {sender} for task_impl:\n\n")
        handle.write(f"{title}\n{body}\n")
        handle.write(f"[team-agent-token:msg_{sender}]\n")
    return inbox


def _run_status_cli(workspace: Path) -> tuple[str, str]:
    stdout = io.StringIO()
    stderr = io.StringIO()
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        cli_main(["status", "--workspace", str(workspace)])
    return stdout.getvalue(), stderr.getvalue()


class FallbackConsumerAcceptanceTests(unittest.TestCase):
    def test_inbox_log_consumed_on_any_cli_invocation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug52-cli-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace)
            _append_fallback_entry(workspace, "fake_impl", "completed fallback result", "important worker result")
            _append_fallback_entry(workspace, "fake_peer", "needs leader attention", "second fallback result")

            stdout, stderr = _run_status_cli(workspace)
            combined = stdout + stderr
            first_block = combined[:500]

            self.assertIn("leader inbox", first_block.lower())
            self.assertRegex(first_block, r"\b2\b.*\bnew\b|\bnew\b.*\b2\b")
            self.assertIn("completed fallback result", first_block)
            self.assertIn("team-agent inbox", first_block)
            self.assertLess(combined.find("leader inbox"), combined.find("team "), combined)

    def test_cursor_advances_after_consume(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug52-cursor-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace)
            inbox = _append_fallback_entry(workspace, "fake_impl", "one shot fallback", "only once")

            first_stdout, first_stderr = _run_status_cli(workspace)
            first_combined = first_stdout + first_stderr
            cursor = workspace / ".team" / "runtime" / "leader-inbox.cursor"

            self.assertIn("one shot fallback", first_combined)
            self.assertTrue(cursor.exists(), "CLI consumer must persist a cursor after printing a summary")
            self.assertEqual(int(cursor.read_text(encoding="utf-8").strip()), inbox.stat().st_size)

            second_stdout, second_stderr = _run_status_cli(workspace)
            second_combined = second_stdout + second_stderr
            self.assertNotIn("one shot fallback", second_combined)
            self.assertNotIn("leader inbox", second_combined.lower())

    def test_worker_to_leader_fallback_ok_is_true(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug52-fallback-ok-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace)
            state_path = workspace / ".team" / "runtime" / "state.json"
            state = runtime.load_runtime_state(workspace)
            state["leader_receiver"] = {
                "mode": "direct_tmux",
                "pane_id": "%52",
                "provider": "codex",
                "session": "bad-leader",
            }
            save_runtime_state(workspace, state)
            self.assertTrue(state_path.exists())

            with patch(
                "team_agent.messaging.leader._validate_leader_receiver",
                return_value={"ok": False, "reason": "leader_pane_wrong_command", "error": "current_command=broot"},
            ), patch(
                "team_agent.messaging.leader._rediscover_leader_receiver",
                return_value={"status": "not_found"},
            ):
                result = runtime.send_message(
                    workspace,
                    "leader",
                    "durable fallback should be success",
                    task_id="task_impl",
                    sender="fake_impl",
                    requires_ack=True,
                )

            self.assertIs(result.get("ok"), True)
            self.assertIn(result.get("status"), FALLBACK_STATUS)
            self.assertIn("fallback_path", result)
            self.assertTrue(Path(result["fallback_path"]).exists())

    def test_summary_size_budget(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug52-budget-") as tmp:
            workspace = Path(tmp)
            _write_workspace(workspace)
            for index in range(50):
                _append_fallback_entry(
                    workspace,
                    f"worker_{index}",
                    f"very long fallback title {index}",
                    "x" * 300,
                )

            stdout, stderr = _run_status_cli(workspace)
            first_status_index = (stdout + stderr).find("team ")
            summary = (stdout + stderr)[:first_status_index if first_status_index >= 0 else 500]

            self.assertLessEqual(len(summary), 500, summary)
            self.assertRegex(summary.lower(), r"truncat|more")
            self.assertIn("team-agent inbox", summary)


if __name__ == "__main__":
    unittest.main(verbosity=2)
