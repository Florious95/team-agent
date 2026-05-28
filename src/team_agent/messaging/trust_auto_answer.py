from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.messaging.deps import _tmux_inject_text


def retry_injection_after_trust_auto_answer(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    injection: dict[str, Any],
    target: str,
    text: str,
    submit_key: str,
    buffer_name: str,
    provider: str,
) -> dict[str, Any]:
    from team_agent.messaging.delivery import _tmux_pane_width, _wait_for_trust_prompt_dismissal
    from team_agent.messaging.leader_panes import attempt_trust_auto_answer
    pane_target = injection.get("pane_id") or target
    # Live wiring: query tmux pane width now and pass via state["pane_width"]
    # (symmetric with _deliver_pending_message). Fail-safe on query failure —
    # leave pane_width absent so the matcher falls back to exact equality.
    width_query = _tmux_pane_width(pane_target)
    trust_state = dict(state) if isinstance(state, dict) else {}
    if width_query.get("ok"):
        trust_state["pane_width"] = width_query["pane_width"]
    answer = attempt_trust_auto_answer(
        workspace,
        pane_target,
        injection.get("pane_capture_tail") or "",
        event_log,
        state=trust_state,
    )
    if not answer.get("answered"):
        return injection
    if not _wait_for_trust_prompt_dismissal(injection.get("pane_id") or target, timeout=3.0):
        retry_blocked = dict(injection)
        retry_blocked["error"] = "trust_prompt_not_dismissed_after_answer"
        retry_blocked["verification"] = "trust_prompt_not_dismissed_after_answer"
        retry_blocked["stage"] = "trust_auto_answer_dismissal_wait"
        return retry_blocked
    return _tmux_inject_text(
        target,
        text,
        submit_key,
        buffer_name,
        provider=provider,
    )
