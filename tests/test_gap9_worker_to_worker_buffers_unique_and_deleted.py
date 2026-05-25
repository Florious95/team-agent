from __future__ import annotations

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


class Gap9WorkerPeerBufferTests(unittest.TestCase):
    def test_gap9_worker_to_worker_buffers_unique_and_deleted(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gap9-peer-buffers-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec_with_agents(workspace, 3)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            sender, recipient_a, recipient_b = [agent["id"] for agent in spec["agents"]]
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-gap9-peer",
                    "leader": spec["leader"],
                    "agents": {
                        agent["id"]: {"status": "running", "provider": "fake", "window": agent["id"]}
                        for agent in spec["agents"]
                    },
                    "tasks": spec["tasks"],
                },
            )

            buffers: dict[str, str] = {}
            pasted_by_target: dict[str, str] = {}
            paste_calls: list[list[str]] = []
            delete_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join([sender, recipient_a, recipient_b])
                elif args[:4] == ["tmux", "display-message", "-p", "-t"]:
                    proc.stdout = "0\n"
                elif args[:2] == ["tmux", "set-buffer"]:
                    buffers[args[3]] = args[4]
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                    pasted_by_target[args[3]] = buffers[args[5]]
                elif args[:2] == ["tmux", "delete-buffer"]:
                    delete_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    target = args[args.index("-t") + 1]
                    proc.stdout = pasted_by_target.get(target, "fake>")
                return proc

            sends: list[dict] = []
            recipients = [recipient_a, recipient_b, recipient_a]
            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.runtime._mirror_peer_message_to_leader", return_value=None),
            ):
                for index, target in enumerate(recipients):
                    result = runtime.send_message(workspace, target, f"peer payload {index}", sender=sender, wait_visible=True)
                    self.assertTrue(result["ok"], result)
                    sends.append(result)

            self.assertEqual(len(buffers), 3)
            self.assertEqual(len(set(buffers)), 3)
            self.assertNotIn("team-agent-message", buffers)
            for index, result in enumerate(sends):
                buffer_name = f"team-agent-send-{result['message_id']}"
                self.assertIn(buffer_name, buffers)
                self.assertIn(f"peer payload {index}", buffers[buffer_name])
                self.assertIn(["tmux", "paste-buffer", "-t", f"team-gap9-peer:{result['to']}", "-b", buffer_name, "-p"], paste_calls)
                self.assertIn(["tmux", "delete-buffer", "-b", buffer_name], delete_calls)


if __name__ == "__main__":
    unittest.main(verbosity=2)
