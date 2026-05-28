from __future__ import annotations

import copy
import json
import os
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Callable
from unittest.mock import Mock, patch

from team_agent import runtime
from team_agent.cli.e2e import _fake_spec
from team_agent.events import EventLog
from team_agent.simple_yaml import dumps
from team_agent.state import save_runtime_state


FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "leader_ownership_lease"
TEAM_ID = "team-refactor-maintainability"
REAL_DEAD_PANE = "%648"
REAL_CALLER_PANE = "%1827"
REAL_UUID = "46b92d5b629cad4930cde96be938475e"


class LeaderOwnershipLeaseAcceptanceTests(unittest.TestCase):
    """Gap 39 lease contract tests.

    These tests use the S0 real fixtures as the broken substrate and patch only
    live tmux/provider probes. The expected behavior is the S1 contract; current
    pre-implementation code is intentionally red.
    """

    def test_1_real_dead_receiver_claim_leader_auto_acquires_and_repairs_both_state_files(self) -> None:
        with self._workspace_from_s0_fixture() as workspace:
            target = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[target], missing={REAL_DEAD_PANE}):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertTrue(result.get("ok"), result)
            self.assertIn(result.get("status"), {"claimed", "adopted_on_restart", "already_bound"}, result)
            self._assert_state_pair_bound(workspace, REAL_CALLER_PANE)
            events = _events(workspace)
            self.assertTrue(_has_event(events, "owner.adopted_on_restart", reason="previous_owner_pane_dead"), events)
            self.assertTrue(_has_event(events, "leader_receiver.rebind_applied", new_pane_id=REAL_CALLER_PANE), events)

    def test_2_live_owner_pane_requires_confirm_and_is_not_stolen(self) -> None:
        with self._workspace_from_s0_fixture(owner_pane=REAL_DEAD_PANE, receiver_pane=REAL_DEAD_PANE) as workspace:
            owner = _leader_target(REAL_DEAD_PANE, workspace, provider="claude_code", command="claude.exe")
            caller = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[owner, caller]):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertFalse(result.get("ok"), result)
            self.assertIn(result.get("reason"), {"previous_owner_alive_refused", "force_confirm_required"}, result)
            self._assert_workspace_receiver(workspace, REAL_DEAD_PANE)
            events = _events(workspace)
            self.assertTrue(_has_event(events, reason="previous_owner_alive_refused"), events)

    def test_3_takeover_and_attach_leader_converge_on_claim_path_and_do_not_split_state(self) -> None:
        self.assertEqual(
            _fixture_json("raw_commands/03_takeover_team_refactor_claimed.stdout")["status"],
            "claimed",
            "S0 fixture anchors the real broken takeover surface.",
        )
        with self._workspace_from_s0_fixture() as workspace:
            target = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            with _leader_env(pane_id=REAL_CALLER_PANE, provider="claude", tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[target], missing={REAL_DEAD_PANE}):
                result = _result_or_error(runtime.takeover, workspace, team=TEAM_ID, confirm=True)

            self.assertTrue(result.get("ok"), result)
            self.assertEqual((result.get("team_owner") or {}).get("pane_id"), REAL_CALLER_PANE, result)
            workspace_owner = ((_workspace_state(workspace).get("teams") or {}).get(TEAM_ID) or {}).get("team_owner") or {}
            self.assertEqual(workspace_owner.get("pane_id"), REAL_CALLER_PANE)
            for field in ("pane_id", "leader_session_uuid", "machine_fingerprint", "provider", "os_user"):
                self.assertEqual(workspace_owner.get(field), result["team_owner"].get(field), field)
            events = _events(workspace)
            self.assertFalse(_has_event(events, "team_owner.takeover"), "takeover must not use a divergent legacy event path")
            self.assertTrue(_has_event(events, "owner.bound_from_caller_pane", caller_pane_id=REAL_CALLER_PANE), events)

    def test_4_claim_leader_derives_identity_from_tmux_pane_without_manual_env_exports(self) -> None:
        self.assertEqual(_fixture_json("raw_commands/03a_takeover_team_refactor_no_caller_identity.stdout")["reason"], "no_caller_identity")
        with self._workspace_from_s0_fixture() as workspace:
            target = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[target], missing={REAL_DEAD_PANE}):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertNotIn(result.get("reason"), {"no_caller_identity", "no_caller_pane"}, result)
            self.assertTrue(result.get("ok"), result)
            self._assert_state_pair_bound(workspace, REAL_CALLER_PANE)

    def test_5_cwd_enumeration_uses_realpath_subtree_and_explicit_team(self) -> None:
        with self._workspace_from_s0_fixture(owner=False, receiver=False) as workspace:
            subdir = workspace / "nested" / "leader"
            subdir.mkdir(parents=True)
            target = _leader_target(REAL_CALLER_PANE, subdir, provider="codex", command="codex")
            target["pane_current_path"] = str(subdir)
            with _leader_env(tmux_pane=REAL_CALLER_PANE, uuid=REAL_UUID), _patched_tmux(targets=[target]):
                result = _result_or_error(runtime.attach_leader, workspace, pane=None, provider="codex")

            self.assertFalse(result.get("ok"), result)
            self.assertRegex(result.get("error", ""), r"tmux leader pane|leader_uuid_missing")
            self.assertFalse(_has_event(_events(workspace), "leader_receiver.ambiguous_candidates"))

    def test_6_two_live_candidates_broadcast_once_and_do_not_silently_bind(self) -> None:
        with self._workspace_from_s0_fixture(owner_pane=REAL_DEAD_PANE, receiver_pane=REAL_DEAD_PANE) as workspace:
            left = _leader_target("%1830", workspace, provider="claude_code", command="claude.exe")
            right = _leader_target("%1831", workspace, provider="claude_code", command="claude.exe")
            injected: list[str] = []
            with _patched_leader_delivery_for_rediscovery([left, right], injected, invalid_old_pane=REAL_DEAD_PANE):
                runtime.send_message(
                    workspace,
                    "leader",
                    "fixture-backed ambiguous leader delivery",
                    sender="worker",
                    requires_ack=False,
                    team=TEAM_ID,
                )

            events = _events(workspace)
            incidents = [event for event in events if event.get("event") == "leader_receiver.ambiguous_candidates"]
            self.assertEqual(len(incidents), 1, events)
            self.assertEqual(sorted(incidents[0].get("candidates") or []), ["%1830", "%1831"])
            self.assertEqual(incidents[0].get("reason"), "force_confirm_required", incidents[0])
            self.assertEqual(sorted(injected), ["%1830", "%1831"])
            self._assert_workspace_receiver(workspace, REAL_DEAD_PANE)

    def test_7_every_refusal_emits_closed_enum_audit_event(self) -> None:
        with self._workspace_from_s0_fixture() as workspace:
            with _leader_env():
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertFalse(result.get("ok"), result)
            events = _events(workspace)
            self.assertTrue(_has_event(events, reason="caller_pane_missing"), events)
            self.assertTrue(_all_lease_reasons_closed(events), events)

    def test_8_busy_or_cd_changed_live_leader_is_not_false_positive_dead(self) -> None:
        with self._workspace_from_s0_fixture(owner_pane="%2000", receiver_pane="%2000") as workspace:
            owner = _leader_target("%2000", workspace / "subdir", provider="claude_code", command="python")
            owner["pane_current_path"] = str(workspace / "subdir")
            owner["process_tree"] = ["claude.exe", "python"]
            caller = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[owner, caller]):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "force_confirm_required", result)
            self._assert_workspace_receiver(workspace, "%2000")

    def test_9_toctou_owner_revives_before_lock_causes_epoch_refusal_not_double_bind(self) -> None:
        with self._workspace_from_s0_fixture(owner_pane="%2100", receiver_pane="%2100", owner_epoch=9) as workspace:
            caller = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            revived_owner = _leader_target("%2100", workspace, provider="claude_code", command="claude.exe")
            target_sets = [
                {"ok": True, "targets": [caller]},
                {"ok": True, "targets": [revived_owner, caller]},
            ]
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[caller, revived_owner], core_side_effect=target_sets):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "owner_epoch_advanced", result)
            self._assert_workspace_receiver(workspace, "%2100")
            self.assertTrue(_has_event(_events(workspace), reason="owner_epoch_advanced"))

    def test_10_dual_state_partial_write_is_detected_and_repaired(self) -> None:
        with self._workspace_from_s0_fixture() as workspace:
            target = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[target], missing={REAL_DEAD_PANE}):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertTrue(result.get("ok"), result)
            self._assert_state_pair_bound(workspace, REAL_CALLER_PANE)
            events = _events(workspace)
            self.assertTrue(
                _has_event(events, "leader_receiver.state_divergence_repaired")
                or _has_event(events, "leader_receiver.state_divergence_detected"),
                events,
            )

    def test_11_plain_shell_send_queues_without_self_binding_and_emits_rebind_required(self) -> None:
        with self._workspace_from_s0_fixture(owner=False, receiver=False) as workspace:
            with _leader_env():
                result = _result_or_error(
                    runtime.send_message,
                    workspace,
                    "worker",
                    "plain shell send must not become leader",
                    sender="leader",
                    requires_ack=True,
                    wait_visible=False,
                    block_until_delivered=False,
                    team=TEAM_ID,
                )

            self.assertTrue(result.get("ok"), result)
            state = _workspace_state(workspace)
            self.assertNotIn("team_owner", state)
            self.assertNotIn("leader_receiver", state)
            self.assertTrue(_has_event(_events(workspace), "leader_receiver.rebind_required", reason="not_in_tmux_pane"))

    def test_12_symlinked_workspace_realpath_matches_canonical_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="leader-lease-real-") as real_tmp, tempfile.TemporaryDirectory(prefix="leader-lease-links-") as link_tmp:
            workspace = Path(real_tmp)
            self._seed_workspace(workspace, owner=False, receiver=False)
            link = Path(link_tmp) / "workspace-link"
            link.symlink_to(workspace, target_is_directory=True)
            subdir_via_link = link / "nested"
            subdir_via_link.mkdir()
            target = _leader_target(REAL_CALLER_PANE, subdir_via_link, provider="codex", command="codex")
            target["pane_current_path"] = str(subdir_via_link)
            with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[target]):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

            self.assertTrue(result.get("ok"), result)
            self.assertEqual(result.get("leader_receiver", {}).get("pane_id"), REAL_CALLER_PANE)
            self._assert_state_pair_bound(workspace, REAL_CALLER_PANE)

    def test_13_ambiguous_claim_branch_writes_workspace_and_team_snapshot_atomically(self) -> None:
        with self._workspace_from_s0_fixture(owner_pane=REAL_DEAD_PANE, receiver_pane=REAL_DEAD_PANE) as workspace:
            EventLog(workspace).write(
                "leader_receiver.ambiguous_candidates",
                incident_id="incident-two-live-leaders",
                old_pane_id=REAL_DEAD_PANE,
                candidates=["%1830", "%1831"],
                team_id=None,
                uuid_prefix=REAL_UUID[:8],
            )
            winner = _leader_target("%1830", workspace, provider="claude_code", command="claude.exe")
            loser = _leader_target("%1831", workspace, provider="claude_code", command="claude.exe")

            with _leader_env(tmux_pane="%1830"), _patched_tmux(targets=[winner, loser], missing={REAL_DEAD_PANE}):
                result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=True)

            self.assertTrue(result.get("ok"), result)
            self.assertEqual(result.get("status"), "claimed", result)
            workspace_state = _workspace_state(workspace)
            team_state = _team_file_state(workspace)
            observed = {
                "workspace_owner": (workspace_state.get("team_owner") or {}).get("pane_id"),
                "workspace_receiver": (workspace_state.get("leader_receiver") or {}).get("pane_id"),
                "workspace_epoch": (workspace_state.get("team_owner") or {}).get("owner_epoch"),
                "team_owner": (team_state.get("team_owner") or {}).get("pane_id"),
                "team_receiver": (team_state.get("leader_receiver") or {}).get("pane_id"),
                "team_epoch": (team_state.get("team_owner") or {}).get("owner_epoch"),
            }
            self.assertEqual(
                observed,
                {
                    "workspace_owner": "%1830",
                    "workspace_receiver": "%1830",
                    "workspace_epoch": observed["workspace_epoch"],
                    "team_owner": "%1830",
                    "team_receiver": "%1830",
                    "team_epoch": observed["workspace_epoch"],
                },
                observed,
            )

    def test_14_liveness_uses_owner_identity_not_provider_command_name(self) -> None:
        with self.subTest("node command without owner identity is not a live owner"):
            with self._workspace_from_s0_fixture(owner_pane="%2200", receiver_pane="%2200") as workspace:
                bogus_owner = _leader_target("%2200", workspace, provider="codex", command="node", uuid="")
                bogus_owner.pop("leader_session_uuid", None)
                bogus_owner["leader_env"] = {"TEAM_AGENT_LEADER_PROVIDER": "codex"}
                caller = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")

                with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[bogus_owner, caller]):
                    result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

                self.assertTrue(result.get("ok"), result)
                self._assert_state_pair_bound(workspace, REAL_CALLER_PANE)

        with self.subTest("matching owner identity stays live even when foreground command changed"):
            with self._workspace_from_s0_fixture(owner_pane="%2300", receiver_pane="%2300") as workspace:
                owner = _leader_target("%2300", workspace, provider="claude_code", command="python")
                owner["process_tree"] = ["claude.exe", "python"]
                caller = _leader_target(REAL_CALLER_PANE, workspace, provider="claude_code", command="claude.exe")

                with _leader_env(tmux_pane=REAL_CALLER_PANE), _patched_tmux(targets=[owner, caller]):
                    result = _result_or_error(runtime.claim_leader, workspace, team=TEAM_ID, confirm=False)

                self.assertFalse(result.get("ok"), result)
                self.assertIn(result.get("reason"), {"force_confirm_required", "previous_owner_alive_refused"}, result)
                self._assert_workspace_receiver(workspace, "%2300")

    @contextmanager
    def _workspace_from_s0_fixture(
        self,
        *,
        owner: bool = True,
        receiver: bool = True,
        owner_pane: str = REAL_CALLER_PANE,
        receiver_pane: str = REAL_DEAD_PANE,
        owner_epoch: int = 7,
    ):
        with tempfile.TemporaryDirectory(prefix="leader-lease-s0-") as tmp:
            workspace = Path(tmp)
            self._seed_workspace(
                workspace,
                owner=owner,
                receiver=receiver,
                owner_pane=owner_pane,
                receiver_pane=receiver_pane,
                owner_epoch=owner_epoch,
            )
            yield workspace

    def _seed_workspace(
        self,
        workspace: Path,
        *,
        owner: bool = True,
        receiver: bool = True,
        owner_pane: str = REAL_CALLER_PANE,
        receiver_pane: str = REAL_DEAD_PANE,
        owner_epoch: int = 7,
    ) -> None:
        workspace.mkdir(parents=True, exist_ok=True)
        spec_path = _write_spec(workspace)
        root_fixture = _fixture_json("state_snapshots/runtime_state.selected-fields.json")
        owner_record = copy.deepcopy(root_fixture["team_owner"]) if owner else None
        receiver_record = copy.deepcopy(root_fixture["leader_receiver"]) if receiver else None
        if owner_record:
            owner_record.update({"pane_id": owner_pane, "owner_epoch": owner_epoch, "leader_session_uuid": REAL_UUID})
        if receiver_record:
            receiver_record.update({"pane_id": receiver_pane, "owner_epoch": owner_epoch, "leader_session_uuid": REAL_UUID})
        state: dict[str, Any] = {
            "active_team_key": TEAM_ID,
            "session_name": TEAM_ID,
            "workspace": str(workspace),
            "spec_path": str(spec_path),
            "team_dir": str(workspace / ".team" / "current"),
            "leader": {"id": "leader"},
            "agents": {"worker": {"status": "running", "provider": "fake", "agent_id": "worker", "window": "worker"}},
            "tasks": [{"id": "task-1", "title": "Fixture task", "status": "running", "assignee": "worker", "deps": []}],
            "coordinator": copy.deepcopy(root_fixture.get("coordinator") or {}),
        }
        if owner_record:
            state["team_owner"] = owner_record
        if receiver_record:
            state["leader_receiver"] = receiver_record
        team_entry = copy.deepcopy(state)
        team_entry.pop("teams", None)
        state["teams"] = {TEAM_ID: team_entry}
        save_runtime_state(workspace, state)

        team_fixture = _fixture_json("state_snapshots/team_refactor_state.selected-fields.json")
        team_state = copy.deepcopy(state)
        team_state.pop("teams", None)
        team_state["active_team_key"] = TEAM_ID
        if owner:
            team_state["team_owner"] = copy.deepcopy(team_fixture.get("team_owner"))
        else:
            team_state.pop("team_owner", None)
        if receiver_record:
            stale_receiver = copy.deepcopy(team_fixture["leader_receiver"])
            stale_receiver.update({"pane_id": receiver_pane, "owner_epoch": owner_epoch, "leader_session_uuid": REAL_UUID})
            team_state["leader_receiver"] = stale_receiver
        _team_state_path(workspace).parent.mkdir(parents=True, exist_ok=True)
        _team_state_path(workspace).write_text(json.dumps(team_state, indent=2, ensure_ascii=False), encoding="utf-8")

    def _assert_state_pair_bound(self, workspace: Path, pane_id: str) -> None:
        workspace_state = _workspace_state(workspace)
        team_state = _team_file_state(workspace)
        for label, state in (("workspace", workspace_state), ("team", team_state)):
            self.assertEqual((state.get("team_owner") or {}).get("pane_id"), pane_id, f"{label} owner: {state}")
            self.assertEqual((state.get("leader_receiver") or {}).get("pane_id"), pane_id, f"{label} receiver: {state}")
            self.assertEqual((state.get("team_owner") or {}).get("leader_session_uuid"), REAL_UUID, f"{label} owner uuid")
            self.assertEqual((state.get("leader_receiver") or {}).get("leader_session_uuid"), REAL_UUID, f"{label} receiver uuid")
            self.assertIn("owner_epoch", state.get("team_owner") or {}, f"{label} owner epoch missing")
            self.assertIn("owner_epoch", state.get("leader_receiver") or {}, f"{label} receiver epoch missing")

    def _assert_workspace_receiver(self, workspace: Path, pane_id: str) -> None:
        self.assertEqual((_workspace_state(workspace).get("leader_receiver") or {}).get("pane_id"), pane_id)


