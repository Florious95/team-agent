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

class RuntimeTests06(unittest.TestCase):
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
                with (
                    patch("team_agent.runtime.shutil_which", return_value="/usr/bin/tmux"),
                    patch("team_agent.runtime._model_checks_for_agents", return_value=[]),
                    self.assertRaises(runtime.RuntimeError) as ctx,
                ):
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
