from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

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


class WorkerPeerDeliverySchedulingTests(unittest.TestCase):
    def test_peer_message_deferred_while_recipient_busy_then_swept_when_idle(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-peer-busy-then-idle-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            worker_a = copy.deepcopy(spec["agents"][0])
            worker_b = copy.deepcopy(spec["agents"][0])
            worker_a["id"] = "worker_a"
            worker_b["id"] = "worker_b"
            spec["agents"] = [worker_a, worker_b]
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
                    "session_name": "peer-team",
                    "leader": spec["leader"],
                    "agents": {
                        "worker_a": {"status": "idle", "provider": "fake", "window": "worker_a"},
                        "worker_b": {"status": "busy", "provider": "fake", "window": "worker_b"},
                    },
                    "tasks": spec["tasks"],
                },
            )
            recipient_idle = {"value": False}
            paste_calls: list[list[str]] = []
            send_key_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "worker_a\nworker_b\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_key_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    target = args[args.index("-t") + 1]
                    if str(target).endswith("worker_b"):
                        proc.stdout = (
                            "› Find and fix a bug in @filename\n"
                            if recipient_idle["value"]
                            else "Thinking...\nWorking...\n"
                        )
                    else:
                        proc.stdout = "› Find and fix a bug in @filename\n"
                elif args[:3] == ["tmux", "display-message", "-p"]:
                    target = args[args.index("-t") + 1] if "-t" in args else "%1"
                    pane_id = "%2" if str(target).endswith("worker_b") else "%1"
                    proc.stdout = f"{pane_id}\tpeer-team\t0\t{target}\t0\t/dev/ttys001\tfake\t1\t{workspace}\t1\n"
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                accepted = TeamOrchestratorTools(workspace).send_message(
                    to="worker_b",
                    content="peer ping",
                    sender="worker_a",
                )
                self.assertTrue(accepted["ok"], accepted)
                rows = MessageStore(workspace).messages()
                self.assertEqual(len(rows), 1, accepted)
                message_id = rows[0]["message_id"]
                self.assertIsNotNone(_event_for_message(workspace, "send.durably_stored", message_id))

                for _ in range(3):
                    runtime.coordinator_tick(workspace)
                self.assertIsNone(
                    _event_for_message(workspace, "send.deliver_attempt", message_id),
                    "peer message should stay deferred while recipient is still BUSY",
                )

                state = load_runtime_state(workspace)
                state["agents"]["worker_b"]["status"] = "idle"
                save_runtime_state(workspace, state)
                recipient_idle["value"] = True
                for _ in range(5):
                    runtime.coordinator_tick(workspace)
                    if _event_for_message(workspace, "send.deliver_attempt", message_id):
                        break

            attempt = _event_for_message(workspace, "send.deliver_attempt", message_id)
            failed = _event_for_message(workspace, "send.failed", message_id)
            self.assertTrue(
                attempt or failed,
                f"peer message {message_id} silently stayed pending after recipient became idle; events={_events(workspace)}",
            )
            if attempt:
                self.assertEqual(attempt["target"], "peer-team:worker_b")


def _event_for_message(workspace: Path, event: str, message_id: str) -> dict | None:
    return next(
        (
            item
            for item in _events(workspace)
            if item.get("event") == event and item.get("message_id") == message_id
        ),
        None,
    )


if __name__ == "__main__":
    unittest.main(verbosity=2)
