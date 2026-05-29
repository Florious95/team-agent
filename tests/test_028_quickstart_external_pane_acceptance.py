from __future__ import annotations

import inspect
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock
from unittest.mock import patch

from team_agent import runtime
from team_agent import _legacy_pane_discovery as legacy_panes
from team_agent.cli.e2e import _fake_spec
from team_agent.events import EventLog
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.leader import (
    _caller_pane_eligibility,
    _pane_is_live_leader,
    _try_readopt_leader_pane,
)
from team_agent.message_store import MessageStore
from team_agent.message_store.leader_notification_log import claim_leader_notification_delivery, leader_notification_log_rows
from team_agent.message_store.schema import initialize_schema
from team_agent.message_store.schema_migration import table_layout
from team_agent.messaging import result_delivery
from team_agent.messaging.leader import claim_leader_receiver
from team_agent.messaging.leader import _send_to_leader_receiver
from team_agent.messaging.leader_panes import (
    _leader_command_looks_usable,
    _validate_leader_receiver,
)
from team_agent.simple_yaml import dumps
from team_agent.state import apply_first_time_leader_binding, load_runtime_state, save_runtime_state, select_runtime_state, validate_leader_uuid_from_targets


REAL_CLAUDE_CODE_BINARY_COMMAND = "2.1.154"


