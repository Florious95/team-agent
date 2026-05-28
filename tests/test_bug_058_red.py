from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests" / "fixtures" / "bug_058" / "wsl_mnt_c_npx_not_found.txt"


class Bug058InstallerWslAcceptanceTests(unittest.TestCase):
    def test_1_fixture_preserves_real_wsl_npx_failure(self) -> None:
        text = FIXTURE.read_text(encoding="utf-8")
        self.assertIn(
            "npm error config prefix cannot be changed from project config: /mnt/c/Users/AlaudaLancy/.npmrc.",
            text,
        )
        self.assertIn("@team-agent/installer@0.2.5", text)
        self.assertIn("sh: 1: team-agent-installer: not found", text)

    def test_2_readme_documents_windows_wsl_prefix_failure_and_recovery(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        section = _section(readme, "Windows + WSL")
        required = [
            "/mnt/c/Users",
            ".npmrc",
            "prefix",
            "config prefix cannot be changed from project config",
            "team-agent-installer: not found",
            "cd ~",
        ]
        for token in required:
            self.assertIn(token, section)
        self.assertRegex(section, r"(move|移).+~/.npmrc|~/.npmrc.+(move|移)")
        self.assertRegex(section, r"(delete|remove|删).+prefix|prefix.+(delete|remove|删)")

    def test_3_package_declares_postinstall_bin_self_check(self) -> None:
        package = _package_json()
        scripts = package.get("scripts") or {}
        self.assertIn("postinstall", scripts)
        postinstall = scripts["postinstall"]
        self.assertIn("node", postinstall)
        self.assertRegex(postinstall, r"npm/[^ ]+\.mjs")

    def test_4_postinstall_script_contains_required_diagnostic_text(self) -> None:
        script = _postinstall_script_path()
        text = script.read_text(encoding="utf-8")
        required = [
            "team-agent-installer",
            "ERROR:",
            "team-agent-installer bin not on PATH after npm install",
            "ACTION:",
            "LOG:",
            "/mnt/c",
            ".npmrc",
            "prefix",
            "cd ~",
        ]
        for token in required:
            self.assertIn(token, text)

    def test_5_postinstall_self_check_reports_wsl_prefix_when_bin_missing(self) -> None:
        node = shutil.which("node")
        if not node:
            self.skipTest("node not installed")
        script = _postinstall_script_path()
        with tempfile.TemporaryDirectory(prefix="team-agent-bug058-") as tmp:
            tmp_path = Path(tmp)
            project = tmp_path / "mnt" / "c" / "Users" / "AlaudaLancy"
            project.mkdir(parents=True)
            (project / ".npmrc").write_text("prefix=/mnt/c/Users/AlaudaLancy/.npm-global\n", encoding="utf-8")
            empty_bin = tmp_path / "empty-bin"
            empty_bin.mkdir()
            env = os.environ.copy()
            env.update({
                "INIT_CWD": str(project),
                "PATH": str(empty_bin),
                "TEAM_AGENT_INSTALLER_SELF_CHECK_ONLY": "1",
            })
            proc = subprocess.run(
                [node, str(script)],
                cwd=project,
                env=env,
                text=True,
                capture_output=True,
                timeout=10,
                check=False,
            )
        output = (proc.stdout or "") + (proc.stderr or "")
        self.assertNotEqual(proc.returncode, 0, output)
        self.assertIn("ERROR: team-agent-installer bin not on PATH after npm install.", output)
        self.assertIn("ACTION:", output)
        self.assertIn("LOG:", output)
        self.assertIn("/mnt/c", output)
        self.assertIn(".npmrc", output)
        self.assertIn("prefix", output)
        self.assertIn("cd ~", output)

    def test_6_package_scripts_do_not_silently_rewrite_npm_prefix_policy(self) -> None:
        scripts = _package_json().get("scripts") or {}
        joined = "\n".join(str(value) for value in scripts.values())
        forbidden = [
            "npm config set prefix",
            "npm config delete prefix",
            "rm .npmrc",
            "unlink .npmrc",
        ]
        for token in forbidden:
            self.assertNotIn(token, joined)


def _package_json() -> dict:
    return json.loads((ROOT / "package.json").read_text(encoding="utf-8"))


def _postinstall_script_path() -> Path:
    scripts = _package_json().get("scripts") or {}
    command = str(scripts.get("postinstall") or "")
    match = re.search(r"(npm/[^\s]+\.mjs)", command)
    if not match:
        raise AssertionError(f"postinstall must run a committed npm/*.mjs script, got: {command!r}")
    path = ROOT / match.group(1)
    if not path.exists():
        raise AssertionError(f"postinstall script does not exist: {path}")
    return path


def _section(markdown: str, heading: str) -> str:
    pattern = re.compile(rf"^##+\s+{re.escape(heading)}\s*$", re.MULTILINE)
    match = pattern.search(markdown)
    if not match:
        raise AssertionError(f"README.md must contain a dedicated heading for {heading!r}")
    next_heading = re.search(r"^##+\s+", markdown[match.end():], re.MULTILINE)
    end = match.end() + next_heading.start() if next_heading else len(markdown)
    return markdown[match.start():end]


if __name__ == "__main__":
    unittest.main(verbosity=2)
