from __future__ import annotations

from team_agent.messaging.deps import (
    DELIVERY_CAPTURE_LINES,
    PASTED_CONTENT_PROMPT_RE,
    TMUX_SUBMIT_MIN_SETTLE_TIMEOUT,
    re,
    run_cmd,
    time,
)

from pathlib import Path
from typing import Any


_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def detect_non_input_scrollback(capture_tail: str) -> str | None:
    nonempty = _non_input_scrollback_lines(capture_tail)
    tail_text = "\n".join(nonempty)
    lower = tail_text.lower()
    stale_before_input = _stale_non_input_before_ready_prompt(nonempty)
    if re.search(r"do\s+you\s+trust\s+the\s+contents\s+of\s+this\s+directory", lower):
        if stale_before_input:
            return None
        return "codex_trust_prompt"
    if "press enter to log in" in lower or "press enter to login" in lower:
        if stale_before_input:
            return None
        return "codex_first_run_auth"
    if "capability may degrade" in lower:
        if stale_before_input:
            return None
        return "codex_compaction_warning"
    if re.search(r"press\s+(enter|return)\s+to\s+continue", lower):
        if stale_before_input:
            return None
        return "generic_press_enter"
    if re.search(r"press\s+any\s+key", lower):
        if stale_before_input:
            return None
        return "generic_press_enter"
    if re.search(r"(\(y/n\)|\([yY]/n\)|\[y/N\]|\[Y/n\]|\[y/n\])", tail_text):
        if stale_before_input:
            return None
        return "y_n_confirm"
    for first, second in zip(nonempty, nonempty[1:]):
        if _starts_numbered_choice(first, "1") and _starts_numbered_choice(second, "2"):
            if stale_before_input:
                return None
            return "numbered_menu"
    if nonempty:
        last = nonempty[-1]
        if re.search(r"(^|[\s~/.\w-])[$%]\s*$", last):
            return "shell_prompt_cli_dead"
    return None


def non_input_scrollback_window(capture_tail: str, limit: int = 15) -> str:
    return "\n".join(_non_input_scrollback_lines(capture_tail, limit=limit))


def _non_input_scrollback_lines(capture_tail: str, limit: int = 15) -> list[str]:
    lines = [_ANSI_ESCAPE_RE.sub("", line).rstrip() for line in capture_tail.splitlines()]
    while lines and not lines[-1].strip():
        lines.pop()
    return [line for line in lines if line.strip()][-limit:]


def _starts_numbered_choice(line: str, number: str) -> bool:
    return bool(re.match(rf"^\s*(?:[›❯>]\s*)?{number}\.\s+", line))


def _stale_non_input_before_ready_prompt(lines: list[str]) -> bool:
    latest_non_input = -1
    latest_ready = -1
    for index, line in enumerate(lines):
        lower = line.lower()
        if (
            "do you trust the contents of this directory" in lower
            or re.search(r"press\s+(enter|return)\s+to\s+continue", lower)
            or re.search(r"press\s+any\s+key", lower)
            or _starts_numbered_choice(line, "1")
            or _starts_numbered_choice(line, "2")
        ):
            latest_non_input = index
        if _is_input_ready_prompt(line):
            latest_ready = index
    return latest_non_input >= 0 and latest_ready > latest_non_input


def _is_input_ready_prompt(line: str) -> bool:
    if _starts_numbered_choice(line, "1") or _starts_numbered_choice(line, "2"):
        return False
    value = line.strip()
    if re.match(r"^[›❯>]\s+\S", value):
        return True
    return bool(re.search(r"\b(codex|claude)\s*[>›❯]\s*$", value, re.IGNORECASE))


def _enable_codex_fast_mode(session_name: str, window_name: str) -> dict[str, Any]:
    target = f"{session_name}:{window_name}"
    proc = run_cmd(["tmux", "send-keys", "-t", target, "/fast", "Enter"], timeout=10)
    if proc.returncode != 0:
        return {"ok": False, "error": proc.stderr.strip() or "tmux send-keys failed"}
    return {"ok": True, "target": target}


def _wait_for_visible_token(target: str, token: str, timeout: float) -> tuple[bool, str]:
    deadline = time.monotonic() + max(timeout, 0.0)
    last = "not_checked"
    while True:
        capture = _capture_tmux_pane_text(target)
        if capture["ok"]:
            if token in capture["capture"] or f"[team-agent-token:{token}]" in capture["capture"]:
                return True, "capture_contains_token"
            last = "capture_missing_token"
        else:
            last = f"capture_failed: {capture.get('error')}"
        if time.monotonic() >= deadline:
            return False, last
        time.sleep(0.1)


def _capture_tmux_pane_text(target: str) -> dict[str, Any]:
    capture = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{DELIVERY_CAPTURE_LINES}", "-t", target], timeout=5)
    if capture.returncode != 0:
        return {"ok": False, "capture": "", "error": capture.stderr.strip() or "tmux capture-pane failed"}
    return {"ok": True, "capture": capture.stdout}


