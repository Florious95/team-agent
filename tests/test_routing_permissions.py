from __future__ import annotations

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

class RoutingPermissionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.spec = load_spec(ROOT / "examples" / "team.spec.yaml")

    def test_default_routes(self) -> None:
        self.assertEqual(route_task(self.spec, {"type": "implementation"})["agent_id"], "codex_implementer")
        self.assertEqual(route_task(self.spec, {"type": "research"})["agent_id"], "codex_researcher")
        self.assertEqual(route_task(self.spec, {"type": "review"})["agent_id"], "codex_reviewer")
        self.assertEqual(route_task(self.spec, {"type": "unknown"})["agent_id"], "leader")

    def test_reviewer_cannot_write(self) -> None:
        reviewer = next(a for a in self.spec["agents"] if a["id"] == "codex_reviewer")
        task = {"type": "implementation", "requires_tools": ["fs_write", "execute_bash"]}
        self.assertIn("fs_write", missing_tools(reviewer, task))
        self.assertIn("execute_bash", missing_tools(reviewer, task))

    def test_prompt_only_visible(self) -> None:
        codex = next(a for a in self.spec["agents"] if a["id"] == "codex_implementer")
        resolved = resolve_permissions(codex)
        self.assertTrue(resolved["has_prompt_only"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
