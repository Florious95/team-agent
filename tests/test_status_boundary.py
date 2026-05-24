from __future__ import annotations

import inspect
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent import runtime, status
from team_agent.message_store import MessageStore
from team_agent.state import save_runtime_state


class StatusBoundaryTests(unittest.TestCase):
    """Pin runtime.py <-> status/ contract by exercising actual behavior,
    not just symbol presence. The lesson from 0a36ad9 was that identity-only
    boundary checks let half-landed extractions pass; every public surface
    here also gets a functional probe."""

    def test_runtime_aliases_resolve_to_status_module(self) -> None:
        pairs = [
            (runtime.status, status.status),
            (runtime.format_status, status.format_status),
            (runtime.peek, status.peek),
            (runtime.approvals, status.approvals),
            (runtime.format_approvals, status.format_approvals),
            (runtime.inbox, status.inbox),
            (runtime.format_inbox, status.format_inbox),
            (runtime._compact_status, status.compact_status),
            (runtime._compact_agent_state, status.compact_agent_state),
            (runtime._compact_task, status.compact_task),
            (runtime._compact_event, status.compact_event),
            (runtime._compact_mapping, status.compact_mapping),
            (runtime._compact_value, status.compact_value),
            (runtime._latest_result_summaries, status.latest_result_summaries),
            (runtime._result_summary_from_row, status.result_summary_from_row),
            (runtime._queued_message_statuses, status.queued_message_statuses),
            (runtime._validate_line_count, status.validate_line_count),
            (runtime._search_lines, status.search_lines),
            (runtime._format_search_matches, status.format_search_matches),
        ]
        for rt_attr, status_attr in pairs:
            self.assertIs(rt_attr, status_attr, f"{rt_attr.__name__} alias drift")
        for constant in (
            "APPROVAL_SCAN_LINES",
            "PEEK_MAX_LINES",
            "PEEK_MAX_MATCHES",
            "PEEK_SEARCH_SCAN_LINES",
            "STATUS_EVENT_LIMIT",
            "STATUS_TEXT_LIMIT",
            "PENDING_DELIVERY_STATUSES",
        ):
            self.assertEqual(getattr(runtime, constant), getattr(status, constant), f"{constant} drift")

    def test_status_helpers_have_explicit_signatures(self) -> None:
        for fn in (
            status.status,
            status.format_status,
            status.peek,
            status.approvals,
            status.format_approvals,
            status.inbox,
            status.format_inbox,
            status.compact_status,
            status.compact_agent_state,
            status.compact_task,
            status.compact_event,
            status.latest_result_summaries,
            status.queued_message_statuses,
            status.search_lines,
        ):
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{fn.__name__} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{fn.__name__} uses **kwargs")

    def test_status_modules_do_not_top_level_import_runtime(self) -> None:
        # Anti-cycle rule: top-level `from team_agent.runtime` or
        # `import team_agent.runtime` would deadlock the module-load graph.
        # Lazy imports inside function bodies are fine.
        for module_name in (
            "team_agent.status.constants",
            "team_agent.status.compact",
            "team_agent.status.queries",
            "team_agent.status.peek",
            "team_agent.status.approvals",
            "team_agent.status.inbox",
            "team_agent.status",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            for line in source.splitlines():
                if not line or line.startswith((" ", "\t")):
                    continue
                self.assertFalse(
                    line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                    f"{module_name} top-level imports runtime: {line!r}",
                )


class CompactValueProbeTests(unittest.TestCase):
    def test_long_string_truncates_with_ellipsis(self) -> None:
        long = "x" * (status.STATUS_TEXT_LIMIT + 50)
        out = status.compact_value(long)
        self.assertEqual(len(out), status.STATUS_TEXT_LIMIT)
        self.assertTrue(out.endswith("…"))

    def test_short_string_passes_through(self) -> None:
        self.assertEqual(status.compact_value("hi"), "hi")

    def test_long_list_of_primitives_summarizes_after_eight(self) -> None:
        out = status.compact_value(list(range(20)))
        self.assertEqual(out[:8], list(range(8)))
        self.assertEqual(out[-1], "... 12 more")

    def test_mixed_list_replaces_with_count(self) -> None:
        self.assertEqual(status.compact_value([{"a": 1}, {"b": 2}]), "2 item(s)")

    def test_dict_strips_noisy_keys(self) -> None:
        out = status.compact_value({"event": "x", "command": "secret", "payload": "skip"})
        self.assertIn("event", out)
        self.assertNotIn("command", out)
        self.assertNotIn("payload", out)


class CompactMappingProbeTests(unittest.TestCase):
    def test_returns_empty_dict_for_non_mapping(self) -> None:
        self.assertEqual(status.compact_mapping("not a dict", {"a"}), {})

    def test_keeps_only_listed_keys(self) -> None:
        out = status.compact_mapping({"a": 1, "b": 2, "c": 3}, {"a", "c"})
        self.assertEqual(out, {"a": 1, "c": 3})


class CompactAgentStateProbeTests(unittest.TestCase):
    def test_compact_agent_keeps_id_and_display_subset(self) -> None:
        out = status.compact_agent_state(
            "worker_a",
            {
                "status": "running",
                "provider": "claude",
                "model": "claude-sonnet-4-6",
                "session_id": "sess-1",
                "display": {"backend": "ghostty_workspace", "status": "opened", "pane_id": "%42", "ignored": True},
                "secret": "do not include",
            },
        )
        self.assertEqual(out["agent_id"], "worker_a")
        self.assertEqual(out["provider"], "claude")
        self.assertNotIn("secret", out)
        self.assertEqual(out["display"]["backend"], "ghostty_workspace")
        self.assertNotIn("ignored", out["display"])


class CompactTaskProbeTests(unittest.TestCase):
    def test_compact_task_keeps_only_known_keys(self) -> None:
        out = status.compact_task({
            "id": "task_a",
            "title": "Implement",
            "status": "pending",
            "assignee": "worker_a",
            "internal_only": "drop me",
        })
        self.assertEqual(out["id"], "task_a")
        self.assertEqual(out["assignee"], "worker_a")
        self.assertNotIn("internal_only", out)


class CompactEventProbeTests(unittest.TestCase):
    def test_compact_event_drops_command_and_payload(self) -> None:
        out = status.compact_event({
            "event": "send.deliver_attempt",
            "ts": "2026-05-25T01:02:03+00:00",
            "agent_id": "worker_a",
            "command": "secret",
            "payload": "skip",
            "prompt": "skip",
        })
        self.assertEqual(out["event"], "send.deliver_attempt")
        self.assertEqual(out["agent_id"], "worker_a")
        for blocked in ("command", "payload", "prompt"):
            self.assertNotIn(blocked, out)

    def test_compact_event_summarizes_agent_lists(self) -> None:
        out = status.compact_event({
            "event": "restart.complete",
            "agents": [{"agent_id": f"w{i}", "restart_mode": "resumed"} for i in range(12)],
        })
        self.assertEqual(out["agent_count"], 12)
        self.assertEqual(len(out["agents"]), 8)


class CompactStatusProbeTests(unittest.TestCase):
    def test_compact_status_caps_queued_and_latest_results(self) -> None:
        data = {
            "team": "leader",
            "session_name": "team-x",
            "tmux_session_present": True,
            "leader_receiver": {"status": "attached", "provider": "codex", "ignored": "drop"},
            "agents": {"alpha": {"status": "running", "provider": "claude"}},
            "agent_health": {},
            "tasks": [],
            "messages": {},
            "queued_messages": [{"message_id": f"m{i}"} for i in range(20)],
            "results": {"total": 0},
            "latest_results": [{"result_id": f"r{i}"} for i in range(20)],
            "coordinator": {"status": "running", "pid": 123, "metadata_ok": True, "schema_ok": True, "extra": "drop"},
            "last_events": [],
        }
        out = status.compact_status(data)
        self.assertEqual(out["team"], "leader")
        self.assertEqual(len(out["queued_messages"]), 8)
        self.assertEqual(len(out["latest_results"]), 5)
        self.assertEqual(out["coordinator"]["pid"], 123)
        self.assertNotIn("extra", out["coordinator"])
        self.assertNotIn("ignored", out["leader_receiver"])


class ResultSummaryFromRowProbeTests(unittest.TestCase):
    def test_extracts_summary_fields_from_envelope_string(self) -> None:
        envelope = {
            "task_id": "task_a",
            "agent_id": "worker_a",
            "status": "success",
            "summary": "did work",
        }
        row = {
            "result_id": "res_abc",
            "task_id": "task_b",
            "agent_id": "worker_b",
            "status": "submitted",
            "created_at": "2026-05-25T00:00:00+00:00",
            "envelope": json.dumps(envelope),
        }
        out = status.result_summary_from_row(row)
        self.assertEqual(out["result_id"], "res_abc")
        # envelope fields override row-level fields
        self.assertEqual(out["task_id"], "task_a")
        self.assertEqual(out["agent_id"], "worker_a")
        self.assertEqual(out["status"], "success")
        self.assertEqual(out["summary"], "did work")
        self.assertEqual(out["created_at"], "2026-05-25T00:00:00+00:00")

    def test_returns_none_on_invalid_envelope(self) -> None:
        self.assertIsNone(status.result_summary_from_row({"envelope": "{bad json"}))
        self.assertIsNone(status.result_summary_from_row({"envelope": None}))


class QueuedMessageStatusesProbeTests(unittest.TestCase):
    def test_only_visible_statuses_are_kept(self) -> None:
        with patch("team_agent.runtime._age_text", return_value="1s ago"):
            rows = [
                {"message_id": "m1", "status": "pending", "recipient": "alpha", "sender": "leader", "error": None, "created_at": "2026-05-25T00:00:00+00:00", "delivery_attempts": 1},
                {"message_id": "m2", "status": "delivered", "recipient": "alpha", "sender": "leader", "error": None, "created_at": "2026-05-25T00:00:00+00:00"},
                {"message_id": "m3", "status": "target_resolved", "recipient": "beta", "sender": "leader", "error": "stale", "created_at": "2026-05-25T00:00:00+00:00"},
            ]
            out = status.queued_message_statuses(rows)
        self.assertEqual({row["message_id"] for row in out}, {"m1", "m3"})
        m3 = next(row for row in out if row["message_id"] == "m3")
        self.assertEqual(m3["reason"], "stale")
        self.assertEqual(m3["age"], "1s ago")


class LatestResultSummariesProbeTests(unittest.TestCase):
    def test_returns_envelope_summaries_in_store_order(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-latest-") as tmp:
            store = MessageStore(Path(tmp))
            store.connect()
            envelope = {
                "schema_version": "result_envelope_v1",
                "task_id": "task_x",
                "agent_id": "alpha",
                "status": "success",
                "summary": "first done",
                "tests": [],
                "next_actions": [],
                "risks": [],
                "artifacts": [],
                "changes": [],
            }
            store.add_result(envelope)
            out = status.latest_result_summaries(store, limit=5)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["task_id"], "task_x")
        self.assertEqual(out[0]["summary"], "first done")


class FormatStatusProbeTests(unittest.TestCase):
    def _seed_workspace(self, tmp: Path) -> None:
        save_runtime_state(
            tmp,
            {
                "session_name": "team-status",
                "leader": {"id": "leader"},
                "agents": {
                    "alpha": {
                        "status": "running",
                        "provider": "claude",
                        "session_id": "sess-1",
                        "captured_via": "fs_watch",
                        "attribution_confidence": "high",
                        "window": "alpha",
                    },
                },
                "tasks": [],
            },
        )

    def test_global_format_renders_team_and_agent_lines(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-fmt-") as tmp:
            workspace = Path(tmp)
            self._seed_workspace(workspace)
            with patch("team_agent.runtime._capture_missing_sessions", return_value=[]), \
                 patch("team_agent.runtime._refresh_agent_runtime_statuses"), \
                 patch("team_agent.runtime._handle_provider_startup_prompts"), \
                 patch("team_agent.runtime._sync_agent_health"), \
                 patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime.coordinator_health", return_value={"status": "running", "pid": 1, "metadata_ok": True, "schema_ok": True}):
                text = status.format_status(workspace)
        self.assertIn("team team-status (up)", text)
        self.assertIn("alpha", text)
        self.assertIn("sid sess-1", text)

    def test_per_agent_format_raises_on_unknown_agent(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-fmt-unknown-") as tmp:
            workspace = Path(tmp)
            self._seed_workspace(workspace)
            with patch("team_agent.runtime._capture_missing_sessions", return_value=[]), \
                 patch("team_agent.runtime._refresh_agent_runtime_statuses"), \
                 patch("team_agent.runtime._handle_provider_startup_prompts"), \
                 patch("team_agent.runtime._sync_agent_health"), \
                 patch("team_agent.runtime._tmux_session_exists", return_value=True), \
                 patch("team_agent.runtime.coordinator_health", return_value={"status": "running", "pid": 1, "metadata_ok": True, "schema_ok": True}):
                with self.assertRaises(runtime.RuntimeError):
                    status.format_status(workspace, agent_id="no_such_agent")


class PeekValidationProbeTests(unittest.TestCase):
    def test_validate_line_count_rejects_zero_and_oversize(self) -> None:
        with self.assertRaises(runtime.RuntimeError):
            status.validate_line_count("--head", 0)
        with self.assertRaises(runtime.RuntimeError):
            status.validate_line_count("--head", status.PEEK_MAX_LINES + 1)

    def test_validate_line_count_accepts_in_range(self) -> None:
        status.validate_line_count("--head", 1)
        status.validate_line_count("--head", status.PEEK_MAX_LINES)


class SearchLinesProbeTests(unittest.TestCase):
    def test_isolated_matches_each_get_their_own_block(self) -> None:
        lines = ["a", "needle here", "b", "c", "d", "e", "needle again", "f"]
        out = status.search_lines(lines, "needle", context=1)
        self.assertEqual(len(out), 2)
        self.assertEqual(out[0]["line"], 2)
        self.assertEqual(out[1]["line"], 7)

    def test_close_matches_merge_into_one_block(self) -> None:
        lines = ["needle", "next", "needle", "tail"]
        out = status.search_lines(lines, "needle", context=2)
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["end_line"], 4)

    def test_truncates_after_max_matches(self) -> None:
        lines = ["needle"] * (status.PEEK_MAX_MATCHES + 5)
        out = status.search_lines(lines, "needle", context=0)
        self.assertLessEqual(len(out), status.PEEK_MAX_MATCHES)


class FormatSearchMatchesProbeTests(unittest.TestCase):
    def test_no_matches_returns_placeholder(self) -> None:
        self.assertEqual(status.format_search_matches([]), "no matches")

    def test_renders_match_block_with_line_range(self) -> None:
        rendered = status.format_search_matches([
            {"line": 7, "start_line": 6, "end_line": 9, "lines": ["a", "needle", "b"]},
        ])
        self.assertIn("match line 7 (6-9):", rendered)
        self.assertIn("needle", rendered)


class InboxProbeTests(unittest.TestCase):
    def test_inbox_returns_messages_keyed_by_agent_id(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-inbox-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            store.create_message("task_x", "leader", "alpha", "hello", reply_to=None, requires_ack=True)
            out = status.inbox(workspace, "alpha", limit=5)
        self.assertTrue(out["ok"])
        self.assertEqual(out["agent_id"], "alpha")
        self.assertEqual(len(out["messages"]), 1)

    def test_format_inbox_returns_no_messages_note_for_empty_agent(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-format-inbox-") as tmp:
            workspace = Path(tmp)
            MessageStore(workspace)
            text = status.format_inbox(workspace, "alpha")
        self.assertIn("alpha: no messages", text)
        self.assertIn("team-agent collect", text)


class ApprovalsProbeTests(unittest.TestCase):
    def test_empty_workspace_returns_no_waiting(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-approvals-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, {"session_name": None, "agents": {}, "tasks": []})
            out = status.approvals(workspace)
        self.assertTrue(out["ok"])
        self.assertFalse(out["waiting"])
        self.assertEqual(out["scan"]["lines"], status.APPROVAL_SCAN_LINES)


if __name__ == "__main__":
    unittest.main()
