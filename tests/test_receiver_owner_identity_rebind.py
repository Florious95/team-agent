from __future__ import annotations

import importlib.util
import json
import os
import unittest
from pathlib import Path
from unittest.mock import patch

_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base", _BASE_PATH)
base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(base)
globals().update({
    name: value
    for name, value in vars(base).items()
    if not name.startswith("__") and not (isinstance(value, type) and issubclass(value, unittest.TestCase))
})

from team_agent.events import EventLog


OWNER = {"pane_id": "%owner-old", "provider": "codex", "machine_fingerprint": "machine-a"}


class ReceiverOwnerIdentityRebindTests(unittest.TestCase):
    def test_stale_receiver_rebinds_by_team_owner_identity_and_delivers_queued_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-owner-rebind-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-owner-alpha",
                    "team_owner": OWNER,
                    "leader": spec["leader"],
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "codex",
                        "pane_id": "%stale",
                        "session_name": "old-leader",
                    },
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            MessageStore(workspace).create_message(
                "task_impl",
                "leader",
                "fake_impl",
                "do work",
                owner_team_id="team-owner-alpha",
            )
            targets = {
                "ok": True,
                "targets": [
                    _target("%intruder", provider="codex", machine="machine-b"),
                    _target("%owner-new", provider="codex", machine="machine-a"),
                ],
            }
            delivered_panes: list[str] = []

            def fake_leader_delivery(_workspace, _target, _content, *_args, **_kwargs):
                delivered_panes.append(load_runtime_state(workspace)["leader_receiver"]["pane_id"])
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with (
                patch("team_agent.runtime._validate_leader_receiver", return_value={"ok": False, "reason": "leader_pane_missing"}),
                patch("team_agent.messaging.leader_panes.core_list_targets", return_value=targets),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "status": "started", "pid": 123}),
            ):
                reported = runtime.report_result(workspace, _result_envelope("success"))

            self.assertTrue(reported["ok"])
            state = load_runtime_state(workspace)
            self.assertEqual(state["leader_receiver"]["pane_id"], "%owner-new")

            with (
                patch.dict(
                    os.environ,
                    {
                        "TEAM_AGENT_LEADER_PANE_ID": OWNER["pane_id"],
                        "TEAM_AGENT_LEADER_PROVIDER": OWNER["provider"],
                        "TEAM_AGENT_MACHINE_FINGERPRINT": OWNER["machine_fingerprint"],
                    },
                ),
                patch("team_agent.messaging.scheduler.deliver_stored_message", side_effect=fake_leader_delivery),
            ):
                runtime._fire_due_scheduled_events(workspace, MessageStore(workspace), EventLog(workspace))

            self.assertEqual(delivered_panes, ["%owner-new"])

    def test_rebind_required_event_names_owner_identity_when_no_matching_pane_exists(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-owner-rebind-missing-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-owner-alpha",
                    "team_owner": OWNER,
                    "leader": spec["leader"],
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "codex",
                        "pane_id": "%stale",
                        "session_name": "old-leader",
                    },
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}],
                },
            )

            with (
                patch("team_agent.runtime._validate_leader_receiver", return_value={"ok": False, "reason": "leader_pane_missing"}),
                patch("team_agent.messaging.leader_panes.core_list_targets", return_value={"ok": True, "targets": []}),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "status": "started", "pid": 123}),
            ):
                runtime.report_result(workspace, _result_envelope("success"))

            rebind_events = [
                event
                for event in _events(workspace)
                if event.get("event") == "leader_receiver.rebind_required"
            ]
            self.assertEqual(len(rebind_events), 1)
            self.assertEqual(rebind_events[0]["owner_identity"], OWNER)


def _target(pane_id: str, provider: str, machine: str) -> dict[str, object]:
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": pane_id.strip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": "codex",
        "fingerprint": f"leaders|1|0|{pane_id}",
        "leader_env": {
            "TEAM_AGENT_LEADER_PANE_ID": OWNER["pane_id"],
            "TEAM_AGENT_LEADER_PROVIDER": provider,
            "TEAM_AGENT_MACHINE_FINGERPRINT": machine,
        },
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
