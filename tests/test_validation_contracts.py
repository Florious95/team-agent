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

class ValidationTests(unittest.TestCase):
    def test_example_spec_validates(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        self.assertEqual(spec["team"]["name"], "teamspec-full-example")

    def test_ghostty_workspace_display_backend_validates(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        spec["runtime"]["display_backend"] = "ghostty_workspace"
        validate_spec(spec, ROOT)

    def test_unknown_provider_fails(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        spec["agents"][0]["provider"] = "unverified_provider"
        with self.assertRaises(ValidationError) as ctx:
            validate_spec(spec, ROOT)
        self.assertIn("unknown provider", str(ctx.exception))

    def test_unknown_routing_target_fails(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        spec["routing"]["rules"][0]["assign_to"] = "nobody"
        with self.assertRaises(ValidationError) as ctx:
            validate_spec(spec, ROOT)
        self.assertIn("unknown agent", str(ctx.exception))

    def test_dependency_cycle_fails(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        spec["tasks"][0]["deps"] = ["task_review"]
        with self.assertRaises(ValidationError) as ctx:
            validate_spec(spec, ROOT)
        self.assertIn("dependency cycle", str(ctx.exception))

    def test_result_envelope_validation(self) -> None:
        validate_result_envelope(_valid_envelope("success"))
        with self.assertRaises(ValidationError):
            validate_result_envelope({"schema_version": "result_envelope_v1"})

    def test_result_envelope_rejects_common_bad_shapes(self) -> None:
        cases: list[tuple[str, dict]] = []
        bad_schema = _valid_envelope("success")
        bad_schema.pop("schema_version")
        bad_schema["schema"] = "result_envelope_v1"
        cases.append(("schema_version", bad_schema))
        summary_object = _valid_envelope("success")
        summary_object["summary"] = {"text": "not a string"}
        cases.append(("/summary", summary_object))
        tests_object = _valid_envelope("success")
        tests_object["tests"] = {"items": []}
        cases.append(("/tests", tests_object))
        string_item = _valid_envelope("success")
        string_item["artifacts"] = ["artifact.md"]
        cases.append(("/artifacts/0", string_item))
        missing_item_field = _valid_envelope("success")
        missing_item_field["next_actions"] = [{}]
        cases.append(("/next_actions/0/description", missing_item_field))
        for expected, envelope in cases:
            with self.subTest(expected=expected):
                with self.assertRaises(ValidationError) as ctx:
                    validate_result_envelope(envelope)
                self.assertIn(expected, str(ctx.exception))

    def test_role_docs_compile_to_compatible_manifest(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-compile-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            out = workspace / "team.spec.yaml"
            result = compile_team(team, out)
            self.assertTrue(result["ok"])
            spec = load_spec(out)
            self.assertEqual(spec["agents"][0]["id"], "implementer")
            self.assertEqual(spec["agents"][0]["profile"], "codex-default")
            self.assertEqual(spec["runtime"]["display_backend"], "adaptive")
            self.assertTrue(spec["communication"]["worker_to_worker"])
            self.assertNotIn("API_KEY", out.read_text(encoding="utf-8"))

    def test_team_front_matter_runtime_defaults_compile(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-compile-frontmatter-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            (team / "TEAM.md").write_text(
                """---
name: doc-team
objective: Compile role docs.
provider: codex
default_model: gpt-5.4
default_auth_mode: subscription
default_profile: codex-default
dangerous_auto_approve: true
fast: true
display_backend: ghostty_window
tick_interval_sec: 1
push_min_interval_sec: 3
stuck_timeout_sec: 5
worker_to_worker: true
---

Document-driven team.
""",
                encoding="utf-8",
            )
            role = team / "agents" / "implementer.md"
            text = role.read_text(encoding="utf-8")
            text = text.replace("model: gpt-5.5\n", "").replace("auth_mode: subscription\n", "").replace("profile: codex-default\n", "")
            role.write_text(text, encoding="utf-8")
            spec = compile_team(team, workspace / "team.spec.yaml")["spec"]
            self.assertTrue(spec["runtime"]["dangerous_auto_approve"])
            self.assertTrue(spec["runtime"]["fast"])
            self.assertEqual(spec["runtime"]["display_backend"], "ghostty_window")
            self.assertEqual(spec["runtime"]["tick_interval_sec"], 1)
            self.assertEqual(spec["runtime"]["push_min_interval_sec"], 3)
            self.assertEqual(spec["runtime"]["stuck_timeout_sec"], 5)
            self.assertTrue(spec["communication"]["worker_to_worker"])
            self.assertEqual(spec["agents"][0]["model"], "gpt-5.4")
            self.assertEqual(spec["agents"][0]["profile"], "codex-default")

    def test_provider_model_defaults_keep_roles_thin(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-provider-model-defaults-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            (team / "TEAM.md").write_text(
                """---
name: debate-team
objective: Compile thin role docs.
provider_models:
  codex: gpt-5.5
  claude: claude-sonnet-4-6
  claude_code: claude-sonnet-4-6
default_auth_mode: subscription
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "claude-default.example.env").write_text(
                "AUTH_MODE=subscription\nPROFILE_NAME=claude-default\n",
                encoding="utf-8",
            )
            (team / "agents" / "implementer.md").write_text(
                """---
name: editor
role: Editor and Defender
provider: claude_code
profile: claude-default
tools:
  - mcp_team
---

Edit and defend the argument.
""",
                encoding="utf-8",
            )

            spec = compile_team(team, workspace / "team.spec.yaml")["spec"]

            self.assertEqual(spec["agents"][0]["model"], "claude-sonnet-4-6")
            self.assertEqual(spec["agents"][0]["auth_mode"], "subscription")

    def test_subscription_role_without_model_uses_builtin_provider_default(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-builtin-model-default-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            (team / "TEAM.md").write_text(
                """---
name: default-model-team
objective: Compile role docs without model fields.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            role = team / "agents" / "implementer.md"
            role.write_text(role.read_text(encoding="utf-8").replace("model: gpt-5.5\n", ""), encoding="utf-8")

            spec = compile_team(team, workspace / "team.spec.yaml")["spec"]

            self.assertEqual(spec["agents"][0]["model"], "gpt-5.5")

    def test_subscription_role_without_profile_compiles_thin_manifest(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-no-profile-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            role = team / "agents" / "implementer.md"
            role.write_text(role.read_text(encoding="utf-8").replace("profile: codex-default\n", ""), encoding="utf-8")

            spec = compile_team(team, workspace / "team.spec.yaml")["spec"]

            self.assertEqual(spec["agents"][0]["auth_mode"], "subscription")
            self.assertNotIn("profile", spec["agents"][0])
            self.assertNotIn("credential_ref", spec["agents"][0])

    def test_role_docs_missing_required_field_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-compile-bad-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            role = team / "agents" / "implementer.md"
            text = role.read_text(encoding="utf-8").replace("provider: codex\n", "")
            role.write_text(text, encoding="utf-8")
            with self.assertRaises(ValidationError) as ctx:
                compile_team(team, workspace / "team.spec.yaml")
            self.assertIn("missing front matter field provider", str(ctx.exception))

    def test_compatible_api_role_without_profile_fails_compile(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-compatible-profile-required-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8")
                .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n")
                .replace("profile: codex-default\n", ""),
                encoding="utf-8",
            )
            with self.assertRaises(ValidationError) as ctx:
                compile_team(team, workspace / "team.spec.yaml")
            self.assertIn("profile is required", str(ctx.exception))

    def test_role_docs_inline_secret_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-compile-secret-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            role = team / "agents" / "implementer.md"
            role.write_text(role.read_text(encoding="utf-8") + "\nAPI_KEY=sk-inline-secret\n", encoding="utf-8")
            with self.assertRaises(ValidationError) as ctx:
                compile_team(team, workspace / "team.spec.yaml")
            self.assertIn("probable inline secret", str(ctx.exception))


if __name__ == "__main__":
    unittest.main(verbosity=2)
