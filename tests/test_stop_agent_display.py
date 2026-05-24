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


class StopAgentDisplayTests(unittest.TestCase):
    def test_stop_agent_cleans_or_relabels_display_workspace_slot(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stop-display-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec_with_agents(workspace, 3)
            spec["runtime"]["session_name"] = "team-stop-display"
            spec["runtime"]["display_backend"] = "ghostty_workspace"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            aggregator = runtime._ghostty_workspace_aggregator_name("team-stop-display")
            agent_ids = [agent["id"] for agent in spec["agents"]]
            display_by_agent = {
                agent_id: {
                    "backend": "ghostty_workspace",
                    "status": "opened",
                    "title": "team-agent:team-stop-display:workspace",
                    "pane_title": f"team-agent:{agent_id}:Worker {index + 1}",
                    "target": f"team-stop-display:{agent_id}",
                    "linked_session": runtime._ghostty_display_session_name("team-stop-display", agent_id),
                    "aggregator_session": aggregator,
                    "display_session": aggregator,
                    "workspace_window": "overview",
                    "pane_id": f"%{index + 10}",
                    "pids": [],
                }
                for index, agent_id in enumerate(agent_ids)
            }
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-stop-display",
                    "agents": {
                        agent_id: {
                            "status": "running",
                            "provider": "fake",
                            "window": agent_id,
                            "session_id": f"session-{agent_id}",
                            "display": display_by_agent[agent_id],
                        }
                        for agent_id in agent_ids
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "ghostty_workspace",
                },
            )
            windows = set(agent_ids)
            pane_titles = {display["pane_id"]: display["pane_title"] for display in display_by_agent.values()}
            stopped_agent = agent_ids[1]

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join(sorted(windows))
                elif args[:3] == ["tmux", "kill-window", "-t"]:
                    windows.discard(args[3].split(":", 1)[1])
                elif args[:3] == ["tmux", "kill-pane", "-t"]:
                    pane_titles.pop(args[3], None)
                elif args[:2] == ["tmux", "select-pane"] and "-t" in args and "-T" in args:
                    pane_titles[args[args.index("-t") + 1]] = args[args.index("-T") + 1]
                elif args[:3] == ["tmux", "list-panes", "-t"]:
                    proc.stdout = "\n".join(
                        f"{pane_id}\t{title}" for pane_id, title in sorted(pane_titles.items())
                    )
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                before = runtime.run_cmd(["tmux", "list-windows", "-t", "team-stop-display"]).stdout.splitlines()
                runtime.stop_agent(workspace, stopped_agent)
                after = runtime.run_cmd(["tmux", "list-windows", "-t", "team-stop-display"]).stdout.splitlines()
                display_slots = runtime.run_cmd(["tmux", "list-panes", "-t", aggregator]).stdout.splitlines()

            self.assertEqual(len(after), len(before) - 1)
            stopped_titles = [line for line in display_slots if stopped_agent in line]
            self.assertTrue(
                not stopped_titles or any(f"stopped: {stopped_agent}" in line for line in stopped_titles),
                "stop-agent must either remove the display slot or relabel it as stopped",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
