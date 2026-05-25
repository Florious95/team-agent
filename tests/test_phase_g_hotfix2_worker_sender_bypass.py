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


_OWNER = {"pane_id": "%leader", "provider": "codex", "machine_fingerprint": "machine-leader"}


def _worker_team_workspace(workspace: Path) -> Path:
    spec = _fake_spec(workspace)
    spec["agents"] = [
        {**spec["agents"][0], "id": "worker_a"},
        {**spec["agents"][0], "id": "worker_b"},
    ]
    spec["runtime"]["max_active_agents"] = 2
    spec["runtime"]["startup_order"] = ["worker_a", "worker_b"]
    spec["routing"]["default_assignee"] = "worker_a"
    spec["routing"]["rules"] = []
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    save_runtime_state(
        workspace,
        {
            "spec_path": str(spec_path),
            "workspace": str(workspace),
            "session_name": "team-worker-bypass",
            "team_owner": _OWNER,
            "leader": spec["leader"],
            "agents": {
                "worker_a": {"status": "running", "provider": "fake", "window": "worker_a"},
                "worker_b": {"status": "running", "provider": "fake", "window": "worker_b"},
            },
            "tasks": spec["tasks"],
            "display_backend": "none",
        },
    )
    return workspace


def _strip_owner_env():
    for key in (
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_PROVIDER",
        "TEAM_AGENT_MACHINE_FINGERPRINT",
        "TEAM_AGENT_ID",
    ):
        os.environ.pop(key, None)


class PhaseGHotfix2WorkerSenderBypassTests(unittest.TestCase):

    def test_worker_to_worker_peer_send_bypasses_owner_gate_with_audit_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-phase-g-bypass-") as tmp:
            workspace = _worker_team_workspace(Path(tmp))
            _strip_owner_env()
            os.environ["TEAM_AGENT_ID"] = "worker_a"
            try:
                with patch(
                    "team_agent.runtime._deliver_pending_message",
                    return_value={"ok": True, "status": "queued", "queued": True},
                ):
                    result = runtime.send_message(
                        workspace,
                        "worker_b",
                        "peer ping from worker_a",
                        sender="worker_a",
                        wait_visible=False,
                    )
            finally:
                _strip_owner_env()
            self.assertTrue(result.get("ok"), result)
            self.assertNotEqual(result.get("reason"), "team_owner_mismatch", result)
            self.assertIn(result.get("status"), {"submitted", "queued", "visible"})
            events_path = workspace / ".team" / "logs" / "events.jsonl"
            self.assertTrue(events_path.exists())
            bypass_events = [
                json.loads(line)
                for line in events_path.read_text(encoding="utf-8").splitlines()
                if '"send.bypassed_owner_gate_worker_sender"' in line
            ]
            self.assertTrue(bypass_events, "expected send.bypassed_owner_gate_worker_sender audit event")
            self.assertEqual(bypass_events[-1].get("sender"), "worker_a")
            self.assertEqual(bypass_events[-1].get("target"), "worker_b")

    def test_unknown_sender_still_refused_by_owner_gate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-phase-g-bypass-deny-") as tmp:
            workspace = _worker_team_workspace(Path(tmp))
            _strip_owner_env()
            os.environ["TEAM_AGENT_ID"] = "unknown_agent"
            try:
                result = runtime.send_message(
                    workspace,
                    "worker_b",
                    "impostor ping",
                    sender="unknown_agent",
                    wait_visible=False,
                )
            finally:
                _strip_owner_env()
            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "team_owner_mismatch", result)

    def test_sender_env_mismatch_does_not_bypass(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-phase-g-bypass-mismatch-") as tmp:
            workspace = _worker_team_workspace(Path(tmp))
            _strip_owner_env()
            os.environ["TEAM_AGENT_ID"] = "worker_a"
            try:
                result = runtime.send_message(
                    workspace,
                    "worker_b",
                    "spoof attempt",
                    sender="worker_b",
                    wait_visible=False,
                )
            finally:
                _strip_owner_env()
            self.assertFalse(result.get("ok"), result)
            self.assertEqual(result.get("reason"), "team_owner_mismatch", result)


if __name__ == "__main__":
    unittest.main(verbosity=2)
