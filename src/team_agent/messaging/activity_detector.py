from __future__ import annotations

from pathlib import Path
import re
from typing import Any

from team_agent.events import EventLog
from team_agent.messaging.deps import datetime, save_runtime_state, team_state_key, timezone

_COMPACTION_RESET_THRESHOLD_DEFAULT = 3


def _compaction_reset_threshold(state: dict[str, Any]) -> int:
    from team_agent.messaging.deps import load_spec
    spec_path = state.get("spec_path")
    if spec_path:
        try:
            spec = load_spec(spec_path)
        except Exception:
            spec = {}
        runtime_cfg = spec.get("runtime", {}) if isinstance(spec, dict) else {}
        raw = runtime_cfg.get("compaction_reset_threshold")
        if raw is not None:
            try:
                value = int(raw)
                if value > 0:
                    return value
            except (TypeError, ValueError):
                pass
    return _COMPACTION_RESET_THRESHOLD_DEFAULT
_PROVIDER_COMMANDS = {"claude", "claude-code", "codex", "node", "node.exe", "claude.exe"}
_COMPACTION_PATTERNS = (
    re.compile(r"context compacted", re.IGNORECASE),
    re.compile(r"compaction occurred", re.IGNORECASE),
)
_IDLE_PROMPT_PATTERNS = (
    re.compile(r"›\s*Find and fix a bug in @filename"),
    re.compile(r"─\s*for agents"),
)
_WORKING_PATTERNS = (
    re.compile(r"\bWorking(?:\s*\((?P<working_seconds>\d+)s\))?", re.IGNORECASE),
    re.compile(r"\bReticulating\b", re.IGNORECASE),
    re.compile(r"\bBaked for (?P<baked_seconds>\d+)s\b", re.IGNORECASE),
    re.compile(r"[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]"),
)


def classify_agent_activity(
    agent_id: str,
    provider: str,
    last_output_at: str | None,
    pane: dict[str, Any] | None,
    scrollback: str,
    *,
    now: datetime | None = None,
    stuck_timeout_sec: int = 300,
) -> dict[str, Any]:
    _ = agent_id, provider
    now = now or datetime.now(timezone.utc)
    pane = pane or {}
    pane_in_mode = str(pane.get("pane_in_mode") or "0")
    if pane_in_mode != "0":
        return {"status": "uncertain", "confidence": 0.9, "rationale": f"pane_in_mode={pane_in_mode}"}
    command = str(pane.get("pane_current_command") or "").split("/")[-1]
    if command and command not in _PROVIDER_COMMANDS:
        return {"status": "uncertain", "confidence": 0.75, "rationale": f"unexpected pane current_command={command}"}
    working = _latest_working_match(scrollback)
    if working:
        label, elapsed = working
        if elapsed is not None and elapsed >= stuck_timeout_sec:
            return {"status": "stuck", "confidence": 0.85, "rationale": f"stale {label} indicator for {elapsed}s"}
        return {"status": "working", "confidence": 0.9, "rationale": f"{label} indicator in scrollback"}
    if any(pattern.search(scrollback) for pattern in _IDLE_PROMPT_PATTERNS):
        return {"status": "idle", "confidence": 0.9, "rationale": "provider idle prompt observed"}
    age = _last_output_age_seconds(last_output_at, now)
    if age is not None and age >= stuck_timeout_sec:
        return {"status": "stuck", "confidence": 0.85, "rationale": "last_output_at exceeded timeout with no idle prompt"}
    if age is not None and age <= 120 and (not command or command in _PROVIDER_COMMANDS):
        return {"status": "working", "confidence": 0.7, "rationale": "recent output from provider command"}
    return {"status": "uncertain", "confidence": 0.5, "rationale": "no decisive prompt or working signal"}


