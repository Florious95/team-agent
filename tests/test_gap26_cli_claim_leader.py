from __future__ import annotations

import json
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent.cli.parser import main
from team_agent.events import EventLog
from team_agent.state import load_runtime_state, save_runtime_state


UUID_A = "a" * 32


class Gap26CliClaimLeaderTests(unittest.TestCase):
    """Stage 15 CI fix: converted from pytest fixtures (tmp_path / monkeypatch / capsys)
    to unittest TestCase so the npm publish workflow's python suite (unittest discover) can
    run it. pytest.raises → unittest.assertRaises; capsys → redirect_stdout(StringIO())."""

    def test_gap26_cli_claim_leader_first_claim_wins(self) -> None:
        with tempfile.TemporaryDirectory(prefix="gap26-cli-claim-") as tmp:
            workspace = Path(tmp)
            save_runtime_state(workspace, _state())
            EventLog(workspace).write(
                "leader_receiver.ambiguous_candidates",
                incident_id="inc-1",
                old_pane_id="%old",
                candidates=["%D", "%E"],
                team_id="team-a",
                uuid_prefix=UUID_A[:12],
            )

            env_first = {
                "TEAM_AGENT_LEADER_PANE_ID": "%D",
                "TEAM_AGENT_LEADER_SESSION_UUID": UUID_A,
                "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE": UUID_A,
                "TMUX_PANE": "%D",
            }
            with (
                patch.dict(os.environ, env_first, clear=False),
                patch("team_agent.leader_binding.run_cmd", return_value=Mock(returncode=0, stdout="codex\n", stderr="")),
                patch(
                    "team_agent.runtime.core_list_targets",
                    return_value={"ok": True, "targets": [_target("%D"), _target("%E")]},
                ),
            ):
                buf = StringIO()
                with redirect_stdout(buf):
                    main(["claim-leader", "--workspace", str(workspace), "--team", "team-a", "--confirm", "--json"])
                claimed = json.loads(buf.getvalue())

            self.assertTrue(claimed["ok"])
            self.assertEqual(claimed["status"], "claimed")
            self.assertEqual(claimed["leader_receiver"]["pane_id"], "%D")
            self.assertEqual(claimed["owner_epoch"], 8)
            state = load_runtime_state(workspace)
            self.assertEqual(state["leader_receiver"]["pane_id"], "%D")
            self.assertEqual(state["team_owner"]["owner_epoch"], 8)

            env_second = {
                "TEAM_AGENT_LEADER_PANE_ID": "%E",
                "TEAM_AGENT_LEADER_SESSION_UUID": UUID_A,
                "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE": UUID_A,
                "TMUX_PANE": "%E",
            }
            with (
                patch.dict(os.environ, env_second, clear=False),
                patch("team_agent.leader_binding.run_cmd", return_value=Mock(returncode=0, stdout="codex\n", stderr="")),
                patch(
                    "team_agent.runtime.core_list_targets",
                    return_value={"ok": True, "targets": [_target("%D"), _target("%E")]},
                ),
            ):
                buf2 = StringIO()
                with redirect_stdout(buf2):
                    with self.assertRaises(SystemExit) as ctx:
                        main(["claim-leader", "--workspace", str(workspace), "--team", "team-a", "--confirm", "--json"])
                refused = json.loads(buf2.getvalue())
            self.assertEqual(ctx.exception.code, 1)
            self.assertEqual(refused["reason"], "owner_epoch_advanced")
            self.assertEqual(refused["bound_pane_id"], "%D")
            self.assertIn("lost the race", refused["error"])


def _state() -> dict:
    state = {
        "active_team_key": "team-a",
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%old",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": UUID_A,
            "owner_epoch": 7,
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%old",
            "leader_session_uuid": UUID_A,
            "owner_epoch": 7,
        },
        "agents": {},
        "tasks": [],
    }
    state["teams"] = {"team-a": dict(state)}
    return state


def _target(pane_id: str) -> dict:
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": pane_id.strip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": "codex",
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": UUID_A},
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
