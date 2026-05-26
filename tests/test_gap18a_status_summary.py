from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent.cli.commands import cmd_status
from team_agent.errors import TeamAgentError


class Gap18StatusSummaryTests(unittest.TestCase):
    def test_status_summary_renders_five_line_triage_shape(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-status-summary-") as tmp:
            summary = "finished a long result summary " + ("x" * 100)
            data = {
                "coordinator": {"status": "running", "schema_ok": True},
                "tmux_session_present": True,
                "leader_receiver": {"pane_id": "%12", "pane_current_command": "codex"},
                "agents": {
                    "runner": {"status": "running"},
                    "busy": {"status": "busy"},
                    "idle": {"status": "running"},
                    "stopped": {"status": "stopped"},
                    "failed": {"status": "failed"},
                    "unknown": {},
                    "blocked": {"status": "running"},
                    "missing": {"status": "interrupted"},
                },
                "agent_health": {
                    "idle": {"status": "IDLE"},
                    "runner": {"status": ""},
                    "blocked": {"status": "blocked"},
                    "missing": {"status": "missing"},
                },
                "queued_messages": [{"message_id": "m1"}, {"message_id": "m2"}],
                "latest_results": [{"agent_id": "runner", "summary": summary, "created_at": "2026-05-26T00:00:00+00:00"}],
            }
            args = Mock(workspace=str(Path(tmp)), json=False, detail=False, summary=True, agent=None)

            with (
                patch("team_agent.cli.commands.runtime.status", return_value=data),
                patch("team_agent.cli.commands.runtime._age_text", return_value="3m ago"),
            ):
                text = cmd_status(args)

        lines = text.splitlines()
        self.assertEqual(len(lines), 5)
        self.assertEqual(lines[0], "coordinator: running schema_ok=True tmux=True")
        self.assertEqual(lines[1], "receiver: %12 cmd=codex")
        self.assertEqual(lines[2], "agents: 8 — running=1 busy=1 idle=1 stopped=1 failed=1 unknown=3")
        self.assertEqual(lines[3], "queued: 2 mailbox messages awaiting delivery")
        self.assertEqual(lines[4], f"latest result: runner -> {summary[:80]} @ 3m ago")

    def test_status_summary_rejects_agent_argument(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-status-summary-") as tmp:
            args = Mock(workspace=str(Path(tmp)), json=False, detail=False, summary=True, agent="developer")

            with self.assertRaisesRegex(TeamAgentError, "status --summary does not accept an agent argument"):
                cmd_status(args)

    def test_status_summary_and_json_conflict(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-status-summary-") as tmp:
            args = Mock(workspace=str(Path(tmp)), json=True, detail=False, summary=True, agent=None)

            with self.assertRaisesRegex(TeamAgentError, "--summary and --json are mutually exclusive"):
                cmd_status(args)


if __name__ == "__main__":
    unittest.main(verbosity=2)