def detect_compaction_degradation(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    *,
    agent_id: str,
    provider: str,
    scrollback: str,
    stuck_loop: bool = False,
) -> dict[str, Any]:
    count = _count_compaction_markers(scrollback)
    owner_team_id = team_state_key(state)
    team_counts = state.setdefault("coordinator", {}).setdefault("compaction_counts", {}).setdefault(owner_team_id, {})
    current = max(int(team_counts.get(agent_id) or 0), count)
    team_counts[agent_id] = current
    save_runtime_state(workspace, state)
    if current <= 0:
        return {"ok": True, "event": "compaction_threshold_crossed.none", "compaction_count": current}
    event_log.write(
        "coordinator.compaction_observed",
        agent_id=agent_id,
        provider=provider,
        team=owner_team_id,
        compaction_count=current,
        stuck_loop=stuck_loop,
    )
    if provider != "codex":
        event = "compaction_threshold_crossed.ignored_lossless_provider"
        event_log.write(event, agent_id=agent_id, provider=provider, team=owner_team_id, compaction_count=current)
        return {"ok": True, "event": event, "agent_id": agent_id, "provider": provider, "compaction_count": current}
    threshold = _compaction_reset_threshold(state)
    if current < threshold and not (current >= 1 and stuck_loop):
        return {"ok": True, "event": "compaction_threshold_crossed.below_threshold", "agent_id": agent_id, "compaction_count": current, "threshold": threshold}
    return _reset_or_recommend(workspace, state, event_log, agent_id, provider, owner_team_id, current, threshold)


def _reset_or_recommend(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    agent_id: str,
    provider: str,
    owner_team_id: str,
    compaction_count: int,
    threshold: int,
) -> dict[str, Any]:
    from team_agent.runtime import reset_agent
    reset = reset_agent(workspace, agent_id, discard_session=True)
    if reset.get("ok"):
        team_counts = state.setdefault("coordinator", {}).setdefault("compaction_counts", {}).setdefault(owner_team_id, {})
        team_counts[agent_id] = 0
        save_runtime_state(workspace, state)
        event = "compaction_threshold_crossed.auto_reset"
        event_log.write(event, agent_id=agent_id, provider=provider, team=owner_team_id, compaction_count=compaction_count, threshold=threshold)
        return {"ok": True, "event": event, "agent_id": agent_id, "compaction_count": compaction_count, "threshold": threshold, "reset": reset}
    event = "compaction_threshold_crossed.recommend_reset"
    message = f"agent {agent_id} crossed Codex compaction threshold; run team-agent reset-agent {agent_id} --discard-session"
    event_log.write(
        event,
        agent_id=agent_id,
        provider=provider,
        team=owner_team_id,
        compaction_count=compaction_count,
        threshold=threshold,
        leader_visible_message=message,
        reset_error=reset.get("error") or reset.get("reason"),
    )
    return {"ok": True, "event": event, "agent_id": agent_id, "compaction_count": compaction_count, "threshold": threshold, "leader_visible_message": message, "reset": reset}


def _latest_working_match(scrollback: str) -> tuple[str, int | None] | None:
    best: tuple[int, str, int | None] | None = None
    for pattern in _WORKING_PATTERNS:
        for match in pattern.finditer(scrollback):
            elapsed_raw = match.groupdict().get("working_seconds") or match.groupdict().get("baked_seconds")
            elapsed = int(elapsed_raw) if elapsed_raw else None
            if best is None or match.start() > best[0]:
                best = (match.start(), match.group(0), elapsed)
    return None if best is None else (best[1], best[2])


def _last_output_age_seconds(last_output_at: str | None, now: datetime) -> float | None:
    if not last_output_at:
        return None
    try:
        last = datetime.fromisoformat(last_output_at)
    except ValueError:
        return None
    if last.tzinfo is None:
        last = last.replace(tzinfo=timezone.utc)
    return max(0.0, (now - last).total_seconds())


def _count_compaction_markers(scrollback: str) -> int:
    return sum(len(pattern.findall(scrollback)) for pattern in _COMPACTION_PATTERNS)
