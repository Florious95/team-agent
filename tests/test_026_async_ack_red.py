from __future__ import annotations

import os
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent.cli import _fake_spec
from team_agent.mcp_server.tools import TeamOrchestratorTools
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


ALLOWED_RESPONSE_KEYS = {"status", "delivery_pending", "poll_via", "message_id"}
FORBIDDEN_LEGACY_KEYS = {"ok", "submitted", "visible", "queued", "durably_stored"}


def _write_worker_pair(workspace: Path) -> None:
    spec = _fake_spec(workspace)
    base = dict(spec["agents"][0])
    spec["agents"] = []
    for agent_id in ("sender_worker", "recipient_worker"):
        item = dict(base)
        item["id"] = agent_id
        item["role"] = agent_id
        spec["agents"].append(item)
    spec["routing"]["default_assignee"] = "recipient_worker"
    spec["routing"]["rules"][0]["assign_to"] = "recipient_worker"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "session_name": None,
            "leader": spec["leader"],
            "agents": {
                "sender_worker": {
                    "status": "running",
                    "provider": "fake",
                    "window": "sender_worker",
                    "session_id": "session-sender",
                },
                "recipient_worker": {
                    "status": "running",
                    "provider": "fake",
                    "window": "recipient_worker",
                    "session_id": "session-recipient",
                },
            },
            "tasks": spec["tasks"],
        },
    )


def _mcp_send(workspace: Path) -> dict:
    with patch.dict(os.environ, {"TEAM_AGENT_ID": "sender_worker"}, clear=False):
        tools = TeamOrchestratorTools(workspace)
        return tools.send_message("recipient_worker", "async handoff", sender="sender_worker")


class AsyncAckAcceptanceTests(unittest.TestCase):
    def test_response_only_three_fields(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug55-keys-") as tmp:
            workspace = Path(tmp)
            _write_worker_pair(workspace)

            result = _mcp_send(workspace)

            self.assertLessEqual(set(result), ALLOWED_RESPONSE_KEYS, result)
            self.assertTrue({"status", "delivery_pending", "poll_via"}.issubset(result), result)
            self.assertTrue(set(result).isdisjoint(FORBIDDEN_LEGACY_KEYS), result)

    def test_status_is_accepted_and_pending_true(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug55-status-") as tmp:
            workspace = Path(tmp)
            _write_worker_pair(workspace)

            result = _mcp_send(workspace)

            self.assertEqual(result.get("status"), "accepted")
            self.assertIs(result.get("delivery_pending"), True)

    def test_no_synchronous_paste_wait(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug55-prompt-") as tmp:
            workspace = Path(tmp)
            _write_worker_pair(workspace)

            started = time.monotonic()
            with patch(
                "team_agent.messaging.send._deliver_pending_message",
                side_effect=AssertionError("MCP async ack must not wait for synchronous paste verification"),
            ):
                result = _mcp_send(workspace)
            elapsed = time.monotonic() - started

            self.assertLess(elapsed, 0.1)
            self.assertEqual(result.get("status"), "accepted")
            self.assertIs(result.get("delivery_pending"), True)

    def test_poll_via_format(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-bug55-poll-") as tmp:
            workspace = Path(tmp)
            _write_worker_pair(workspace)

            result = _mcp_send(workspace)

            poll_via = result.get("poll_via")
            self.assertIsInstance(poll_via, str)
            parts = poll_via.split()
            self.assertEqual(parts[:2], ["team-agent", "inbox"])
            self.assertEqual(len(parts), 3)
            self.assertTrue(parts[2].strip())


if __name__ == "__main__":
    unittest.main(verbosity=2)
