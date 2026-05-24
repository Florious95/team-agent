from __future__ import annotations

import contextlib
import unittest
from pathlib import Path
from unittest.mock import patch

from team_agent import runtime
from team_agent.lifecycle import operations, start


class LifecycleDelegationTests(unittest.TestCase):
    def test_start_agent_delegates_through_runtime_lock(self) -> None:
        calls: list[tuple[str, object]] = []

        @contextlib.contextmanager
        def fake_lock(workspace: Path, name: str):
            calls.append(("lock", name))
            yield

        def fake_unlocked(workspace: Path, agent_id: str, force: bool, open_display: bool, allow_fresh: bool, team: str | None = None) -> dict:
            calls.append(("unlocked", (agent_id, force, open_display, allow_fresh)))
            return {"ok": True}

        with (
            patch.object(runtime, "_runtime_lock", side_effect=fake_lock),
            patch.object(start, "_start_agent_unlocked", side_effect=fake_unlocked),
        ):
            result = runtime.start_agent(Path("/tmp/workspace"), "worker", force=True, open_display=False, allow_fresh=True)

        self.assertEqual(result, {"ok": True})
        self.assertEqual(calls, [("lock", "start-agent"), ("unlocked", ("worker", True, False, True))])

    def test_lifecycle_modules_have_explicit_runtime_symbol_lists(self) -> None:
        self.assertFalse(hasattr(start, "_sync_runtime_globals"))
        self.assertFalse(hasattr(operations, "_sync_runtime_globals"))
        self.assertIn("_runtime_lock", start._RUNTIME_SYMBOLS)
        self.assertIn("start_agent", operations._RUNTIME_SYMBOLS)


if __name__ == "__main__":
    unittest.main()
