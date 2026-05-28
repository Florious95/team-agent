from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class Bug058PostinstallWarnOnlyTests(unittest.TestCase):
    def test_missing_installer_bin_warns_without_failing_postinstall(self) -> None:
        node = shutil.which("node")
        if not node:
            self.skipTest("node not installed")
        script = ROOT / "npm" / "bincheck.mjs"
        with tempfile.TemporaryDirectory(prefix="team-agent-bug058-warn-") as tmp:
            empty_bin = Path(tmp) / "empty-bin"
            empty_bin.mkdir()
            env = os.environ.copy()
            env.pop("TEAM_AGENT_INSTALLER_SELF_CHECK_ONLY", None)
            env.update({"PATH": str(empty_bin), "INIT_CWD": tmp})
            proc = subprocess.run(
                [node, str(script)],
                env=env,
                text=True,
                capture_output=True,
                timeout=10,
                check=False,
            )
        output = (proc.stdout or "") + (proc.stderr or "")
        self.assertEqual(proc.returncode, 0, output)
        self.assertIn("ERROR: team-agent-installer bin not on PATH after npm install.", output)
        self.assertIn("ACTION:", output)
        self.assertIn("LOG:", output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
