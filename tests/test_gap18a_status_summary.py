from __future__ import annotations

from unittest.mock import Mock, patch

import pytest

from team_agent.cli.commands import cmd_status
from team_agent.errors import TeamAgentError


def test_status_summary_renders_five_line_triage_shape(tmp_path) -> None:
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
        },
        "agent_health": {
            "idle": {"status": "IDLE"},
            "runner": {"status": ""},
        },
        "queued_messages": [{"message_id": "m1"}, {"message_id": "m2"}],
        "latest_results": [{"agent_id": "runner", "summary": summary, "created_at": "2026-05-26T00:00:00+00:00"}],
    }
    args = Mock(workspace=str(tmp_path), json=False, detail=False, summary=True, agent=None)

    with patch("team_agent.cli.commands.runtime.status", return_value=data), patch("team_agent.cli.commands.runtime._age_text", return_value="3m ago"):
        text = cmd_status(args)

    lines = text.splitlines()
    assert len(lines) == 5
    assert lines[0] == "coordinator: running schema_ok=True tmux=True"
    assert lines[1] == "receiver: %12 cmd=codex"
    assert lines[2] == "agents: 6 — running=2 busy=1 idle=1 stopped=1 failed=1"
    assert lines[3] == "queued: 2 mailbox messages awaiting delivery"
    assert lines[4] == f"latest result: runner -> {summary[:80]} @ 3m ago"


def test_status_summary_and_json_conflict(tmp_path) -> None:
    args = Mock(workspace=str(tmp_path), json=True, detail=False, summary=True, agent=None)

    with pytest.raises(TeamAgentError, match="--summary and --json are mutually exclusive"):
        cmd_status(args)
