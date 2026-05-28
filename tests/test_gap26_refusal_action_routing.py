from __future__ import annotations

import os
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent.state import check_team_owner, derive_leader_session_uuid


class Gap26RefusalActionRoutingTests(unittest.TestCase):
    def test_same_pane_allows_owner_even_when_uuid_drifted(self) -> None:
        workspace = Path("/tmp/team-agent-gap26")
        with patch.dict(
            os.environ,
            {
                "TMUX_PANE": "%owner",
                "TEAM_AGENT_LEADER_SESSION_UUID": "b" * 32,
            },
            clear=True,
        ):
            refusal = check_team_owner(_state(workspace, _uuid(workspace)))

        self.assertIsNone(refusal, refusal)

    def test_team_owner_mismatch_is_reserved_for_confirmed_different_live_owner(self) -> None:
        workspace = Path("/tmp/team-agent-gap26")
        with patch.dict(
            os.environ,
            {
                "TMUX_PANE": "%branch",
                "TEAM_AGENT_LEADER_PANE_ID": "%branch",
                "TEAM_AGENT_LEADER_SESSION_UUID": "c" * 32,
            },
            clear=True,
        ):
            refusal = check_team_owner(_state(workspace, _uuid(workspace)))

        self.assertIsNone(
            refusal,
            "without a confirmed-live owner pane, uuid drift alone must not produce team_owner_mismatch",
        )


def _uuid(path: Path) -> str:
    return derive_leader_session_uuid("machine-a", str(path.resolve()), "alice", "team-a")


def _state(path: Path, uuid: str) -> dict:
    return {
        "workspace": str(path),
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%owner",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": uuid,
        },
        "agents": {},
        "tasks": [],
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
