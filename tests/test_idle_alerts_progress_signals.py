from __future__ import annotations

import importlib.util
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

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

from team_agent.events import EventLog
from team_agent.messaging import idle_alerts


_TEAM = "team-progress-signals"


def _setup_team(workspace: Path, *, last_output_ago_seconds: float = 1800.0) -> tuple[dict, MessageStore, EventLog]:
    spec = _fake_spec(workspace)
    spec_path = workspace / "team.spec.yaml"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    state = {
        "spec_path": str(spec_path),
        "team_dir": str(workspace / ".team" / _TEAM),
        "session_name": _TEAM,
        "leader": spec["leader"],
        "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%leader"},
        "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
        "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "in_progress"}],
    }
    save_runtime_state(workspace, state)
    store = MessageStore(workspace)
    store.upsert_agent_health(
        "fake_impl", "idle",
        last_output_at=(datetime.now(timezone.utc) - timedelta(seconds=last_output_ago_seconds)).isoformat(),
        owner_team_id=_TEAM,
    )
    return state, store, EventLog(workspace)


class IdleAlertsProgressSignalsTests(unittest.TestCase):
    def test_recent_leader_send_suppresses_idle_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-progress-leader-send-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team(workspace)
            now = datetime.now(timezone.utc)
            # Leader-side send delivered to a worker 5 seconds ago — clear "team is doing work" signal.
            event_log.write("send.deliver_attempt", team=_TEAM, target="fake_impl", message_id="msg_recent")
            delivered: list[dict] = []
            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=lambda *_a, **_k: delivered.append("x") or {"ok": True, "status": "submitted", "message_id": "msg_x"}):
                alerts = idle_alerts.detect_idle_fallbacks(workspace, state, store, event_log, now=now)
            self.assertEqual(alerts, [], "recent send.deliver_attempt must suppress idle_fallback fire")
            self.assertEqual(delivered, [])
            skipped = [e for e in _events(workspace) if e.get("event") == "coordinator.idle_fallback_skipped"]
            self.assertTrue(skipped, "expected coordinator.idle_fallback_skipped event")
            self.assertEqual(skipped[-1].get("reason"), "recent_team_progress")
            self.assertEqual(skipped[-1].get("progress_source"), "event_log")

    def test_recent_worker_mcp_call_suppresses_idle_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-progress-mcp-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team(workspace)
            now = datetime.now(timezone.utc)
            # Worker just called mcp.report_result / mcp.read_* — actively engaged, not truly idle.
            event_log.write("mcp.report_result", team=_TEAM, agent_id="fake_impl", result_id="res_abc")
            event_log.write("mcp.read_state", team=_TEAM, agent_id="fake_impl")
            alerts = idle_alerts.detect_idle_fallbacks(workspace, state, store, event_log, now=now)
            self.assertEqual(alerts, [])
            skipped = [e for e in _events(workspace) if e.get("event") == "coordinator.idle_fallback_skipped"]
            self.assertTrue(skipped)
            self.assertEqual(skipped[-1].get("reason"), "recent_team_progress")
            self.assertEqual(skipped[-1].get("progress_source"), "event_log")

    def test_silence_beyond_threshold_still_fires(self) -> None:
        with tempfile.TemporaryDirectory(prefix="team-agent-progress-silence-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team(workspace)
            # All progress signals predate the stable-idle window AND the event-log scan window
            # by a wide margin. agent_health.last_output_at = 30 min ago (set in _setup_team).
            # No fresh events written. The detector must fire normally.
            now = datetime.now(timezone.utc)
            delivered: list[dict] = []
            def fake_deliver(_workspace, _state, leader_id, content, *_a, **_kw):
                delivered.append({"to": leader_id})
                return {"ok": True, "status": "submitted", "message_id": "msg_alert"}
            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_deliver):
                alerts = idle_alerts.detect_idle_fallbacks(workspace, state, store, event_log, now=now)
            self.assertEqual([a["alert_type"] for a in alerts], ["idle_fallback"])
            self.assertEqual(len(delivered), 1)

    def test_progress_signal_persists_across_coordinator_ticks(self) -> None:
        # Anti-thrash: with a recent event-log signal, multiple sequential coordinator_tick passes
        # must consistently suppress fire (no toggle, no double-injection).
        with tempfile.TemporaryDirectory(prefix="team-agent-progress-no-thrash-") as tmp:
            workspace = Path(tmp)
            state, store, event_log = _setup_team(workspace)
            base_now = datetime.now(timezone.utc)
            event_log.write("send.deliver_attempt", team=_TEAM, target="fake_impl", message_id="msg_anchor")
            delivered: list[dict] = []
            def fake_deliver(_workspace, _state, leader_id, content, *_a, **_kw):
                delivered.append({"to": leader_id})
                return {"ok": True, "status": "submitted", "message_id": f"msg_{len(delivered)}"}
            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_deliver):
                for offset in (0.0, 5.0, 15.0, 30.0, 45.0):
                    alerts = idle_alerts.detect_idle_fallbacks(
                        workspace, state, store, event_log,
                        now=base_now + timedelta(seconds=offset),
                    )
                    self.assertEqual(alerts, [], f"unexpected fire at offset={offset}s")
            self.assertEqual(delivered, [], "no leader delivery across ticks while progress signal is fresh")
            skipped = [e for e in _events(workspace) if e.get("event") == "coordinator.idle_fallback_skipped"]
            self.assertGreaterEqual(len(skipped), 5)
            for evt in skipped[-5:]:
                self.assertEqual(evt.get("reason"), "recent_team_progress")

    def test_multi_team_unscoped_event_does_not_suppress_other_team_idle(self) -> None:
        # Spark MEDIUM #3: in a multi-team workspace, an unscoped progress event must not
        # cross-suppress a sibling team's idle_fallback. Team A activity (or activity without
        # team scope) cannot keep team B silent.
        with tempfile.TemporaryDirectory(prefix="team-agent-progress-multi-") as tmp:
            workspace = Path(tmp)
            spec = _fake_spec(workspace)
            spec_path_alpha = workspace / "team.spec.yaml"
            spec_path_alpha.write_text(dumps(spec), encoding="utf-8")
            beta_dir = workspace / ".team" / "beta"
            beta_dir.mkdir(parents=True, exist_ok=True)
            spec_path_beta = beta_dir / "team.spec.yaml"
            spec_path_beta.write_text(dumps(spec), encoding="utf-8")
            beta_state = {
                "spec_path": str(spec_path_beta),
                "team_dir": str(beta_dir),
                "session_name": "team-beta",
                "leader": spec["leader"],
                "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%beta_leader"},
                "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "in_progress"}],
            }
            workspace_state = {
                "spec_path": str(spec_path_alpha),
                "team_dir": str(workspace / ".team" / "alpha"),
                "session_name": "team-alpha",
                "leader": spec["leader"],
                "leader_receiver": {"mode": "direct_tmux", "status": "attached", "provider": "codex", "pane_id": "%alpha_leader"},
                "agents": {"fake_impl": {"status": "running", "provider": "fake", "window": "fake_impl"}},
                "tasks": [{**spec["tasks"][0], "assignee": "fake_impl", "status": "in_progress"}],
                "teams": {
                    "alpha": {"spec_path": str(spec_path_alpha), "session_name": "team-alpha", "agents": {"fake_impl": {"status": "running"}}},
                    "beta": beta_state,
                },
            }
            save_runtime_state(workspace, workspace_state)
            store = MessageStore(workspace)
            store.upsert_agent_health(
                "fake_impl", "idle",
                last_output_at=(datetime.now(timezone.utc) - timedelta(minutes=30)).isoformat(),
                owner_team_id="beta",
            )
            event_log = EventLog(workspace)
            # Team A activity, unscoped (no team= field): under the pre-fix behavior this
            # would suppress every team's idle including beta's.
            event_log.write("send.deliver_attempt", target="fake_impl", message_id="msg_alpha_only")
            # Also write a team-A-scoped event for realism.
            event_log.write("mcp.report_result", team="alpha", agent_id="fake_impl", result_id="res_alpha")

            delivered: list[dict] = []
            def fake_deliver(_workspace, _state, leader_id, content, *_a, **_kw):
                delivered.append({"to": leader_id})
                return {"ok": True, "status": "submitted", "message_id": "msg_beta_alert"}

            with patch("team_agent.runtime._send_to_leader_receiver", side_effect=fake_deliver):
                alerts = idle_alerts.detect_idle_fallbacks(
                    workspace, beta_state, store, event_log,
                    now=datetime.now(timezone.utc),
                )

            self.assertEqual([a["alert_type"] for a in alerts], ["idle_fallback"],
                "beta team must fire its own idle_fallback; cross-team or unscoped activity must not suppress it")
            self.assertEqual(len(delivered), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
