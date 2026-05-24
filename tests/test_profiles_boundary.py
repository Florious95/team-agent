from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import team_agent.profiles as profiles


class ProfilesBoundaryTests(unittest.TestCase):
    def test_package_reexports_profile_surface(self) -> None:
        for name in profiles._REQUIRED_EXPORTS:
            self.assertIn(name, profiles.__all__)
            self.assertTrue(hasattr(profiles, name))

    def test_split_profile_modules_preserve_basic_flow(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-boundary-") as tmp:
            workspace = Path(tmp)
            created = profiles.init_profile(workspace, "codex-default", "compatible_api")
            self.assertTrue(created["ok"])
            profile_path = Path(created["path"])
            profile_path.write_text(
                "AUTH_MODE=compatible_api\nBASE_URL=https://example.invalid/v1\nAPI_KEY=secret\nMODEL=test-model\n",
                encoding="utf-8",
            )
            shown = profiles.show_profile(workspace, "codex-default")
            self.assertTrue(shown["ok"])
            self.assertTrue(shown["values"]["API_KEY"]["redacted"])
            agent = {
                "id": "worker",
                "provider": "codex",
                "profile": "codex-default",
                "auth_mode": "compatible_api",
            }
            validation = profiles.validate_agent_profile(workspace, agent)
            self.assertTrue(validation["ok"])
            self.assertEqual(profiles.effective_model(agent, workspace), "test-model")


if __name__ == "__main__":
    unittest.main()