def _wait_for_message_ready(
    target: str,
    message_id: str,
    timeout: float,
    expected_text: str = "",
    allow_pasted_prompt: bool = True,
    baseline_capture: str = "",
) -> tuple[bool, str, str]:
    deadline = time.monotonic() + max(timeout, 0.0)
    last = "not_checked"
    last_capture = ""
    baseline_had_pasted_prompt = _capture_has_pasted_content_prompt(baseline_capture)
    while True:
        capture = _capture_tmux_pane_text(target)
        if capture["ok"]:
            capture_text = capture["capture"]
            last_capture = capture_text
            token_visible = (
                message_id in capture_text
                or f"[team-agent-token:{message_id}]" in capture_text
            )
            fragment_visible = bool(expected_text) and _capture_contains_message_fragment(
                capture_text, expected_text
            )
            queued_only = _capture_brief_only_in_codex_queued_region(
                capture_text, message_id, expected_text
            )
            if queued_only:
                last = "capture_token_in_codex_queued_message_block"
            elif token_visible:
                return True, "capture_contains_token", capture_text
            elif fragment_visible:
                return True, "capture_contains_message_fragment", capture_text
            elif allow_pasted_prompt and _capture_has_pasted_content_prompt(capture_text) and not baseline_had_pasted_prompt:
                return True, "capture_contains_new_pasted_content_prompt", capture_text
            else:
                last = "capture_missing_token"
        else:
            last = f"capture_failed: {capture.get('error')}"
        if time.monotonic() >= deadline:
            return False, last, last_capture
        time.sleep(0.1)


CODEX_QUEUED_MESSAGE_HEADER = "Messages to be submitted after next tool call"


def _capture_codex_queued_message_region(capture_text: str) -> str:
    """Return the contiguous Codex "Messages to be submitted after next tool
    call" block, or "" if the indicator is not present. Codex renders this UI
    when a paste lands while the model is mid-turn — text inside is queued,
    not part of any active model turn."""
    if CODEX_QUEUED_MESSAGE_HEADER not in capture_text:
        return ""
    lines = capture_text.splitlines()
    region: list[str] = []
    inside = False
    for line in lines:
        if not inside:
            if CODEX_QUEUED_MESSAGE_HEADER in line:
                inside = True
                region.append(line)
            continue
        if re.match(r"^\s*[›❯]\s*\S", line):
            break
        region.append(line)
    return "\n".join(region)


def _capture_brief_only_in_codex_queued_region(
    capture_text: str, token: str, expected_text: str = ""
) -> bool:
    """True iff the brief (token or strong message fragment) appears only inside
    Codex's queued-message region and nowhere in an active turn marker window.
    Used to refuse declaring a brief delivered when Codex has queued it instead
    of starting a new model turn (Gap 43 stray-trust-answer scenario)."""
    region = _capture_codex_queued_message_region(capture_text)
    if not region:
        return False
    token_in_region = bool(token) and (
        token in region or f"[team-agent-token:{token}]" in region
    )
    fragment_in_region = bool(expected_text) and _capture_contains_message_fragment(
        region, expected_text
    )
    if not (token_in_region or fragment_in_region):
        return False
    return not _capture_brief_in_active_turn_marker(capture_text, token, expected_text)


def _capture_brief_in_active_turn_marker(
    capture_text: str, token: str, expected_text: str = ""
) -> bool:
    """True iff the brief content appears in a `›`/`❯`-marker active-turn
    window that is not a stray trust-choice keystroke and is outside any
    Codex queued-message region."""
    lines = capture_text.splitlines()
    for index, line in enumerate(lines):
        if not re.match(r"^\s*[❯›>]\s*", line):
            continue
        payload = re.sub(r"^\s*[❯›>]\s*", "", line).strip()
        if payload in {"1", "2"}:
            continue
        window_lines = lines[index : index + 12]
        window = "\n".join(window_lines)
        if CODEX_QUEUED_MESSAGE_HEADER in window:
            window = window.split(CODEX_QUEUED_MESSAGE_HEADER, 1)[0]
        if token and (token in window or f"[team-agent-token:{token}]" in window):
            return True
        if expected_text and _capture_contains_message_fragment(window, expected_text):
            return True
    return False


def _wait_for_worker_message_ready(target: str, message_id: str, timeout: float, expected_text: str = "") -> tuple[bool, str, str]:
    return _wait_for_message_ready(target, message_id, timeout, expected_text=expected_text)


