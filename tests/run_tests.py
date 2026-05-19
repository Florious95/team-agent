from __future__ import annotations

import json
import copy
import fcntl
import os
import shlex
import shutil
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import cli, runtime
from team_agent.compiler import compile_team
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.message_store import MessageStore
from team_agent.mcp_server import TeamOrchestratorTools
from team_agent.permissions import missing_tools, resolve_permissions
from team_agent.providers import ResumeUnavailable, compile_system_prompt, get_adapter
from team_agent.profiles import doctor_profile, init_profile, prepare_agent_profile_launch, show_profile
from team_agent.routing import route_task
from team_agent.rust_core import render_message as core_render_message
from team_agent.simple_yaml import dumps
from team_agent.spec import ValidationError, load_spec, validate_result_envelope, validate_spec
from team_agent.state import load_runtime_state, save_runtime_state


ROOT = Path(__file__).resolve().parents[1]


class ValidationTests(unittest.TestCase):
    def test_example_spec_validates(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        self.assertEqual(spec["team"]["name"], "teamspec-full-example")

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
            self.assertEqual(spec["runtime"]["display_backend"], "ghostty_window")
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


class RuntimeTests(unittest.TestCase):
    def test_fake_e2e(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-test-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-test-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            launched = runtime.launch(spec_path, auto_approve=True)
            try:
                self.assertTrue(launched["ok"])
                import time

                sent = runtime.send_message(workspace, None, "do fake work", task_id="task_impl", requires_ack=True)
                self.assertTrue(sent["ok"])
                collected = {"collected": []}
                for _ in range(10):
                    time.sleep(0.5)
                    collected = runtime.collect(workspace)
                    if collected["collected"]:
                        break
                status = runtime.status(workspace, as_json=True)
                if collected["collected"]:
                    self.assertEqual(collected["collected"][0]["task_id"], "task_impl")
                else:
                    self.assertEqual(status["tasks"][0]["status"], "done")
                    self.assertTrue(status["tasks"][0].get("accepted_result_id"))
                self.assertEqual(status["messages"].get("acknowledged"), 1)
                state_text = (workspace / "team_state.md").read_text(encoding="utf-8")
                self.assertIn("Fake worker handled", state_text)
                events = _events(workspace)
                routing_events = [e for e in events if e["event"] == "routing.decision"]
                self.assertTrue(any(e["source"] == "launch" and e["reason"] == "matched routing rule implementation-to-fake" for e in routing_events))
                self.assertTrue(any(e["source"] == "send" and e["selected_agent"] == "fake_impl" for e in routing_events))
            finally:
                runtime.shutdown(workspace)

    def test_launch_auto_attaches_codex_leader_receiver_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-attach-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["leader"]["provider"] = "codex"
            spec["runtime"]["session_name"] = "team-agent-launch-attach-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")

            def fake_attach(workspace_arg, state, pane, provider, event_log, source, require_current=False):
                self.assertEqual(workspace_arg, workspace)
                self.assertIsNone(pane)
                self.assertEqual(provider, "codex")
                self.assertEqual(source, "launch")
                self.assertFalse(require_current)
                receiver = {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "provider": "codex",
                    "pane_id": "%9",
                    "session_name": "leader-session",
                }
                state["leader_receiver"] = receiver
                event_log.write("leader_receiver.attached", target="%9", provider="codex", source="launch")
                return receiver, {"ok": True}

            with (
                patch("team_agent.runtime._attach_leader_to_state", side_effect=fake_attach) as attached,
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="", stderr="")),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)

            self.assertTrue(launched["ok"])
            self.assertEqual(attached.call_count, 1)
            self.assertEqual(launched["leader_receiver"]["pane_id"], "%9")
            self.assertEqual(load_runtime_state(workspace)["leader_receiver"]["pane_id"], "%9")
            self.assertTrue(any(e["event"] == "leader_receiver.attached" and e["source"] == "launch" for e in _events(workspace)))

    def test_launch_fast_runtime_sends_codex_fast_command(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-fast-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec["agents"][0]["model"] = "gpt-5.4-mini"
            spec["leader"]["provider"] = "fake"
            spec["runtime"]["fast"] = True
            spec["runtime"]["session_name"] = "team-agent-launch-fast-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.is_installed.return_value = True
            adapter.mcp_config.return_value = {}
            adapter.install_mcp.return_value = workspace / ".team/runtime/mcp/codebase.json"
            adapter.handle_startup_prompts.return_value = []

            sent_fast = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "send-keys", "-t"] and "/fast" in args:
                    sent_fast.append(args)
                return proc

            with (
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime.shell_command_for_agent", return_value="true"),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)
            self.assertTrue(launched["ok"])
            self.assertEqual(sent_fast[0][-2:], ["/fast", "Enter"])

    def test_launch_opens_displays_after_all_worker_windows_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-launch-display-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["display_backend"] = "ghostty_window"
            spec["leader"]["provider"] = "fake"
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            spec["agents"].append(peer)
            spec_path.write_text(dumps(spec), encoding="utf-8")
            started_windows: set[str] = set()
            display_snapshots: list[set[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "new-session", "-d"]:
                    started_windows.add(args[6])
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                return proc

            def fake_open_display(workspace_arg, session_name, window_name, agent, event_log):
                display_snapshots.append(set(started_windows))
                return {"status": "opened", "target": f"{session_name}:{window_name}"}

            with (
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._open_ghostty_worker_window", side_effect=fake_open_display),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)

            self.assertTrue(launched["ok"])
            self.assertEqual(started_windows, {"fake_impl", "fake_peer"})
            self.assertEqual(len(display_snapshots), 2)
            self.assertTrue(all(snapshot == {"fake_impl", "fake_peer"} for snapshot in display_snapshots))

    def test_incremental_task_keeps_existing_team_state(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-incremental-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-incremental-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            launched = runtime.launch(spec_path, auto_approve=True)
            tools = TeamOrchestratorTools(workspace)
            try:
                self.assertTrue(launched["ok"])
                first = runtime.send_message(workspace, None, "initial task", task_id="task_impl", requires_ack=True)
                self.assertTrue(first["ok"])
                import time

                time.sleep(1.0)
                runtime.collect(workspace)
                state = runtime.status(workspace, as_json=True)
                self.assertEqual(state["tasks"][0]["status"], "done")
                original_session = state["session_name"]

                followup_task = {
                    "id": "task_followup",
                    "title": "Incremental follow-up",
                    "type": "implementation",
                    "assignee": None,
                    "deps": ["task_impl"],
                    "acceptance": ["follow-up result collected"],
                    "status": "pending",
                    "requires_tools": ["fs_write", "execute_bash"],
                    "files": ["src/followup.py"],
                    "risk": "low",
                }
                assigned = tools.assign_task(followup_task, message="handle follow-up")
                self.assertTrue(assigned["ok"])
                time.sleep(1.0)
                collected = runtime.collect(workspace)
                state = runtime.status(workspace, as_json=True)
                by_id = {task["id"]: task for task in state["tasks"]}
                if collected["collected"]:
                    self.assertEqual(collected["collected"][0]["task_id"], "task_followup")
                else:
                    self.assertEqual(by_id["task_followup"]["status"], "done")
                    self.assertTrue(by_id["task_followup"].get("accepted_result_id"))
                self.assertEqual(state["session_name"], original_session)
                self.assertEqual(by_id["task_impl"]["status"], "done")
                self.assertEqual(by_id["task_followup"]["status"], "done")
            finally:
                runtime.shutdown(workspace)

    def test_busy_agent_pending_message_delivers_after_idle_detection(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-busy-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-busy-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            launched = runtime.launch(spec_path, auto_approve=True)
            try:
                self.assertTrue(launched["ok"])
                import time

                time.sleep(0.2)
                state = load_runtime_state(workspace)
                state["agents"]["fake_impl"]["status"] = "busy"
                save_runtime_state(workspace, state)

                sent = runtime.send_message(workspace, None, "queued while busy", task_id="task_impl", requires_ack=True)
                self.assertFalse(sent["ok"])
                self.assertEqual(sent["status"], "accepted")
                self.assertEqual(MessageStore(workspace).message_counts().get("accepted"), 1)

                pumped = runtime.collect(workspace)
                self.assertIn(sent["message_id"], pumped["delivered_messages"])

                collected = {"collected": []}
                for _ in range(10):
                    time.sleep(0.5)
                    collected = runtime.collect(workspace)
                    if collected["collected"]:
                        break
                status = runtime.status(workspace, as_json=True)
                if collected["collected"]:
                    self.assertEqual(collected["collected"][0]["task_id"], "task_impl")
                else:
                    self.assertEqual(status["tasks"][0]["status"], "done")
                    self.assertTrue(status["tasks"][0].get("accepted_result_id"))
                events = _events(workspace)
                self.assertTrue(any(e["event"] == "runtime.status_detected" and e["status"] == "running" for e in events))
                self.assertTrue(any(e["event"] == "send.pending_delivered" for e in events))
            finally:
                runtime.shutdown(workspace)

    def test_manual_route_override_is_logged(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-route-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            alt = dict(spec["agents"][0])
            alt["id"] = "fake_alt"
            spec["agents"].append(alt)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "missing-route-test",
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_alt": {"status": "running", "provider": "fake", "window": "fake_alt"},
                    },
                    "tasks": spec["tasks"],
                },
            )
            result = runtime.send_message(workspace, "fake_alt", "manual override", task_id="task_impl")
            self.assertFalse(result["ok"])
            routing_events = [e for e in _events(workspace) if e["event"] == "routing.decision"]
            override = routing_events[-1]
            self.assertEqual(override["source"], "send")
            self.assertEqual(override["route_agent"], "fake_impl")
            self.assertEqual(override["selected_agent"], "fake_alt")
            self.assertTrue(override["manual_override"])

    def test_attach_leader_registers_existing_tmux_pane(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-attach-leader-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": spec["tasks"],
                },
            )
            session, pane_id, _ = _start_fake_leader_pane(workspace)
            try:
                attached = runtime.attach_leader(workspace, pane=pane_id, provider="fake")
                self.assertTrue(attached["ok"])
                receiver = load_runtime_state(workspace)["leader_receiver"]
                self.assertEqual(receiver["mode"], "direct_tmux")
                self.assertEqual(receiver["pane_id"], pane_id)
                self.assertEqual(receiver["provider"], "fake")
                self.assertTrue(any(e["event"] == "leader_receiver.attached" for e in _events(workspace)))
            finally:
                runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)

    def test_attach_leader_without_pane_infers_active_tmux_pane(self) -> None:
        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
                proc.returncode = 1
                proc.stderr = "no current client"
                return proc
            if args[:3] == ["tmux", "list-panes", "-a"]:
                proc.stdout = "%9\tsession\t1\twin\t0\t/dev/ttys001\tnode\t1\n"
                return proc
            raise AssertionError(args)

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            pane, discovery = runtime._resolve_leader_pane(None, "codex")
        self.assertEqual(discovery, "active_pane_scan")
        self.assertEqual(pane["pane_id"], "%9")
        self.assertEqual(pane["pane_current_command"], "node")

    def test_resolve_leader_scans_workspace_when_tool_shell_has_wrong_tmux_client(self) -> None:
        workspace = Path("/tmp/team-agent-workspace-scan")

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
                proc.stdout = "%1\tother\t1\twin\t0\t/dev/ttys001\tyazi\t1\t/tmp\t1\n"
                return proc
            if args[:3] == ["tmux", "list-panes", "-a"]:
                proc.stdout = "\n".join(
                    [
                        "%1\tother\t1\twin\t0\t/dev/ttys001\tyazi\t1\t/tmp\t1",
                        "%2\tcodexwork\t1\twin\t1\t/dev/ttys002\tnode\t1\t/tmp/team-agent-workspace-scan\t1",
                        "%3\tcodexwork\t1\twin\t2\t/dev/ttys003\tnode\t0\t/tmp/team-agent-workspace-scan\t1",
                    ]
                )
                return proc
            raise AssertionError(args)

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            pane, discovery = runtime._resolve_leader_pane(None, "codex", workspace=workspace, require_current=True)

        self.assertEqual(discovery, "workspace_pane_scan")
        self.assertEqual(pane["pane_id"], "%2")
        self.assertEqual(pane["pane_current_path"], str(workspace))

    def test_resolve_leader_reports_ambiguous_workspace_panes(self) -> None:
        workspace = Path("/tmp/team-agent-ambiguous")

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"] and "-t" not in args:
                proc.returncode = 1
                return proc
            if args[:3] == ["tmux", "list-panes", "-a"]:
                proc.stdout = "\n".join(
                    [
                        "%2\tcodexwork-a\t1\twin\t1\t/dev/ttys002\tnode\t1\t/tmp/team-agent-ambiguous\t1",
                        "%3\tcodexwork-b\t1\twin\t1\t/dev/ttys003\tnode\t1\t/tmp/team-agent-ambiguous\t1",
                    ]
                )
                return proc
            raise AssertionError(args)

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            with self.assertRaises(TeamAgentRuntimeError) as ctx:
                runtime._resolve_leader_pane(None, "codex", workspace=workspace, require_current=True)

        self.assertIn("multiple tmux leader panes match this workspace", str(ctx.exception))
        self.assertIn("%2", str(ctx.exception))
        self.assertIn("%3", str(ctx.exception))

    def test_leader_start_plan_creates_tmux_session_outside_tmux_and_passes_args(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-plan-") as tmp:
            workspace = Path(tmp)
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            with (
                patch.dict(os.environ, {"TMUX": ""}, clear=False),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
            ):
                plan = runtime.leader_start_plan("codex", ["--model", "gpt-5.5"], workspace)
            self.assertEqual(plan["mode"], "new_tmux_session")
            self.assertEqual(plan["argv"][:3], ["tmux", "new-session", "-s"])
            self.assertIn("team-agent-leader-codex", plan["session_name"])
            self.assertIn("exec codex --model gpt-5.5", plan["argv"][-1])

    def test_leader_start_plan_inside_tmux_execs_provider_in_current_pane(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-current-") as tmp:
            adapter = Mock()
            adapter.command_name = "claude"
            adapter.is_installed.return_value = True
            with (
                patch.dict(os.environ, {"TMUX": "/tmp/tmux.sock,1,0"}, clear=False),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
            ):
                plan = runtime.leader_start_plan("claude_code", ["--dangerously-skip-permissions"], Path(tmp))
            self.assertEqual(plan["mode"], "exec_provider")
            self.assertEqual(plan["argv"], ["claude", "--dangerously-skip-permissions"])

    def test_launch_requires_current_tmux_leader_for_real_workers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-required-") as tmp:
            workspace = Path(tmp)
            spec_path = workspace / "team.spec.yaml"
            spec = _fake_spec(workspace)
            spec["leader"]["provider"] = "codex"
            spec["agents"][0]["provider"] = "codex"
            spec["runtime"]["session_name"] = "team-agent-leader-required-" + workspace.name[-6:]
            spec_path.write_text(dumps(spec), encoding="utf-8")
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime._tmux_current_client_pane_info", return_value=None),
                patch("team_agent.runtime._tmux_list_panes", return_value=[]),
                self.assertRaises(TeamAgentRuntimeError) as ctx,
            ):
                runtime.launch(spec_path, auto_approve=True)
            self.assertIn("could not locate a tmux-managed leader pane", str(ctx.exception))

    def test_worker_to_leader_direct_injection_uses_standard_payload_and_does_not_route_task(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-direct-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": [task],
                },
            )
            session, pane_id, capture_path = _start_fake_leader_pane(workspace)
            runtime.attach_leader(workspace, pane=pane_id, provider="fake")
            unique = "direct leader payload " + workspace.name[-6:]
            try:
                result = runtime.send_message(
                    workspace,
                    "leader",
                    unique,
                    task_id="task_impl",
                    sender="fake_impl",
                    requires_ack=True,
                )
                self.assertTrue(result["ok"])
                self.assertEqual(result["channel"], "direct_tmux")
                self.assertEqual(result["submit_key"], "Enter")
                _wait_for_file_line(capture_path, unique)
                text = capture_path.read_text(encoding="utf-8")
                self.assertIn("Team Agent message from fake_impl for task_impl:", text)
                self.assertIn(f"[team-agent-token:{result['message_id']}]", text)
                self.assertNotIn("TEAM_AGENT_MESSAGE", text)
                events = _events(workspace)
                attempt = next(e for e in events if e["event"] == "leader_receiver.deliver_attempt")
                payload = attempt["payload"]
                self.assertEqual(payload["content"], unique)
                self.assertEqual(payload["from"], "fake_impl")
                self.assertEqual(payload["to"], "leader")
                self.assertEqual(payload["task_id"], "task_impl")
                self.assertFalse(payload["requires_ack"])
                self.assertTrue(payload["message_id"].startswith("msg_"))
                self.assertEqual(result["status"], "submitted")
                self.assertTrue(result["visible"])
                self.assertTrue(result["submitted"])
                state = load_runtime_state(workspace)
                self.assertEqual(state["tasks"][0]["assignee"], "fake_impl")
                self.assertEqual(state["tasks"][0]["status"], "pending")
                self.assertFalse((workspace / ".team" / "runtime" / "leader-inbox.log").exists())
                delivered = next(e for e in events if e["event"] == "leader_receiver.submitted")
                self.assertEqual(delivered["target"], pane_id)
                self.assertEqual(delivered["submit_key"], "Enter")
                self.assertTrue(delivered["submitted"])
                self.assertEqual(delivered["submit_verification"], "Enter_sent_after_visible_token")
            finally:
                runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)

    def test_worker_to_leader_missing_pane_falls_back_and_diagnose_explains(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-leader-missing-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": [task],
                },
            )
            session, pane_id, _ = _start_fake_leader_pane(workspace)
            runtime.attach_leader(workspace, pane=pane_id, provider="fake")
            runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)

            result = runtime.send_message(
                workspace,
                "leader",
                "this should use fallback audit",
                task_id="task_impl",
                sender="fake_impl",
            )
            self.assertFalse(result["ok"])
            self.assertEqual(result["status"], "fallback")
            self.assertEqual(result["message_status"], "failed")
            self.assertEqual(result["reason"], "leader_pane_missing")
            state = load_runtime_state(workspace)
            self.assertEqual(state["tasks"][0]["assignee"], "fake_impl")
            self.assertEqual(state["tasks"][0]["status"], "pending")
            fallback = workspace / ".team" / "runtime" / "leader-inbox.log"
            self.assertIn("Team Agent message from fake_impl for task_impl:", fallback.read_text(encoding="utf-8"))
            diagnosed = runtime.diagnose(workspace)
            self.assertIn("leader_pane_missing", {issue["kind"] for issue in diagnosed["issues"]})

    def test_codex_working_submit_key_uses_enter_for_followup_submission(self) -> None:
        submit_key, reason = runtime._choose_leader_submit_key("codex", "• running command esc to interrupt")
        self.assertEqual(submit_key, "Enter")
        self.assertEqual(reason, "codex_busy_submit_followup")

    def test_pasted_content_prompt_recognizes_new_claude_text_placeholder(self) -> None:
        self.assertTrue(runtime._capture_has_pasted_content_prompt("› [Pasted text #1 +67 lines]"))
        self.assertTrue(
            runtime._capture_has_pasted_content_prompt(
                "› [Pasted text #1 +67\n"
                "lines][Pasted text #2 +66 lines]\n"
            )
        )

    def test_message_fragment_recognizes_literal_visible_long_paste(self) -> None:
        expected = (
            "Team Agent message from reviewer:\n\n"
            "### 总体判断\n\n"
            "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
            "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。\n\n"
            "[team-agent-token:msg_long]"
        )
        capture = (
            "### 总体判断\n\n"
            "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
            "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。"
        )
        self.assertTrue(runtime._capture_contains_message_fragment(capture, expected))

    def test_message_fragment_matching_ignores_generic_header(self) -> None:
        expected = "Team Agent message from player_d:\n\n投A：时间感太泛\n\n[team-agent-token:msg_aaa046530604]"

        self.assertFalse(runtime._capture_contains_message_fragment("Team Agent message from player_d:", expected))
        self.assertTrue(runtime._capture_contains_message_fragment("投A：时间感太泛", expected))

    def test_wait_for_message_ready_does_not_accept_old_header(self) -> None:
        expected = "Team Agent message from player_d:\n\n投A：时间感太泛\n\n[team-agent-token:msg_aaa046530604]"

        def fake_run_cmd(args: list[str], timeout: int = 20):
            self.assertEqual(args[:3], ["tmux", "capture-pane", "-p"])
            return Mock(returncode=0, stdout="Team Agent message from player_d:\n\n旧消息", stderr="")

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            visible, verification, _ = runtime._wait_for_message_ready(
                "%1",
                "msg_aaa046530604",
                0.0,
                expected_text=expected,
                allow_pasted_prompt=False,
            )

        self.assertFalse(visible)
        self.assertEqual(verification, "capture_missing_token")

    def test_wait_for_message_ready_accepts_only_new_pasted_prompt(self) -> None:
        pasted = "› [Pasted Content 123 chars]"

        def fake_run_cmd(args: list[str], timeout: int = 20):
            self.assertEqual(args[:3], ["tmux", "capture-pane", "-p"])
            return Mock(returncode=0, stdout=pasted, stderr="")

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
            visible, verification, _ = runtime._wait_for_message_ready("%1", "msg_new", 0.0, baseline_capture="")
            old_visible, old_verification, _ = runtime._wait_for_message_ready(
                "%1",
                "msg_new",
                0.0,
                baseline_capture=pasted,
            )

        self.assertTrue(visible)
        self.assertEqual(verification, "capture_contains_new_pasted_content_prompt")
        self.assertFalse(old_visible)
        self.assertEqual(old_verification, "capture_missing_token")

    def test_leader_tmux_injection_retries_until_visible_then_submits_enter(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
            "team_agent.runtime._wait_for_message_ready",
            side_effect=[
                (False, "capture_missing_token", ""),
                (False, "capture_missing_token", ""),
                (False, "capture_missing_token", ""),
                (True, "capture_contains_token", "[team-agent-token:msg_retry]"),
            ],
        ):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_retry]",
                "Enter",
                "team-agent-test",
                attempts=2,
            )
        self.assertTrue(result["ok"])
        self.assertTrue(result["submitted"])
        self.assertEqual(result["attempts"][0]["verification"], "capture_missing_token")
        self.assertEqual(result["attempts"][1]["verification"], "capture_contains_token")
        self.assertEqual(sum(1 for call in calls if call[:3] == ["tmux", "send-keys", "-t"]), 1)
        self.assertIn("Enter", calls[-1])

    def test_leader_tmux_injection_submits_pasted_content_prompt_until_cleared(self) -> None:
        calls: list[list[str]] = []
        paste_calls: list[list[str]] = []
        send_calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                paste_calls.append(args)
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› [Pasted Content 1093 chars]" if paste_calls and len(send_calls) < 2 else "claude>"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_pasted]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_new_pasted_content_prompt")
        self.assertEqual(result["submit_verification"], "pasted_content_prompt_absent_after_submit")
        self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter"])

    def test_leader_tmux_injection_submits_new_pasted_text_prompt(self) -> None:
        paste_calls: list[list[str]] = []
        send_calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                paste_calls.append(args)
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› [Pasted text #1 +67 lines]" if paste_calls and not send_calls else "claude>"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_pasted_new]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_new_pasted_content_prompt")
        self.assertEqual(result["submit_verification"], "pasted_content_prompt_absent_after_submit")
        self.assertEqual([call[-1] for call in send_calls], ["Enter"])

    def test_leader_tmux_injection_submits_visible_message_fragment(self) -> None:
        send_calls: list[list[str]] = []
        text = (
            "Team Agent message from reviewer:\n\n"
            "### 总体判断\n\n"
            "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
            "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。\n\n"
            "[team-agent-token:msg_fragment]"
        )

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = (
                    "### 总体判断\n\n"
                    "这是一篇情感调度克制、场景选择具有当代质感的好短篇，"
                    "林小满挑那颗最大的杏仁是整篇小说真正的情感核心。"
                )
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text("%1", text, "Enter", "team-agent-test", attempts=1)
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_message_fragment")
        self.assertEqual(result["submit_verification"], "Enter_sent_after_visible_fragment")
        self.assertEqual([call[-1] for call in send_calls], ["Enter"])

    def test_leader_tmux_injection_submits_preexisting_visible_fragment_without_repaste(self) -> None:
        calls: list[list[str]] = []
        send_calls: list[list[str]] = []
        text = (
            "Team Agent message from reviewer:\n\n"
            "### 总体判断\n\n"
            "保留称粮段，外卖备注段可以压缩，这是截图里已经进入输入框的长结果片段。\n\n"
            "[team-agent-token:msg_preexisting]"
        )

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› 保留称粮段，外卖备注段可以压缩，这是截图里已经进入输入框的长结果片段。"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text("%1", text, "Enter", "team-agent-test", attempts=1)
        self.assertTrue(result["ok"])
        self.assertEqual(result["verification"], "capture_contains_message_fragment")
        self.assertEqual(result["submit_verification"], "Enter_sent_after_visible_fragment")
        self.assertEqual(result["attempts"][0]["buffer_method"], "preexisting_prompt")
        self.assertEqual([call[-1] for call in send_calls], ["Enter"])
        self.assertFalse(any(call[:2] == ["tmux", "paste-buffer"] for call in calls))

    def test_leader_tmux_injection_exits_copy_mode_before_paste(self) -> None:
        calls: list[list[str]] = []
        mode_checks = 0

        def fake_run_cmd(args: list[str], timeout: int = 20):
            nonlocal mode_checks
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                mode_checks += 1
                proc.stdout = "1\n" if mode_checks == 1 else "0\n"
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "[team-agent-token:msg_copy_mode]"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_copy_mode]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertTrue(result["ok"])
        self.assertTrue(result["attempts"][0]["recovered_from_mode"])
        self.assertIn(["tmux", "send-keys", "-t", "%1", "-X", "cancel"], calls)

    def test_leader_tmux_injection_reports_unverified_when_pasted_prompt_stays(self) -> None:
        paste_calls: list[list[str]] = []
        send_calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            elif args[:2] == ["tmux", "paste-buffer"]:
                paste_calls.append(args)
            elif args[:3] == ["tmux", "send-keys", "-t"]:
                send_calls.append(args)
            elif args[:3] == ["tmux", "capture-pane", "-p"]:
                proc.stdout = "› [Pasted Content 1093 chars]" if paste_calls else "claude>"
            return proc

        with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
            result = runtime._tmux_inject_text(
                "%1",
                "Team Agent message\n\n[team-agent-token:msg_stuck]",
                "Enter",
                "team-agent-test",
                attempts=1,
            )
        self.assertFalse(result["ok"])
        self.assertEqual(result["stage"], "submit-verification")
        self.assertEqual(result["submit_verification"], "pasted_content_prompt_still_present_after_retries")
        self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter", "Enter"])

    def test_tmux_set_buffer_uses_stdin_for_large_text(self) -> None:
        large_text = "x" * (runtime.TMUX_STDIN_BUFFER_THRESHOLD + 1)
        loaded = Mock(returncode=0, stdout="", stderr="")
        with (
            patch("team_agent.runtime._tmux_load_buffer_stdin", return_value=loaded) as load_buffer,
            patch("team_agent.runtime.run_cmd") as run_cmd,
        ):
            result = runtime._tmux_set_buffer_text("team-agent-large", large_text)
        self.assertTrue(result["ok"])
        self.assertEqual(result["method"], "stdin_load_buffer")
        self.assertEqual(result["text_bytes"], len(large_text))
        load_buffer.assert_called_once_with("team-agent-large", large_text)
        run_cmd.assert_not_called()

    def test_leader_tmux_injection_large_text_uses_stdin_bracketed_paste_and_adaptive_wait(self) -> None:
        calls: list[list[str]] = []
        wait_timeouts: list[float] = []
        large_text = "Team Agent message\n\n" + ("超长正文\n" * 12000) + "\n[team-agent-token:msg_large]"

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            proc = Mock(returncode=0, stdout="", stderr="")
            if args[:3] == ["tmux", "display-message", "-p"]:
                proc.stdout = "0\n"
            if args[:2] == ["tmux", "set-buffer"]:
                raise AssertionError("large leader payload must not be passed as a command argument")
            return proc

        def fake_wait(target: str, message_id: str, timeout: float, expected_text: str = "", **kwargs):
            wait_timeouts.append(timeout)
            if timeout == 0:
                return False, "capture_missing_token", ""
            return True, "capture_contains_token", "[team-agent-token:msg_large]"

        with (
            patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            patch("team_agent.runtime._tmux_load_buffer_stdin", return_value=Mock(returncode=0, stdout="", stderr="")),
            patch("team_agent.runtime._wait_for_message_ready", side_effect=fake_wait),
            patch("team_agent.runtime.time.sleep", return_value=None),
        ):
            result = runtime._tmux_inject_text("%1", large_text, "Enter", "team-agent-large", attempts=1)

        self.assertTrue(result["ok"])
        self.assertEqual(result["attempts"][0]["buffer_method"], "stdin_load_buffer")
        self.assertGreater(result["attempts"][0]["text_bytes"], runtime.TMUX_STDIN_BUFFER_THRESHOLD)
        self.assertGreater(wait_timeouts[-1], runtime.TMUX_PASTE_MIN_READY_TIMEOUT)
        paste_call = next(call for call in calls if call[:2] == ["tmux", "paste-buffer"])
        self.assertIn("-p", paste_call)

    def test_rust_core_renderer_integration(self) -> None:
        payload = {"message_id": "msg_test", "from": "worker", "task_id": "task_x", "content": "hello"}
        rendered = core_render_message(payload)
        self.assertTrue(rendered["ok"])
        self.assertIn("Team Agent message from worker for task_x:", rendered["text"])
        self.assertIn("[team-agent-token:msg_test]", rendered["text"])
        self.assertIn(rendered["engine"], {"rust", "python_fallback"})

    def test_stale_leader_target_rediscovery_unique_and_ambiguous(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-rediscover-") as tmp:
            workspace = Path(tmp)
            event_log = runtime.EventLog(workspace)
            receiver = {"provider": "codex", "pane_id": "%old"}
            one = {
                "ok": True,
                "targets": [
                    {
                        "pane_id": "%1",
                        "session_name": "s",
                        "window_index": "1",
                        "window_name": "w",
                        "pane_index": "0",
                        "pane_tty": "/dev/ttys001",
                        "pane_current_command": "node",
                        "pane_active": True,
                        "fingerprint": "s|1|0|/dev/ttys001",
                    }
                ],
            }
            with patch("team_agent.runtime.core_list_targets", return_value=one):
                result = runtime._rediscover_leader_receiver(receiver, event_log)
            self.assertEqual(result["status"], "updated")
            self.assertEqual(result["receiver"]["pane_id"], "%1")
            many = {"ok": True, "targets": [one["targets"][0], {**one["targets"][0], "pane_id": "%2"}]}
            with patch("team_agent.runtime.core_list_targets", return_value=many):
                result = runtime._rediscover_leader_receiver(receiver, event_log)
            self.assertEqual(result["status"], "ambiguous")

    def test_profile_init_doctor_and_preflight_are_secret_safe(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-") as tmp:
            workspace = Path(tmp)
            init = init_profile(workspace, "codex-default", "subscription")
            self.assertTrue(init["ok"])
            self.assertFalse(init["secret_written"])
            self.assertTrue((workspace / ".team" / "current" / "profiles" / "codex-default.env").exists())
            boundary = workspace / ".team" / "current" / "profiles" / "AGENTS.md"
            self.assertIn("Do not read", boundary.read_text(encoding="utf-8"))
            claude_boundary = workspace / ".team" / "current" / "profiles" / "CLAUDE.md"
            self.assertIn("Do not read", claude_boundary.read_text(encoding="utf-8"))
            team = _write_doc_team(workspace)
            doctor = doctor_profile(workspace, "codex-default")
            self.assertTrue(doctor["ok"])
            self.assertFalse(doctor["secret_values_printed"])
            self.assertFalse(doctor["raw_file_read_allowed_for_agents"])
            real_profile = team / "profiles" / "codex-default.env"
            real_profile.write_text(
                "AUTH_MODE=subscription\nAPI_KEY=sk-do-not-print\nBASE_URL=https://user:url-password-do-not-print@example.com/v1?api_key=sk-do-not-print\n",
                encoding="utf-8",
            )
            doctor = doctor_profile(workspace, "codex-default")
            self.assertNotIn("sk-do-not-print", json.dumps(doctor))
            show = show_profile(workspace, "codex-default")
            self.assertTrue(show["safe_for_agent_context"])
            self.assertFalse(show["raw_file_read_allowed_for_agents"])
            self.assertEqual(show["values"]["API_KEY"], {"present": True, "redacted": True})
            self.assertNotIn("sk-do-not-print", json.dumps(show))
            self.assertNotIn("url-password-do-not-print", json.dumps(show))
            self.assertEqual(show["values"]["BASE_URL"]["value"], "https://[redacted]@example.com/v1")
            out = workspace / "team.spec.yaml"
            compile_team(team, out)
            self.assertNotIn("sk-do-not-print", out.read_text(encoding="utf-8"))
            preflight = runtime.preflight(team)
            self.assertIn("summary", preflight)
            self.assertIn("next_actions", preflight)
            self.assertIn("details_log", preflight)

    def test_compatible_profile_template_requires_local_values_before_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-compatible-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            init = init_profile(workspace, "deepseek", "compatible_api")
            self.assertTrue(init["ok"])
            profile_text = Path(init["path"]).read_text(encoding="utf-8")
            self.assertIn("BASE_URL=\n", profile_text)
            self.assertIn("API_KEY=\n", profile_text)
            self.assertIn("MODEL=\n", profile_text)
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8")
                .replace("provider: codex\n", "provider: claude_code\n")
                .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n")
                .replace("profile: codex-default\n", "profile: deepseek\n"),
                encoding="utf-8",
            )
            result = runtime.preflight(team)
            profiles = next(check for check in result["checks"] if check["name"] == "profiles")
            self.assertFalse(profiles["ok"])
            implementer = next(item for item in profiles["checks"] if item["agent_id"] == "implementer")
            self.assertIn("BASE_URL", implementer["missing_required"])
            self.assertIn("API_KEY", implementer["missing_required"])
            self.assertTrue(any("profile show <name>" in action for action in result["next_actions"]))
            self.assertTrue(any("must not read .team/*/profiles/*.env" in action for action in result["next_actions"]))
            self.assertNotIn("sk-", json.dumps(result))

    def test_profile_model_satisfies_compatible_role_doc_without_role_model(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-model-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            profile_dir = team / "profiles"
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            role = team / "agents" / "implementer.md"
            role.write_text(
                """---
name: implementer
role: Implementation Engineer
provider: claude_code
auth_mode: compatible_api
profile: deepseek
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Implement bounded tasks and report result_envelope_v1.
""",
                encoding="utf-8",
            )
            compiled = compile_team(team, workspace / "team.spec.yaml")["spec"]
            self.assertIsNone(compiled["agents"][0]["model"])
            preflight = runtime.preflight(team)
            self.assertTrue(preflight["ok"])
            profiles = next(check for check in preflight["checks"] if check["name"] == "profiles")
            implementer = next(item for item in profiles["checks"] if item["agent_id"] == "implementer")
            self.assertEqual(implementer["effective_model"], "deepseek-v4-flash")
            self.assertEqual(implementer["model_source"], "profile")

    def test_validate_accepts_team_directory_role_docs(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-validate-dir-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)

            result = runtime.validate_file(team)

            self.assertTrue(result["ok"])
            self.assertEqual(result["type"], "team_dir")
            self.assertEqual(result["team"], "doc-team")
            self.assertEqual(result["agents"], ["implementer"])

    def test_preflight_treats_missing_rust_core_as_python_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-preflight-python-fallback-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: fallback-team
objective: Preflight without rust core.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "agents" / "fake.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake
auth_mode: subscription
tools:
  - mcp_team
---

Work.
""",
                encoding="utf-8",
            )
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.core_binary", return_value=None),
            ):
                result = runtime.preflight(team)

            self.assertTrue(result["ok"], result)
            rust = next(check for check in result["checks"] if check["name"] == "rust_core")
            self.assertEqual(rust["status"], "python_fallback")

    def test_role_model_mismatch_with_profile_model_fails_preflight(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-model-mismatch-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            profile_dir = team / "profiles"
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8")
                .replace("provider: codex\n", "provider: claude_code\n")
                .replace("model: gpt-5.5\n", "model: deepseek-chat\n")
                .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n")
                .replace("profile: codex-default\n", "profile: deepseek\n"),
                encoding="utf-8",
            )
            preflight = runtime.preflight(team)
            self.assertFalse(preflight["ok"])
            self.assertIn("does not match profile MODEL", json.dumps(preflight, ensure_ascii=False))

    def test_profile_env_is_sourced_without_leaking_secret_in_launch_command(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-profile-env-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "claude_reviewer")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            profile_launch = prepare_agent_profile_launch(workspace, agent)
            self.assertEqual(
                profile_launch["claude_projects_root"],
                str(workspace / ".team" / "runtime" / "provider-config" / "claude_reviewer" / "claude" / "projects"),
            )
            with patch.dict(
                os.environ,
                {
                    "HTTPS_PROXY": "https://claude:ambient-secret@proxy.example:8443",
                    "NODE_EXTRA_CA_CERTS": "/tmp/claude-proxy-ca.crt",
                },
            ):
                command = shell_command_for_agent(agent, workspace, {})
            env_path = workspace / ".team" / "runtime" / "provider-env" / "claude_reviewer.env"
            self.assertTrue(env_path.exists())
            self.assertIn(str(env_path), command)
            self.assertIn("--model deepseek-v4-pro", command)
            self.assertNotIn("sk-do-not-print", command)
            self.assertNotIn("ambient-secret", command)
            env_text = env_path.read_text(encoding="utf-8")
            self.assertIn("unset ANTHROPIC_API_KEY", env_text)
            self.assertNotIn("unset HTTPS_PROXY", env_text)
            self.assertNotIn("unset NODE_EXTRA_CA_CERTS", env_text)
            self.assertIn("export CLAUDE_CONFIG_DIR=", env_text)
            config_dir = workspace / ".team" / "runtime" / "provider-config" / "claude_reviewer" / "claude"
            self.assertIn(str(config_dir), env_text)
            settings = json.loads((config_dir / "settings.json").read_text(encoding="utf-8"))
            self.assertEqual(settings["theme"], "auto")
            state = json.loads((config_dir / ".claude.json").read_text(encoding="utf-8"))
            self.assertTrue(state["hasCompletedOnboarding"])
            self.assertTrue(state["projects"][str(workspace)]["hasTrustDialogAccepted"])
            self.assertIn("export ANTHROPIC_AUTH_TOKEN=", env_text)
            self.assertNotIn("export ANTHROPIC_API_KEY=", env_text)
            self.assertNotIn("ambient-secret", env_text)
            self.assertEqual(
                agent["_provider_profile"]["claude_projects_root"],
                str(config_dir / "projects"),
            )

    def test_compatible_claude_mcp_is_persisted_in_managed_config(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-compatible-mcp-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "claude_reviewer")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            mcp_config = get_adapter("claude_code").mcp_config(workspace, "claude_reviewer")

            command = shell_command_for_agent(agent, workspace, mcp_config)

            self.assertNotIn("--mcp-config", command)
            self.assertNotIn("--strict-mcp-config", command)
            self.assertNotIn("sk-do-not-print", command)
            config_dir = workspace / ".team" / "runtime" / "provider-config" / "claude_reviewer" / "claude"
            state = json.loads((config_dir / ".claude.json").read_text(encoding="utf-8"))
            project = state["projects"][str(workspace)]
            self.assertIn(str(workspace.resolve()), state["projects"])
            self.assertTrue(project["hasTrustDialogAccepted"])
            server = project["mcpServers"]["team_orchestrator"]
            self.assertEqual(server["env"]["TEAM_AGENT_ID"], "claude_reviewer")
            self.assertEqual(server["args"][:2], ["-m", "team_agent.mcp_server"])

    def test_subscription_claude_keeps_strict_command_line_mcp(self) -> None:
        adapter = get_adapter("claude_code")
        agent = _provider_agent("claude_code", "claude_reviewer")
        agent["auth_mode"] = "subscription"
        agent["_runtime"] = {}
        with tempfile.TemporaryDirectory(prefix="team-agent-subscription-mcp-") as tmp:
            workspace = Path(tmp)
            cmd = adapter.build_command(agent, workspace, adapter.mcp_config(workspace, "claude_reviewer"))

        self.assertIn("--mcp-config", cmd)
        self.assertIn("--strict-mcp-config", cmd)

    def test_attach_profile_resume_root_uses_current_compatible_claude_config(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-resume-root-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "coder")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            previous = {"session_id": "old-session", "claude_projects_root": str(Path.home() / ".claude" / "projects")}

            prepared = runtime._attach_profile_resume_root(workspace, agent, previous)

            expected = workspace / ".team" / "runtime" / "provider-config" / "coder" / "claude" / "projects"
            self.assertEqual(prepared["claude_projects_root"], str(expected))
            self.assertEqual(agent["_provider_profile"]["claude_projects_root"], str(expected))

    def test_compatible_profile_direct_proxy_mode_unsets_native_proxy_environment(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-profile-direct-") as tmp:
            workspace = Path(tmp)
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro",
                        "PROXY_MODE=direct",
                        "PROFILE_SMOKE=false",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            agent = _provider_agent("claude_code", "claude_reviewer")
            agent["auth_mode"] = "compatible_api"
            agent["profile"] = "deepseek"
            with patch.dict(
                os.environ,
                {
                    "HTTPS_PROXY": "https://claude:ambient-secret@proxy.example:8443",
                    "NODE_EXTRA_CA_CERTS": "/tmp/claude-proxy-ca.crt",
                },
            ):
                command = shell_command_for_agent(agent, workspace, {})
            env_path = workspace / ".team" / "runtime" / "provider-env" / "claude_reviewer.env"
            self.assertTrue(env_path.exists())
            self.assertNotIn("ambient-secret", command)
            env_text = env_path.read_text(encoding="utf-8")
            self.assertIn("unset HTTPS_PROXY", env_text)
            self.assertIn("unset https_proxy", env_text)
            self.assertIn("unset NODE_EXTRA_CA_CERTS", env_text)
            self.assertIn("export CLAUDE_CONFIG_DIR=", env_text)
            self.assertNotIn("ambient-secret", env_text)

    def test_subscription_profile_keeps_native_settings_environment(self) -> None:
        from team_agent.providers import shell_command_for_agent

        with tempfile.TemporaryDirectory(prefix="team-agent-profile-native-") as tmp:
            workspace = Path(tmp)
            init_profile(workspace, "claude-native", "subscription")
            agent = _provider_agent("claude_code", "claude_native")
            agent["auth_mode"] = "subscription"
            agent["profile"] = "claude-native"
            with patch.dict(os.environ, {"HTTPS_PROXY": "https://claude:keep-native-proxy@proxy.example:8443"}):
                command = shell_command_for_agent(agent, workspace, {})
            env_path = workspace / ".team" / "runtime" / "provider-env" / "claude_native.env"
            self.assertFalse(env_path.exists())
            self.assertNotIn("unset HTTPS_PROXY", command)
            self.assertNotIn("keep-native-proxy", command)

    def test_launch_blocks_on_compatible_api_smoke_failure_before_worker_windows(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-smoke-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0].update(
                {
                    "provider": "claude_code",
                    "model": "deepseek-v4-pro-bad",
                    "auth_mode": "compatible_api",
                    "profile": "deepseek",
                    "credential_ref": "profile:deepseek",
                }
            )
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            profile_dir = workspace / ".team" / "current" / "profiles"
            profile_dir.mkdir(parents=True)
            (profile_dir / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-pro-bad",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            adapter = Mock()
            adapter.command_name = "claude"
            adapter.is_installed.return_value = True
            with (
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.profiles.urllib.request.urlopen", side_effect=Exception("model rejected sk-do-not-print")),
                self.assertRaises(TeamAgentRuntimeError) as ctx,
            ):
                runtime.launch(spec_path, auto_approve=True)
            message = str(ctx.exception)
            self.assertIn("provider profile smoke check failed", message)
            self.assertNotIn("sk-do-not-print", message)

    def test_quick_start_reports_proxy_connectivity_profile_smoke_blocker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-proxy-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir()
            (team / "TEAM.md").write_text(
                """---
name: proxy-team
objective: Proxy smoke failure.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                        "HTTPS_PROXY=http://user:secret@proxy.local:8443",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            (team / "agents" / "coder.md").write_text(
                """---
name: coder
role: Implementation Worker
provider: claude_code
auth_mode: compatible_api
profile: deepseek
tools:
  - fs_read
  - mcp_team
---

Work.
""",
                encoding="utf-8",
            )
            with (
                patch.dict(
                    os.environ,
                    {
                        "HTTPS_PROXY": "http://user:secret@proxy.local:8443",
                        "https_proxy": "http://user:secret@proxy.local:8443",
                    },
                ),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.profiles.urllib.request.urlopen", side_effect=Exception("Connection reset by peer")),
            ):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(team)
                finally:
                    os.chdir(cwd)
            self.assertFalse(result["ok"])
            self.assertEqual(result["step"], "preflight")
            self.assertTrue(any("proxy_connectivity_failed" in item for item in result["blockers"]))
            rendered = json.dumps(result, ensure_ascii=False)
            self.assertIn("proxy.local:8443", rendered)
            self.assertNotIn("secret", rendered)
            self.assertNotIn("sk-do-not-print", rendered)

    def test_compatible_api_smoke_reports_ambient_proxy_choice_by_default(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-ambient-proxy-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir()
            (team / "TEAM.md").write_text(
                """---
name: ambient-proxy-team
objective: Ambient proxy reported.
display_backend: none
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "deepseek.env").write_text(
                "\n".join(
                    [
                        "AUTH_MODE=compatible_api",
                        "PROFILE_NAME=deepseek",
                        "BASE_URL=https://api.deepseek.com/anthropic",
                        "API_KEY=sk-do-not-print",
                        "MODEL=deepseek-v4-flash",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            (team / "agents" / "coder.md").write_text(
                """---
name: coder
role: Implementation Worker
provider: claude_code
auth_mode: compatible_api
profile: deepseek
tools:
  - fs_read
  - mcp_team
---

Work.
""",
                encoding="utf-8",
            )
            with (
                patch.dict(os.environ, {"HTTPS_PROXY": "http://user:ambient-secret@proxy.local:8443"}),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.profiles.urllib.request.urlopen", side_effect=Exception("Connection reset by peer")),
            ):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(team)
                finally:
                    os.chdir(cwd)
            self.assertFalse(result["ok"])
            self.assertTrue(any("proxy_source=ambient" in item for item in result["blockers"]))
            self.assertTrue(any("PROXY_MODE=direct" in item for item in result["next_actions"]))
            rendered = json.dumps(result, ensure_ascii=False)
            self.assertIn("proxy.local:8443", rendered)
            self.assertNotIn("ambient-secret", rendered)
            self.assertNotIn("sk-do-not-print", rendered)

    def test_preflight_reports_invalid_codex_model_before_quick_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-preflight-model-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            role = team / "agents" / "implementer.md"
            role.write_text(
                role.read_text(encoding="utf-8").replace("model: gpt-5.5\n", "model: GPT-5.3-Codex-Spark\n"),
                encoding="utf-8",
            )
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            adapter.validate_model.return_value = {
                "ok": False,
                "provider": "codex",
                "model": "GPT-5.3-Codex-Spark",
                "reason": "model_id_not_exact",
                "suggested_model": "gpt-5.3-codex-spark",
            }
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                preflight = runtime.preflight(team)
            self.assertFalse(preflight["ok"])
            models = next(check for check in preflight["checks"] if check["name"] == "models")
            self.assertFalse(models["ok"])
            self.assertEqual(models["checks"][0]["suggested_model"], "gpt-5.3-codex-spark")

    def test_message_store_creates_blackbox_runtime_tables(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-schema-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            conn = store.connect()
            try:
                tables = {
                    row[0]
                    for row in conn.execute("select name from sqlite_master where type='table'").fetchall()
                }
            finally:
                conn.close()
            self.assertIn("scheduled_events", tables)
            self.assertIn("delivery_tokens", tables)
            self.assertIn("agent_health", tables)
            self.assertIn("peer_allowlist", tables)
            self.assertIn("result_watchers", tables)

    def test_message_store_claim_for_delivery_is_single_consumer(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claim-") as tmp:
            workspace = Path(tmp)
            store = MessageStore(workspace)
            message_id = store.create_message(None, "leader", "fake_impl", "hello")
            self.assertTrue(store.claim_for_delivery(message_id))
            self.assertFalse(store.claim_for_delivery(message_id))
            row = next(row for row in store.messages() if row["message_id"] == message_id)
            self.assertEqual(row["status"], "target_resolved")

    def test_send_default_timeout_reports_submitted_unverified(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-visible-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "no visible token here"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                sent = runtime.send_message(workspace, "fake_impl", "hello", timeout=0.01)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted_unverified")
            self.assertFalse(sent["visible"])
            self.assertTrue(sent["submitted"])
            self.assertIn("capture did not confirm", sent["warning"])
            self.assertEqual(MessageStore(workspace).delivery_tokens()[0]["message_id"], sent["message_id"])

    def test_worker_delivery_retries_paste_until_message_ready(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-paste-retry-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    if len(paste_calls) == 1:
                        proc.stdout = "prompt without delivered message"
                    elif send_calls:
                        proc.stdout = "codex>"
                    else:
                        proc.stdout = "› [Pasted Content 1093 chars]"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "hello", timeout=0.01)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted")
            self.assertEqual(len(paste_calls), 2)
            self.assertEqual(len(send_calls), 1)
            self.assertEqual(sent["paste_attempts"][0]["verification"], "capture_missing_token")
            self.assertEqual(sent["paste_attempts"][1]["verification"], "capture_contains_new_pasted_content_prompt")

    def test_worker_delivery_submits_new_pasted_text_prompt_once_with_adaptive_wait(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-new-paste-prompt-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []
            wait_timeouts: list[float] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "codex>"
                return proc

            def fake_wait(target: str, message_id: str, timeout: float, expected_text: str = ""):
                wait_timeouts.append(timeout)
                return True, "capture_contains_pasted_content_prompt", "› [Pasted text #1 +67 lines]"

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._wait_for_worker_message_ready", side_effect=fake_wait),
                patch("team_agent.runtime.time.sleep", return_value=None),
            ):
                sent = runtime.send_message(workspace, "fake_impl", "x" * 6000, timeout=30)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted")
            self.assertEqual(len(paste_calls), 1)
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])
            self.assertEqual(sent["paste_attempts"][0]["verification"], "capture_contains_pasted_content_prompt")
            self.assertLess(wait_timeouts[0], 30)
            self.assertEqual(wait_timeouts[0], runtime.TMUX_PASTE_MIN_READY_TIMEOUT)

    def test_worker_delivery_submits_visible_message_fragment_once(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-fragment-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            content = "这是一段足够长的评审正文片段，用来模拟长文本已经进入输入框但 token 不在可见区。"
            paste_calls: list[list[str]] = []
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    paste_calls.append(args)
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = content
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", content, timeout=30)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted")
            self.assertEqual(len(paste_calls), 1)
            self.assertEqual([call[-1] for call in send_calls], ["Enter"])
            self.assertEqual(sent["verification"], "capture_contains_message_fragment")

    def test_worker_pasted_content_prompt_retries_enter_until_submitted(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-enter-retry-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "codex>" if len(send_calls) >= 2 else "› [Pasted Content 1093 chars]"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "long payload", timeout=0.01)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted")
            self.assertEqual(sent["submit_verification"], "pasted_content_prompt_absent_after_submit")
            self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter"])
            self.assertEqual(sent["submit_attempts"][0]["verification"], "pasted_content_prompt_still_present")
            self.assertEqual(sent["submit_attempts"][1]["verification"], "pasted_content_prompt_absent")

    def test_worker_pasted_content_prompt_reports_unverified_when_enter_does_not_submit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-enter-stuck-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "› [Pasted Content 1093 chars]"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "long payload", timeout=0.01)
            self.assertFalse(sent["ok"])
            self.assertEqual(sent["status"], "injected_unverified")
            self.assertEqual(sent["submit_verification"], "pasted_content_prompt_still_present_after_retries")
            self.assertEqual([call[-1] for call in send_calls], ["Enter", "Enter", "Enter"])
            events = _events(workspace)
            unverified = next(e for e in events if e["event"] == "send.unverified")
            self.assertEqual(unverified["submit_verification"], "pasted_content_prompt_still_present_after_retries")

    def test_send_no_wait_keeps_injected_semantics(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-no-wait-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "still hidden"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                sent = runtime.send_message(workspace, "fake_impl", "hello", wait_visible=False)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "injected")

    def test_delivery_claim_prevents_duplicate_worker_injection(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-duplicate-delivery-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "session",
                "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            message_id = MessageStore(workspace).create_message(None, "gpt", "fake_impl", "hello")
            paste_calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                if args[:2] == ["tmux", "paste-buffer"]:
                    paste_calls.append(args)
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    return Mock(returncode=0, stdout=message_id, stderr="")
                return Mock(returncode=0, stdout="", stderr="")

            with (
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.time.sleep", return_value=None),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                first = runtime._deliver_pending_message(workspace, state, message_id)
                second = runtime._deliver_pending_message(workspace, state, message_id)

            self.assertTrue(first["ok"])
            self.assertEqual(first["status"], "visible")
            self.assertTrue(second["ok"])
            self.assertEqual(second["status"], "visible")
            self.assertEqual(second["reason"], "message_already_claimed")
            self.assertEqual(len(paste_calls), 1)
            attempts = [e for e in _events(workspace) if e["event"] == "send.deliver_attempt" and e["message_id"] == message_id]
            self.assertEqual(len(attempts), 1)

    def test_send_watch_result_registers_result_watcher(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watch-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl"}],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "still hidden"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                sent = runtime.send_message(workspace, "fake_impl", "hello", wait_visible=False, watch_result=True)
            self.assertTrue(sent["ok"])
            self.assertTrue(sent["watch_result"])
            self.assertEqual(sent["watch"]["task_id"], "task_impl")
            self.assertIn("notify the leader", sent["watch"]["notice"])
            watchers = MessageStore(workspace).result_watchers()
            self.assertEqual(len(watchers), 1)
            self.assertEqual(watchers[0]["status"], "pending")
            self.assertEqual(watchers[0]["task_id"], "task_impl")
            self.assertEqual(watchers[0]["agent_id"], "fake_impl")
            self.assertEqual(watchers[0]["message_id"], sent["message_id"])

    def test_send_watch_result_registers_after_submitted_unverified(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watch-submitted-unverified-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl"}],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "streaming output without token"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                sent = runtime.send_message(workspace, "fake_impl", "hello", timeout=0.01, watch_result=True)
            self.assertTrue(sent["ok"])
            self.assertEqual(sent["status"], "submitted_unverified")
            self.assertTrue(sent["submitted"])
            self.assertTrue(sent["watch_result"])
            watchers = MessageStore(workspace).result_watchers()
            self.assertEqual(len(watchers), 1)
            self.assertEqual(watchers[0]["message_id"], sent["message_id"])
            self.assertTrue(any(e["event"] == "send.submitted_unverified" for e in _events(workspace)))

    def test_ghostty_attach_args_split_tmux_command(self) -> None:
        display_session = runtime._ghostty_display_session_name("team-hello-world-team", "coder")
        args = runtime._ghostty_attach_args(display_session, "team-agent:coder:前沿")
        self.assertEqual(
            args,
            [
                "open",
                "-na",
                "Ghostty.app",
                "--args",
                "--title=team-agent:coder:前沿",
                "-e",
                "tmux",
                "attach-session",
                "-t",
                display_session,
            ],
        )
        self.assertIn("__display__coder__", display_session)
        self.assertNotIn(":", display_session)
        self.assertNotIn("sh", args)
        self.assertNotIn("-lc", args)
        self.assertNotIn("\\u", " ".join(args))

    def test_ghostty_display_session_is_linked_and_window_selected(self) -> None:
        calls: list[list[str]] = []

        def fake_run_cmd(args: list[str], timeout: int = 20):
            calls.append(args)
            return Mock(returncode=0, stdout="", stderr="")

        with (
            patch("team_agent.runtime._tmux_window_exists", return_value=True),
            patch("team_agent.runtime._tmux_session_exists", side_effect=[True]),
            patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
        ):
            result = runtime._prepare_ghostty_display_session("team-chat0", "alice", "team-chat0__display__alice__12345678")

        self.assertTrue(result["ok"])
        self.assertEqual(
            calls,
            [
                ["tmux", "kill-session", "-t", "team-chat0__display__alice__12345678"],
                ["tmux", "new-session", "-d", "-t", "team-chat0", "-s", "team-chat0__display__alice__12345678"],
                ["tmux", "select-window", "-t", "team-chat0__display__alice__12345678:alice"],
            ],
        )

    def test_status_peek_inbox_and_agent_health(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-status-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {
                        "fake_impl": {
                            "status": "running",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "session-123",
                            "captured_via": "fs_watch",
                            "attribution_confidence": "high",
                        }
                    },
                    "tasks": [{**spec["tasks"][0], "assignee": "fake_impl"}],
                },
            )
            MessageStore(workspace).create_message("task_impl", "leader", "fake_impl", "hello")

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "TEAM_AGENT_FAKE_READY agent=fake_impl"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                text = runtime.format_status(workspace)
                detail = runtime.format_status(workspace, "fake_impl")
                peeked = runtime.peek(workspace, "fake_impl", tail=20)
                searched = runtime.peek(workspace, "fake_impl", search="fake_ready", context=0)
            self.assertIn("fake_impl  IDLE  task_impl", text)
            self.assertIn("results total 0 uncollected 0 collected 0 invalid 0", text)
            self.assertIn("sid session-123", text)
            self.assertIn("via fs_watch high", text)
            self.assertIn("recent messages", detail)
            self.assertIn("session_id: session-123", detail)
            self.assertIn("TEAM_AGENT_FAKE_READY", peeked["text"])
            self.assertIn("TEAM_AGENT_FAKE_READY", searched["text"])
            with self.assertRaises(TeamAgentRuntimeError):
                runtime.peek(workspace, "fake_impl")
            inbox = runtime.format_inbox(workspace, "fake_impl")
            self.assertIn("leader -> fake_impl", inbox)
            self.assertIn("final results are not in inbox", inbox)

    def test_approvals_returns_structured_prompt_without_terminal_page(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-approvals-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "\n".join(
                        [
                            "Allow the team_orchestrator MCP server to run tool \"report_result\"?",
                            "envelope: {\"schema_version\":\"result_envelope_v1\",\"summary\":\"large private payload\"}",
                            "› 1. Allow        Run the tool and continue.",
                            "  2. Allow for this session  Run the tool and remember this choice for this session.",
                            "  3. Always allow  Run the tool and remember this choice for future tool calls.",
                            "  4. Cancel       Cancel this tool call.",
                            "enter to submit | esc to cancel",
                        ]
                    )
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.approvals(workspace)
                text = runtime.format_approvals(workspace)
            self.assertTrue(result["waiting"])
            item = result["approvals"][0]
            self.assertEqual(item["agent_id"], "fake_impl")
            self.assertEqual(item["kind"], "mcp_tool")
            self.assertEqual(item["tool"], "report_result")
            self.assertIn("Allow", item["choices"])
            self.assertNotIn("large private payload", json.dumps(result, ensure_ascii=False))
            self.assertIn("report_result", text)
            self.assertNotIn("large private payload", text)

    def test_coordinator_auto_approves_internal_mcp_prompt_with_retry_verification(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-auto-approval-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def prompt_text() -> str:
                return "\n".join(
                    [
                        "Allow the team_orchestrator MCP server to run tool \"report_result\"?",
                        "› 1. Allow        Run the tool and continue.",
                        "  2. Allow for this session  Run the tool and remember this choice for this session.",
                        "  3. Always allow  Run the tool and remember this choice for future tool calls.",
                        "  4. Cancel       Cancel this tool call.",
                        "enter to submit | esc to cancel",
                    ]
                )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    if f"-{runtime.APPROVAL_SCAN_LINES}" in args:
                        proc.stdout = prompt_text() if len(send_calls) < 2 else "codex>"
                    else:
                        proc.stdout = "codex>" if len(send_calls) >= 2 else "›"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual([call[-2:] for call in send_calls], [["Down", "Enter"], ["Down", "Enter"]])
            events = _events(workspace)
            handled = [e for e in events if e["event"] == "runtime.internal_mcp_approval.auto"]
            self.assertEqual(len(handled), 1)
            self.assertTrue(handled[0]["ok"])
            self.assertEqual(handled[0]["tool"], "report_result")
            self.assertEqual(handled[0]["choice"], "Allow for this session")
            self.assertEqual(handled[0]["verification"], "prompt_absent_after_submit")
            self.assertEqual(len(handled[0]["attempts"]), 2)

    def test_coordinator_auto_approves_claude_internal_mcp_prompt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-auto-approval-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "claude_code"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "claude_code", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []

            def prompt_text() -> str:
                return "\n".join(
                    [
                        "Tool use",
                        'team_orchestrator - send_message(to: "gpt", content: "private payload omitted") (MCP)',
                        "Send a message to a teammate, the leader, or '*' for all other team members.",
                        "",
                        "Do you want to proceed?",
                        "❯ 1. Yes",
                        "  2. Yes, and don't ask again for team_orchestrator - send_message commands in /Users/alauda/Documents/code/agent前沿探索/11",
                        "  3. No",
                        "",
                        "Esc to cancel · Tab to amend",
                    ]
                )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = prompt_text() if not send_calls else "Claude Code\n>"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                approvals = runtime.approvals(workspace)
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(approvals["waiting"])
            self.assertEqual(approvals["approvals"][0]["tool"], "send_message")
            self.assertNotIn("private payload", json.dumps(approvals, ensure_ascii=False))
            self.assertTrue(result["ok"])
            self.assertEqual([call[-2:] for call in send_calls], [["Down", "Enter"]])
            events = _events(workspace)
            handled = [e for e in events if e["event"] == "runtime.internal_mcp_approval.auto"]
            self.assertEqual(len(handled), 1)
            self.assertTrue(handled[0]["ok"])
            self.assertEqual(handled[0]["tool"], "send_message")
            self.assertIn("don't ask again", handled[0]["choice"])
            self.assertEqual(handled[0]["verification"], "prompt_absent_after_submit")

    def test_coordinator_does_not_auto_approve_non_allowlisted_mcp_prompt(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-auto-approval-skip-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []
            prompt = "\n".join(
                [
                    "Allow the team_orchestrator MCP server to run tool \"assign_task\"?",
                    "› 1. Allow        Run the tool and continue.",
                    "  2. Allow for this session  Run the tool and remember this choice for this session.",
                    "  3. Always allow  Run the tool and remember this choice for future tool calls.",
                    "  4. Cancel       Cancel this tool call.",
                    "enter to submit | esc to cancel",
                ]
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = prompt
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual(send_calls, [])
            self.assertEqual(MessageStore(workspace).agent_health()["fake_impl"]["status"], "AWAITING_APPROVAL")
            events = _events(workspace)
            skipped = [e for e in events if e["event"] == "runtime.internal_mcp_approval.skipped"]
            self.assertEqual(len(skipped), 1)
            self.assertEqual(skipped[0]["tool"], "assign_task")
            self.assertEqual(skipped[0]["reason"], "tool_not_allowlisted")

    def test_stale_approval_prompt_in_scrollback_is_not_current_approval(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-stale-approval-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "codex", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            send_calls: list[list[str]] = []
            stale_prompt = "\n".join(
                [
                    "Allow the team_orchestrator MCP server to run tool \"report_result\"?",
                    "› 1. Allow        Run the tool and continue.",
                    "  2. Allow for this session  Run the tool and remember this choice for this session.",
                    "enter to submit | esc to cancel",
                    "• Called",
                    "  └ team_orchestrator.report_result({\"summary\":\"done\"})",
                    "    {\"ok\": true}",
                    "› Implement {feature}",
                ]
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "send-keys", "-t"]:
                    send_calls.append(args)
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = stale_prompt
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                approvals = runtime.approvals(workspace)
                result = runtime.coordinator_tick(workspace)
            self.assertFalse(approvals["waiting"])
            self.assertTrue(result["ok"])
            self.assertEqual(send_calls, [])
            self.assertNotEqual(MessageStore(workspace).agent_health()["fake_impl"]["status"], "AWAITING_APPROVAL")

    def test_coordinator_tick_updates_health_and_scheduled_events(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["tick_interval_sec"] = 1
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            store = MessageStore(workspace)
            store.add_scheduled_event("2000-01-01T00:00:00+00:00", "leader", "health_ping", {"note": "test"})

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "has-session", "-t"]:
                    return proc
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n"
                    return proc
                if args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "TEAM_AGENT_FAKE_READY agent=fake_impl"
                    return proc
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                result = runtime.coordinator_tick(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual(store.agent_health()["fake_impl"]["status"], "IDLE")
            self.assertEqual(result["scheduled"], [1])

    def test_coordinator_process_start_stop_restart_and_self_kill(self) -> None:
        import warnings

        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-proc-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {},
                    "tasks": spec["tasks"],
                },
            )
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", ResourceWarning)
                first = runtime.start_coordinator(workspace)
            self.assertTrue(first["ok"])
            self.assertTrue(runtime.coordinator_health(workspace)["ok"])
            stopped = runtime.stop_coordinator(workspace)
            self.assertTrue(stopped["ok"])
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", ResourceWarning)
                second = runtime.start_coordinator(workspace)
            self.assertTrue(second["ok"])
            self.assertNotEqual(first["pid"], second["pid"])
            runtime.stop_coordinator(workspace)

        with tempfile.TemporaryDirectory(prefix="team-agent-coordinator-selfkill-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "definitely-missing-session",
                    "agents": {},
                    "tasks": spec["tasks"],
                },
            )
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", ResourceWarning)
                started = runtime.start_coordinator(workspace)
            self.assertTrue(started["ok"])
            import time

            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and runtime.coordinator_health(workspace)["ok"]:
                time.sleep(0.1)
            self.assertFalse(runtime.coordinator_health(workspace)["ok"])

    def test_peer_talk_default_allows_team_scoped_target(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-peer-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            second = copy.deepcopy(spec["agents"][0])
            second["id"] = "fake_peer"
            spec["agents"].append(second)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_peer": {"status": "running", "provider": "fake", "window": "fake_peer"},
                    },
                    "tasks": spec["tasks"],
                },
            )

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\nfake_peer\n"
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "msg_"
                return proc

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd):
                allowed = runtime.send_message(workspace, "fake_peer", "peer hello", sender="fake_impl", wait_visible=False)
            self.assertTrue(allowed["ok"])
            self.assertEqual(allowed["status"], "injected")
            self.assertFalse([e for e in _events(workspace) if e["event"] == "send.peer_rejected"])

    def test_broadcast_sends_only_to_current_team_and_excludes_sender(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-broadcast-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            qa = copy.deepcopy(spec["agents"][0])
            qa["id"] = "fake_qa"
            spec["agents"].extend([peer, qa])
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "session",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                        "fake_peer": {"status": "running", "provider": "fake", "window": "fake_peer"},
                        "fake_qa": {"status": "running", "provider": "fake", "window": "fake_qa"},
                        "stale_outside_team": {"status": "running", "provider": "fake", "window": "stale_outside_team"},
                    },
                    "tasks": spec["tasks"],
                },
            )
            worker_targets: list[str] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\nfake_peer\nfake_qa\nstale_outside_team\n"
                elif args[:3] == ["tmux", "paste-buffer", "-t"]:
                    worker_targets.append(args[3])
                    self.assertNotIn("fake_impl", args[3])
                    self.assertNotIn("stale_outside_team", args[3])
                elif args[:3] == ["tmux", "capture-pane", "-p"]:
                    proc.stdout = "prompt"
                return proc

            def fake_leader(*args, **kwargs):
                return {"ok": True, "message_id": "msg_leader", "status": "submitted", "to": "leader", "channel": "direct_tmux"}

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_leader),
            ):
                result = runtime.send_message(workspace, "*", "broadcast hello", sender="fake_impl", wait_visible=False)

            self.assertTrue(result["ok"])
            self.assertEqual(result["status"], "broadcast_delivered")
            self.assertEqual(result["targets"], ["leader", "fake_peer", "fake_qa"])
            self.assertEqual(result["delivered_count"], 3)
            self.assertEqual(result["failed_count"], 0)
            self.assertEqual(worker_targets, ["session:fake_peer", "session:fake_qa"])
            events = _events(workspace)
            complete = next(e for e in events if e["event"] == "send.broadcast_complete")
            self.assertEqual(complete["targets"], ["leader", "fake_peer", "fake_qa"])
            rejected = runtime.send_message(workspace, "stale_outside_team", "nope", sender="fake_impl", wait_visible=False)
            self.assertFalse(rejected["ok"])
            self.assertEqual(rejected["reason"], "target_not_in_team")

    def test_quick_start_accepts_loose_role_doc_directory(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-start-") as tmp:
            workspace = Path(tmp)
            roles = workspace / "roles"
            roles.mkdir()
            (roles / "TEAM.md").write_text(
                """---
name: quick-test
objective: Team config only.
display_backend: none
fast: false
---

This file is not an agent role.
""",
                encoding="utf-8",
            )
            (roles / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake-model
auth_mode: subscription
profile: fake-default
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(roles, name="quick-test")
                finally:
                    os.chdir(cwd)
            try:
                self.assertTrue(result["ok"])
                self.assertIn("team quick-test ready", result["summary"])
                self.assertIn("Do not wait, sleep, or poll status", result["ready_signal"])
                self.assertIn("Dispatch work with team-agent send", result["next_actions"][0])
                self.assertTrue((workspace / ".team" / "current" / "TEAM.md").exists())
                self.assertFalse((workspace / ".team" / "current" / "agents" / "TEAM.md").exists())
                self.assertTrue((workspace / ".team" / "current" / "profiles" / "fake-default.example.env").exists())
                self.assertTrue((workspace / ".team" / "current" / "team.spec.yaml").exists())
                self.assertTrue((workspace / ".team" / "current" / "team_state.md").exists())
                self.assertFalse((workspace / "team.spec.yaml").exists())
                self.assertFalse((workspace / "team_state.md").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(state["spec_path"], str((workspace / ".team" / "current" / "team.spec.yaml").resolve()))
                self.assertIn("fake_impl", state["agents"])
                self.assertNotIn("leader", state["agents"])
            finally:
                runtime.shutdown(workspace)

    def test_quick_start_accepts_team_root_agents_layout_without_leader_worker(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-skill-example-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "current"
            (team / "agents").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: skill-example
objective: Minimal example team.
dangerous_auto_approve: false
display_backend: none
fast: false
---

Team config only.
""",
                encoding="utf-8",
            )
            (team / "agents" / "coder.md").write_text(
                """---
name: coder
role: Coder
provider: fake
model: fake
auth_mode: subscription
profile: fake-default
tools:
  - mcp_team
---

Coder role.
""",
                encoding="utf-8",
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(team)
                finally:
                    os.chdir(cwd)
            try:
                self.assertTrue(result["ok"])
                self.assertEqual(result["spec"], str((workspace / ".team" / "current" / "team.spec.yaml").resolve()))
                self.assertTrue((workspace / ".team" / "current" / "team.spec.yaml").exists())
                self.assertTrue((workspace / ".team" / "current" / "team_state.md").exists())
                self.assertFalse((workspace / "team.spec.yaml").exists())
                self.assertFalse((workspace / "team_state.md").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(sorted(state["agents"]), ["coder"])
            finally:
                runtime.shutdown(workspace)

    def test_quick_start_refuses_to_overwrite_existing_context_without_fresh(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-start-existing-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(team / "team.spec.yaml"),
                    "workspace": str(workspace),
                    "session_name": "team-doc-team",
                    "leader": {"provider": "codex"},
                    "agents": {
                        "implementer": {
                            "status": "stopped",
                            "provider": "codex",
                            "agent_id": "implementer",
                            "window": "implementer",
                            "session_id": "old-session",
                        }
                    },
                    "tasks": [],
                },
            )
            cwd = os.getcwd()
            os.chdir(workspace)
            try:
                result = runtime.quick_start(team)
            finally:
                os.chdir(cwd)

            self.assertFalse(result["ok"])
            self.assertEqual(result["step"], "existing_runtime_state")
            self.assertEqual(result["session_name"], "team-doc-team")
            self.assertTrue(any("team-agent restart" in action for action in result["next_actions"]))
            self.assertTrue(any("--fresh" in action for action in result["next_actions"]))

    def test_quick_start_team_id_stores_loose_docs_outside_current(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-team-id-") as tmp:
            workspace = Path(tmp)
            roles = workspace / "roles"
            roles.mkdir()
            (roles / "TEAM.md").write_text(
                """---
name: alpha-team
objective: Team config only.
display_backend: none
fast: false
---

Team config.
""",
                encoding="utf-8",
            )
            (roles / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake-model
auth_mode: subscription
profile: fake-default
tools:
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                cwd = os.getcwd()
                os.chdir(workspace)
                try:
                    result = runtime.quick_start(roles, team_id="alpha")
                finally:
                    os.chdir(cwd)
            try:
                self.assertTrue(result["ok"], result)
                self.assertEqual(result["team_dir"], str((workspace / ".team" / "alpha").resolve()))
                self.assertTrue((workspace / ".team" / "alpha" / "TEAM.md").exists())
                self.assertFalse((workspace / ".team" / "current" / "TEAM.md").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(state["spec_path"], str((workspace / ".team" / "alpha" / "team.spec.yaml").resolve()))
            finally:
                runtime.shutdown(workspace)

    def test_start_writes_compiled_spec_inside_selected_team_dir(self) -> None:
        if not shutil.which("tmux"):
            self.skipTest("tmux not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-start-team-dir-") as tmp:
            workspace = Path(tmp)
            team = workspace / ".team" / "alpha"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: alpha-team
objective: Team config only.
display_backend: none
fast: false
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "fake-default.example.env").write_text(
                "AUTH_MODE=subscription\nPROFILE_NAME=fake-default\n",
                encoding="utf-8",
            )
            (team / "agents" / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
model: fake-model
auth_mode: subscription
profile: fake-default
tools:
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )
            try:
                result = runtime.start(team, yes=True)
                self.assertTrue(result["ok"], result)
                self.assertEqual(result["spec"], str((team / "team.spec.yaml").resolve()))
                self.assertTrue((team / "team.spec.yaml").exists())
                self.assertTrue((team / "team_state.md").exists())
                self.assertFalse((workspace / "team.spec.yaml").exists())
                state = load_runtime_state(workspace)
                self.assertEqual(state["spec_path"], str((team / "team.spec.yaml").resolve()))
            finally:
                runtime.shutdown(workspace)

    def test_preflight_uses_selected_team_profile_dir_not_current(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-profile-team-dir-") as tmp:
            workspace = Path(tmp)
            current_profiles = workspace / ".team" / "current" / "profiles"
            current_profiles.mkdir(parents=True)
            (current_profiles / "shared.env").write_text(
                "AUTH_MODE=compatible_api\nPROFILE_NAME=shared\nPROFILE_SMOKE=false\n",
                encoding="utf-8",
            )
            team = workspace / ".team" / "alpha"
            (team / "agents").mkdir(parents=True)
            (team / "profiles").mkdir(parents=True)
            (team / "TEAM.md").write_text(
                """---
name: alpha-team
objective: Team config only.
display_backend: none
fast: false
---

Team config.
""",
                encoding="utf-8",
            )
            (team / "profiles" / "shared.env").write_text(
                "AUTH_MODE=compatible_api\nPROFILE_NAME=shared\nMODEL=alpha-model\nPROFILE_SMOKE=false\n",
                encoding="utf-8",
            )
            (team / "agents" / "fake_impl.md").write_text(
                """---
name: fake_impl
role: Fake Implementer
provider: fake
auth_mode: compatible_api
profile: shared
tools:
  - mcp_team
---

Handle fake tasks.
""",
                encoding="utf-8",
            )

            result = runtime.preflight(team)

            profiles = next(check for check in result["checks"] if check["name"] == "profiles")
            self.assertTrue(profiles["ok"], profiles)
            models = next(check for check in result["checks"] if check["name"] == "models")
            model = next(item for item in models["checks"] if item["agent_id"] == "fake_impl")
            self.assertEqual(model["model"], "alpha-model")

    def test_quick_start_requires_yes_only_for_dangerous_auto_approve(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-quick-start-danger-") as tmp:
            workspace = Path(tmp)
            team = _write_doc_team(workspace)
            team_doc = team / "TEAM.md"
            team_doc.write_text(
                team_doc.read_text(encoding="utf-8").replace("provider: codex\n", "provider: codex\ndangerous_auto_approve: true\n"),
                encoding="utf-8",
            )
            cwd = os.getcwd()
            os.chdir(workspace)
            try:
                with self.assertRaises(runtime.RuntimeError) as ctx:
                    runtime.quick_start(team)
            finally:
                os.chdir(cwd)
            self.assertIn("requires --yes", str(ctx.exception))

    def test_runtime_state_session_fields_are_backward_compatible(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-old-state-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-old",
                "agents": {
                    "fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"},
                    "legacy_bad": "untouched",
                },
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            loaded = load_runtime_state(workspace)
            agent = loaded["agents"]["fake_impl"]
            for key in ["session_id", "rollout_path", "captured_at", "captured_via", "attribution_confidence", "spawn_cwd"]:
                self.assertIn(key, agent)
                self.assertIsNone(agent[key])
            self.assertEqual(loaded["agents"]["legacy_bad"], "untouched")

    def test_runtime_state_missing_file_default_is_literal(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-missing-state-") as tmp:
            self.assertEqual(load_runtime_state(Path(tmp)), {"agents": {}, "tasks": [], "session_name": None})

    def test_snapshot_state_session_fields_are_backward_compatible(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-old-snapshot-") as tmp:
            state_path = Path(tmp) / "state.json"
            state_path.write_text(
                json.dumps(
                    {
                        "session_name": "team-old",
                        "agents": {
                            "fake_impl": {"status": "stopped", "provider": "fake", "window": "fake_impl"},
                            "legacy_bad": None,
                        },
                        "tasks": [],
                    }
                ),
                encoding="utf-8",
            )
            loaded = runtime._load_snapshot_state(state_path)
            self.assertIsNotNone(loaded)
            agent = loaded["agents"]["fake_impl"]
            for key in ["session_id", "rollout_path", "captured_at", "captured_via", "attribution_confidence", "spawn_cwd"]:
                self.assertIn(key, agent)
                self.assertIsNone(agent[key])
            self.assertIsNone(loaded["agents"]["legacy_bad"])

    def test_snapshot_state_malformed_json_returns_none(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-bad-snapshot-") as tmp:
            state_path = Path(tmp) / "state.json"
            state_path.write_text("{bad json", encoding="utf-8")
            self.assertIsNone(runtime._load_snapshot_state(state_path))

    def test_session_state_normalization_ignores_non_dict_agents_container(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-odd-agents-") as tmp:
            workspace = Path(tmp)
            state_path = runtime.runtime_state_path(workspace)
            state_path.parent.mkdir(parents=True)
            state_path.write_text(json.dumps({"session_name": "team-old", "agents": "legacy-bad", "tasks": []}), encoding="utf-8")

            loaded = load_runtime_state(workspace)
            self.assertEqual(loaded["agents"], "legacy-bad")

            snapshot = runtime._load_snapshot_state(state_path)
            self.assertIsNotNone(snapshot)
            self.assertEqual(snapshot["agents"], "legacy-bad")

    def test_copy_session_metadata_copies_known_fields_only(self) -> None:
        full_source = {
            "session_id": "session-123",
            "rollout_path": "rollout.jsonl",
            "captured_at": "2026-05-16T00:00:00+00:00",
            "captured_via": "fs_watch",
            "attribution_confidence": "high",
            "spawn_cwd": "/tmp/workspace",
            "extra": "ignored",
        }
        target = {"preexisting": "kept"}

        runtime._copy_session_metadata(target, full_source)

        for key in runtime.SESSION_STATE_FIELDS:
            self.assertEqual(target[key], full_source[key])
        self.assertEqual(target["preexisting"], "kept")
        self.assertNotIn("extra", target)

        missing_source = {"session_id": "session-456"}
        runtime._copy_session_metadata(target, missing_source)

        self.assertEqual(target["session_id"], "session-456")
        for key in runtime.SESSION_STATE_FIELDS:
            if key != "session_id":
                self.assertIsNone(target[key])

    def test_clear_session_capture_fields_preserves_spawn_cwd(self) -> None:
        target = {
            "session_id": "session-123",
            "rollout_path": "rollout.jsonl",
            "captured_at": "2026-05-16T00:00:00+00:00",
            "captured_via": "fs_watch",
            "attribution_confidence": "high",
            "spawn_cwd": "/tmp/workspace",
        }

        runtime._clear_session_capture_fields(target)

        for key in runtime.SESSION_CAPTURE_FIELDS:
            self.assertIsNone(target[key])
        self.assertEqual(target["spawn_cwd"], "/tmp/workspace")

    def test_claude_adapter_predetermines_session_id_and_resume_command(self) -> None:
        adapter = get_adapter("claude_code")
        agent = _provider_agent("claude_code", "claude_reviewer")
        agent["_runtime"] = {}
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-session-") as tmp:
            workspace = Path(tmp) / "worker"
            workspace.mkdir()
            projects_root = Path(tmp) / "projects"
            cmd = adapter.build_command(agent, workspace, {})
            self.assertIn("--session-id", cmd)
            session_id = cmd[cmd.index("--session-id") + 1]
            missing = adapter.capture_session_id(
                "claude_reviewer",
                {
                    "cwd": str(workspace),
                    "predetermined_session_id": session_id,
                    "claude_projects_root": str(projects_root),
                },
                timeout_s=0,
            )
            self.assertIsNone(missing)
            _write_claude_transcript(projects_root, workspace, session_id, "Read .team/current/agents/claude_reviewer.md")
            captured = adapter.capture_session_id(
                "claude_reviewer",
                {
                    "cwd": str(workspace),
                    "predetermined_session_id": session_id,
                    "claude_projects_root": str(projects_root),
                },
                timeout_s=0,
            )
            self.assertEqual(captured["session_id"], session_id)
            self.assertEqual(captured["captured_via"], "fs_watch")
            self.assertTrue(
                adapter.session_is_resumable(
                    {"session_id": session_id, "spawn_cwd": str(workspace), "claude_projects_root": str(projects_root)},
                    workspace,
                )
            )
            resume = adapter.build_resume_command(
                {
                    "session_id": session_id,
                    "spawn_cwd": str(workspace),
                    "claude_projects_root": str(projects_root),
                    "_agent_spec": agent,
                },
                workspace,
                {},
            )
            self.assertIn("--resume", resume)
            self.assertEqual(resume[resume.index("--resume") + 1], session_id)
            self.assertEqual(get_adapter("claude").command_name, "claude")

    def test_prepare_resume_state_repairs_claude_session_from_verified_event_history(self) -> None:
        adapter = get_adapter("claude_code")
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-repair-") as tmp:
            workspace = Path(tmp) / "workspace"
            workspace.mkdir()
            projects_root = Path(tmp) / "projects"
            stale_session = "00000000-0000-4000-8000-000000000001"
            good_session = "00000000-0000-4000-8000-000000000002"
            _write_claude_transcript(
                projects_root,
                workspace,
                good_session,
                "Team Agent message from leader:\nRead .team/current/agents/analyst.md and write docs/requirements.md",
            )
            event_log = runtime.EventLog(workspace)
            event_log.write(
                "session.captured",
                agent_id="analyst",
                provider="claude",
                session_id=good_session,
                rollout_path=str(_claude_transcript_path(projects_root, workspace, good_session)),
                captured_via="fs_watch",
                attribution_confidence="high",
            )
            event_log.write(
                "session.captured",
                agent_id="analyst",
                provider="claude",
                session_id=stale_session,
                rollout_path=None,
                captured_via="predetermined",
                attribution_confidence="high",
            )
            previous = {
                "status": "stopped",
                "provider": "claude",
                "session_id": stale_session,
                "spawn_cwd": str(workspace),
                "claude_projects_root": str(projects_root),
            }

            repaired = runtime._prepare_resume_state(workspace, "analyst", previous, adapter, event_log)

            self.assertEqual(repaired["session_id"], good_session)
            self.assertEqual(repaired["captured_via"], "event_log_repair")
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "resume.session_unverified" for e in events))
            self.assertTrue(any(e["event"] == "resume.session_repaired" for e in events))

    def test_prepare_resume_state_copies_adapter_repaired_session_metadata(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-adapter-repair-") as tmp:
            workspace = Path(tmp)
            event_log = runtime.EventLog(workspace)
            adapter = Mock()
            adapter.session_is_resumable.return_value = False
            adapter.recover_session_id.return_value = {
                "session_id": "repaired-session",
                "rollout_path": "repaired.jsonl",
                "captured_at": "2026-05-16T00:00:00+00:00",
                "captured_via": "fs_repair",
                "attribution_confidence": "high",
                "spawn_cwd": str(workspace / "repaired-cwd"),
            }
            previous = {
                "status": "stopped",
                "provider": "claude_code",
                "spawn_cwd": str(workspace / "old-cwd"),
            }

            repaired = runtime._prepare_resume_state(workspace, "analyst", previous, adapter, event_log)

            self.assertEqual(repaired["session_id"], "repaired-session")
            self.assertEqual(repaired["rollout_path"], "repaired.jsonl")
            self.assertEqual(repaired["captured_at"], "2026-05-16T00:00:00+00:00")
            self.assertEqual(repaired["captured_via"], "fs_repair")
            self.assertEqual(repaired["attribution_confidence"], "high")
            self.assertEqual(repaired["spawn_cwd"], str(workspace / "repaired-cwd"))

    def test_prepare_resume_state_allow_fresh_clears_capture_fields_and_preserves_spawn_cwd(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-resume-allow-fresh-") as tmp:
            workspace = Path(tmp)
            adapter = Mock()
            adapter.session_is_resumable.return_value = False
            adapter.recover_session_id.return_value = None
            previous = {
                "status": "stopped",
                "provider": "claude_code",
                "session_id": "stale-session",
                "rollout_path": "stale.jsonl",
                "captured_at": "2026-05-16T00:00:00+00:00",
                "captured_via": "fs_watch",
                "attribution_confidence": "high",
                "spawn_cwd": str(workspace / "old-cwd"),
            }

            prepared = runtime._prepare_resume_state(
                workspace,
                "analyst",
                previous,
                adapter,
                runtime.EventLog(workspace),
                allow_fresh_on_resume_failure=True,
            )

            for key in runtime.SESSION_CAPTURE_FIELDS:
                self.assertIsNone(prepared[key])
            self.assertEqual(prepared["spawn_cwd"], str(workspace / "old-cwd"))

    def test_prepare_resume_state_repairs_claude_session_from_pending_isolated_id(self) -> None:
        adapter = get_adapter("claude_code")
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-pending-repair-") as tmp:
            workspace = Path(tmp) / "workspace"
            workspace.mkdir()
            projects_root = Path(tmp) / "isolated" / "projects"
            pending_session = "00000000-0000-4000-8000-000000000123"
            stale_session = "00000000-0000-4000-8000-000000000999"
            _write_claude_transcript(
                projects_root,
                workspace,
                pending_session,
                "Team Agent worker coder with role Implementation Worker",
            )
            previous = {
                "status": "stopped",
                "provider": "claude_code",
                "session_id": stale_session,
                "_pending_session_id": pending_session,
                "spawn_cwd": str(workspace),
                "claude_projects_root": str(projects_root),
            }

            repaired = runtime._prepare_resume_state(workspace, "coder", previous, adapter, runtime.EventLog(workspace))

            self.assertEqual(repaired["session_id"], pending_session)
            self.assertEqual(repaired["captured_via"], "fs_repair")

    def test_prepare_resume_state_fails_closed_for_unverified_existing_session(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-resume-fail-closed-") as tmp:
            workspace = Path(tmp)
            adapter = Mock()
            adapter.session_is_resumable.return_value = False
            adapter.recover_session_id.return_value = None
            previous = {
                "status": "stopped",
                "provider": "claude_code",
                "session_id": "stale-session",
                "spawn_cwd": str(workspace),
            }

            with self.assertRaises(ResumeUnavailable):
                runtime._prepare_resume_state(workspace, "analyst", previous, adapter, runtime.EventLog(workspace))

            events = _events(workspace)
            self.assertTrue(any(e["event"] == "resume.session_required_missing" for e in events))

    def test_capture_agent_session_passes_claude_projects_root(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-claude-capture-root-") as tmp:
            workspace = Path(tmp)
            projects_root = workspace / ".team" / "runtime" / "provider-config" / "analyst" / "claude" / "projects"
            seen: dict[str, Any] = {}

            adapter = Mock()
            adapter.capture_session_id.side_effect = lambda agent_id, spawn_context, timeout_s=3.0: seen.update(spawn_context) or {
                "session_id": "session-123",
                "rollout_path": "transcript.jsonl",
                "captured_at": "2026-05-16T00:00:00+00:00",
                "captured_via": "fs_watch",
                "attribution_confidence": "high",
                "spawn_cwd": str(workspace),
            }
            agent_state = {
                "provider": "claude_code",
                "session_name": "team-root",
                "window": "analyst",
                "spawn_cwd": str(workspace),
                "spawned_at": "2026-05-16T00:00:00+00:00",
                "claude_projects_root": str(projects_root),
                "_pending_session_id": "pending-session",
            }
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                result = runtime._capture_agent_session(workspace, "analyst", agent_state, runtime.EventLog(workspace), timeout_s=0)

            self.assertEqual(result["session_id"], "session-123")
            self.assertEqual(seen["claude_projects_root"], str(projects_root))
            for key in runtime.SESSION_STATE_FIELDS:
                self.assertEqual(agent_state[key], result[key])
            self.assertNotIn("_pending_session_id", agent_state)
            event = _events(workspace)[-1]
            self.assertEqual(event["event"], "session.captured")
            self.assertEqual(event["session_id"], "session-123")

    def test_startup_verify_rejects_window_that_disappears_immediately(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-startup-stability-") as tmp:
            workspace = Path(tmp)
            calls = 0

            def fake_run_cmd(args: list[str], timeout: int = 20):
                nonlocal calls
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    calls += 1
                    if calls == 1:
                        proc.stdout = "coder\n"
                    else:
                        proc.returncode = 1
                        proc.stderr = "can't find session"
                return proc

            adapter = Mock()
            adapter.handle_startup_prompts.return_value = []
            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch("team_agent.runtime.time.sleep", return_value=None):
                ok = runtime._handle_startup_prompts_and_verify_window(
                    adapter,
                    runtime.EventLog(workspace),
                    "restart",
                    "coder",
                    "claude_code",
                    "team-quick-exit",
                    "resumed",
                )

            self.assertFalse(ok)
            event = _events(workspace)[-1]
            self.assertEqual(event["event"], "restart.window_missing_after_start")
            self.assertTrue(event["saw_window"])

    def test_codex_adapter_captures_rollout_by_cwd_and_ignores_other_workers(self) -> None:
        adapter = get_adapter("codex")
        with tempfile.TemporaryDirectory(prefix="team-agent-codex-rollout-") as tmp:
            root = Path(tmp) / "sessions"
            worker_a = Path(tmp) / "worker-a"
            worker_b = Path(tmp) / "worker-b"
            worker_a.mkdir()
            worker_b.mkdir()
            spawn_time = datetime(2026, 5, 16, 2, 45, tzinfo=timezone.utc)
            _write_codex_rollout(root, "019e2eac-0000-7000-a000-000000000001", worker_a, spawn_time)
            _write_codex_rollout(root, "019e2eac-0000-7000-a000-000000000002", worker_b, spawn_time)
            captured = adapter.capture_session_id(
                "worker-b",
                {"cwd": str(worker_b), "spawn_time": spawn_time.isoformat(), "sessions_root": str(root)},
                timeout_s=0,
            )
            self.assertEqual(captured["session_id"], "019e2eac-0000-7000-a000-000000000002")
            self.assertEqual(captured["captured_via"], "fs_watch")
            self.assertEqual(captured["attribution_confidence"], "high")
            missing = adapter.capture_session_id(
                "worker-missing",
                {"cwd": str(Path(tmp) / "missing"), "spawn_time": spawn_time.isoformat(), "sessions_root": str(root)},
                timeout_s=0,
            )
            self.assertIsNone(missing)
            _write_codex_rollout(root, "019e2eac-0000-7000-a000-000000000003", worker_a, spawn_time)
            second_same_cwd = adapter.capture_session_id(
                "worker-a-2",
                {
                    "cwd": str(worker_a),
                    "spawn_time": spawn_time.isoformat(),
                    "sessions_root": str(root),
                    "exclude_session_ids": ["019e2eac-0000-7000-a000-000000000001"],
                },
                timeout_s=0,
            )
            self.assertEqual(second_same_cwd["session_id"], "019e2eac-0000-7000-a000-000000000003")

    def test_codex_model_validation_rejects_display_name_instead_of_slug(self) -> None:
        adapter = get_adapter("codex")
        old_cache = getattr(adapter, "_model_catalog_cache", None)
        adapter._model_catalog_cache = None
        proc = Mock(
            returncode=0,
            stdout=json.dumps(
                {
                    "models": [
                        {"slug": "gpt-5.3-codex-spark", "display_name": "GPT-5.3-Codex-Spark"},
                        {"slug": "gpt-5.5", "display_name": "GPT-5.5"},
                    ]
                }
            ),
            stderr="",
        )
        try:
            with patch.object(adapter, "is_installed", return_value=True), patch("team_agent.providers.subprocess.run", return_value=proc):
                invalid = adapter.validate_model("GPT-5.3-Codex-Spark")
                valid = adapter.validate_model("gpt-5.3-codex-spark")
        finally:
            adapter._model_catalog_cache = old_cache
        self.assertFalse(invalid["ok"])
        self.assertEqual(invalid["reason"], "model_id_not_exact")
        self.assertEqual(invalid["suggested_model"], "gpt-5.3-codex-spark")
        self.assertTrue(valid["ok"])

    def test_codex_model_validation_fails_closed_when_catalog_unavailable(self) -> None:
        adapter = get_adapter("codex")
        old_cache = getattr(adapter, "_model_catalog_cache", None)
        adapter._model_catalog_cache = None
        proc = Mock(returncode=1, stdout="", stderr="catalog unavailable")
        try:
            with patch.object(adapter, "is_installed", return_value=True), patch("team_agent.providers.subprocess.run", return_value=proc):
                result = adapter.validate_model("gpt-5.5")
        finally:
            adapter._model_catalog_cache = old_cache
        self.assertFalse(result["ok"])
        self.assertEqual(result["status"], "model_catalog_unavailable")
        self.assertEqual(result["reason"], "model_catalog_command_failed")

    def test_codex_startup_prompt_ignores_stale_trust_text_after_ready_prompt(self) -> None:
        adapter = get_adapter("codex")
        capture = Mock(
            returncode=0,
            stdout=(
                "Do you trust the contents of this directory?\n"
                "Press enter to continue\n"
                "OpenAI Codex\n"
                "› "
            ),
            stderr="",
        )

        def fake_run(args: list[str], **_: Any) -> Mock:
            if args[:3] == ["tmux", "capture-pane", "-p"]:
                return capture
            raise AssertionError(f"unexpected command: {args}")

        with patch("team_agent.providers.subprocess.run", side_effect=fake_run):
            handled = adapter.handle_startup_prompts("team-stale", "worker", checks=1, sleep_s=0.0)

        self.assertEqual(handled, [])

    def test_runtime_startup_prompt_checks_are_bounded_per_spawn(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-startup-check-limit-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-startup-check",
                "agents": {
                    "coder": {
                        "status": "running",
                        "provider": "codex",
                        "window": "coder",
                        "spawned_at": "2026-05-16T00:00:00+00:00",
                    }
                },
            }
            adapter = Mock()
            adapter.handle_startup_prompts.return_value = [{"prompt": "codex_workspace_trust", "action": "sent_enter"}]
            with (
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
            ):
                for _ in range(5):
                    runtime._handle_provider_startup_prompts(workspace, state, runtime.EventLog(workspace))

            self.assertEqual(adapter.handle_startup_prompts.call_count, runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT)
            self.assertEqual(
                state["agents"]["coder"]["startup_prompt_check_count"],
                runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT,
            )
            events = [event for event in _events(workspace) if event["event"] == "runtime.startup_prompt_handled"]
            self.assertEqual(len(events), runtime.STARTUP_PROMPT_RUNTIME_CHECK_LIMIT)

    def test_launch_blocks_invalid_model_before_tmux_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-invalid-model-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "codex"
            spec["agents"][0]["model"] = "GPT-5.3-Codex-Spark"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.command_name = "codex"
            adapter.is_installed.return_value = True
            adapter.validate_model.return_value = {
                "ok": False,
                "provider": "codex",
                "model": "GPT-5.3-Codex-Spark",
                "reason": "model_id_not_exact",
                "suggested_model": "gpt-5.3-codex-spark",
            }
            with (
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd") as run_cmd_mock,
            ):
                with self.assertRaises(TeamAgentRuntimeError) as ctx:
                    runtime.launch(spec_path, auto_approve=True)
            self.assertIn("use 'gpt-5.3-codex-spark'", str(ctx.exception))
            run_cmd_mock.assert_not_called()
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "launch.model_check" and not e["ok"] for e in events))

    def test_restart_resumes_known_sessions_and_fresh_spawns_missing_sessions(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-unit-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            state = {
                "spec_path": str(spec_path),
                "workspace": str(workspace),
                "session_name": "team-restart-unit",
                "leader": spec["leader"],
                "agents": {
                    "fake_impl": {
                        "status": "stopped",
                        "provider": "fake",
                        "window": "fake_impl",
                        "session_id": "fake-session-1",
                        "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / "fake_impl.json"),
                    }
                },
                "tasks": spec["tasks"],
                "display_backend": "none",
            }
            save_runtime_state(workspace, state)

            started_windows: set[str] = set()
            fake_run_cmd = _make_fake_tmux_window_run_cmd(started_windows)

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
                "team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 321, "status": "started"}
            ):
                resumed = runtime.restart(workspace)
            self.assertEqual(resumed["agents"][0]["restart_mode"], "resumed")
            self.assertEqual(load_runtime_state(workspace)["agents"]["fake_impl"]["session_id"], "fake-session-1")

            state = load_runtime_state(workspace)
            state["agents"]["fake_impl"]["session_id"] = None
            save_runtime_state(workspace, state)
            started_windows.clear()
            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
                "team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 322, "status": "started"}
            ):
                fresh = runtime.restart(workspace)
            self.assertEqual(fresh["agents"][0]["restart_mode"], "fresh")
            fresh_agent = load_runtime_state(workspace)["agents"]["fake_impl"]
            for key in runtime.SESSION_CAPTURE_FIELDS:
                self.assertIsNone(fresh_agent[key])
            self.assertEqual(fresh_agent["spawn_cwd"], str(workspace))
            self.assertTrue(any(e["event"] == "restart.fresh_spawn" for e in _events(workspace)))

    def test_restart_requires_team_selector_when_multiple_snapshots_exist(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-multiple-") as tmp:
            workspace = Path(tmp)
            for session_name, session_id in [("team-alpha", "session-alpha"), ("team-beta", "session-beta")]:
                spec = _fake_spec(workspace)
                spec["team"]["name"] = session_name.replace("team-", "")
                spec["runtime"]["session_name"] = session_name
                spec_path = workspace / f"{session_name}.spec.yaml"
                spec_path.write_text(dumps(spec), encoding="utf-8")
                state = {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": session_name,
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "agent_id": "fake_impl",
                            "window": "fake_impl",
                            "session_id": session_id,
                            "mcp_config": str(workspace / ".team" / "runtime" / "mcp" / "fake_impl.json"),
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                }
                runtime._save_team_runtime_snapshot(workspace, state)

            with self.assertRaises(TeamAgentRuntimeError) as ctx:
                runtime.restart(workspace)
            self.assertIn("multiple restartable teams", str(ctx.exception))
            self.assertIn("team-alpha", str(ctx.exception))
            self.assertIn("team-beta", str(ctx.exception))

            started_windows: set[str] = set()
            fake_run_cmd = _make_fake_tmux_window_run_cmd(started_windows)

            with patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd), patch(
                "team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 321, "status": "started"}
            ):
                result = runtime.restart(workspace, team="team-alpha")

            self.assertEqual(result["session_name"], "team-alpha")
            self.assertEqual(result["agents"][0]["restart_mode"], "resumed")
            self.assertEqual(load_runtime_state(workspace)["agents"]["fake_impl"]["session_id"], "session-alpha")

    def test_restart_opens_displays_after_all_worker_windows_start(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-display-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["display_backend"] = "ghostty_window"
            peer = copy.deepcopy(spec["agents"][0])
            peer["id"] = "fake_peer"
            spec["agents"].append(peer)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-restart-display",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {"status": "stopped", "provider": "fake", "window": "fake_impl", "session_id": None},
                        "fake_peer": {"status": "stopped", "provider": "fake", "window": "fake_peer", "session_id": None},
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "ghostty_window",
                },
            )
            started_windows: set[str] = set()
            display_snapshots: list[set[str]] = []
            fake_run_cmd = _make_fake_tmux_window_run_cmd(started_windows)

            def fake_open_display(workspace_arg, session_name, window_name, agent, event_log):
                display_snapshots.append(set(started_windows))
                return {"status": "opened", "target": f"{session_name}:{window_name}"}

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime._open_ghostty_worker_window", side_effect=fake_open_display),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.restart(workspace, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(started_windows, {"fake_impl", "fake_peer"})
            self.assertEqual(len(display_snapshots), 2)
            self.assertTrue(all(snapshot == {"fake_impl", "fake_peer"} for snapshot in display_snapshots))

    def test_restart_first_resume_exit_fallback_recreates_session_and_opens_display(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-first-fallback-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["display_backend"] = "ghostty_window"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            (workspace / "docs").mkdir()
            (workspace / "docs" / "requirements.md").write_text("requirements already written", encoding="utf-8")
            (workspace / "contract").mkdir()
            (workspace / "contract" / "README.md").write_text("contract already written", encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-restart-first-fallback",
                    "leader": spec["leader"],
                    "agents": {
                        "fake_impl": {
                            "status": "stopped",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "stale-session",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            session_exists = False
            started_windows: set[str] = set()
            list_checks = 0
            starts: list[tuple[str, str, str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                nonlocal session_exists, list_checks
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:2] == ["tmux", "has-session"]:
                    proc.returncode = 0 if session_exists else 1
                elif args[:3] == ["tmux", "new-session", "-d"]:
                    session_exists = True
                    started_windows.clear()
                    started_windows.add(args[6])
                    starts.append(("new-session", args[6], args[-1]))
                elif args[:2] == ["tmux", "new-window"]:
                    if not session_exists:
                        proc.returncode = 1
                        proc.stderr = f"can't find window: {args[3]}"
                    else:
                        started_windows.add(args[5])
                        starts.append(("new-window", args[5], args[-1]))
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    list_checks += 1
                    if list_checks == 1:
                        session_exists = False
                        started_windows.clear()
                        proc.returncode = 1
                        proc.stderr = "can't find session: team-restart-first-fallback"
                    else:
                        proc.stdout = "\n".join(sorted(started_windows))
                return proc

            def fake_fresh_command(agent, workspace_arg, mcp_config):
                return "fresh-command"

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.shell_resume_command_for_agent", return_value="resume-command"),
                patch("team_agent.runtime.shell_command_for_agent", side_effect=fake_fresh_command),
                patch(
                    "team_agent.runtime._open_ghostty_worker_window",
                    return_value={"status": "opened", "target": "team-restart-first-fallback:fake_impl"},
                ) as open_display,
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 335, "status": "started"}),
            ):
                result = runtime.restart(workspace, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(result["agents"][0]["restart_mode"], "fresh")
            self.assertEqual(starts, [("new-session", "fake_impl", "resume-command"), ("new-session", "fake_impl", "fresh-command")])
            open_display.assert_called_once()
            state = load_runtime_state(workspace)
            self.assertEqual(state["display_backend"], "ghostty_window")
            self.assertEqual(state["agents"]["fake_impl"]["display"]["status"], "opened")
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "restart.window_missing_after_start" for e in events))
            self.assertTrue(any(e["event"] == "restart.resume_window_missing_fallback_fresh" for e in events))
            fresh_starts = [e for e in events if e["event"] == "restart.agent_start" and e.get("restart_mode") == "fresh"]
            self.assertEqual(fresh_starts[-1]["tmux_start_mode"], "new-session")

    def test_start_agent_repairs_missing_worker_window_without_restart(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-start-agent-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-start-agent",
                    "agents": {
                        "fake_impl": {
                            "status": "missing",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "fake-session-1",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            calls: list[list[str]] = []
            captured_runtime: list[dict[str, Any]] = []
            started_windows: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "other\n" + "\n".join(sorted(started_windows))
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                return proc

            def fake_resume_command(agent, previous, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            with (
                patch(
                    "team_agent.runtime._detect_inherited_dangerous_permissions",
                    return_value={
                        "enabled": True,
                        "provider": "codex",
                        "flag": "--dangerously-bypass-approvals-and-sandbox",
                    },
                ),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.shell_resume_command_for_agent", side_effect=fake_resume_command),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 333, "status": "started"}),
            ):
                result = runtime.start_agent(workspace, "fake_impl", open_display=False, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(result["start_mode"], "resumed")
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve"])
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve_inherited"])
            self.assertIn(["tmux", "new-window", "-t", "team-start-agent", "-n", "fake_impl", "sh", "-lc", "true"], calls)
            state = load_runtime_state(workspace)
            self.assertEqual(state["agents"]["fake_impl"]["status"], "running")
            self.assertTrue(any(e["event"] == "start_agent.complete" for e in _events(workspace)))

    def test_start_agent_falls_back_to_fresh_when_resume_window_exits(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-start-agent-fallback-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "workspace": str(workspace),
                    "session_name": "team-start-agent-fallback",
                    "agents": {
                        "fake_impl": {
                            "status": "missing",
                            "provider": "fake",
                            "window": "fake_impl",
                            "session_id": "stale-session",
                        }
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            start_modes: list[str] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=0, stdout="", stderr="")
                if args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "fake_impl\n" if start_modes and start_modes[-1] == "fresh" else "other\n"
                elif args[:2] == ["tmux", "new-window"]:
                    command = args[-1]
                    start_modes.append("fresh" if command == "fresh-command" else "resumed")
                return proc

            with (
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.shell_resume_command_for_agent", return_value="resume-command"),
                patch("team_agent.runtime.shell_command_for_agent", return_value="fresh-command"),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 334, "status": "started"}),
            ):
                result = runtime.start_agent(workspace, "fake_impl", open_display=False, allow_fresh=True)

            self.assertTrue(result["ok"])
            self.assertEqual(result["start_mode"], "fresh")
            self.assertEqual(start_modes, ["resumed", "fresh"])
            agent_state = load_runtime_state(workspace)["agents"]["fake_impl"]
            for key in runtime.SESSION_CAPTURE_FIELDS:
                self.assertIsNone(agent_state[key])
            self.assertEqual(agent_state["spawn_cwd"], str(workspace))
            events = _events(workspace)
            self.assertTrue(any(e["event"] == "start_agent.window_missing_after_start" for e in events))
            self.assertTrue(any(e["event"] == "start_agent.resume_window_missing_fallback_fresh" for e in events))

    def test_shutdown_checks_session_capture_before_kill(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-shutdown-capture-") as tmp:
            workspace = Path(tmp)
            state = {
                "session_name": "team-shutdown-capture",
                "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl", "session_id": None}},
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            with patch("team_agent.runtime._capture_missing_sessions", return_value=[] ) as capture, patch(
                "team_agent.runtime._tmux_session_exists", return_value=False
            ):
                runtime.shutdown(workspace)
            capture.assert_called_once()
            self.assertTrue(any(e["event"] == "shutdown.session_capture_checked" for e in _events(workspace)))

    def test_shutdown_closes_ghostty_display_session_before_base_session_without_pid(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-shutdown-display-") as tmp:
            workspace = Path(tmp)
            session_name = "team-shutdown-display"
            display_session = "team-shutdown-display__display__fake_impl__12345678"
            state = {
                "session_name": session_name,
                "agents": {
                    "fake_impl": {
                        "status": "running",
                        "provider": "fake",
                        "window": "fake_impl",
                        "session_id": "fake-session",
                        "display": {
                            "backend": "ghostty_window",
                            "title": "team-agent:fake_impl:Implementation Worker",
                            "display_session": display_session,
                            "pids": [],
                        },
                    }
                },
                "tasks": [],
            }
            save_runtime_state(workspace, state)
            calls: list[list[str]] = []

            def fake_run_cmd(args: list[str], timeout: int = 20):
                calls.append(args)
                if args[:2] == ["pgrep", "-f"]:
                    return Mock(returncode=1, stdout="", stderr="")
                return Mock(returncode=0, stdout="", stderr="")

            def fake_session_exists(name: str | None) -> bool:
                return name in {session_name, display_session}

            with (
                patch("team_agent.runtime._capture_missing_sessions", return_value=[]),
                patch("team_agent.runtime._tmux_session_exists", side_effect=fake_session_exists),
                patch("team_agent.runtime._tmux_window_exists", return_value=True),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
            ):
                runtime.shutdown(workspace)

            kill_targets = [call[-1] for call in calls if call[:3] == ["tmux", "kill-session", "-t"]]
            self.assertIn(display_session, kill_targets)
            self.assertIn(session_name, kill_targets)
            self.assertLess(kill_targets.index(display_session), kill_targets.index(session_name))
            self.assertTrue(any(e["event"] == "display.ghostty_display_session_closed" for e in _events(workspace)))

    def test_send_lock_reports_busy_instead_of_parallel_write(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-send-lock-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            lock_path = workspace / ".team" / "runtime" / "send.lock"
            lock_path.parent.mkdir(parents=True, exist_ok=True)
            with lock_path.open("w", encoding="utf-8") as lock_file:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaises(runtime.RuntimeError) as ctx:
                    runtime.send_message(workspace, None, "blocked by lock", task_id="task_impl", lock_timeout=0.01)
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
            self.assertIn("locked by another team-agent process", str(ctx.exception))
            self.assertTrue(any(e["event"] == "runtime.lock_busy" for e in _events(workspace)))

    def test_repair_state_is_task_scoped_and_validated(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-repair-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            second = copy.deepcopy(spec["tasks"][0])
            second["id"] = "task_other"
            spec["tasks"].append(second)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": copy.deepcopy(spec["tasks"]),
                },
            )
            result = runtime.repair_state(workspace, "task_impl", assignee="leader", status_value="pending", summary="manual repair")
            self.assertTrue(result["ok"])
            state = load_runtime_state(workspace)
            by_id = {task["id"]: task for task in state["tasks"]}
            self.assertEqual(by_id["task_impl"]["assignee"], "leader")
            self.assertEqual(by_id["task_impl"]["last_result_summary"], "manual repair")
            self.assertNotEqual(by_id["task_other"].get("last_result_summary"), "manual repair")
            with self.assertRaises(runtime.RuntimeError):
                runtime.repair_state(workspace, "task_impl", assignee="nobody")
            with self.assertRaises(runtime.RuntimeError):
                runtime.repair_state(workspace, "task_impl", status_value="almost_done")

    def test_human_confirmation_blocks_send_until_confirmed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-human-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["human_confirmation"] = True
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})

            blocked = runtime.send_message(workspace, None, "needs approval", task_id="task_impl")
            self.assertFalse(blocked["ok"])
            self.assertEqual(blocked["reason"], "human_confirmation_required")
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "blocked")
            self.assertTrue(any(e["event"] == "send.human_confirmation_required" for e in _events(workspace)))

    def test_human_confirmation_can_be_confirmed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-human-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["human_confirmation"] = True
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "missing-human-test",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )

            sent = runtime.send_message(workspace, None, "confirmed", task_id="task_impl", confirm_human=True)
            self.assertFalse(sent["ok"])
            state = load_runtime_state(workspace)
            self.assertTrue(state["tasks"][0]["human_confirmed"])
            self.assertEqual(state["tasks"][0]["assignee"], "fake_impl")
            self.assertTrue(any(e["event"] == "send.human_confirmation_granted" for e in _events(workspace)))

    def test_invalid_result_file_does_not_mutate_task_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-invalid-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["retry_limit"] = 1
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})
            result_path = workspace / "bad-result.json"
            result_path.write_text(
                json.dumps(
                    {
                        "schema_version": "result_envelope_v1",
                        "task_id": "task_impl",
                        "agent_id": "fake_impl",
                        "status": "success",
                    }
                ),
                encoding="utf-8",
            )
            result = runtime.collect(workspace, result_file=result_path)
            self.assertFalse(result["ok"])
            self.assertIsNone(result["invalid_results"][0]["result_id"])
            self.assertIn("/summary", result["invalid_results"][0]["error"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "pending")
            self.assertIsNone(state["tasks"][0]["assignee"])
            self.assertTrue(any(e["event"] == "collect.invalid_result" for e in _events(workspace)))

    def test_collect_invalid_stored_result_does_not_break_team_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-invalid-stored-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})
            store = MessageStore(workspace)
            result_id = "res_bad_nested"
            bad = _result_envelope("success")
            bad["artifacts"] = ["bad-artifact.md"]
            conn = store.connect()
            try:
                conn.execute(
                    "insert into results values (?, ?, ?, ?, ?, ?)",
                    (result_id, "task_impl", "fake_impl", json.dumps(bad), "success", "now"),
                )
                conn.commit()
            finally:
                conn.close()
            result = runtime.collect(workspace)
            self.assertFalse(result["ok"])
            self.assertEqual(result["invalid_results"][0]["result_id"], result_id)
            self.assertIn("/artifacts/0", result["invalid_results"][0]["error"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "pending")
            state_text = (workspace / "team_state.md").read_text(encoding="utf-8")
            self.assertIn("task_impl [pending]", state_text)

    def test_status_and_collect_expose_uncollected_report_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-result-status-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})

            accepted = runtime.report_result(workspace, _result_envelope("success"))
            self.assertTrue(accepted["ok"])
            status = runtime.status(workspace, as_json=True)
            self.assertEqual(status["results"]["uncollected"], 1)
            self.assertEqual(status["results"]["by_status"]["success"], 1)
            self.assertIsNone(status["messages"].get("failed"))
            self.assertIn("uncollected 1", runtime.format_status(workspace))
            self.assertIn("1 uncollected result(s) pending", runtime.format_inbox(workspace, "fake_impl"))

            collected = runtime.collect(workspace)
            self.assertEqual(collected["results"]["uncollected"], 0)
            self.assertEqual(collected["results"]["collected"], 1)
            status = runtime.status(workspace, as_json=True)
            self.assertEqual(status["tasks"][0]["status"], "done")
            self.assertEqual(status["tasks"][0]["accepted_result_id"], accepted["result_id"])

    def test_collect_accepts_message_scoped_report_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-message-result-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake"}},
                    "tasks": spec["tasks"],
                },
            )
            message_id = MessageStore(workspace).create_message(
                None,
                "leader",
                "fake_impl",
                "do one visible action",
                requires_ack=True,
            )
            envelope = _result_envelope("success")
            envelope["task_id"] = message_id

            reported = runtime.report_result(workspace, envelope)
            collected = runtime.collect(workspace)

        self.assertTrue(reported["ok"])
        self.assertEqual(reported["acknowledged_messages"], [message_id])
        self.assertTrue(collected["ok"])
        self.assertEqual(collected["invalid_results"], [])
        self.assertEqual(collected["collected_results"][0]["scope"], "message")

    def test_report_result_queues_leader_notification_without_blocking_mcp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-report-notify-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            with patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 123, "status": "started"}):
                accepted = runtime.report_result(workspace, _result_envelope("success"))
            self.assertTrue(accepted["ok"])
            self.assertFalse(accepted["leader_notified"])
            self.assertEqual(accepted["notification_status"], "queued")
            self.assertEqual(accepted["notification_channel"], "coordinator")
            self.assertIsNotNone(accepted["notification_event_id"])

            with patch(
                "team_agent.runtime._send_to_leader_receiver",
                return_value={"ok": True, "message_id": "msg_notice", "status": "submitted", "channel": "direct_tmux"},
            ) as notify:
                tick = runtime.coordinator_tick(workspace)
            self.assertTrue(tick["ok"])
            args, _ = notify.call_args
            self.assertEqual(args[2], "leader")
            self.assertIn("Task task_impl reported success from fake_impl", args[3])
            self.assertEqual(args[4], "task_impl")
            self.assertEqual(args[5], "fake_impl")
            self.assertFalse(args[6])

    def test_coordinator_collects_watched_result_and_notifies_leader(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-watch-notify-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            task = {**spec["tasks"][0], "assignee": "fake_impl", "status": "running"}
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            watcher_id = store.create_result_watcher("task_impl", "fake_impl", "msg_sent", "leader")
            result_id = store.add_result(_result_envelope("success"))
            notifications: list[dict[str, Any]] = []

            def fake_send_message(*args, **kwargs):
                notifications.append({"args": args, "kwargs": kwargs})
                return {"ok": True, "message_id": "msg_notice", "status": "submitted"}

            with patch("team_agent.runtime.send_message", side_effect=fake_send_message):
                tick = runtime.coordinator_tick(workspace)

            self.assertTrue(tick["ok"])
            self.assertEqual(tick["results"]["collected"], 1)
            self.assertEqual(tick["results"]["notified"][0]["watcher_id"], watcher_id)
            self.assertEqual(MessageStore(workspace).results()[0]["status"], "collected")
            watcher = MessageStore(workspace).result_watchers()[0]
            self.assertEqual(watcher["status"], "notified")
            self.assertEqual(watcher["result_id"], result_id)
            self.assertEqual(watcher["notified_message_id"], "msg_notice")
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "done")
            self.assertEqual(state["tasks"][0]["accepted_result_id"], result_id)
            self.assertEqual(notifications[0]["args"][1], "leader")
            self.assertIn("No manual polling is needed", notifications[0]["args"][2])

    def test_failed_result_consumes_retry_then_fails(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-retry-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["tasks"][0]["retry_limit"] = 1
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(workspace, {"spec_path": str(spec_path), "session_name": None, "agents": {}, "tasks": spec["tasks"]})
            result_path = workspace / "failed-result.json"

            result_path.write_text(json.dumps(_result_envelope("failed")), encoding="utf-8")
            first = runtime.collect(workspace, result_file=result_path)
            self.assertTrue(first["ok"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "needs_retry")
            self.assertEqual(state["tasks"][0]["retry_count"], 1)

            result_path.write_text(json.dumps(_result_envelope("failed")), encoding="utf-8")
            second = runtime.collect(workspace, result_file=result_path)
            self.assertTrue(second["ok"])
            state = runtime.status(workspace, as_json=True)
            self.assertEqual(state["tasks"][0]["status"], "failed")
            self.assertEqual(state["tasks"][0]["retry_count"], 1)

    def test_diagnose_missing_session_and_mcp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "definitely-missing-session",
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            result = runtime.diagnose(workspace)
            kinds = {issue["kind"] for issue in result["issues"]}
            self.assertIn("tmux_session_missing", kinds)
            self.assertIn("mcp_not_installed", kinds)
            repair_kinds = {repair["kind"] for repair in result["suggested_repairs"]}
            self.assertIn("mcp_approval_prompt", repair_kinds)
            self.assertIn("leader_receiver", repair_kinds)

    def test_diagnose_reports_interrupted_worker_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-interrupted-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {"fake_impl": {"status": "interrupted", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            result = runtime.diagnose(workspace)
            self.assertIn("worker_interrupted", {issue["kind"] for issue in result["issues"]})
            self.assertIn("interrupted_worker", {repair["kind"] for repair in result["suggested_repairs"]})

    def test_diagnose_provider_missing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-diagnose-provider-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": spec["tasks"],
                },
            )
            adapter = Mock(command_name="missing-fake")
            adapter.is_installed.return_value = False
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                result = runtime.diagnose(workspace)
            provider_issue = next(issue for issue in result["issues"] if issue["kind"] == "provider_missing")
            self.assertEqual(provider_issue["agent_id"], "fake_impl")
            self.assertEqual(provider_issue["command"], "missing-fake")

    def test_doctor_reports_missing_provider_auth(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-doctor-auth-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["agents"][0]["provider"] = "claude_code"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock(command_name="claude")
            adapter.is_installed.return_value = True
            adapter.version.return_value = "Claude Code"
            adapter.auth_hint.return_value = {"status": "missing", "detail": "not logged in"}
            with patch("team_agent.runtime.get_adapter", return_value=adapter):
                result = runtime.doctor(spec_path)
            self.assertFalse(result["ok"])
            self.assertEqual(result["missing_provider_auth"], ["claude_code"])

    def test_claude_auth_hint_uses_cli_status(self) -> None:
        adapter = get_adapter("claude_code")
        proc = Mock(returncode=1, stdout='{"loggedIn": false, "authMethod": "none"}', stderr="")
        with patch.object(adapter, "is_installed", return_value=True), patch("team_agent.providers.subprocess.run", return_value=proc):
            result = adapter.auth_hint()
        self.assertEqual(result["status"], "missing")
        self.assertIn("loggedIn", result["detail"])

    def test_mcp_json_rpc_tools_list(self) -> None:
        import subprocess
        import sys

        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-") as tmp:
            proc = subprocess.Popen(
                [sys.executable, "-m", "team_agent.mcp_server", "--workspace", tmp],
                cwd=ROOT,
                text=True,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            assert proc.stdin is not None
            assert proc.stdout is not None
            try:
                proc.stdin.write('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}\n')
                proc.stdin.flush()
                init = json.loads(proc.stdout.readline())
                self.assertEqual(init["result"]["serverInfo"]["name"], "team_orchestrator")
                proc.stdin.write('{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n')
                proc.stdin.flush()
                tools = json.loads(proc.stdout.readline())
                by_name = {tool["name"]: tool for tool in tools["result"]["tools"]}
                names = set(by_name)
                self.assertIn("report_result", names)
                self.assertIn("assign_task", names)
                send_schema = by_name["send_message"]["inputSchema"]
                self.assertEqual(send_schema["required"], ["to", "content"])
                self.assertEqual(set(send_schema["properties"]), {"to", "content"})
                self.assertFalse(send_schema["additionalProperties"])
                report_schema = by_name["report_result"]["inputSchema"]
                self.assertEqual(report_schema["required"], ["summary"])
                self.assertNotIn("envelope", report_schema["properties"])
                self.assertFalse(report_schema["additionalProperties"])
            finally:
                proc.stdin.close()
                proc.stdout.close()
                if proc.stderr is not None:
                    proc.stderr.close()
                proc.kill()
                proc.wait(timeout=5)

    def test_mcp_send_message_accepts_thin_args_and_returns_compact_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-thin-send-") as tmp:
            workspace = Path(tmp)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                with patch("team_agent.mcp_server.runtime.send_message") as send:
                    send.return_value = {
                        "ok": True,
                        "status": "submitted",
                        "message_id": "msg_123",
                        "to": "leader",
                        "visible": True,
                        "submitted": True,
                        "channel": "direct_tmux",
                        "leader_receiver": {"pane_id": "%1"},
                        "verification": {"token": "msg_123"},
                    }
                    result = TeamOrchestratorTools(workspace).send_message(to="leader", content="hello")
            send.assert_called_once()
            args, kwargs = send.call_args
            self.assertEqual(args[:3], (workspace.resolve(), "leader", "hello"))
            self.assertEqual(kwargs["sender"], "fake_impl")
            self.assertIsNone(kwargs["task_id"])
            self.assertFalse(kwargs["requires_ack"])
            self.assertEqual(
                result,
                {
                    "ok": True,
                    "status": "submitted",
                    "message_id": "msg_123",
                    "to": "leader",
                    "submitted": True,
                    "visible": True,
                },
            )

    def test_mcp_send_message_accepts_broadcast_target(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-broadcast-") as tmp:
            workspace = Path(tmp)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                with patch("team_agent.mcp_server.runtime.send_message") as send:
                    send.return_value = {
                        "ok": True,
                        "status": "broadcast_delivered",
                        "to": "*",
                        "targets": ["leader", "fake_peer"],
                        "delivered_count": 2,
                        "failed_count": 0,
                        "deliveries": [{"ok": True, "to": "leader"}, {"ok": True, "to": "fake_peer"}],
                    }
                    result = TeamOrchestratorTools(workspace).send_message(to="*", content="hello all")
            args, kwargs = send.call_args
            self.assertEqual(args[:3], (workspace.resolve(), "*", "hello all"))
            self.assertEqual(kwargs["sender"], "fake_impl")
            self.assertTrue(kwargs["requires_ack"])
            self.assertEqual(
                result,
                {
                    "ok": True,
                    "status": "broadcast_delivered",
                    "to": "*",
                    "targets": ["leader", "fake_peer"],
                    "delivered_count": 2,
                    "failed_count": 0,
                },
            )

    def test_mcp_send_message_without_env_infers_worker_before_leader_send(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-missing-id-send-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            old_agent = os.environ.pop("TEAM_AGENT_ID", None)
            try:
                with patch("team_agent.mcp_server.runtime.send_message") as send:
                    send.return_value = {"ok": True, "status": "submitted", "message_id": "msg_123", "to": "leader"}
                    result = TeamOrchestratorTools(workspace).send_message(to="leader", content="hello")
            finally:
                if old_agent is not None:
                    os.environ["TEAM_AGENT_ID"] = old_agent
            self.assertTrue(result["ok"])
            args, kwargs = send.call_args
            self.assertEqual(args[:3], (workspace.resolve(), "leader", "hello"))
            self.assertEqual(kwargs["sender"], "fake_impl")
            self.assertFalse(kwargs["requires_ack"])

    def test_mcp_report_result_accepts_summary_and_fills_envelope(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-thin-report-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            store.create_message("task_impl", "leader", "fake_impl", "do work", requires_ack=True)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                result = TeamOrchestratorTools(workspace).report_result(
                    summary="done",
                    tests=[{"command": "unit", "status": "passed"}],
                )
            self.assertTrue(result["ok"])
            self.assertEqual(result["task_id"], "task_impl")
            self.assertEqual(result["agent_id"], "fake_impl")
            self.assertEqual(result["acknowledged_count"], 1)
            self.assertNotIn("acknowledged_messages", result)
            stored = MessageStore(workspace).results()[0]
            envelope = json.loads(stored["envelope"])
            self.assertEqual(envelope["schema_version"], "result_envelope_v1")
            self.assertEqual(envelope["task_id"], "task_impl")
            self.assertEqual(envelope["agent_id"], "fake_impl")
            self.assertEqual(envelope["summary"], "done")
            self.assertEqual(envelope["changes"], [])
            self.assertEqual(envelope["tests"], [{"command": "unit", "status": "passed"}])

    def test_mcp_report_result_without_env_infers_task_and_agent(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-missing-id-report-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                    "tasks": [task],
                },
            )
            MessageStore(workspace).create_message("task_impl", "leader", "fake_impl", "do work", requires_ack=True)
            old_agent = os.environ.pop("TEAM_AGENT_ID", None)
            try:
                with patch(
                    "team_agent.runtime._notify_leader_of_report_result",
                    return_value={"ok": True, "message_id": "msg_notice", "status": "submitted", "channel": "direct_tmux"},
                ):
                    result = TeamOrchestratorTools(workspace).report_result(summary="done")
            finally:
                if old_agent is not None:
                    os.environ["TEAM_AGENT_ID"] = old_agent
            self.assertTrue(result["ok"])
            self.assertEqual(result["task_id"], "task_impl")
            self.assertEqual(result["agent_id"], "fake_impl")
            self.assertTrue(result["leader_notified"])
            envelope = json.loads(MessageStore(workspace).results()[0]["envelope"])
            self.assertEqual(envelope["task_id"], "task_impl")
            self.assertEqual(envelope["agent_id"], "fake_impl")

    def test_mcp_report_result_normalizes_common_loose_shapes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-mcp-lenient-report-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            task = copy.deepcopy(spec["tasks"][0])
            task["assignee"] = "fake_impl"
            task["status"] = "running"
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": None,
                    "leader": spec["leader"],
                    "agents": {},
                    "tasks": [task],
                },
            )
            store = MessageStore(workspace)
            store.create_message("task_impl", "leader", "fake_impl", "do work", requires_ack=True)
            with patch.dict(os.environ, {"TEAM_AGENT_ID": "fake_impl"}):
                first = TeamOrchestratorTools(workspace).report_result(
                    summary="Created hello.py in the current workspace and verified it prints Hello, World!",
                    status="success",
                    changes=[{"path": "hello.py", "description": "Added minimal Python Hello World script."}],
                    tests=[{"command": "python3 hello.py", "status": "passed", "output": "Hello, World!"}],
                )
                second = TeamOrchestratorTools(workspace).report_result(
                    summary="Created hello.py in the current workspace and verified it prints Hello, World!",
                    status="success",
                    changes=[{"kind": "created", "path": "hello.py", "summary": "Added minimal Python Hello World script."}],
                    tests=[{"command": "python3 hello.py", "status": "passed"}],
                )
            self.assertTrue(first["ok"])
            self.assertTrue(second["ok"])
            rows = MessageStore(workspace).results()
            first_envelope = json.loads(rows[0]["envelope"])
            second_envelope = json.loads(rows[1]["envelope"])
            self.assertEqual(
                first_envelope["changes"],
                [{"path": "hello.py", "kind": "created", "description": "Added minimal Python Hello World script."}],
            )
            self.assertEqual(
                first_envelope["tests"],
                [{"command": "python3 hello.py", "status": "passed", "detail": "Hello, World!"}],
            )
            self.assertEqual(
                second_envelope["changes"],
                [{"path": "hello.py", "kind": "created", "description": "Added minimal Python Hello World script."}],
            )

    def test_compile_system_prompt_prepends_teammate_runtime_contract(self) -> None:
        agent = _provider_agent("codex", "codex_implementer")
        agent["system_prompt"]["inline"] = "ROLE_MARKER: review code."
        prompt = compile_system_prompt(agent)
        self.assertLess(prompt.index("Team Agent worker `codex_implementer`"), prompt.index("Team Agent Teammate Runtime Contract"))
        self.assertLess(prompt.index("Team Agent Teammate Runtime Contract"), prompt.index("ROLE_MARKER"))
        self.assertIn("role `reviewer`", prompt)
        self.assertIn("Plain text you write in this worker", prompt)
        self.assertIn("team_orchestrator.send_message(to='leader'", prompt)
        self.assertIn("to='*' to notify every other team member", prompt)
        self.assertIn("team_orchestrator.report_result exactly once", prompt)

    def test_provider_mcp_config_uses_local_python_module(self) -> None:
        config = get_adapter("codex").mcp_config(ROOT, "codex_implementer")["team_orchestrator"]
        self.assertIn("python", Path(config["command"]).name)
        self.assertEqual(config["args"][:2], ["-m", "team_agent.mcp_server"])
        self.assertIn("PYTHONPATH", config["env"])

    def test_worker_command_exports_current_path_for_codex_wrapper(self) -> None:
        from team_agent.providers import shell_command_for_agent

        agent = _provider_agent("fake", "fake_impl")
        with patch.dict(os.environ, {"PATH": "/Users/alauda/.local/bin:/opt/homebrew/bin"}):
            command = shell_command_for_agent(agent, ROOT, {})
        self.assertIn("PATH=/Users/alauda/.local/bin:/opt/homebrew/bin", command)
        self.assertNotIn("HTTPS_PROXY", command)

    def test_gemini_install_mcp_writes_settings_and_cleanup_restores(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gemini-mcp-") as tmp:
            home = Path(tmp) / "home"
            workspace = Path(tmp) / "workspace"
            settings_path = home / ".gemini" / "settings.json"
            settings_path.parent.mkdir(parents=True)
            settings_path.write_text(
                json.dumps(
                    {
                        "mcpServers": {
                            "team_orchestrator": {"command": "old", "args": [], "env": {}},
                            "unrelated": {"command": "keep", "args": [], "env": {}},
                        }
                    }
                ),
                encoding="utf-8",
            )
            adapter = get_adapter("gemini_cli")
            config = adapter.mcp_config(workspace, "gemini_researcher")
            with patch("team_agent.providers.Path.home", return_value=home):
                mcp_path = adapter.install_mcp(workspace, "gemini_researcher", config)
                settings = json.loads(settings_path.read_text(encoding="utf-8"))
                self.assertEqual(settings["mcpServers"]["team_orchestrator"]["args"][:2], ["-m", "team_agent.mcp_server"])
                self.assertEqual(settings["mcpServers"]["unrelated"]["command"], "keep")
                adapter.cleanup_mcp(workspace, "gemini_researcher", mcp_path)
            restored = json.loads(settings_path.read_text(encoding="utf-8"))
            self.assertEqual(restored["mcpServers"]["team_orchestrator"]["command"], "old")
            self.assertEqual(restored["mcpServers"]["unrelated"]["command"], "keep")

    def test_shutdown_restores_gemini_mcp_settings(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-gemini-shutdown-") as tmp:
            home = Path(tmp) / "home"
            workspace = Path(tmp) / "workspace"
            settings_path = home / ".gemini" / "settings.json"
            settings_path.parent.mkdir(parents=True)
            settings_path.write_text(json.dumps({"mcpServers": {}}), encoding="utf-8")
            adapter = get_adapter("gemini_cli")
            config = adapter.mcp_config(workspace, "gemini_researcher")
            with patch("team_agent.providers.Path.home", return_value=home):
                mcp_path = adapter.install_mcp(workspace, "gemini_researcher", config)
                save_runtime_state(
                    workspace,
                    {
                        "spec_path": str(workspace / "team.spec.yaml"),
                        "session_name": None,
                        "agents": {
                            "gemini_researcher": {
                                "status": "running",
                                "provider": "gemini_cli",
                                "window": "gemini_researcher",
                                "mcp_config": str(mcp_path),
                            }
                        },
                        "tasks": [],
                    },
                )
                runtime.shutdown(workspace)
            settings = json.loads(settings_path.read_text(encoding="utf-8"))
            self.assertNotIn("team_orchestrator", settings["mcpServers"])

    def test_claude_default_command_avoids_dangerous_bypass(self) -> None:
        agent = _provider_agent("claude_code", "claude_reviewer")
        cmd = get_adapter("claude_code").build_command(agent, ROOT, {})
        self.assertNotIn("--dangerously-skip-permissions", cmd)
        self.assertIn("--permission-mode", cmd)
        self.assertIn("default", cmd)

    def test_claude_dangerous_auto_approve_requires_runtime_opt_in(self) -> None:
        agent = _provider_agent("claude_code", "claude_reviewer")
        agent["_runtime"] = {"dangerous_auto_approve": True}
        cmd = get_adapter("claude_code").build_command(agent, ROOT, {})
        self.assertIn("--dangerously-skip-permissions", cmd)
        self.assertNotIn("--permission-mode", cmd)

    def test_gemini_default_command_avoids_dangerous_bypass(self) -> None:
        agent = _provider_agent("gemini_cli", "gemini_reviewer")
        cmd = get_adapter("gemini_cli").build_command(agent, ROOT, {})
        self.assertNotIn("--yolo", cmd)
        self.assertNotIn("--sandbox", cmd)

    def test_gemini_dangerous_auto_approve_requires_runtime_opt_in(self) -> None:
        agent = _provider_agent("gemini_cli", "gemini_reviewer")
        agent["_runtime"] = {"dangerous_auto_approve": True}
        cmd = get_adapter("gemini_cli").build_command(agent, ROOT, {})
        self.assertIn("--yolo", cmd)
        self.assertIn("--sandbox", cmd)
        self.assertIn("false", cmd)

    def test_codex_default_command_avoids_dangerous_bypass(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        agent = next(a for a in spec["agents"] if a["id"] == "codex_implementer")
        cmd = get_adapter("codex").build_command(agent, ROOT, {})
        self.assertNotIn("--yolo", cmd)
        self.assertNotIn("--dangerously-bypass-approvals-and-sandbox", cmd)
        self.assertIn("--sandbox", cmd)
        self.assertIn("--ask-for-approval", cmd)
        self.assertIn("--disable", cmd)
        self.assertIn("apps", cmd)
        self.assertIn("shell_snapshot", cmd)

    def test_codex_dangerous_auto_approve_requires_runtime_opt_in(self) -> None:
        spec = load_spec(ROOT / "examples" / "team.spec.yaml")
        agent = dict(next(a for a in spec["agents"] if a["id"] == "codex_implementer"))
        agent["_runtime"] = {"dangerous_auto_approve": True}
        cmd = get_adapter("codex").build_command(agent, ROOT, {})
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", cmd)
        self.assertNotIn("--yolo", cmd)

    def test_dangerous_auto_approve_visible_in_dry_run(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-safety-") as tmp:
            workspace = Path(tmp)
            spec = load_spec(ROOT / "examples" / "team.spec.yaml")
            spec["team"]["workspace"] = str(workspace)
            spec["runtime"]["dangerous_auto_approve"] = True
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            result = runtime.launch(spec_path, dry_run=True)
            self.assertTrue(result["safety"]["dangerous_auto_approve"])
            self.assertTrue(result["safety"]["requires_explicit_yes"])

    def test_leader_dangerous_permissions_detect_from_process_ancestry(self) -> None:
        with patch(
            "team_agent.runtime._process_ancestry",
            return_value=[
                {"pid": 10, "ppid": 9, "command": "python3 -m team_agent"},
                {"pid": 9, "ppid": 8, "command": "codex --dangerously-bypass-approvals-and-sandbox"},
            ],
        ):
            inherited = runtime._detect_inherited_dangerous_permissions()
        self.assertTrue(inherited["enabled"])
        self.assertEqual(inherited["provider"], "codex")
        self.assertEqual(inherited["flag"], "--dangerously-bypass-approvals-and-sandbox")

    def test_launch_inherits_leader_dangerous_permissions_in_dry_run(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-inherit-dangerous-") as tmp:
            workspace = Path(tmp)
            spec = load_spec(ROOT / "examples" / "team.spec.yaml")
            spec["team"]["workspace"] = str(workspace)
            spec["runtime"]["dangerous_auto_approve"] = False
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            with patch(
                "team_agent.runtime._detect_inherited_dangerous_permissions",
                return_value={
                    "enabled": True,
                    "provider": "claude",
                    "flag": "--dangerously-skip-permissions",
                    "pid": 123,
                },
            ):
                result = runtime.launch(spec_path, dry_run=True)
            self.assertTrue(result["safety"]["dangerous_auto_approve"])
            self.assertTrue(result["safety"]["dangerous_auto_approve_inherited"])
            self.assertEqual(result["safety"]["dangerous_auto_approve_source"], "leader_process")
            self.assertFalse(result["safety"]["requires_explicit_yes"])

    def test_launch_session_conflict_guides_to_rename_not_shutdown(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-session-conflict-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-conflict"
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            with (
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime._tmux_session_exists", return_value=True),
            ):
                with self.assertRaises(runtime.RuntimeError) as ctx:
                    runtime.launch(spec_path, auto_approve=True)
            message = str(ctx.exception)
            self.assertIn("tmux session already exists: team-conflict", message)
            self.assertIn("will not terminate existing tmux sessions", message)
            self.assertIn("different team name", message)
            self.assertNotIn("team-agent shutdown", message)

    def test_launch_passes_inherited_dangerous_permissions_to_worker_runtime(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-inherit-runtime-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec["runtime"]["session_name"] = "team-agent-inherit-runtime-" + workspace.name[-6:]
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            adapter = Mock()
            adapter.is_installed.return_value = True
            adapter.mcp_config.return_value = {}
            adapter.install_mcp.return_value = workspace / ".team/runtime/mcp/fake_impl.json"
            adapter.handle_startup_prompts.return_value = []
            captured_runtime: list[dict[str, Any]] = []

            def fake_shell_command(agent, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            with (
                patch(
                    "team_agent.runtime._detect_inherited_dangerous_permissions",
                    return_value={
                        "enabled": True,
                        "provider": "codex",
                        "flag": "--dangerously-bypass-approvals-and-sandbox",
                    },
                ),
                patch("team_agent.runtime.get_adapter", return_value=adapter),
                patch("team_agent.runtime.shell_command_for_agent", side_effect=fake_shell_command),
                patch("team_agent.runtime._tmux_session_exists", return_value=False),
                patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                patch("team_agent.runtime.run_cmd", return_value=Mock(returncode=0, stdout="", stderr="")),
            ):
                launched = runtime.launch(spec_path, auto_approve=True)
            self.assertTrue(launched["ok"])
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve"])
            self.assertTrue(captured_runtime[0]["dangerous_auto_approve_inherited"])
            self.assertEqual(captured_runtime[0]["dangerous_auto_approve_source"], "leader_process")

    def test_restart_passes_inherited_dangerous_permissions_to_resume_and_fresh_workers(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-restart-inherit-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            fresh_agent = copy.deepcopy(spec["agents"][0])
            fresh_agent["id"] = "fake_fresh"
            spec["agents"].append(fresh_agent)
            spec_path = workspace / "team.spec.yaml"
            spec_path.write_text(dumps(spec), encoding="utf-8")
            save_runtime_state(
                workspace,
                {
                    "spec_path": str(spec_path),
                    "session_name": "team-restart-inherit",
                    "agents": {
                        "fake_impl": {"status": "stopped", "provider": "fake", "window": "fake_impl", "session_id": "fake-session-1"},
                        "fake_fresh": {"status": "stopped", "provider": "fake", "window": "fake_fresh", "session_id": None},
                    },
                    "tasks": spec["tasks"],
                    "display_backend": "none",
                },
            )
            captured_runtime: list[dict[str, Any]] = []

            def fake_resume_command(agent, previous, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            def fake_fresh_command(agent, workspace_arg, mcp_config):
                captured_runtime.append(agent["_runtime"])
                return "true"

            started_windows: set[str] = set()

            def fake_run_cmd(args: list[str], timeout: int = 20):
                proc = Mock(returncode=1 if args[:2] == ["tmux", "has-session"] else 0, stdout="", stderr="")
                if args[:3] == ["tmux", "new-session", "-d"]:
                    started_windows.add(args[6])
                elif args[:2] == ["tmux", "new-window"]:
                    started_windows.add(args[5])
                elif args[:3] == ["tmux", "list-windows", "-t"]:
                    proc.stdout = "\n".join(sorted(started_windows))
                return proc

            with (
                patch(
                    "team_agent.runtime._detect_inherited_dangerous_permissions",
                    return_value={
                        "enabled": True,
                        "provider": "claude",
                        "flag": "--dangerously-skip-permissions",
                    },
                ),
                patch("team_agent.runtime.shell_resume_command_for_agent", side_effect=fake_resume_command),
                patch("team_agent.runtime.shell_command_for_agent", side_effect=fake_fresh_command),
                patch("team_agent.runtime.run_cmd", side_effect=fake_run_cmd),
                patch("team_agent.runtime.start_coordinator", return_value={"ok": True, "pid": 444, "status": "started"}),
            ):
                restarted = runtime.restart(workspace)

            self.assertTrue(restarted["ok"])
            self.assertEqual(len(captured_runtime), 2)
            self.assertTrue(all(item["dangerous_auto_approve"] for item in captured_runtime))
            self.assertTrue(all(item["dangerous_auto_approve_inherited"] for item in captured_runtime))
            self.assertEqual({item["dangerous_auto_approve_source"] for item in captured_runtime}, {"leader_process"})


class CliContractTests(unittest.TestCase):
    def test_all_help_commands(self) -> None:
        import subprocess
        import sys

        commands = [
            "codex",
            "claude",
            "quick-start",
            "init",
            "validate",
            "compile",
            "profile",
            "launch",
            "preflight",
            "start",
            "wait-ready",
            "settle",
            "status",
            "approvals",
            "peek",
            "inbox",
            "sessions",
            "attach-leader",
            "send",
            "collect",
            "diagnose",
            "repair-state",
            "validate-result",
            "doctor",
            "shutdown",
            "restart",
            "start-agent",
            "install-skill",
            "e2e",
            "allow-peer-talk",
            "advanced",
        ]
        for command in commands:
            proc = subprocess.run(
                [sys.executable, "-m", "team_agent", command, "--help"],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("usage:", proc.stdout)

    def test_top_help_is_blackbox_surface(self) -> None:
        import subprocess
        import sys

        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "--help"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("{codex,claude,quick-start,send,status,approvals,inbox,shutdown,restart,start-agent,doctor}", proc.stdout)
        self.assertNotIn("peek", proc.stdout)
        self.assertNotIn("compile", proc.stdout)
        self.assertNotIn("launch", proc.stdout)
        self.assertIn("advanced --help", proc.stdout)

    def test_leader_commands_pass_provider_flags_without_argparse_consuming_them(self) -> None:
        with patch("team_agent.runtime.start_leader") as started:
            cli.main(["codex", "--dangerously-bypass-approvals-and-sandbox", "hello"])
        self.assertEqual(started.call_args.args[0], "codex")
        self.assertEqual(started.call_args.args[1], ["--dangerously-bypass-approvals-and-sandbox", "hello"])
        with patch("team_agent.runtime.start_leader") as started:
            cli.main(["claude", "--dangerously-skip-permissions"])
        self.assertEqual(started.call_args.args[0], "claude_code")
        self.assertEqual(started.call_args.args[1], ["--dangerously-skip-permissions"])

    def test_skill_blackbox_lint(self) -> None:
        text = (ROOT / "skills" / "team-agent" / "SKILL.md").read_text(encoding="utf-8")
        required = [
            "cat > .team/current/TEAM.md",
            "cat > .team/current/agents/coder.md",
            "tools:\\n  - fs_read",
            "~/.codex/config.toml",
            "team-agent quick-start .team/current",
            "team-agent restart .",
            "team-agent start-agent",
            "team-agent approvals",
            "team-agent profile show <name> --workspace . --json",
            "session_id",
            "captured_via",
            "restart.fresh_spawn",
            "report_result",
            "AWAITING_APPROVAL",
            "Never read raw provider profile files into model context",
            ".team/runtime/provider-env/*.env",
        ]
        for item in required:
            self.assertIn(item, text)
        self.assertNotIn("team-agent peek", text)
        self.assertNotIn("team-agent peek coder", text)
        self.assertNotIn("provider: codex\nmodel: gpt-5.5\nauth_mode", text.split("cat > .team/current/TEAM.md", 1)[1].split("EOF", 1)[0])

    def test_cli_errors_are_three_part_and_logged(self) -> None:
        import subprocess
        import sys

        with tempfile.TemporaryDirectory(prefix="team-agent-cli-error-") as tmp:
            env = dict(os.environ)
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            env["PYTHONPATH"] = str(ROOT / "src")
            proc = subprocess.run(
                [sys.executable, "-m", "team_agent", "peek", "missing", "--tail", "10", "--workspace", tmp],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("raw worker terminal inspection requires explicit user authorization", proc.stderr)
            self.assertIn("error:", proc.stderr)
            self.assertIn("action:", proc.stderr)
            self.assertIn("log:", proc.stderr)

    def test_quick_start_session_conflict_payload_only_guides_rename(self) -> None:
        args = Mock(command="quick-start")
        payload = cli._cli_error_payload(
            TeamAgentRuntimeError(
                "tmux session already exists: team-same. "
                "Startup will not terminate existing tmux sessions because they may belong to active teams."
            ),
            args,
            Path("/tmp/team-agent-error.log"),
        )
        self.assertEqual(payload["reason"], "tmux_session_name_conflict")
        self.assertEqual(payload["session_name"], "team-same")
        self.assertIn("change `name:` in TEAM.md", payload["action"])
        self.assertEqual(payload["next_actions"], ["Change `name:` in TEAM.md and run `team-agent quick-start` again."])
        self.assertNotIn("team-agent shutdown", payload["action"])

    def test_send_help_shows_canonical_examples(self) -> None:
        import subprocess
        import sys

        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "send", "--help"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('team-agent send --task <task_id> --json "<message>"', proc.stdout)
        self.assertIn('team-agent send --no-ack --json <agent_id> "<message>"', proc.stdout)

    def test_send_option_order_error_hint(self) -> None:
        import subprocess
        import sys

        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "send", "blackbox_tester", "--no-ack", "--json", "message"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("options must appear before target/message", proc.stderr)

    def test_install_script_writes_working_wrappers(self) -> None:
        import os
        import subprocess
        import sys

        with tempfile.TemporaryDirectory(prefix="team-agent-install-") as tmp:
            proc = subprocess.run(
                [sys.executable, "scripts/install.py", "--prefix", tmp],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            bin_dir = Path(tmp) / "bin"
            team_agent = bin_dir / "team-agent"
            orchestrator = bin_dir / "team_orchestrator"
            self.assertTrue(team_agent.exists())
            self.assertTrue(orchestrator.exists())
            env = dict(os.environ)
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            for wrapper in [team_agent, orchestrator]:
                help_proc = subprocess.run(
                    [str(wrapper), "--help"],
                    cwd=ROOT,
                    env=env,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(help_proc.returncode, 0, help_proc.stderr)
                self.assertIn("usage:", help_proc.stdout)

    def test_npx_installer_installs_runtime_wrappers_and_skills(self) -> None:
        import os
        import subprocess
        import sys

        node = shutil.which("node")
        if not node:
            self.skipTest("node not installed")
        with tempfile.TemporaryDirectory(prefix="team-agent-npx-install-") as tmp:
            home = Path(tmp) / "home"
            prefix = Path(tmp) / "prefix"
            runtime_dir = Path(tmp) / "runtime"
            home.mkdir()
            env = dict(os.environ)
            env["HOME"] = str(home)
            env["TEAM_AGENT_PYTHON"] = sys.executable
            proc = subprocess.run(
                [
                    node,
                    "npm/install.mjs",
                    "install",
                    "--prefix",
                    str(prefix),
                    "--runtime-dir",
                    str(runtime_dir),
                ],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            team_agent = prefix / "bin" / "team-agent"
            self.assertTrue(team_agent.exists())
            self.assertTrue((home / ".codex" / "skills" / "team-agent" / "SKILL.md").exists())
            self.assertTrue((home / ".claude" / "skills" / "team-agent" / "SKILL.md").exists())
            help_proc = subprocess.run(
                [str(team_agent), "--help"],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(help_proc.returncode, 0, help_proc.stderr)
            self.assertIn("usage:", help_proc.stdout)
            uninstall = subprocess.run(
                [
                    node,
                    "npm/install.mjs",
                    "uninstall",
                    "--prefix",
                    str(prefix),
                    "--runtime-dir",
                    str(runtime_dir),
                    "--purge-runtime",
                ],
                cwd=ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(uninstall.returncode, 0, uninstall.stderr)
            self.assertFalse(team_agent.exists())
            self.assertFalse((home / ".codex" / "skills" / "team-agent").exists())
            self.assertFalse((home / ".claude" / "skills" / "team-agent").exists())
            self.assertFalse(runtime_dir.exists())

    def test_install_skill_dry_run_json(self) -> None:
        import json as json_module
        import os
        import subprocess
        import sys

        env = dict(os.environ)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(ROOT / "src")
        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "install-skill", "--target", "codex", "--dry-run", "--json"],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json_module.loads(proc.stdout)
        self.assertTrue(result["ok"])
        self.assertTrue(result["dry_run"])
        self.assertTrue(result["source"].endswith("skills/team-agent/SKILL.md"))

    def test_install_skill_all_dry_run_reports_both_targets(self) -> None:
        import subprocess
        import sys

        env = dict(os.environ)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(ROOT / "src")
        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "install-skill", "--target", "all", "--dry-run", "--json"],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json.loads(proc.stdout)
        self.assertTrue(result["ok"])
        destinations = [item["dest"] for item in result["targets"]]
        self.assertTrue(any("/.codex/skills/team-agent/SKILL.md" in dest for dest in destinations))
        self.assertTrue(any("/.claude/skills/team-agent/SKILL.md" in dest for dest in destinations))
        self.assertTrue(all(item["dry_run"] for item in result["targets"]))
        self.assertTrue(all(item["source"].endswith("skills/team-agent/SKILL.md") for item in result["targets"]))

    def test_validate_result_command(self) -> None:
        import json as json_module
        import os
        import subprocess
        import sys

        env = dict(os.environ)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = str(ROOT / "src")
        envelope = _result_envelope("success")
        proc = subprocess.run(
            [sys.executable, "-m", "team_agent", "validate-result", json_module.dumps(envelope), "--json"],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json_module.loads(proc.stdout)
        self.assertTrue(result["ok"])
        self.assertEqual(result["task_id"], "task_impl")


def _fake_spec(workspace: Path) -> dict:
    from team_agent.cli import _fake_spec as cli_fake_spec

    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-agent-test"
    return spec


def _make_fake_tmux_window_run_cmd(started_windows: set[str]):
    def fake_run_cmd(args: list[str], timeout: int = 20):
        proc = Mock(returncode=1 if args[:2] == ["tmux", "has-session"] else 0, stdout="", stderr="")
        if args[:3] == ["tmux", "new-session", "-d"]:
            started_windows.add(args[6])
        elif args[:2] == ["tmux", "new-window"]:
            started_windows.add(args[5])
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            proc.stdout = "\n".join(sorted(started_windows))
        return proc

    return fake_run_cmd


def _write_doc_team(workspace: Path) -> Path:
    team = workspace / ".team" / "current"
    (team / "agents").mkdir(parents=True, exist_ok=True)
    (team / "profiles").mkdir(parents=True, exist_ok=True)
    (team / "TEAM.md").write_text(
        """---
name: doc-team
objective: Compile role docs.
provider: codex
model: gpt-5.5
---

Document-driven team.
""",
        encoding="utf-8",
    )
    (team / "profiles" / "codex-default.example.env").write_text(
        "AUTH_MODE=subscription\nPROFILE_NAME=codex-default\n",
        encoding="utf-8",
    )
    (team / "agents" / "implementer.md").write_text(
        """---
name: implementer
role: Implementation Engineer
provider: codex
model: gpt-5.5
auth_mode: subscription
profile: codex-default
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Implement bounded tasks and report result_envelope_v1.
""",
        encoding="utf-8",
    )
    return team


def _provider_agent(provider: str, agent_id: str) -> dict:
    return {
        "id": agent_id,
        "role": "reviewer",
        "provider": provider,
        "model": None,
        "working_directory": ".",
        "system_prompt": {"inline": "Review the task and report a result envelope.", "file": None},
        "tools": ["fs_read", "git_diff", "mcp_team", "provider_builtin"],
        "permission_mode": "restricted",
        "preferred_for": ["review"],
        "avoid_for": [],
        "output_contract": {"format": "result_envelope_v1", "required_fields": []},
    }


def _write_codex_rollout(root: Path, session_id: str, cwd: Path, timestamp: datetime) -> Path:
    day = root / f"{timestamp:%Y}" / f"{timestamp:%m}" / f"{timestamp:%d}"
    day.mkdir(parents=True, exist_ok=True)
    path = day / f"rollout-{timestamp:%Y-%m-%dT%H-%M-%S}-{session_id}.jsonl"
    payload = {
        "session_meta": {
            "payload": {
                "id": session_id,
                "timestamp": timestamp.isoformat().replace("+00:00", "Z"),
                "cwd": str(cwd),
                "originator": "codex-tui",
            }
        }
    }
    path.write_text(json.dumps(payload) + "\n", encoding="utf-8")
    return path


def _write_claude_transcript(root: Path, cwd: Path, session_id: str, content: str) -> Path:
    path = _claude_transcript_path(root, cwd, session_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        {"type": "permission-mode", "permissionMode": "default", "sessionId": session_id},
        {
            "parentUuid": None,
            "isSidechain": False,
            "type": "user",
            "message": {"role": "user", "content": content},
            "uuid": "message-1",
            "timestamp": "2026-05-16T00:00:00+00:00",
            "cwd": str(cwd),
            "sessionId": session_id,
        },
    ]
    path.write_text("".join(json.dumps(line) + "\n" for line in lines), encoding="utf-8")
    return path


def _claude_transcript_path(root: Path, cwd: Path, session_id: str) -> Path:
    try:
        cwd_text = str(cwd.resolve())
    except OSError:
        cwd_text = str(cwd)
    encoded = "".join(ch if (ch.isascii() and (ch.isalnum() or ch in "._-")) else "-" for ch in cwd_text)
    return root / encoded / f"{session_id}.jsonl"


def _events(workspace: Path) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def _start_fake_leader_pane(workspace: Path) -> tuple[str, str, Path]:
    session = "team-agent-leader-" + workspace.name[-8:]
    capture_path = workspace / "fake-leader-input.log"
    command = f"cat > {shlex.quote(str(capture_path))}"
    proc = runtime.run_cmd(["tmux", "new-session", "-d", "-s", session, "-n", "leader", command], timeout=5)
    if proc.returncode != 0:
        raise AssertionError(proc.stderr)
    pane_proc = runtime.run_cmd(
        ["tmux", "display-message", "-p", "-t", f"{session}:leader", "-F", "#{pane_id}"],
        timeout=5,
    )
    if pane_proc.returncode != 0:
        runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)
        raise AssertionError(pane_proc.stderr)
    return session, pane_proc.stdout.strip(), capture_path


def _wait_for_file_line(path: Path, needle: str, timeout_sec: float = 5.0) -> str:
    import time

    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        if path.exists():
            for line in path.read_text(encoding="utf-8").splitlines():
                if needle in line:
                    return line
        time.sleep(0.05)
    raise AssertionError(f"{needle!r} not found in {path}")


def _valid_envelope(status: str) -> dict:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": "t1",
        "agent_id": "a1",
        "status": status,
        "summary": status,
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


def _result_envelope(status: str) -> dict:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": "task_impl",
        "agent_id": "fake_impl",
        "status": status,
        "summary": status,
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


if __name__ == "__main__":
    unittest.main(verbosity=2)
