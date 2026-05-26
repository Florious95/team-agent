from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent.cli.commands import cmd_watch
from team_agent.message_store import MessageStore
from team_agent.paths import logs_dir
from team_agent.watch import WatchCursor, collect_watch_lines


class Gap18WatchTests(unittest.TestCase):
    def test_watch_renders_supported_event_classes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-watch-") as tmp:
            workspace = Path(tmp)
            _write_events(
                workspace,
                [
                    {"event": "ignored.noise", "value": "nope"},
                    {"event": "result_received", "agent_id": "worker_a", "summary": "done\nwith details"},
                    {"event": "leader_receiver.injected", "message_id": "msg_123456789abc", "recipient": "worker_b"},
                    {"event": "leader_receiver.submitted", "message_id": "msg_abcdef987654", "target": "%12"},
                    {"event": "send.failed", "recipient": "worker_c", "reason": "pane missing"},
                    {"event": "leader_receiver.rebind_required", "old_pane_id": "%old", "reason": "pane_gone"},
                    {
                        "event": "leader.api_error",
                        "error_class": "Overloaded",
                        "provider": "codex",
                        "matched_pattern_snippet": "API Error: Overloaded",
                    },
                ],
            )

            lines = collect_watch_lines(workspace, WatchCursor())

        self.assertEqual(
            lines,
            [
                "result_received: worker_a -> done with details",
                "leader_receiver.injected: msg_12345678 -> worker_b",
                "leader_receiver.injected: msg_abcdef98 -> %12",
                "send.failed: worker_c reason=pane missing",
                "leader_receiver.rebind_required: pane=%old reason=pane_gone",
                "leader.api_error: Overloaded provider=codex snippet=API Error: Overloaded",
            ],
        )

    def test_watch_polls_latest_results_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-watch-results-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            store.add_result(
                {
                    "schema_version": "result_envelope_v1",
                    "task_id": "task-1",
                    "agent_id": "worker_a",
                    "status": "success",
                    "summary": "finished " + ("x" * 100),
                    "artifacts": [],
                    "changes": [],
                    "tests": [],
                    "risks": [],
                    "next_actions": [],
                }
            )
            cursor = WatchCursor()

            first = collect_watch_lines(workspace, cursor)
            second = collect_watch_lines(workspace, cursor)

        self.assertEqual(first, ["result_received: worker_a -> finished " + ("x" * 71)])
        self.assertEqual(second, [])

    def test_watch_exits_cleanly_on_keyboard_interrupt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-watch-cli-") as tmp:
            args = Mock(workspace=tmp, team=None)
            with patch("team_agent.watch.run_watch", side_effect=KeyboardInterrupt):
                with self.assertRaises(SystemExit) as ctx:
                    cmd_watch(args)

        self.assertEqual(ctx.exception.code, 0)

    def test_watch_ignores_archived_event_segments(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap18-watch-archives-") as tmp:
            workspace = Path(tmp)
            log_dir = logs_dir(workspace)
            log_dir.mkdir(parents=True, exist_ok=True)
            (log_dir / "events.jsonl.1").write_text(
                json.dumps({"event": "send.failed", "recipient": "archived", "reason": "old"}) + "\n",
                encoding="utf-8",
            )
            _write_events(workspace, [{"event": "send.failed", "recipient": "current", "reason": "new"}])

            lines = collect_watch_lines(workspace, WatchCursor())

        self.assertEqual(lines, ["send.failed: current reason=new"])


def _write_events(workspace: Path, events: list[dict]) -> None:
    log_dir = logs_dir(workspace)
    log_dir.mkdir(parents=True, exist_ok=True)
    text = "".join(json.dumps(event, ensure_ascii=False) + "\n" for event in events)
    (log_dir / "events.jsonl").write_text(text, encoding="utf-8")


if __name__ == "__main__":
    unittest.main(verbosity=2)
