from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "tools" / "check_line_count_gate.py"


def _run_gate(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        cwd=workspace,
        text=True,
        capture_output=True,
        check=False,
    )


class CheckLineCountGateTests(unittest.TestCase):
    def test_hard_mode_fails_on_over_limit_file(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            target = root / "too_long.py"
            target.write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("too_long.py", proc.stdout)
            self.assertIn("3 lines > 2", proc.stdout)

    def test_diagnostics_only_reports_over_limit_but_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-diag-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            (root / "too_long.py").write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
            )

            self.assertEqual(proc.returncode, 0)
            self.assertIn("too_long.py", proc.stdout)

    def test_missing_allowlist_passes_when_files_are_under_limit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-missing-allow-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            (root / "ok.py").write_text("a\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(Path(tmp) / "tests" / "line_count_allowlist.json"),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_empty_allowlist_passes_when_files_are_under_limit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-empty-allow-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text('{"approved_exceptions": {}, "temporary_debt": {}}\n', encoding="utf-8")
            (root / "ok.py").write_text("a\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_blank_allowlist_passes_when_files_are_under_limit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-blank-allow-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text("", encoding="utf-8")
            (root / "ok.py").write_text("a\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_non_empty_temporary_debt_fails_even_when_files_are_under_limit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-nonempty-allow-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text('{"approved_exceptions": {}, "temporary_debt": {"src/legacy.py": {}}}\n', encoding="utf-8")
            (root / "ok.py").write_text("a\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("temporary_debt must be empty", proc.stderr)
            self.assertIn("temporary_debt entries: 1", proc.stdout)

    def test_invalid_allowlist_json_fails_fast_when_empty_allowlist_required(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-invalid-allow-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text("{not-json", encoding="utf-8")
            (root / "ok.py").write_text("a\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("invalid JSON", proc.stderr)

    def test_temporary_debt_is_diagnostic_when_empty_debt_not_required(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-allow-diag-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text('{"approved_exceptions": {}, "temporary_debt": {"src/legacy.py": {}}}\n', encoding="utf-8")
            (root / "ok.py").write_text("a\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("temporary_debt entries: 1", proc.stdout)

    def test_root_and_glob_scope_recursive_python_files_only(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-scope-") as tmp:
            root = Path(tmp) / "src"
            nested = root / "pkg"
            nested.mkdir(parents=True)
            (nested / "too_long.py").write_text("a\nb\nc\n", encoding="utf-8")
            (nested / "ignored.txt").write_text("a\nb\nc\n", encoding="utf-8")
            (Path(tmp) / "outside.py").write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("too_long.py", proc.stdout)
            self.assertNotIn("ignored.txt", proc.stdout)
            self.assertNotIn("outside.py", proc.stdout)

    def test_path_separator_glob_matches_relative_path(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-path-glob-") as tmp:
            root = Path(tmp) / "src"
            nested = root / "pkg"
            nested.mkdir(parents=True)
            (nested / "too_long.py").write_text("a\nb\nc\n", encoding="utf-8")
            (root / "top.py").write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "pkg/*.py",
                "--max-lines",
                "2",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("too_long.py", proc.stdout)
            self.assertNotIn("top.py", proc.stdout)

    def test_count_equal_to_max_lines_passes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-boundary-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()
            (root / "exact.py").write_text("a\nb\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_empty_root_passes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-empty-root-") as tmp:
            root = Path(tmp) / "src"
            root.mkdir()

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_approved_exception_under_named_ceiling_passes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-approved-") as tmp:
            root = Path(tmp) / "src"
            nested = root / "team_agent"
            nested.mkdir(parents=True)
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text(
                '{"approved_exceptions": {"src/team_agent/runtime.py": {"max_lines": 4}}, "temporary_debt": {}}\n',
                encoding="utf-8",
            )
            (nested / "runtime.py").write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("approved exceptions: 1 files", proc.stdout)

    def test_approved_exception_over_named_ceiling_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-approved-ceiling-") as tmp:
            root = Path(tmp) / "src"
            nested = root / "team_agent"
            nested.mkdir(parents=True)
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text(
                '{"approved_exceptions": {"src/team_agent/runtime.py": {"max_lines": 2}}, "temporary_debt": {}}\n',
                encoding="utf-8",
            )
            (nested / "runtime.py").write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("runtime.py", proc.stdout)
            self.assertIn("3 lines > 2", proc.stdout)

    def test_unapproved_file_still_uses_global_limit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-line-gate-unapproved-") as tmp:
            root = Path(tmp) / "src"
            nested = root / "team_agent"
            nested.mkdir(parents=True)
            allowlist = Path(tmp) / "tests" / "line_count_allowlist.json"
            allowlist.parent.mkdir()
            allowlist.write_text(
                '{"approved_exceptions": {"src/team_agent/runtime.py": {"max_lines": 4}}, "temporary_debt": {}}\n',
                encoding="utf-8",
            )
            (nested / "other.py").write_text("a\nb\nc\n", encoding="utf-8")

            proc = _run_gate(
                Path(tmp),
                "--root",
                str(root),
                "--glob",
                "*.py",
                "--max-lines",
                "2",
                "--allowlist",
                str(allowlist),
                "--require-empty-temporary-debt",
                "--hard",
            )

            self.assertEqual(proc.returncode, 1)
            self.assertIn("other.py", proc.stdout)
            self.assertIn("over-limit: 1 files", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
