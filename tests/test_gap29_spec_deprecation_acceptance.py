"""Gap 29 / F3 spec deprecation acceptance.

Stage 7 attempt 5 SC-A surfaced a product bug: cd08303 wired the deprecation
warning + structured event for `spec.runtime.auto_trust_own_workspace`, but
neither the JSON schema nor the Python spec validator accepted the field.
team.spec.yaml validation rejected the field outright, so the deprecation
logic was never reached — the warning that was supposed to remind operators
to switch to the env-var path NEVER ran in production.

This acceptance test pins three properties down so the field works end-to-end:

  (1) the JSON schema documents the field as deprecated boolean (no enforcement
      from JSON Schema — the runtime gate is the Python validator — but the
      docstring stays the canonical source of the removal target version).
  (2) the Python spec validator accepts a runtime block that includes
      auto_trust_own_workspace: true and rejects a non-boolean value.
  (3) attempt_trust_auto_answer with that spec still emits the deprecation
      stderr line AND the trust_auto_answer_spec_opt_in_deprecated structured
      event — the contract from the earlier cd08303 / e7892cf / 10078e7 chain.
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch

from team_agent.cli import _fake_spec as _cli_fake_spec
from team_agent.errors import ValidationError
from team_agent.events import EventLog
from team_agent.messaging import leader_panes as leader_panes_mod
from team_agent.messaging.leader_panes import attempt_trust_auto_answer
from team_agent.spec import validate_spec


_BASE_PATH = Path(__file__).with_name("run_tests.py")
_SPEC = importlib.util.spec_from_file_location("team_agent_run_tests_base_gap29_spec_deprecation", _BASE_PATH)
_base = importlib.util.module_from_spec(_SPEC)
assert _SPEC.loader is not None
_SPEC.loader.exec_module(_base)


def _ok_proc() -> SimpleNamespace:
    return SimpleNamespace(returncode=0, stdout="", stderr="")


def _spec_with_auto_trust(workspace: Path, *, auto_trust_value: Any) -> dict[str, Any]:
    """Reuse the cli fake spec (already validator-clean) and patch the
    deprecated runtime field. Parameterized on the value so we can drive
    boolean accept, boolean false, and non-boolean reject cases."""
    spec = _cli_fake_spec(workspace)
    spec["runtime"]["auto_trust_own_workspace"] = auto_trust_value
    return spec


class Gap29SpecDeprecationAcceptance(unittest.TestCase):

    def setUp(self) -> None:
        self._tmp_ctx = tempfile.TemporaryDirectory(prefix="gap29-spec-dep-")
        self.workspace = Path(self._tmp_ctx.name).resolve()
        (self.workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
        self.event_log = EventLog(self.workspace)
        self._env_backup = os.environ.get("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE")
        os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)

    def tearDown(self) -> None:
        if self._env_backup is None:
            os.environ.pop("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", None)
        else:
            os.environ["TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"] = self._env_backup
        self._tmp_ctx.cleanup()

    def _emitted(self) -> list[dict[str, Any]]:
        path = self.workspace / ".team" / "logs" / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def test_json_schema_documents_field_as_deprecated_boolean(self) -> None:
        schema_path = Path(__file__).resolve().parents[1] / "schemas" / "team.schema.json"
        schema = json.loads(schema_path.read_text(encoding="utf-8"))
        runtime_props = schema["properties"]["runtime"]["properties"]
        self.assertIn("auto_trust_own_workspace", runtime_props)
        entry = runtime_props["auto_trust_own_workspace"]
        self.assertEqual(entry["type"], "boolean")
        self.assertTrue(entry.get("deprecated"),
            "schema must mark auto_trust_own_workspace deprecated=true")
        self.assertIn("0.3.0", entry.get("description", ""),
            "schema description must name the removal target version (0.3.0)")
        self.assertIn("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", entry.get("description", ""),
            "schema description must point at the env-var as the preferred opt-in")

    def test_python_validator_accepts_runtime_with_deprecated_field_set_true(self) -> None:
        """Before this fix, team.spec.yaml validation rejected the field
        because spec.py's allowed set did not include it. validate_spec must
        return cleanly when the field is present and boolean."""
        spec = _spec_with_auto_trust(self.workspace, auto_trust_value=True)
        # Should not raise — purely a positive acceptance.
        validate_spec(spec, base_dir=self.workspace)

    def test_python_validator_accepts_runtime_with_deprecated_field_set_false(self) -> None:
        spec = _spec_with_auto_trust(self.workspace, auto_trust_value=False)
        validate_spec(spec, base_dir=self.workspace)

    def test_python_validator_accepts_runtime_when_field_is_absent(self) -> None:
        spec = _spec_with_auto_trust(self.workspace, auto_trust_value=False)
        spec["runtime"].pop("auto_trust_own_workspace")
        validate_spec(spec, base_dir=self.workspace)

    def test_python_validator_rejects_non_boolean_value(self) -> None:
        spec = _spec_with_auto_trust(self.workspace, auto_trust_value="yes-please")
        with self.assertRaises(ValidationError) as ctx:
            validate_spec(spec, base_dir=self.workspace)
        self.assertIn("/runtime/auto_trust_own_workspace", str(ctx.exception))
        self.assertIn("must be a boolean", str(ctx.exception))

    def test_spec_opt_in_path_now_reaches_deprecation_warning_and_event(self) -> None:
        """End-to-end acceptance: with the schema/validator gates open, a spec
        carrying auto_trust_own_workspace=True now flows through to
        attempt_trust_auto_answer, which emits the stderr deprecation line AND
        the trust_auto_answer_spec_opt_in_deprecated structured event."""
        leader_panes_mod._reset_spec_opt_in_deprecation_state()
        spec = _spec_with_auto_trust(self.workspace, auto_trust_value=True)
        # validate first to prove the gate is open.
        validate_spec(spec, base_dir=self.workspace)
        capture_tail = (
            "Do you trust the contents of this directory and want to allow execution of source files?\n"
            f"\n  ▌ {self.workspace}\n"
            "\n  ▌ 1. Yes, proceed\n  ▌ 2. No, exit\n"
        )
        stderr_buf = io.StringIO()
        # attempt_trust_auto_answer now routes through _tmux_inject_text with
        # bypass_non_input_gate=True (commit 617a517 made tmux_io the canonical
        # paste boundary). Mock that injector to simulate a successful
        # answer-key delivery without touching real tmux.
        ok_inject = {"ok": True, "verification": "non_input_gate_bypassed"}
        with patch("team_agent.messaging.leader_panes._tmux_inject_text",
                   return_value=ok_inject), \
             contextlib.redirect_stderr(stderr_buf):
            result = attempt_trust_auto_answer(
                self.workspace, "%worker", capture_tail, self.event_log, spec=spec,
            )
        self.assertTrue(result["answered"])
        self.assertIn("spec.runtime.auto_trust_own_workspace is deprecated", stderr_buf.getvalue())
        self.assertIn("TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE", stderr_buf.getvalue())
        self.assertIn("0.3.0", stderr_buf.getvalue())
        deprecated_events = [ev for ev in self._emitted()
                             if ev.get("event") == "trust_auto_answer_spec_opt_in_deprecated"]
        self.assertEqual(len(deprecated_events), 1)
        self.assertEqual(deprecated_events[0]["deprecated_field"],
                         "spec.runtime.auto_trust_own_workspace")
        self.assertEqual(deprecated_events[0]["removal_target_version"], "0.3.0")


if __name__ == "__main__":
    unittest.main(verbosity=2)
