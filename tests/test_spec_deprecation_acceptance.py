from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from typing import Any

from team_agent.cli import _fake_spec
from team_agent.simple_yaml import dumps
from team_agent.spec import load_spec


ROOT = Path(__file__).resolve().parents[1]
DEPRECATED_EVENT = "trust_auto_answer_spec_opt_in_deprecated"
ENV_OPT_IN = "TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"
REMOVAL_TARGET = "0.3.0"


class SpecDeprecationAcceptanceTests(unittest.TestCase):
    def test_1_load_without_auto_trust_field_emits_no_warning_or_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-spec-deprecation-default-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_spec(workspace, auto_trust=_MISSING)
            stderr = io.StringIO()

            with redirect_stderr(stderr):
                spec = load_spec(spec_path)

            self.assertNotIn("auto_trust_own_workspace", spec["runtime"])
            self.assertEqual(stderr.getvalue(), "")
            self.assertEqual(_deprecated_events(workspace), [])

    def test_2_load_with_auto_trust_false_emits_no_warning_or_event(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-spec-deprecation-false-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_spec(workspace, auto_trust=False)
            stderr = io.StringIO()

            with redirect_stderr(stderr):
                spec = load_spec(spec_path)

            self.assertIs(spec["runtime"]["auto_trust_own_workspace"], False)
            self.assertEqual(stderr.getvalue(), "")
            self.assertEqual(_deprecated_events(workspace), [])

    def test_3_load_with_auto_trust_true_warns_and_emits_event_at_load_time(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-spec-deprecation-true-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_spec(workspace, auto_trust=True)

            proc = _run_load_spec_subprocess(spec_path, load_count=1)

            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn(ENV_OPT_IN, proc.stderr)
            self.assertIn(REMOVAL_TARGET, proc.stderr)
            events = _deprecated_events(workspace)
            self.assertEqual(len(events), 1, _events(workspace))
            self.assertEqual(events[0]["deprecated_field"], "spec.runtime.auto_trust_own_workspace")
            self.assertEqual(events[0]["preferred_opt_in"], f"env:{ENV_OPT_IN}")
            self.assertEqual(events[0]["removal_target_version"], REMOVAL_TARGET)

    def test_4_multiple_loads_warn_once_but_emit_event_per_load(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-spec-deprecation-multiple-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_spec(workspace, auto_trust=True)

            proc = _run_load_spec_subprocess(spec_path, load_count=3)

            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(proc.stderr.count(ENV_OPT_IN), 1, proc.stderr)
            self.assertEqual(proc.stderr.count(REMOVAL_TARGET), 1, proc.stderr)
            events = _deprecated_events(workspace)
            self.assertEqual(len(events), 3, _events(workspace))
            self.assertTrue(all(event["deprecated_field"] == "spec.runtime.auto_trust_own_workspace" for event in events))
            self.assertTrue(all(event["preferred_opt_in"] == f"env:{ENV_OPT_IN}" for event in events))
            self.assertTrue(all(event["removal_target_version"] == REMOVAL_TARGET for event in events))

    def test_5_quick_start_layout_writes_event_to_workspace_root_log(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-spec-deprecation-current-") as tmp:
            workspace = Path(tmp)
            spec_path = _write_quick_start_spec(workspace, auto_trust=True)
            expected_log = workspace / ".team" / "logs" / "events.jsonl"
            nested_log = workspace / ".team" / "current" / ".team" / "logs" / "events.jsonl"

            proc = _run_load_spec_subprocess(spec_path, load_count=1)

            self.assertEqual(proc.returncode, 0, proc.stderr)
            events = _deprecated_events(workspace)
            failures: list[str] = []
            if len(events) != 1:
                failures.append(f"expected one root event in {expected_log}, got {events!r}")
            if nested_log.exists():
                failures.append(f"deprecated event log must not be nested at {nested_log}")
            self.assertEqual([], failures)


class _Missing:
    pass


_MISSING = _Missing()


def _write_spec(workspace: Path, *, auto_trust: bool | _Missing) -> Path:
    workspace.mkdir(parents=True, exist_ok=True)
    spec = _fake_spec(workspace)
    if isinstance(auto_trust, _Missing):
        spec["runtime"].pop("auto_trust_own_workspace", None)
    else:
        spec["runtime"]["auto_trust_own_workspace"] = auto_trust
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec_path


def _write_quick_start_spec(workspace: Path, *, auto_trust: bool) -> Path:
    spec = _fake_spec(workspace)
    spec["runtime"]["auto_trust_own_workspace"] = auto_trust
    spec_dir = workspace / ".team" / "current"
    spec_dir.mkdir(parents=True, exist_ok=True)
    spec_path = spec_dir / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    return spec_path


def _run_load_spec_subprocess(spec_path: Path, *, load_count: int) -> subprocess.CompletedProcess[str]:
    script = textwrap.dedent(
        f"""
        from pathlib import Path
        from team_agent.spec import load_spec

        path = Path({str(spec_path)!r})
        for _ in range({load_count}):
            load_spec(path)
        """
    )
    env = os.environ.copy()
    src_path = str(ROOT / "src")
    env["PYTHONPATH"] = src_path + os.pathsep + env.get("PYTHONPATH", "")
    return subprocess.run(
        [sys.executable, "-c", script],
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def _deprecated_events(workspace: Path) -> list[dict[str, Any]]:
    return [event for event in _events(workspace) if event.get("event") == DEPRECATED_EVENT]


def _events(workspace: Path) -> list[dict[str, Any]]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


if __name__ == "__main__":
    unittest.main(verbosity=2)