def _write_spec(workspace: Path) -> Path:
    spec = _fake_spec(workspace)
    spec["team"]["name"] = TEAM_ID
    spec["team"]["workspace"] = str(workspace)
    spec["runtime"]["session_name"] = TEAM_ID
    spec["routing"]["default_assignee"] = "worker"
    spec["agents"][0]["id"] = "worker"
    spec["agents"][0]["working_directory"] = str(workspace)
    spec["runtime"]["startup_order"] = ["worker"]
    spec["tasks"][0]["assignee"] = "worker"
    for rule in spec["routing"].get("rules", []):
        rule["assign_to"] = "worker"
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec_path


def _fixture_json(relative: str) -> dict[str, Any]:
    return json.loads((FIXTURE_ROOT / relative).read_text(encoding="utf-8"))


def _workspace_state(workspace: Path) -> dict[str, Any]:
    return json.loads((workspace / ".team" / "runtime" / "state.json").read_text(encoding="utf-8"))


def _team_file_state(workspace: Path) -> dict[str, Any]:
    return json.loads(_team_state_path(workspace).read_text(encoding="utf-8"))


def _team_state_path(workspace: Path) -> Path:
    return workspace / ".team" / "runtime" / "teams" / TEAM_ID / "state.json"


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def _has_event(events: list[dict[str, Any]], event_name: str | None = None, **fields: Any) -> bool:
    for event in events:
        if event_name is not None and event.get("event") != event_name:
            continue
        if all(event.get(key) == value for key, value in fields.items()):
            return True
    return False


