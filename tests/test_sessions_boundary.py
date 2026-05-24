from __future__ import annotations

import inspect
import unittest

from team_agent import runtime, sessions


class SessionsBoundaryTests(unittest.TestCase):
    """Explicit boundary between runtime.py and the new sessions/ package."""

    def test_runtime_aliases_resolve_to_sessions_module(self) -> None:
        self.assertIs(runtime._capture_missing_sessions, sessions.capture_missing_sessions)
        self.assertIs(runtime._capture_agent_session, sessions.capture_agent_session)
        self.assertIs(runtime._copy_session_metadata, sessions.copy_session_metadata)
        self.assertIs(runtime._clear_session_capture_fields, sessions.clear_session_capture_fields)
        self.assertIs(runtime._attach_profile_resume_root, sessions.attach_profile_resume_root)
        self.assertIs(runtime._prepare_resume_state, sessions.prepare_resume_state)
        self.assertIs(runtime._recover_resume_session_from_events, sessions.recover_resume_session_from_events)
        self.assertIs(runtime.sessions, sessions.sessions_overview)

    def test_session_helpers_keep_explicit_signatures(self) -> None:
        # Spark guidance forbids cross-module *args/**kwargs wrappers; ensure
        # every public sessions/ helper exposes a typed signature.
        for fn in (
            sessions.capture_missing_sessions,
            sessions.capture_agent_session,
            sessions.copy_session_metadata,
            sessions.clear_session_capture_fields,
            sessions.attach_profile_resume_root,
            sessions.prepare_resume_state,
            sessions.recover_resume_session_from_events,
            sessions.sessions_overview,
        ):
            sig = inspect.signature(fn)
            kinds = {param.kind for param in sig.parameters.values()}
            self.assertNotIn(inspect.Parameter.VAR_POSITIONAL, kinds, f"{fn.__name__} uses *args")
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, kinds, f"{fn.__name__} uses **kwargs")

    def test_sessions_module_does_not_import_runtime(self) -> None:
        # The package must not pull team_agent.runtime into the import graph
        # (would create a circular dependency once messaging/lifecycle pull
        # session helpers via runtime aliases).
        for module_name in (
            "team_agent.sessions.capture",
            "team_agent.sessions.resume",
            "team_agent.sessions.inventory",
            "team_agent.sessions",
        ):
            module = __import__(module_name, fromlist=["__file__"])
            source = inspect.getsource(module)
            self.assertNotIn("team_agent.runtime", source, f"{module_name} imports runtime")


class CopySessionMetadataTests(unittest.TestCase):
    def test_copy_only_known_fields_filters_extra_keys(self) -> None:
        full_source = {
            "session_id": "sess-1",
            "rollout_path": "/tmp/rollout.jsonl",
            "captured_at": "2026-05-24T01:02:03+00:00",
            "captured_via": "fs_watch",
            "attribution_confidence": "high",
            "spawn_cwd": "/work",
            "_pending_session_id": "stale",
            "garbage_key": "should not appear",
        }
        target: dict = {}
        sessions.copy_session_metadata(target, full_source)
        # known fields should be present; unrelated keys must not be copied
        self.assertEqual(target.get("session_id"), "sess-1")
        self.assertEqual(target.get("rollout_path"), "/tmp/rollout.jsonl")
        self.assertNotIn("garbage_key", target)
        self.assertNotIn("_pending_session_id", target)


class ClearSessionCaptureFieldsTests(unittest.TestCase):
    def test_clear_nulls_capture_fields_only(self) -> None:
        target = {
            "session_id": "sess-x",
            "rollout_path": "/tmp/x.jsonl",
            "captured_at": "2026-05-24T01:02:03+00:00",
            "captured_via": "fs_watch",
            "attribution_confidence": "high",
            "spawn_cwd": "/work",
            "provider": "claude",
            "status": "running",
        }
        sessions.clear_session_capture_fields(target)
        # All capture-tracked fields should be None now.
        self.assertIsNone(target["session_id"])
        self.assertIsNone(target["rollout_path"])
        self.assertIsNone(target["captured_at"])
        # Non-capture fields like provider and status must remain untouched.
        self.assertEqual(target["provider"], "claude")
        self.assertEqual(target["status"], "running")


class SessionsOverviewTests(unittest.TestCase):
    def test_empty_workspace_returns_empty_sessions_list(self) -> None:
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory(prefix="team-agent-sessions-empty-") as tmp:
            workspace = Path(tmp)
            result = sessions.sessions_overview(workspace)
            self.assertTrue(result["ok"])
            self.assertEqual(result["sessions"], [])
            self.assertEqual(result["workspace"], str(workspace))


if __name__ == "__main__":
    unittest.main()
