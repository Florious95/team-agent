from __future__ import annotations

import importlib.util
import json
import os
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli import _fake_spec as cli_fake_spec
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.simple_yaml import dumps
from team_agent.state import (
    derive_leader_session_uuid,
    save_runtime_state,
)


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _setup_claim_workspace(tmp: str, *, with_ambiguous_incident: bool, incident_ts: str | None = None) -> tuple[Path, str, str, str]:
    workspace = Path(tmp)
    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-stage13"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    workspace_abspath = str(workspace.resolve())
    user = os.environ.get("USER") or os.environ.get("USERNAME") or ""
    owner_uuid = derive_leader_session_uuid("mfp-stage13", workspace_abspath, user, "current")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "team_dir": str(team_dir),
            "workspace": workspace_abspath,
            "session_name": "team-stage13",
            "leader": spec["leader"],
            "team_owner": {
                "pane_id": "%stale",
                "provider": "codex",
                "machine_fingerprint": "mfp-stage13",
                "leader_session_uuid": owner_uuid,
                "owner_epoch": 0,
            },
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": "%stale",
                "leader_session_uuid": owner_uuid,
                "owner_epoch": 0,
            },
            "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
            "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}],
        },
    )
    candidate_a = "%cand_a"
    candidate_b = "%cand_b"
    if with_ambiguous_incident:
        ts = incident_ts or (datetime.now(timezone.utc) - timedelta(seconds=30)).isoformat()
        EventLog(workspace).write(
            "leader_receiver.ambiguous_candidates",
            incident_id="stage13-incident-1",
            team_id="current",
            old_pane_id="%stale",
            candidates=[candidate_a, candidate_b],
            ts=ts,
        )
    return workspace, owner_uuid, candidate_a, candidate_b


def _fake_target(pane_id: str, owner_uuid: str) -> dict[str, Any]:
    return {
        "pane_id": pane_id,
        "session_name": "team-stage13",
        "window_index": "0",
        "window_name": pane_id.lstrip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.lstrip('%')[:3]}",
        "pane_current_command": "claude.exe",
        "pane_active": True,
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid},
        "leader_session_uuid": owner_uuid,
    }


def _env_patch_for_pane(pane: str, owner_uuid: str) -> dict[str, str]:
    return {
        "TMUX_PANE": pane,
        "TEAM_AGENT_LEADER_PANE_ID": pane,
        "TEAM_AGENT_LEADER_PROVIDER": "codex",
        "TEAM_AGENT_MACHINE_FINGERPRINT": "mfp-stage13",
        "TEAM_AGENT_LEADER_SESSION_UUID": owner_uuid,
        "TEAM_AGENT_ID": "leader",
    }