def _all_lease_reasons_closed(events: list[dict[str, Any]]) -> bool:
    allowed = {
        "vacant_acquired",
        "previous_owner_pane_dead",
        "previous_owner_alive_refused",
        "owner_epoch_advanced",
        "force_confirm_required",
        "caller_not_leader_shaped",
        "caller_pane_missing",
        "caller_cwd_mismatch",
        "not_in_tmux_pane",
    }
    lease_events = [event for event in events if event.get("reason") in allowed or str(event.get("event", "")).startswith(("owner.", "leader_receiver."))]
    return bool(lease_events) and all((event.get("reason") in allowed) for event in lease_events if "reason" in event)


def _leader_target(
    pane_id: str,
    workspace_or_cwd: Path,
    *,
    provider: str,
    command: str,
    uuid: str = REAL_UUID,
    os_user: str | None = None,
    host: str = "test-host",
) -> dict[str, Any]:
    cwd = Path(workspace_or_cwd)
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": "leader",
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": command,
        "pane_current_path": str(cwd),
        "pane_active": "1",
        "session_attached": "1",
        "pane_in_mode": "0",
        "provider": provider,
        "host": host,
        "os_user": os_user or os.environ.get("USER") or "",
        "leader_session_uuid": uuid,
        "leader_env": {
            "TEAM_AGENT_LEADER_SESSION_UUID": uuid,
            "TEAM_AGENT_LEADER_PANE_ID": pane_id,
            "TEAM_AGENT_LEADER_PROVIDER": provider,
            "TEAM_AGENT_TEAM_ID": TEAM_ID,
        },
        "fingerprint": f"leaders|1|0|/dev/ttys{pane_id.strip('%')}",
    }


