from __future__ import annotations

import inspect
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from team_agent import diagnose, runtime


PUBLIC_NAMES = [
    "compact_model_checks",
    "diagnose",
    "doctor",
    "ensure_profiles_for_roles",
    "format_model_check_failures",
    "format_profile_check_failures",
    "format_profile_smoke_failures",
    "model_checks_for_agents",
    "prepare_quick_start_team",
    "preflight",
    "preflight_blockers",
    "preflight_next_actions",
    "profile_checks_for_agents",
    "profile_smoke_checks_for_agents",
    "quick_start",
    "repair_state",
    "settle",
    "start",
    "wait_ready",
]


class DiagnoseBoundaryTests(unittest.TestCase):
    """Pin runtime.py <-> diagnose/ contract via the calibrated convention
    from the b8760dc spark review: ONE identity smoke + ONE lightweight
    loop verifying every re-exported name is bound + per-helper behavior
    assertions (below) + e2e probes for the biggest orchestration symbol."""

    def test_runtime_alias_identity_smoke(self) -> None:
        # Representative aliases prove the runtime re-export wiring is live;
        # the loop below catches per-symbol drift without exhaustive assertIs.
        self.assertIs(runtime.diagnose, diagnose.diagnose)
        self.assertIs(runtime.preflight, diagnose.preflight)
        self.assertTrue(callable(runtime.quick_start))

    def test_every_public_name_is_re_exported_on_runtime(self) -> None:
        for name in PUBLIC_NAMES:
            self.assertTrue(hasattr(diagnose, name), f"team_agent.diagnose missing {name}")
            # runtime exposes the same callable -- aliased name keeps underscore
            # prefix for the private helpers but resolves to the same object.
            public_attr = getattr(diagnose, name)
            runtime_attr = getattr(runtime, name, None) or getattr(runtime, f"_{name}", None)
            self.assertIsNotNone(runtime_attr, f"runtime missing alias for {name}")
            if name == "quick_start":
                self.assertTrue(callable(runtime_attr), "runtime.quick_start is a 0.2.6 positive-source wrapper")
                continue
            self.assertIs(public_attr, runtime_attr, f"runtime alias for {name} drifted")

    def test_helpers_have_explicit_signatures(self) -> None:
        for name in PUBLIC_NAMES:
            fn = getattr(diagnose, name)
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{name} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{name} uses **kwargs")

    def test_modules_do_not_top_level_import_runtime(self) -> None:
        for module_name in (
            "team_agent.diagnose.checks",
            "team_agent.diagnose.health",
            "team_agent.diagnose.preflight",
            "team_agent.diagnose.quick_start",
            "team_agent.diagnose",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            for line in source.splitlines():
                if not line or line.startswith((" ", "\t")):
                    continue
                self.assertFalse(
                    line.startswith(("from team_agent.runtime", "import team_agent.runtime")),
                    f"{module_name} top-level imports runtime: {line!r}",
                )


class FormatFailureProbeTests(unittest.TestCase):
    def test_model_failure_includes_suggested_model_when_present(self) -> None:
        text = diagnose.format_model_check_failures([
            {"agent_id": "alpha", "provider": "codex", "model": "gpt-5", "suggested_model": "gpt-5-codex"},
        ])
        self.assertIn("alpha", text)
        self.assertIn("gpt-5-codex", text)

    def test_model_failure_falls_back_to_reason_status(self) -> None:
        text = diagnose.format_model_check_failures([
            {"agent_id": "beta", "provider": "claude", "model": "x", "reason": "model_id_not_found"},
        ])
        self.assertIn("model_id_not_found", text)

    def test_profile_failure_includes_missing_required_list(self) -> None:
        text = diagnose.format_profile_check_failures([
            {"agent_id": "gamma", "profile": "p", "auth_mode": "compatible_api", "missing_required": ["API_KEY", "BASE_URL"]},
        ])
        self.assertIn("API_KEY, BASE_URL", text)

    def test_profile_smoke_failure_includes_http_status_and_error(self) -> None:
        text = diagnose.format_profile_smoke_failures([
            {"agent_id": "delta", "provider": "codex", "profile": "p", "status": "http_error", "reason": "401", "http_status": 401, "error": "unauthorized"},
        ])
        self.assertIn("http_status=401", text)
        self.assertIn("unauthorized", text)


class CompactModelChecksProbeTests(unittest.TestCase):
    def test_keeps_only_documented_keys(self) -> None:
        out = diagnose.compact_model_checks([
            {"agent_id": "alpha", "provider": "claude", "model": "m", "ok": True, "status": "ok", "extra": "drop"},
        ])
        self.assertEqual(out[0]["agent_id"], "alpha")
        self.assertNotIn("extra", out[0])


class ModelChecksForAgentsProbeTests(unittest.TestCase):
    def test_paused_agents_are_skipped(self) -> None:
        agents = [{"id": "x", "provider": "codex", "paused": True, "model": "m"}]
        self.assertEqual(diagnose.model_checks_for_agents(agents), [])

    def test_compatible_api_codex_is_deferred_to_smoke(self) -> None:
        agents = [{"id": "y", "provider": "codex", "auth_mode": "compatible_api", "model": "m"}]
        with patch("team_agent.runtime.get_adapter") as adapter_mock:
            out = diagnose.model_checks_for_agents(agents)
            adapter_mock.assert_not_called()
        self.assertEqual(out[0]["status"], "profile_model_deferred_to_smoke")
        self.assertEqual(out[0]["agent_id"], "y")


class PreflightBlockersProbeTests(unittest.TestCase):
    def test_skips_passing_checks(self) -> None:
        self.assertEqual(diagnose.preflight_blockers([{"name": "tmux", "ok": True}]), ["unknown preflight blocker"])

    def test_compile_failure_renders_error(self) -> None:
        blockers = diagnose.preflight_blockers([{"name": "compile", "ok": False, "error": "syntax error"}])
        self.assertIn("compile: syntax error", blockers)

    def test_nested_check_failures_render_with_metadata(self) -> None:
        blockers = diagnose.preflight_blockers([
            {
                "name": "profiles",
                "ok": False,
                "checks": [
                    {"agent_id": "alpha", "profile": "p", "reason": "profile_required_values_missing", "missing_required": ["API_KEY"]},
                ],
            },
        ])
        self.assertIn("alpha", blockers[0])
        self.assertIn("missing=API_KEY", blockers[0])


class PreflightNextActionsProbeTests(unittest.TestCase):
    def test_proxy_failure_adds_proxy_hint(self) -> None:
        actions = diagnose.preflight_next_actions(["profile_smoke: alpha proxy_connectivity_failed"])
        self.assertTrue(any("proxy" in action for action in actions))

    def test_missing_profile_field_adds_profile_hint(self) -> None:
        actions = diagnose.preflight_next_actions(["profiles: alpha missing=API_KEY"])
        self.assertTrue(any("profile" in action.lower() for action in actions))


class SettleProbeTests(unittest.TestCase):
    def test_settle_returns_collected_count_summary(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-settle-") as tmp:
            workspace = Path(tmp)
            (workspace / ".team" / "logs").mkdir(parents=True, exist_ok=True)
            fake_collect = {"ok": True, "collected": [{"result_id": "r1"}, {"result_id": "r2"}]}
            fake_status = {"agents": {}, "tasks": [], "results": {"total": 0}}
            with patch("team_agent.runtime.collect", return_value=fake_collect), \
                 patch("team_agent.runtime.status", return_value=fake_status):
                out = diagnose.settle(workspace)
            self.assertTrue(out["ok"])
            self.assertIn("collected 2 result(s)", out["summary"])


class RepairStateProbeTests(unittest.TestCase):
    def _setup(self, workspace: Path) -> None:
        from team_agent.state import save_runtime_state
        from team_agent.simple_yaml import dumps
        spec = {
            "version": 1,
            "team": {"name": "team-diag", "mode": "supervisor_worker", "objective": "x", "workspace": str(workspace)},
            "leader": {"id": "leader", "role": "leader", "provider": "fake", "model": None, "tools": ["fs_read"], "context_policy": {"keep_user_thread": True, "receive_worker_outputs": "structured_only", "max_worker_result_tokens": 100}},
            "agents": [{"id": "alpha", "role": "impl", "provider": "fake", "model": None, "working_directory": str(workspace), "system_prompt": {"inline": "x", "file": None}, "tools": ["fs_read"], "permission_mode": "restricted", "preferred_for": [], "avoid_for": [], "output_contract": {"format": "result_envelope_v1", "required_fields": ["task_id", "status", "summary", "artifacts"]}}],
            "routing": {"default_assignee": "alpha", "rules": []},
            "communication": {"protocol": "mcp_inbox", "topology": "leader_centered", "worker_to_worker": False, "ack_timeout_sec": 2, "result_format": "result_envelope_v1", "message_store": {"sqlite": ".team/runtime/team.db", "mirror_files": ".team/messages"}},
            "runtime": {"backend": "tmux", "display_backend": "none", "session_name": "team-diag", "auto_launch": True, "require_user_approval_before_launch": False, "max_active_agents": 1, "startup_order": ["alpha"]},
            "context": {"state_file": "team_state.md", "artifact_dir": ".team/artifacts", "log_dir": ".team/logs", "summarization": {"worker_full_logs": "retain_outside_leader_context", "state_update": "after_each_result"}},
            "tasks": [{"id": "task_x", "title": "T", "type": "implementation", "assignee": None, "deps": [], "acceptance": ["a"], "status": "pending", "requires_tools": [], "files": [], "risk": "low"}],
        }
        spec_path = workspace / "team.spec.yaml"
        spec_path.write_text(dumps(spec), encoding="utf-8")
        save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": "team-diag", "agents": {}, "tasks": spec["tasks"]})

    def test_repair_updates_task_assignee_and_status(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-repair-") as tmp:
            workspace = Path(tmp)
            self._setup(workspace)
            out = diagnose.repair_state(workspace, "task_x", assignee="alpha", status_value="ready", summary="repaired")
            self.assertTrue(out["ok"])
            self.assertEqual(out["after"]["assignee"], "alpha")
            self.assertEqual(out["after"]["status"], "ready")
            self.assertEqual(out["after"]["last_result_summary"], "repaired")

    def test_repair_rejects_unknown_assignee(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-repair-bad-") as tmp:
            workspace = Path(tmp)
            self._setup(workspace)
            with self.assertRaises(runtime.RuntimeError):
                diagnose.repair_state(workspace, "task_x", assignee="nobody")


class DoctorEndToEndProbeTests(unittest.TestCase):
    """End-to-end probe for doctor: exercises the full path with mocked
    provider/tmux/coordinator probes."""

    def test_doctor_with_no_spec_reports_tmux_and_coordinator(self) -> None:
        with patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"), \
             patch("team_agent.runtime.coordinator_health", return_value={"ok": True, "status": "running", "pid": 1}), \
             patch("team_agent.runtime.get_adapter") as adapter_mock:
            mock_adapter = Mock(command_name="codex", is_installed=lambda: True, version=lambda: "1.0", auth_hint=lambda: {"status": "present"})
            adapter_mock.return_value = mock_adapter
            out = diagnose.doctor()
        self.assertTrue(out["tmux"]["installed"])
        self.assertIn("codex", out["providers"])
        self.assertTrue(out["coordinator"]["ok"])


class QuickStartEndToEndProbeTests(unittest.TestCase):
    """End-to-end probe for quick_start, the BIGGEST orchestration symbol in
    the diagnose lane. Exercises the full path with patched
    runtime.launch / runtime.start_coordinator / wait_ready / preflight."""

    def _seed_agents_dir(self, parent: Path) -> Path:
        agents_dir = parent / "agents-src"
        agents_dir.mkdir()
        (agents_dir / "alpha.md").write_text("---\nid: alpha\nrole: impl\nprovider: fake\n---\n# alpha\n", encoding="utf-8")
        return agents_dir

    def test_quick_start_existing_runtime_short_circuits_to_restart_hint(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-qs-exist-") as tmp:
            parent = Path(tmp)
            agents_dir = self._seed_agents_dir(parent)
            existing = {"team_name": "old", "session_name": "team-old", "state_path": "/tmp/state.json"}
            with patch("team_agent.runtime._compile_team_dir_spec", return_value={"spec": {"runtime": {"session_name": "team-x"}, "team": {"name": "team-x"}, "agents": []}}), \
                 patch("team_agent.runtime._quick_start_existing_context", return_value=existing), \
                 patch("team_agent.diagnose.quick_start.ensure_profiles_for_roles"), \
                 patch.object(Path, "cwd", return_value=parent):
                out = diagnose.quick_start(agents_dir, name="team-x", fresh=False)
        self.assertFalse(out["ok"])
        self.assertEqual(out["step"], "existing_runtime_state")
        self.assertEqual(out["session_name"], "team-old")

    def test_quick_start_preflight_failure_short_circuits(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-qs-pre-") as tmp:
            parent = Path(tmp)
            agents_dir = self._seed_agents_dir(parent)
            with patch("team_agent.runtime._compile_team_dir_spec", return_value={"spec": {"runtime": {"session_name": "team-x"}, "team": {"name": "team-x"}, "agents": []}}), \
                 patch("team_agent.runtime._quick_start_existing_context", return_value=None), \
                 patch("team_agent.diagnose.quick_start.ensure_profiles_for_roles"), \
                 patch("team_agent.diagnose.quick_start.preflight", return_value={"ok": False, "summary": "boom", "blockers": ["x"], "next_actions": ["fix"], "checks": []}), \
                 patch.object(Path, "cwd", return_value=parent):
                out = diagnose.quick_start(agents_dir, name="team-x")
        self.assertFalse(out["ok"])
        self.assertEqual(out["step"], "preflight")
        self.assertEqual(out["summary"], "boom")
        self.assertEqual(out["blockers"], ["x"])

    def test_quick_start_full_path_returns_ready_signal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-qs-ok-") as tmp:
            parent = Path(tmp)
            agents_dir = self._seed_agents_dir(parent)
            launched = {"ok": True, "session_name": "team-x", "agents": [{"id": "alpha"}]}
            coord = {"ok": True, "pid": 7, "status": "started"}
            ready = {"ok": True, "summary": "ready", "readiness": {}}
            preflight_ok = {"ok": True, "checks": [], "blockers": [], "summary": "ok", "details_log": "/tmp/x.json", "next_actions": []}
            with patch("team_agent.runtime._compile_team_dir_spec", return_value={"spec": {"runtime": {"session_name": "team-x", "dangerous_auto_approve": False}, "team": {"name": "team-x"}, "agents": [{"id": "alpha"}]}}), \
                 patch("team_agent.runtime._quick_start_existing_context", return_value=None), \
                 patch("team_agent.diagnose.quick_start.ensure_profiles_for_roles"), \
                 patch("team_agent.diagnose.quick_start.preflight", return_value=preflight_ok), \
                 patch("team_agent.runtime.launch", return_value=launched), \
                 patch("team_agent.runtime.start_coordinator", return_value=coord), \
                 patch("team_agent.diagnose.quick_start.wait_ready", return_value=ready), \
                 patch.object(Path, "cwd", return_value=parent):
                out = diagnose.quick_start(agents_dir, name="team-x")
        self.assertTrue(out["ok"])
        self.assertEqual(out["session_name"], "team-x")
        self.assertEqual(out["coordinator"]["pid"], 7)
        self.assertIn("ready_signal", out)
        self.assertIn("quick-start completed", out["ready_signal"])


if __name__ == "__main__":
    unittest.main()
