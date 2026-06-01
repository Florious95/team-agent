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
    re.compile(r"^›[^\n]*\n(?:\s*\n){0,8}\s*gpt-[\w.-]+\s+\S+\s+·", re.MULTILINE),
    # Codex idle input prompt line (rotating hints like
    # "› Use /skills to list available skills"). Working lines start with a
    # spinner/✱ glyph, not "›". An optional leading "│ " tolerates a boxed
    # input frame.
    re.compile(r"^(?:│\s*)?›\s", re.MULTILINE),
    # Claude Code idle input prompt: an empty "❯" line (the box may render the
    # trailing space as U+00A0). Only the empty prompt is idle; a "❯ <command>"
    # line is a submitted turn, so the trailing-content form is deliberately
    # excluded to avoid false IDLE while Claude is still working.
    re.compile(r"^(?:│\s*)?❯[ \t\xa0]*$", re.MULTILINE),
)
# Substantive working indicators carry their own text ("Working", "Thinking",
# "esc to interrupt", ...). The bare spinner glyph alone is only a pane-refresh
# artifact, so it is kept separate: it still counts as working when nothing else
# is present, but it must not override a fresh idle prompt (C14).
_SUBSTANTIVE_WORKING_PATTERNS = (
    re.compile(r"\bWorking(?:\s*\((?P<working_seconds>\d+)s\))?", re.IGNORECASE),
    re.compile(r"\bReticulating\b", re.IGNORECASE),
    re.compile(r"\bBaked for (?P<baked_seconds>\d+)s\b", re.IGNORECASE),
    re.compile(r"\bThinking\b", re.IGNORECASE),
    re.compile(r"esc to interrupt", re.IGNORECASE),
)
_SPINNER_GLYPH_PATTERN = re.compile(r"[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]")
_WORKING_PATTERNS = _SUBSTANTIVE_WORKING_PATTERNS + (_SPINNER_GLYPH_PATTERN,)
# A live provider working footer is a bullet status line carrying a live
# elapsed-time counter plus the "esc to interrupt" hint, e.g.
#   "• Working (35s • esc to interrupt) · 1 background terminal running"
#   "• Waiting for background terminal (1m 06s • esc to interrupt) · ..."
# This is matched by the COMMON shape, not per verb (Working/Waiting/Baked/...):
# a "•" line with a parenthesized elapsed counter in either "Ns" or "Nm NNs"
# form, followed by "esc to interrupt" inside the same parentheses. That live
# counter + interrupt hint is only rendered during an active interruptible turn
# and is removed when the turn ends, so it never appears in prose/scrollback
# history (unlike a bare "Working" word or an "esc to interrupt" mention). It is
# the positive "provider is working right now" signal that the permanent input
# box ("› ... gpt-" / "❯") rendered below it must not override.
_LIVE_WORKING_PATTERNS = (
    re.compile(r"•\s*[^\n]*?\(\s*(?:\d+m\s*)?\d+s\b[^)\n]*esc to interrupt", re.IGNORECASE),
)


def _latest_live_working_footer(scrollback: str) -> str | None:
    best: tuple[int, str] | None = None
    for pattern in _LIVE_WORKING_PATTERNS:
        for match in pattern.finditer(scrollback):
            if best is None or match.start() > best[0]:
                best = (match.start(), match.group(0))
    return best[1] if best else None