@contextmanager
def _leader_env(
    *,
    pane_id: str | None = None,
    provider: str | None = None,
    tmux_pane: str | None = None,
    uuid: str | None = None,
):
    keys = [
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_PROVIDER",
        "TEAM_AGENT_MACHINE_FINGERPRINT",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_WORKSPACE",
        "TMUX_PANE",
    ]
    old = {key: os.environ.get(key) for key in keys}
    try:
        for key in keys:
            os.environ.pop(key, None)
        if pane_id is not None:
            os.environ["TEAM_AGENT_LEADER_PANE_ID"] = pane_id
        if provider is not None:
            os.environ["TEAM_AGENT_LEADER_PROVIDER"] = provider
        if tmux_pane is not None:
            os.environ["TMUX_PANE"] = tmux_pane
        if uuid is not None:
            os.environ["TEAM_AGENT_LEADER_SESSION_UUID"] = uuid
        if tmux_pane is not None:
            os.environ["TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE"] = uuid or REAL_UUID
        yield
    finally:
        for key, value in old.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


@contextmanager
def _patched_tmux(
    *,
    targets: list[dict[str, Any]],
    missing: set[str] | None = None,
    core_side_effect: list[dict[str, Any]] | None = None,
):
    missing = missing or set()
    targets_by_pane = {target["pane_id"]: target for target in targets}

    def pane_info(pane_id: str | None) -> dict[str, Any] | None:
        if pane_id in missing:
            return None
        return targets_by_pane.get(str(pane_id))

    def leader_binding_run_cmd(args: list[str], timeout: int = 5) -> Mock:
        if args[:2] == ["tmux", "display-message"] and "-t" in args:
            pane_id = args[args.index("-t") + 1]
            target = pane_info(pane_id)
            command = (target or {}).get("pane_current_command", "")
            return Mock(returncode=0 if target else 1, stdout=f"{command}\n" if target else "", stderr="")
        return _fake_run_cmd(args, timeout)

    core_result = {"ok": True, "targets": targets}
    core_value: Any = core_side_effect if core_side_effect is not None else core_result
    patches = [
        patch("team_agent.runtime.core_list_targets", side_effect=core_value if isinstance(core_value, list) else None, return_value=None if isinstance(core_value, list) else core_value),
        patch("team_agent.messaging.leader_panes.core_list_targets", side_effect=core_value if isinstance(core_value, list) else None, return_value=None if isinstance(core_value, list) else core_value),
        patch("team_agent.runtime._tmux_pane_info", side_effect=pane_info),
        patch("team_agent.messaging.leader_panes._tmux_pane_info", side_effect=pane_info),
        patch("team_agent.runtime._tmux_current_client_pane_info", return_value=None),
        patch("team_agent.messaging.leader_panes._tmux_current_client_pane_info", return_value=None),
        patch("team_agent.runtime._tmux_list_panes", return_value=targets),
        patch("team_agent.messaging.leader_panes._tmux_list_panes", return_value=targets),
        patch("team_agent.runtime.run_cmd", side_effect=_fake_run_cmd),
        patch("team_agent.messaging.leader_panes.run_cmd", side_effect=_fake_run_cmd),
        patch("team_agent.leader_binding.run_cmd", side_effect=leader_binding_run_cmd),
    ]
    with _patch_all(patches):
        yield