class Stage13InboxHintTests(unittest.TestCase):

    def test_claim_leader_appends_inbox_hint_when_ambiguous_incident_recent(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage13-hint-present-") as tmp:
            incident_ts = (datetime.now(timezone.utc) - timedelta(seconds=42)).isoformat()
            workspace, owner_uuid, candidate_a, candidate_b = _setup_claim_workspace(
                tmp, with_ambiguous_incident=True, incident_ts=incident_ts,
            )
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            with (
                patch.dict(os.environ, _env_patch_for_pane(candidate_a, owner_uuid), clear=False),
                patch("team_agent.leader_binding.run_cmd", return_value=Mock(returncode=0, stdout="codex\n", stderr="")),
                patch.object(runtime, "core_list_targets", return_value=targets),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["status"], "claimed")
            self.assertIn("inbox_hint", result, f"claim response missing inbox_hint; got {result}")
            hint = result["inbox_hint"]
            self.assertEqual(hint["since"], incident_ts,
                "inbox_hint.since must equal the ambiguous_candidates incident timestamp")
            self.assertEqual(hint["incident_id"], "stage13-incident-1")
            self.assertIn("team-agent inbox leader --since", hint["command"])
            self.assertIn(incident_ts, hint["command"])
            self.assertIn("ambiguous-leader", hint["message"].lower())

    def test_claim_leader_omits_inbox_hint_when_no_ambiguous_history(self) -> None:
        # Spec: "If no ambiguous-incident timestamp exists for this team (clean claim, no
        # prior ambiguity), OMIT the inbox_hint block entirely — no hint when nothing was
        # missed." claim_leader's precondition requires an incident to accept the call, so
        # the only path that reaches a 'claimed' result with no ts is when the incident
        # record itself lacks a ts field. EventLog auto-stamps every event with ts, so we
        # mock _latest_ambiguous_incident to return an incident dict with ts deliberately
        # absent. The contract under test: response['ok']=True without inbox_hint.
        from team_agent import leader as leader_module
        with tempfile.TemporaryDirectory(prefix="stage13-hint-absent-") as tmp:
            workspace, owner_uuid, candidate_a, candidate_b = _setup_claim_workspace(
                tmp, with_ambiguous_incident=False,
            )
            targets = {"ok": True, "targets": [_fake_target(candidate_a, owner_uuid), _fake_target(candidate_b, owner_uuid)]}
            incident_without_ts = {
                "incident_id": "stage13-incident-no-ts",
                "team_id": "current",
                "old_pane_id": "%stale",
                "candidates": [candidate_a, candidate_b],
                # NOTE: deliberately no "ts" key — exercises the omit branch.
            }
            with (
                patch.dict(os.environ, _env_patch_for_pane(candidate_a, owner_uuid), clear=False),
                patch("team_agent.leader_binding.run_cmd", return_value=Mock(returncode=0, stdout="codex\n", stderr="")),
                patch.object(runtime, "core_list_targets", return_value=targets),
                patch.object(leader_module, "_latest_ambiguous_incident", return_value=incident_without_ts),
                patch.object(leader_module, "_incident_already_claimed", return_value=False),
            ):
                result = runtime.claim_leader(workspace, confirm=True)

            self.assertTrue(result["ok"], result)
            self.assertEqual(result["status"], "claimed")
            self.assertNotIn("inbox_hint", result,
                f"claim with an incident lacking ts must not carry inbox_hint; got {result}")

    def test_inbox_since_filter_returns_only_messages_after_timestamp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage13-inbox-since-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            # Three messages addressed to leader at controlled times. SQLite's CURRENT_TIMESTAMP
            # default is set inside create_message; we override created_at via direct SQL so
            # the test is deterministic.
            ids = [store.create_message("task_x", "worker_a", "leader", f"msg body {i}") for i in range(3)]
            t0 = (datetime.now(timezone.utc) - timedelta(minutes=10)).isoformat()
            t_mid = (datetime.now(timezone.utc) - timedelta(minutes=5)).isoformat()
            t_late = (datetime.now(timezone.utc) - timedelta(minutes=1)).isoformat()
            from contextlib import closing as _closing
            with _closing(store.connect()) as conn:
                with conn:
                    for idx, ts in zip(ids, (t0, t_mid, t_late)):
                        conn.execute("update messages set created_at = ? where message_id = ?", (ts, idx))

            full = runtime.inbox(workspace, "leader")
            self.assertEqual(len(full["messages"]), 3, full)

            since_mid = runtime.inbox(workspace, "leader", since=t_mid)
            self.assertEqual(len(since_mid["messages"]), 2,
                f"since={t_mid} should return only the 2 later messages; got {since_mid}")
            self.assertEqual(since_mid["since"], t_mid)

            since_late = runtime.inbox(workspace, "leader", since=t_late)
            self.assertEqual(len(since_late["messages"]), 1)

            future = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
            since_future = runtime.inbox(workspace, "leader", since=future)
            self.assertEqual(since_future["messages"], [])

    def test_inbox_json_output_machine_parseable(self) -> None:
        with tempfile.TemporaryDirectory(prefix="stage13-inbox-json-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            store.create_message("task_y", "worker_a", "leader", "hello from worker")
            payload = runtime.inbox(workspace, "leader")
            # Confirm it's a dict with the documented shape and roundtrips via json.
            self.assertTrue(payload.get("ok"))
            self.assertEqual(payload.get("agent_id"), "leader")
            self.assertIsInstance(payload.get("messages"), list)
            self.assertEqual(len(payload["messages"]), 1)
            self.assertEqual(payload["messages"][0]["content"], "hello from worker")
            # json roundtrip — verifies no non-serialisable types leak through
            text = json.dumps(payload, default=str)
            self.assertIn("hello from worker", text)
            self.assertIn("\"agent_id\": \"leader\"", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
