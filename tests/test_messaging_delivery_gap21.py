from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path

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


class MessagingDeliveryGap21Tests(unittest.TestCase):
    def test_send_to_busy_worker_delivers_without_queued_busy_gate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-busy-deliver-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "busy", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            delete_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:2] == ["tmux", "delete-buffer"]:
                    delete_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "fake>" if send_calls else ("› [Pasted Content 1093 chars]" if paste_calls else "streaming")
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "deliver while busy")

            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])
            buffer_name = f"team-agent-send-{sent['message_id']}"
            self.assertTrue(any(buffer_name in call for call in paste_calls))
            self.assertTrue(any(buffer_name in call for call in delete_calls))
            events = _events(workspace)
            self.assertFalse(any(e["event"] == "send.queued_busy" for e in events))
            attempt = next(e for e in events if e["event"] == "send.deliver_attempt")
            self.assertTrue(attempt["recipient_busy"])
            self.assertEqual(attempt["recipient_status"], "busy")

    def test_worker_to_worker_busy_recipient_delivers_without_queued_busy_gate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-peer-busy-deliver-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            spec["agents"].append(peer)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_peer": {"status": "busy", "provider": "fake", "window": "fake_peer"},
                    },
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\nfake_peer\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "fake>" if send_calls else ("› [Pasted Content 1093 chars]" if paste_calls else "streaming")
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.runtime._mirror_peer_message_to_leader", return_value=None),
            ):
                sent = runtime.send_message(workspace, "fake_peer", "peer hello", sender="fake_impl")

            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])
            self.assertFalse(any(e["event"] == "send.queued_busy" for e in _events(workspace)))

    def test_codex_delivery_verifies_framework_message_as_own_turn(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-codex-turn-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "busy", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []
            token = ""

            def fake_run_cmd(args: list[str], timeout: int = 20):
                nonlocal token
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:2] == ["tmux", "set-buffer"]:
                    token = args[-1].split("[team-agent-token:", 1)[1].split("]", 1)[0]
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = (
                        f"❯ Team Agent message from leader:\n\nframework payload\n\n[team-agent-token:{token}]"
                        if send_calls
                        else ("› [Pasted Content 1093 chars]" if paste_calls else "streaming output")
                    )
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "framework payload")

            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "delivered")
            self.assertEqual(sent["turn_verification"], "leader_new_turn_boundary_verified")
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
