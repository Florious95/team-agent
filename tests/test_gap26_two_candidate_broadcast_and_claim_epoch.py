from __future__ import annotations

import json
from unittest.mock import patch

from team_agent.events import EventLog
from team_agent.messaging.leader import claim_leader_receiver
from team_agent.state import load_runtime_state, save_runtime_state


UUID_A = "a" * 32


def test_gap26_two_candidate_broadcast_and_claim_epoch(tmp_path) -> None:
    state = _state()
    save_runtime_state(tmp_path, state)
    event_log = EventLog(tmp_path)
    candidates = [_target("%D"), _target("%E")]
    injected: list[str] = []

    with (
        patch("team_agent.messaging.leader_panes.core_list_targets", return_value={"ok": True, "targets": candidates}),
        patch("team_agent.runtime._tmux_inject_text", side_effect=lambda target, *_args, **_kwargs: injected.append(target) or {"ok": True}),
    ):
        first = _rediscover(state["leader_receiver"], event_log, state["team_owner"])
        second = _rediscover(state["leader_receiver"], event_log, state["team_owner"])

    assert first["status"] == "ambiguous"
    assert first["deduped"] is False
    assert second["status"] == "ambiguous"
    assert second["deduped"] is True
    assert injected == ["%D", "%E"]
    events = _events(tmp_path)
    incidents = [event for event in events if event.get("event") == "leader_receiver.ambiguous_candidates"]
    assert len(incidents) == 1
    queued = [event for event in events if event.get("event") == "leader_receiver.ambiguous_candidate_queued"]
    assert [event["pane_id"] for event in queued] == ["%D", "%E"]

    claimed = claim_leader_receiver(tmp_path, state, candidates[0], event_log, confirm=True, expected_epoch=7)
    assert claimed["ok"] is True
    assert claimed["status"] == "claimed"
    assert claimed["leader_receiver"]["pane_id"] == "%D"
    assert claimed["owner_epoch"] == 8

    after = load_runtime_state(tmp_path)
    refused = claim_leader_receiver(tmp_path, after, candidates[1], event_log, confirm=True, expected_epoch=7)
    assert refused["ok"] is False
    assert refused["reason"] == "owner_epoch_advanced"
    assert refused["bound_pane_id"] == "%D"
    assert refused["owner_epoch"] == 8


def _rediscover(receiver, event_log, owner):
    from team_agent.messaging.leader_panes import _rediscover_leader_receiver

    return _rediscover_leader_receiver(
        receiver,
        event_log,
        owner,
        invalidation_reason="leader_pane_missing",
        team_id="team-a",
    )


def _state() -> dict:
    return {
        "session_name": "team-a",
        "team_owner": {
            "pane_id": "%old",
            "provider": "codex",
            "machine_fingerprint": "machine-a",
            "leader_session_uuid": UUID_A,
            "owner_epoch": 7,
        },
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%old",
            "leader_session_uuid": UUID_A,
            "owner_epoch": 7,
        },
        "agents": {},
        "tasks": [],
    }


def _target(pane_id: str) -> dict[str, object]:
    return {
        "pane_id": pane_id,
        "session_name": "leaders",
        "window_index": "1",
        "window_name": pane_id.strip("%"),
        "pane_index": "0",
        "pane_tty": f"/dev/ttys{pane_id.strip('%')}",
        "pane_current_command": "codex",
        "pane_active": "1",
        "fingerprint": f"leaders|1|0|{pane_id}",
        "leader_env": {"TEAM_AGENT_LEADER_SESSION_UUID": UUID_A},
    }


def _events(workspace) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