@contextmanager
def _patched_leader_delivery_for_rediscovery(targets: list[dict[str, Any]], injected: list[str], *, invalid_old_pane: str):
    def validate(receiver: dict[str, Any]) -> dict[str, Any]:
        if receiver.get("pane_id") == invalid_old_pane:
            return {"ok": False, "reason": "leader_pane_missing", "error": "tmux pane does not exist"}
        return {"ok": True, "pane": receiver, "capture": "idle"}

    with (
        _patched_tmux(targets=targets, missing={invalid_old_pane}),
        patch("team_agent.messaging.leader._validate_leader_receiver", side_effect=validate),
        patch("team_agent.messaging.leader_panes._tmux_inject_text", side_effect=lambda target, *_args, **_kwargs: injected.append(target) or {"ok": True}),
        patch("team_agent.messaging.leader._tmux_inject_text", side_effect=lambda target, *_args, **_kwargs: injected.append(target) or {"ok": True}),
    ):
        yield


@contextmanager
def _patch_all(patches: list[Any]):
    exits = []
    try:
        for item in patches:
            exits.append(item.__enter__())
        yield
    finally:
        for item in reversed(patches):
            item.__exit__(None, None, None)


def _fake_run_cmd(args: list[str], timeout: int = 20) -> Mock:
    if args[:2] == ["tmux", "capture-pane"]:
        return Mock(returncode=0, stdout="› idle\n", stderr="")
    return Mock(returncode=0, stdout="", stderr="")


def _result_or_error(func: Callable[..., dict[str, Any]], *args: Any, **kwargs: Any) -> dict[str, Any]:
    try:
        return func(*args, **kwargs)
    except Exception as exc:
        return {"ok": False, "status": "exception", "reason": exc.__class__.__name__, "error": str(exc)}


if __name__ == "__main__":
    unittest.main(verbosity=2)