def classify_agent_activity(
    agent_id: str,
    provider: str,
    last_output_at: str | None,
    pane: dict[str, Any] | None,
    scrollback: str,
    *,
    now: datetime | None = None,
    stuck_timeout_sec: int = 300,
    active_task: bool = False,
    pane_delta_recent: bool = False,
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
    substantive = _latest_working_match(scrollback, _SUBSTANTIVE_WORKING_PATTERNS)
    idle_pos = _latest_idle_prompt_position(scrollback)
    # bug-071: a live provider working footer ("Working (Ns ...)") plus an active
    # task is an active turn. The provider input box ("› ... gpt-" / "❯") is
    # permanent UI rendered BELOW the footer, so the position-based idle-prompt
    # check would otherwise flip a working Codex turn to IDLE. Checked before the
    # idle-prompt rule. The seconds-counter form never appears in prose, so a
    # real idle prompt (no live footer) is unaffected (C14); gating on
    # active_task keeps task-less classifier cases on the existing logic.
    live_footer = _latest_live_working_footer(scrollback)
    if active_task and live_footer is not None:
        return {"status": "working", "confidence": 0.9, "rationale": f"live working footer '{live_footer}' with active task"}
    # C14: a fresh idle prompt is the strongest signal. Only a substantive
    # working indicator positioned after the prompt counts as newer work; a
    # trailing bare spinner glyph (pane refresh) or pane delta must not flip a
    # fresh idle prompt to WORKING.
    if idle_pos is not None and (substantive is None or idle_pos > substantive[0]):
        return {"status": "idle", "confidence": 0.9, "rationale": "provider idle prompt is the latest scrollback signal"}
    if working:
        _pos, label, elapsed = working
        if elapsed is not None and elapsed >= stuck_timeout_sec:
            return {"status": "stuck", "confidence": 0.85, "rationale": f"stale {label} indicator for {elapsed}s"}
        return {"status": "working", "confidence": 0.9, "rationale": f"{label} indicator is the latest scrollback signal"}
    # C15: an active task whose pane changed since the last sync is real work,
    # not idle. Placed after the idle-prompt check so a fresh idle prompt always
    # wins; without an active task this rule never fires and raw running may stay
    # IDLE.
    if active_task and pane_delta_recent and (not command or command in _PROVIDER_COMMANDS):
        return {"status": "working", "confidence": 0.9, "rationale": "active task with recent pane delta"}
    age = _last_output_age_seconds(last_output_at, now)
    if age is not None and age >= stuck_timeout_sec:
        return {"status": "stuck", "confidence": 0.85, "rationale": "last_output_at exceeded timeout with no idle prompt"}
    if age is not None and age <= 120 and (not command or command in _PROVIDER_COMMANDS):
        return {"status": "working", "confidence": 0.7, "rationale": "recent output from provider command"}
    return {"status": "uncertain", "confidence": 0.5, "rationale": "no decisive prompt or working signal"}


def _latest_idle_prompt_position(scrollback: str) -> int | None:
    best: int | None = None
    for pattern in _IDLE_PROMPT_PATTERNS:
        for match in pattern.finditer(scrollback):
            if best is None or match.start() > best:
                best = match.start()
    return best


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
    try:
        save_runtime_state(workspace, state)
    except Exception as exc:
        event_log.write("runtime.state.save_failed", phase="compaction_detect", error=str(exc), exc_type=type(exc).__name__)
        return {"ok": False, "event": "compaction_threshold_crossed.unpersisted", "agent_id": agent_id, "compaction_count": current}
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
        try:
            save_runtime_state(workspace, state)
        except Exception as exc:
            event_log.write("runtime.state.save_failed", phase="compaction_detect", error=str(exc), exc_type=type(exc).__name__)
            return {"ok": False, "event": "compaction_threshold_crossed.unpersisted", "agent_id": agent_id, "compaction_count": compaction_count}
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


def _latest_working_match(
    scrollback: str, patterns: tuple[re.Pattern[str], ...] = _WORKING_PATTERNS
) -> tuple[int, str, int | None] | None:
    best: tuple[int, str, int | None] | None = None
    for pattern in patterns:
        for match in pattern.finditer(scrollback):
            elapsed_raw = match.groupdict().get("working_seconds") or match.groupdict().get("baked_seconds")
            elapsed = int(elapsed_raw) if elapsed_raw else None
            if best is None or match.start() > best[0]:
                best = (match.start(), match.group(0), elapsed)
    return best


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
