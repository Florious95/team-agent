from __future__ import annotations

import importlib.util
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
from team_agent.messaging import scheduler
from team_agent.state import save_runtime_state


class CodexCompactionAutoResetTests(unittest.TestCase):
    def test_codex_three_compactions_plus_stuck_loop_triggers_auto_reset_or_leader_recommendation(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-codex-compaction-") as tmp:
            workspace = Path(tmp)
            state = _state(workspace, provider="codex")
            save_runtime_state(workspace, state)
            event_log = EventLog(workspace)
            reset_calls: list[dict] = []

            def fake_reset(_workspace, agent_id, discard_session=False, **_kwargs):
                reset_calls.append({"agent_id": agent_id, "discard_session": discard_session})
                return {"ok": True, "status": "reset", "agent_id": agent_id, "discard_session": discard_session}

            with patch("team_agent.runtime.reset_agent", side_effect=fake_reset):
                out = _detect_compaction_degradation(
                    workspace,
                    state,
                    event_log,
                    provider="codex",
                    scrollback=_codex_compacted_scrollback(3),
                    stuck_loop=True,
                )

            event_names = [event["event"] for event in _events(workspace)]
            self.assertIn(
                out["event"],
                {"compaction_threshold_crossed.auto_reset", "compaction_threshold_crossed.recommend_reset"},
            )
            if out["event"] == "compaction_threshold_crossed.auto_reset":
                self.assertEqual(reset_calls, [{"agent_id": "fake_impl", "discard_session": True}])
                self.assertIn("compaction_threshold_crossed.auto_reset", event_names)
            else:
                self.assertEqual(reset_calls, [])
                self.assertIn("compaction_threshold_crossed.recommend_reset", event_names)
                self.assertIn("fake_impl", out["leader_visible_message"])

    def test_claude_compaction_signature_does_not_trigger_codex_reset_policy(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-compaction-") as tmp:
            workspace = Path(tmp)
            state = _state(workspace, provider="claude")
            save_runtime_state(workspace, state)
            event_log = EventLog(workspace)

            with patch("team_agent.runtime.reset_agent") as reset:
                out = _detect_compaction_degradation(
                    workspace,
                    state,
                    event_log,
                    provider="claude",
                    scrollback=_claude_compacted_scrollback(3),
                    stuck_loop=True,
                )

            self.assertEqual(out["event"], "compaction_threshold_crossed.ignored_lossless_provider")
            reset.assert_not_called()
            self.assertFalse(
                any(event["event"].startswith("compaction_threshold_crossed.auto_reset") for event in _events(workspace))
            )


def _detect_compaction_degradation(
    workspace: Path,
    state: dict,
    event_log: EventLog,
    provider: str,
    scrollback: str,
    stuck_loop: bool,
) -> dict:
    try:
        detect = scheduler.detect_compaction_degradation
    except AttributeError as exc:
        raise AssertionError(
            "Gap 19 requires scheduler.detect_compaction_degradation(...) to count Codex compactions "
            "and trigger reset-agent --discard-session or a leader-visible reset recommendation"
        ) from exc
    return detect(
        workspace,
        state,
        event_log,
        agent_id="fake_impl",
        provider=provider,
        scrollback=scrollback,
        stuck_loop=stuck_loop,
    )


def _state(workspace: Path, provider: str) -> dict:
    spec = _fake_spec(workspace)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return {
        "spec_path": str(spec_path),
        "session_name": "team-compact",
        "leader": spec["leader"],
        "agents": {"fake_impl": {"status": "running", "provider": provider, "window": "fake_impl"}},
        "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "in_progress"}],
    }


def _codex_compacted_scrollback(count: int) -> str:
    return "\n".join(["Context compacted. Continue from summary."] * count + ["Working...", "Working..."])


def _claude_compacted_scrollback(count: int) -> str:
    return "\n".join(["Context compacted automatically; continuing with lossless summary."] * count + ["─ for agents"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