class QuickStartExternalPaneAcceptanceTests(unittest.TestCase):
    def test_1_pane_is_usable_leader_accepts_real_2_1_154_command_when_cwd_matches_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-pane-usable-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            usable = legacy_panes._pane_is_usable_leader(pane, "claude_code", workspace)

        self.assertTrue(usable, pane)

    def test_2_resolve_leader_pane_adopts_current_client_with_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-resolve-current-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            with patch("team_agent._legacy_pane_discovery.run_cmd", side_effect=_tmux_run_cmd(current=pane, panes=[])):
                try:
                    resolved, discovery = runtime._resolve_leader_pane(
                        None,
                        "claude_code",
                        workspace=workspace,
                        require_current=True,
                    )
                except TeamAgentRuntimeError as exc:
                    self.fail(f"current client pane with matching cwd must be adopted, got: {exc}")

        self.assertEqual(discovery, "current_client")
        self.assertEqual(resolved["pane_id"], "%3622")

    def test_3_resolve_leader_pane_rejects_current_client_when_cwd_does_not_match_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-wanted-") as wanted, tempfile.TemporaryDirectory(prefix="ta-028-other-") as other:
            workspace = Path(wanted)
            pane = _pane("%3622", Path(other), command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            with patch("team_agent._legacy_pane_discovery.run_cmd", side_effect=_tmux_run_cmd(current=pane, panes=[])):
                with self.assertRaises(TeamAgentRuntimeError):
                    runtime._resolve_leader_pane(
                        None,
                        "claude_code",
                        workspace=workspace,
                        require_current=True,
                    )

    def test_4_quick_start_facing_error_does_not_recommend_nonexistent_pane_option(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-quick-start-error-") as wanted, tempfile.TemporaryDirectory(prefix="ta-028-other-") as other:
            workspace = Path(wanted)
            pane = _pane("%3622", Path(other), command=REAL_CLAUDE_CODE_BINARY_COMMAND)

            with patch("team_agent._legacy_pane_discovery.run_cmd", side_effect=_tmux_run_cmd(current=pane, panes=[])):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime._resolve_leader_pane(
                        None,
                        "claude_code",
                        workspace=workspace,
                        require_current=True,
                    )

        message = str(ctx.exception)
        self.assertIn("could not locate a tmux-managed leader pane", message)
        self.assertNotIn("--pane", message)

    def test_5_first_time_leader_binding_does_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-delivery-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "uuid-owner",
            }
            state: dict = {}
            identity = {
                "leader_session_uuid": "uuid-owner",
                "machine_fingerprint": "machine-a",
            }
            result = apply_first_time_leader_binding(
                workspace,
                state,
                dict(receiver),
                dict(pane),
                identity,
                source="launch",
            )

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_pane_wrong_command", result)

    def test_6_worker_to_leader_receiver_validation_does_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-validate-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "uuid-owner",
            }
            with patch("team_agent._legacy_pane_discovery._tmux_pane_info", return_value=dict(pane)), patch(
                "team_agent.messaging.leader_panes.run_cmd",
                return_value=Mock(returncode=0, stdout="leader idle\n", stderr=""),
            ):
                result = _validate_leader_receiver(dict(receiver))

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_pane_wrong_command", result)

    def test_7_receiver_claim_does_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-claim-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            claim_state = {
                "team_owner": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "uuid-owner",
                    "machine_fingerprint": "machine-a",
                },
                "leader_receiver": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "uuid-owner",
                    "owner_epoch": 1,
                },
            }
            claim_candidate = dict(pane)
            claim_candidate["provider"] = "claude_code"
            result = claim_leader_receiver(
                workspace,
                claim_state,
                claim_candidate,
                EventLog(workspace),
                confirm=True,
            )

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "wrong_command", result)

    def test_8_leader_command_looks_usable_accepts_any_non_empty_command(self) -> None:
        for command in [REAL_CLAUDE_CODE_BINARY_COMMAND, "node", "custom-agent-cli", "/opt/bin/some-wrapper"]:
            with self.subTest(command=command):
                self.assertTrue(_leader_command_looks_usable(command, "claude_code"))

        self.assertFalse(_leader_command_looks_usable("", "claude_code"))

    def test_9_attach_receiver_rebind_helpers_do_not_reject_real_2_1_154_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-leader-helpers-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            pane["leader_session_uuid"] = "uuid-owner"
            command_only_pane = dict(pane)
            command_only_pane.pop("leader_session_uuid", None)
            state = {"leader_receiver": {"pane_id": "%old", "provider": "claude_code", "leader_session_uuid": "uuid-owner"}}
            receiver = {"pane_id": "%old", "provider": "claude_code", "leader_session_uuid": "uuid-owner"}
            owner_record = {"pane_id": "%old", "provider": "claude_code", "leader_session_uuid": "uuid-owner"}
            targets = {"ok": True, "targets": [dict(pane)]}

            eligibility = _caller_pane_eligibility(dict(command_only_pane), workspace)
            readopt = _try_readopt_leader_pane(
                workspace,
                state,
                receiver,
                dict(pane),
                targets,
                owner_record,
                "claude_code",
                "manual",
                EventLog(workspace),
            )

        self.assertTrue(_pane_is_live_leader(command_only_pane), command_only_pane)
        self.assertTrue(eligibility.get("ok"), eligibility)
        self.assertIsNotNone(readopt, "attach/readopt must not reject 2.1.154 by command name")

    def test_10_state_uuid_validation_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-receiver-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "owner-uuid-from-state",
            }
            targets = {"ok": True, "targets": [dict(pane)]}

            result = validate_leader_uuid_from_targets(dict(receiver), targets)

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_uuid_missing", result)

    def test_11_receiver_validation_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-receiver-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            receiver = {
                "mode": "direct_tmux",
                "provider": "claude_code",
                "pane_id": pane["pane_id"],
                "leader_session_uuid": "owner-uuid-from-state",
            }
            with patch("team_agent._legacy_pane_discovery._tmux_pane_info", return_value=dict(pane)), patch(
                "team_agent.messaging.leader_panes.run_cmd",
                return_value=Mock(returncode=0, stdout="leader idle\n", stderr=""),
            ):
                result = _validate_leader_receiver(dict(receiver))

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "leader_uuid_missing", result)

    def test_12_receiver_claim_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-claim-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            claim_state = {
                "team_owner": {
                    "pane_id": "%3622",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "machine_fingerprint": "machine-a",
                },
                "leader_receiver": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "owner_epoch": 1,
                },
            }
            candidate = dict(pane)
            candidate["provider"] = "claude_code"
            result = claim_leader_receiver(workspace, claim_state, candidate, EventLog(workspace), confirm=True)

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "uuid_mismatch", result)

    def test_13_different_live_pane_still_cannot_claim_over_owner_without_uuid(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-no-uuid-isolation-") as tmp:
            workspace = Path(tmp)
            candidate = _pane("%not-owner", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            candidate["provider"] = "claude_code"
            claim_state = {
                "team_owner": {
                    "pane_id": "%owner",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "machine_fingerprint": "machine-a",
                },
                "leader_receiver": {
                    "pane_id": "%owner",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "owner_epoch": 1,
                },
            }
            result = claim_leader_receiver(workspace, claim_state, candidate, EventLog(workspace), confirm=True)

        self.assertFalse(result.get("ok"), result)
        self.assertIn(result.get("reason"), {"uuid_mismatch", "owner_pane_mismatch"}, result)

    def test_14_readopt_allows_owner_pane_without_injected_uuid_env(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-readopt-no-uuid-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3622", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            state = {
                "leader_receiver": {
                    "pane_id": "%old",
                    "provider": "claude_code",
                    "leader_session_uuid": "owner-uuid-from-state",
                    "owner_epoch": 4,
                }
            }
            receiver = dict(state["leader_receiver"])
            owner_record = {
                "pane_id": "%3622",
                "provider": "claude_code",
                "leader_session_uuid": "owner-uuid-from-state",
                "owner_epoch": 4,
            }
            targets = {"ok": True, "targets": [dict(pane)]}

            readopt = _try_readopt_leader_pane(
                workspace,
                state,
                receiver,
                dict(pane),
                targets,
                owner_record,
                "claude_code",
                "manual",
                EventLog(workspace),
            )

        self.assertIsNotNone(readopt, "readopt must use owner pane equality, not injected UUID env")
        self.assertEqual(readopt["pane_id"], "%3622")

    def test_15_restart_preserves_top_level_spec_session_and_team_dir_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-restart-identity-") as tmp:
            workspace = Path(tmp)
            spec, spec_path, team_dir = _write_current_team_spec(workspace)
            save_runtime_state(
                workspace,
                _restartable_state(workspace, spec, spec_path, team_dir),
            )
            started_windows: set[str] = set()

            with patch("team_agent.runtime.run_cmd", side_effect=_fake_tmux_run_cmd(started_windows)), patch(
                "team_agent.runtime.start_coordinator",
                return_value={"ok": True, "pid": 123, "status": "started"},
            ):
                result = runtime.restart(workspace, team="current")

            state_after = load_runtime_state(workspace)

        self.assertTrue(result.get("ok"), result)
        self.assertTrue(state_after.get("spec_path"), state_after)
        self.assertTrue(state_after.get("session_name"), state_after)
        self.assertTrue(state_after.get("team_dir"), state_after)

    def test_16_send_resolves_team_spec_from_team_dir_when_spec_path_is_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-send-team-dir-") as tmp:
            workspace = Path(tmp)
            spec, _spec_path, team_dir = _write_current_team_spec(workspace)
            state = _restartable_state(workspace, spec, None, team_dir)
            state.pop("spec_path", None)
            state["teams"]["current"].pop("spec_path", None)
            save_runtime_state(workspace, state)

            with patch(
                "team_agent.messaging.send._deliver_pending_message",
                return_value={"ok": True, "status": "submitted", "message_id": "msg-delivered"},
            ):
                try:
                    result = runtime.send_message(
                        workspace,
                        "fake_impl",
                        "hello despite missing spec_path",
                        sender="leader",
                        requires_ack=False,
                        wait_visible=False,
                        block_until_delivered=False,
                        team="current",
                    )
                except Exception as exc:
                    self.fail(f"send must resolve spec from team_dir instead of falling back to missing root spec: {exc}")

        self.assertTrue(result.get("ok"), result)
        self.assertNotEqual(result.get("reason"), "target_not_in_team", result)

    def test_17_worker_to_leader_external_no_uuid_pane_injects_directly_not_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-worker-leader-direct-") as tmp:
            workspace = Path(tmp)
            pane = _pane("%3190", workspace, command=REAL_CLAUDE_CODE_BINARY_COMMAND)
            state = {
                "workspace": str(workspace),
                "leader": {"id": "leader", "provider": "claude_code"},
                "team_owner": {
                    "pane_id": "%3190",
                    "provider": "claude_code",
                    "leader_session_uuid": "legacy-owner-uuid",
                    "owner_epoch": 7,
                },
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "provider": "claude_code",
                    "pane_id": "%3190",
                    "owner_epoch": 7,
                },
            }
            injected: list[tuple[str, str]] = []

            def fake_inject(target: str, text: str, *_args, **_kwargs) -> dict:
                injected.append((target, text))
                return {
                    "ok": True,
                    "verification": "submitted",
                    "turn_verification": "submitted",
                    "attempts": [],
                    "submit_attempts": [],
                }

            with patch("team_agent._legacy_pane_discovery._tmux_pane_info", return_value=dict(pane)), patch(
                "team_agent.messaging.leader_panes.run_cmd",
                return_value=Mock(returncode=0, stdout="Claude idle\n", stderr=""),
            ), patch("team_agent.messaging.leader._tmux_inject_text", side_effect=fake_inject):
                result = _send_to_leader_receiver(
                    workspace,
                    state,
                    "leader",
                    "E2E pane identity hello TOKEN-028",
                    None,
                    "developer",
                    False,
                    EventLog(workspace),
                )
            inbox = workspace / ".team" / "runtime" / "leader-inbox.log"
            fallback_created = inbox.exists()

        self.assertTrue(result.get("ok"), result)
        self.assertEqual(result.get("status"), "submitted", result)
        self.assertEqual(result.get("channel"), "direct_tmux", result)
        self.assertEqual(injected[0][0], "%3190")
        self.assertIn("TOKEN-028", injected[0][1])
        self.assertFalse(fallback_created, "direct delivery must not fall back to leader-inbox.log")

    def test_18_lease_mutation_entrypoints_route_through_dual_state_writer(self) -> None:
        mutation_sources = {
            "runtime.takeover": runtime.takeover,
            "runtime.quick_start": runtime.quick_start,
            "state.populate_team_owner_from_env": __import__("team_agent.state", fromlist=["populate_team_owner_from_env"]).populate_team_owner_from_env,
            "state.apply_first_time_leader_binding": apply_first_time_leader_binding,
            "messaging.leader.claim_leader_receiver": claim_leader_receiver,
        }

        missing = [
            name
            for name, func in mutation_sources.items()
            if "_write_lease_dual_state" not in inspect.getsource(func)
        ]

        self.assertEqual(
            missing,
            [],
            "all lease mutations must converge on _write_lease_dual_state so team_owner and leader_receiver stay atomic",
        )

    def test_19_leader_notification_log_schema_keys_by_team_and_epoch_not_uuid(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-schema-dedupe-") as tmp:
            store = MessageStore(Path(tmp))
            with store.connect() as conn:
                rows = conn.execute("pragma table_info(leader_notification_log)").fetchall()

        columns = [row["name"] for row in rows]
        primary_key = [row["name"] for row in sorted([row for row in rows if row["pk"]], key=lambda r: r["pk"])]
        self.assertIn("owner_team_id", columns)
        self.assertIn("owner_epoch", columns)
        self.assertNotIn("leader_session_uuid", primary_key)
        self.assertEqual(primary_key, ["result_id", "owner_team_id", "owner_epoch"])

    def test_20_leader_notification_claim_dedupes_same_epoch_and_reopens_after_epoch_advance(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-claim-dedupe-") as tmp:
            store = MessageStore(Path(tmp))
            try:
                first = claim_leader_notification_delivery(
                    store,
                    result_id="res-pane-identity",
                    owner_team_id="current",
                    owner_epoch=7,
                    proposed_message_id="msg-first",
                    envelope_hash="hash",
                    pane_id="%3190",
                )
                same_epoch = claim_leader_notification_delivery(
                    store,
                    result_id="res-pane-identity",
                    owner_team_id="current",
                    owner_epoch=7,
                    proposed_message_id="msg-duplicate",
                    envelope_hash="hash",
                    pane_id="%3190",
                )
                next_epoch = claim_leader_notification_delivery(
                    store,
                    result_id="res-pane-identity",
                    owner_team_id="current",
                    owner_epoch=8,
                    proposed_message_id="msg-after-claim",
                    envelope_hash="hash",
                    pane_id="%4001",
                )
            except TypeError as exc:
                self.fail(f"claim_leader_notification_delivery must accept owner_epoch and not require leader_session_uuid: {exc}")

        self.assertEqual(first["status"], "claimed_by_you")
        self.assertEqual(same_epoch["status"], "already_notified_by")
        self.assertEqual(next_epoch["status"], "claimed_by_you")
        rows = leader_notification_log_rows(store)
        self.assertEqual(len(rows), 2)

    def test_21_legacy_uuid_dedupe_schema_migrates_to_team_epoch_key(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-migrate-dedupe-") as tmp:
            workspace = Path(tmp)
            db_path = workspace / ".team" / "runtime" / "team.db"
            db_path.parent.mkdir(parents=True)
            conn = sqlite3.connect(db_path)
            conn.row_factory = sqlite3.Row
            try:
                conn.execute(
                    """
                    create table leader_notification_log (
                      result_id text not null,
                      leader_session_uuid text not null,
                      notified_message_id text not null,
                      notified_at text not null,
                      leader_pane_id_at_notify text,
                      envelope_content_hash text,
                      owner_team_id text,
                      primary key (result_id, leader_session_uuid)
                    )
                    """
                )
                conn.execute(
                    "insert into leader_notification_log values (?, ?, ?, ?, ?, ?, ?)",
                    ("res-old", "uuid-old", "msg-old", "2026-05-29T00:00:00+00:00", "%3190", "hash", "current"),
                )
                conn.commit()
                initialize_schema(conn, db_path=db_path)
                layout = table_layout(conn, "leader_notification_log")
                row_count = conn.execute("select count(*) as n from leader_notification_log").fetchone()["n"]
                pk_rows = conn.execute("pragma table_info(leader_notification_log)").fetchall()
            finally:
                conn.close()

        primary_key = [row["name"] for row in sorted([row for row in pk_rows if row["pk"]], key=lambda r: r["pk"])]
        self.assertIn("owner_epoch", layout)
        self.assertEqual(primary_key, ["result_id", "owner_team_id", "owner_epoch"])
        self.assertEqual(row_count, 1)

    def test_22_contract_contains_non_mockable_external_claude_e2e_acceptance(self) -> None:
        contract = (Path(__file__).parent / "contracts" / "0.2.8-quickstart-external-pane-contract.md").read_text(encoding="utf-8")
        self.assertIn("End-To-End Real-Machine Acceptance", contract)
        self.assertIn("official Claude Code", contract)
        self.assertIn("no `TEAM_AGENT_LEADER_SESSION_UUID`", contract)
        self.assertIn("not substitutable by unit\ntests", contract)

    def test_23_restart_preserves_selected_team_entry_for_post_restart_team_resolution(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-bug070-restart-team-entry-") as tmp:
            workspace = Path(tmp)
            spec, spec_path, team_dir = _write_current_team_spec(workspace)
            state = _restartable_state(workspace, spec, spec_path, team_dir)
            state.pop("teams", None)
            state["leader_receiver"] = _receiver("%3190")
            save_runtime_state(workspace, state)
            started_windows: set[str] = set()

            with patch("team_agent.runtime.run_cmd", side_effect=_fake_tmux_run_cmd(started_windows)), patch(
                "team_agent.runtime.start_coordinator",
                return_value={"ok": True, "pid": 123, "status": "started"},
            ), patch("team_agent.leader.autobind_leader_receiver_from_env", return_value=_receiver("%3190")):
                result = runtime.restart(workspace, team="current")

            state_after = load_runtime_state(workspace)

        self.assertTrue(result.get("ok"), result)
        self.assertIn("current", state_after.get("teams") or {}, state_after)
        current = state_after["teams"]["current"]
        for field in ("spec_path", "session_name", "team_dir", "leader_receiver"):
            self.assertTrue(current.get(field), current)
        selected = select_runtime_state(workspace, "current")
        self.assertEqual(selected.get("active_team_key"), "current")
        self.assertEqual(selected.get("session_name"), spec["runtime"]["session_name"])

    def test_24_post_restart_result_notification_resolves_current_team_before_delivery(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ta-028-bug070-notification-") as tmp:
            workspace = Path(tmp)
            spec, spec_path, team_dir = _write_current_team_spec(workspace)
            state = _restartable_state(workspace, spec, spec_path, team_dir)
            state.pop("teams", None)
            state["team_owner"] = _owner("%3190")
            state["leader_receiver"] = _receiver("%3190")
            save_runtime_state(workspace, state)
            store = MessageStore(workspace)
            envelope = {
                "schema_version": "result_envelope_v1",
                "task_id": "task_1",
                "agent_id": "fake_impl",
                "status": "success",
                "summary": "post-restart report",
                "artifacts": [],
                "changes": [],
                "next_actions": [],
                "risks": [],
                "tests": [],
            }
            result_id = store.add_result(envelope, owner_team_id="current")
            watcher_id = store.create_result_watcher("task_1", "fake_impl", None, owner_team_id="current")
            watcher = next(row for row in store.result_watchers(owner_team_id="current") if row["watcher_id"] == watcher_id)
            result = {
                "result_id": result_id,
                "task_id": "task_1",
                "agent_id": "fake_impl",
                "status": "success",
                "summary": "post-restart report",
            }
            deliveries: list[dict] = []

            def fake_send(*_args, **_kwargs) -> dict:
                deliveries.append({"called": True})
                return {"ok": True, "status": "submitted", "message_id": "msg-notified"}

            with patch("team_agent.messaging.internal_delivery._send_single_message_unlocked", side_effect=fake_send):
                notified = result_delivery.notify_result_watchers(
                    workspace,
                    result,
                    EventLog(workspace),
                    watchers=[watcher],
                )

        self.assertTrue(notified, notified)
        self.assertTrue(notified[0].get("ok"), notified)
        self.assertNotIn("team 'current' not found", str(notified[0].get("error") or ""))
        self.assertEqual(len(deliveries), 1, "post-restart report_result notification must reach internal delivery")


def _pane(pane_id: str, cwd: Path, *, command: str) -> dict[str, str]:
    return {
        "pane_id": pane_id,
        "session_name": "remote-control",
        "window_index": "1",
        "window_name": "leader",
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": command,
        "pane_active": "1",
        "pane_current_path": str(cwd),
        "session_attached": "1",
        "pane_in_mode": "0",
    }


def _tmux_line(pane: dict[str, str]) -> str:
    return "\t".join(
        [
            pane["pane_id"],
            pane["session_name"],
            pane["window_index"],
            pane["window_name"],
            pane["pane_index"],
            pane["pane_tty"],
            pane["pane_current_command"],
            pane["pane_active"],
            pane["pane_current_path"],
            pane["session_attached"],
            pane["pane_in_mode"],
        ]
    )


def _tmux_run_cmd(*, current: dict[str, str] | None, panes: list[dict[str, str]]):
    def fake_run_cmd(args: list[str], timeout: int = 20):
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
            if current is None:
                proc.returncode = 1
                proc.stderr = "no current client"
            else:
                proc.stdout = _tmux_line(current)
            return proc
        if args[:3] == ["tmux", "list-panes", "-a"]:
            proc.stdout = "\n".join(_tmux_line(pane) for pane in panes)
            return proc
        raise AssertionError(args)

    return fake_run_cmd


def _write_current_team_spec(workspace: Path) -> tuple[dict, Path, Path]:
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    spec = _fake_spec(workspace)
    spec["team"]["name"] = "current"
    spec["runtime"]["session_name"] = "team-current"
    spec_path = team_dir / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec, spec_path, team_dir


def _restartable_state(workspace: Path, spec: dict, spec_path: Path | None, team_dir: Path) -> dict:
    state = {
        "workspace": str(workspace),
        "spec_path": str(spec_path) if spec_path is not None else None,
        "team_dir": str(team_dir),
        "session_name": spec["runtime"]["session_name"],
        "active_team_key": "current",
        "leader": spec["leader"],
        "agents": {
            "fake_impl": {
                "status": "stopped",
                "provider": "fake",
                "agent_id": "fake_impl",
                "window": "fake_impl",
                "session_id": "fake-session-1",
                "mcp_config": str(workspace / ".team/runtime/mcp/fake_impl.json"),
            }
        },
        "tasks": spec["tasks"],
        "display_backend": "none",
    }
    compact = dict(state)
    compact.pop("teams", None)
    state["teams"] = {"current": dict(compact)}
    return state


def _owner(pane_id: str) -> dict:
    return {
        "pane_id": pane_id,
        "provider": "claude_code",
        "owner_epoch": 7,
        "leader_session_uuid": "legacy-uuid",
    }


def _receiver(pane_id: str) -> dict:
    return {
        "mode": "direct_tmux",
        "provider": "claude_code",
        "pane_id": pane_id,
        "owner_epoch": 7,
    }


def _fake_tmux_run_cmd(started_windows: set[str]):
    def fake_run_cmd(args: list[str], timeout: int = 20):
        proc = Mock(returncode=1 if args[:2] == ["tmux", "has-session"] else 0, stdout="", stderr="")
        if args[:3] == ["tmux", "new-session", "-d"]:
            started_windows.add(args[6])
        elif args[:2] == ["tmux", "new-window"]:
            started_windows.add(args[5])
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            proc.stdout = "\n".join(sorted(started_windows))
        return proc

    return fake_run_cmd


if __name__ == "__main__":
    unittest.main(verbosity=2)