def _capture_has_pasted_content_prompt(text: str) -> bool:
    lines = [line.rstrip() for line in text.splitlines() if line.strip()]
    if not lines:
        return False
    tail = [line.strip() for line in lines[-12:]]
    tail_text = " ".join(tail)
    if not PASTED_CONTENT_PROMPT_RE.search(tail_text):
        return False
    prompt_markers = ("›", "❯", ">")
    if PASTED_CONTENT_PROMPT_RE.search(tail[-1]):
        return True
    if tail[-1].endswith(("chars]", "line]", "lines]")):
        return True
    if any(line.startswith(prompt_markers) for line in tail):
        return True
    if re.search(r"\b(codex|claude)\s*[>›❯]", tail_text, re.IGNORECASE):
        return True
    return False


def _capture_contains_message_fragment(capture_text: str, expected_text: str) -> bool:
    haystack = _compact_visible_text(capture_text)
    if not haystack:
        return False
    fragments = _message_fragment_candidates(expected_text)
    if not fragments:
        return False
    return any(fragment in haystack for fragment in fragments)


def _message_fragment_candidates(text: str) -> list[str]:
    sanitized = re.sub(r"\[team-agent-token:[^\]]+\]", "", text)
    fragments: list[str] = []
    for line in _message_content_lines(sanitized):
        compact = _compact_visible_text(line)
        if not _is_strong_message_fragment(compact):
            continue
        if len(compact) <= 72:
            fragments.append(compact)
            continue
        midpoint = len(compact) // 2
        fragments.extend(
            [
                compact[:36],
                compact[max(0, midpoint - 18) : midpoint + 18],
                compact[-36:],
            ]
        )
    unique: list[str] = []
    seen: set[str] = set()
    for fragment in fragments:
        if fragment in seen:
            continue
        seen.add(fragment)
        unique.append(fragment)
    return unique


def _message_content_lines(text: str) -> list[str]:
    lines = text.splitlines()
    if lines and lines[0].strip().startswith("Team Agent message from "):
        lines = lines[1:]
    return [line for line in lines if line.strip()]


def _is_strong_message_fragment(compact: str) -> bool:
    if not compact:
        return False
    generic_prefixes = (
        "TeamAgentmessagefrom",
        "TeamAgentpeermessagefrom",
        "TeamAgentstoredthisresult",
        "TeamAgenthascollectedthisresult",
        "Nomanualpolling",
    )
    if compact.startswith(generic_prefixes):
        return False
    if re.fullmatch(r"[-:：>›❯]+", compact):
        return False
    if re.search(r"(msg|res)_[0-9A-Fa-f]{8,}", compact):
        return True
    cjk_count = len(re.findall(r"[\u4e00-\u9fff]", compact))
    if cjk_count >= 4 and len(compact) >= 6:
        return True
    return len(compact) >= 18


def _compact_visible_text(text: str) -> str:
    return re.sub(r"\s+", "", text)


def _submit_worker_prompt(
    target: str,
    before_capture: str,
    submit_key: str = "Enter",
    attempts: int = 3,
    settle_timeout: float = TMUX_SUBMIT_MIN_SETTLE_TIMEOUT,
) -> dict[str, Any]:
    verify_pasted_prompt = _capture_has_pasted_content_prompt(before_capture)
    attempt_log: list[dict[str, Any]] = []
    for attempt in range(1, max(attempts, 1) + 1):
        proc = run_cmd(["tmux", "send-keys", "-t", target, submit_key], timeout=10)
        if proc.returncode != 0:
            return {
                "ok": False,
                "stage": "send-keys",
                "verification": "send_keys_failed",
                "error": proc.stderr.strip(),
                "attempts": attempt_log,
            }
        if not verify_pasted_prompt:
            return {
                "ok": True,
                "stage": "submitted",
                "verification": "enter_sent_without_placeholder_check",
                "attempts": attempt_log + [{"attempt": attempt, "submitted": True, "verification": "not_required"}],
            }
        cleared, verification = _wait_for_pasted_prompt_cleared(target, settle_timeout)
        attempt_log.append({"attempt": attempt, "submitted": True, "verification": verification})
        if cleared:
            return {
                "ok": True,
                "stage": "submitted",
                "verification": "pasted_content_prompt_absent_after_submit",
                "attempts": attempt_log,
            }
    return {
        "ok": False,
        "stage": "submit-verification",
        "verification": "pasted_content_prompt_still_present_after_retries",
        "error": "pasted content prompt still present after Enter retries",
        "attempts": attempt_log,
    }


def _wait_for_pasted_prompt_cleared(target: str, timeout: float) -> tuple[bool, str]:
    polls = max(1, int(max(timeout, 0.0) / 0.1) + 1)
    last = "pasted_content_prompt_still_present"
    for poll in range(polls):
        capture = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{DELIVERY_CAPTURE_LINES}", "-t", target], timeout=5)
        if capture.returncode != 0:
            last = "capture_failed"
        elif not _capture_has_pasted_content_prompt(capture.stdout):
            return True, "pasted_content_prompt_absent"
        else:
            last = "pasted_content_prompt_still_present"
        if poll < polls - 1:
            time.sleep(0.1)
    return False, last
