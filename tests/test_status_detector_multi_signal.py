from __future__ import annotations

import importlib.util
import unittest
from datetime import datetime, timedelta, timezone
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

from team_agent.messaging import scheduler


class StatusDetectorMultiSignalTests(unittest.TestCase):
    def test_codex_working_is_not_reported_idle(self) -> None:
        out = _classify(
            last_output_age_sec=12,
            pane={"pane_current_command": "node", "pane_in_mode": "0"},
            scrollback="Thinking...\nWorking...",
        )
        self.assertEqual(out["status"], "working")
        self.assertGreaterEqual(out["confidence"], 0.8)
        self.assertIn("Working", out["rationale"])

    def test_provider_idle_prompt_is_idle_even_after_old_last_output(self) -> None:
        for scrollback in ("› Find and fix a bug in @filename", "─ for agents"):
            with self.subTest(scrollback=scrollback):
                out = _classify(
                    last_output_age_sec=420,
                    pane={"pane_current_command": "node", "pane_in_mode": "0"},
                    scrollback=scrollback,
                )
                self.assertEqual(out["status"], "idle")
                self.assertGreaterEqual(out["confidence"], 0.8)
                self.assertIn("prompt", out["rationale"])

    def test_copy_scroll_or_search_mode_is_uncertain(self) -> None:
        out = _classify(
            last_output_age_sec=420,
            pane={"pane_current_command": "node", "pane_in_mode": "1"},
            scrollback="› Find and fix a bug in @filename",
        )
        self.assertEqual(out["status"], "uncertain")
        self.assertIn("pane_in_mode", out["rationale"])

    def test_old_output_without_prompt_or_spinner_is_high_confidence_stuck(self) -> None:
        out = _classify(
            last_output_age_sec=420,
            pane={"pane_current_command": "node", "pane_in_mode": "0"},
            scrollback="Running tool call...\n(no recent prompt)",
        )
        self.assertEqual(out["status"], "stuck")
        self.assertGreaterEqual(out["confidence"], 0.8)
        self.assertIn("no idle prompt", out["rationale"])


def _classify(last_output_age_sec: int, pane: dict[str, str], scrollback: str) -> dict:
    now = datetime.now(timezone.utc)
    last_output_at = (now - timedelta(seconds=last_output_age_sec)).isoformat()
    try:
        classify = scheduler.classify_agent_activity
    except AttributeError as exc:
        raise AssertionError(
            "Gap 13 requires scheduler.classify_agent_activity(...) returning "
            "status idle/working/stuck/uncertain with confidence and rationale"
        ) from exc
    return classify(
        agent_id="fake_impl",
        provider="codex",
        last_output_at=last_output_at,
        pane=pane,
        scrollback=scrollback,
        now=now,
        stuck_timeout_sec=300,
    )


if __name__ == "__main__":
    unittest.main(verbosity=2)
