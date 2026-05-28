from __future__ import annotations

from team_agent.messaging.deps import (
    TMUX_PASTE_BYTES_PER_SECOND,
    TMUX_PASTE_MAX_READY_TIMEOUT,
    TMUX_PASTE_MIN_READY_TIMEOUT,
    TMUX_STDIN_BUFFER_THRESHOLD,
    TMUX_SUBMIT_BYTES_PER_SECOND,
    TMUX_SUBMIT_MAX_SETTLE_TIMEOUT,
    TMUX_SUBMIT_MIN_SETTLE_TIMEOUT,
    _capture_tmux_pane_text,
    _tmux_load_buffer_stdin as _runtime_tmux_load_buffer_stdin,
    _submit_worker_prompt,
    _wait_for_message_ready,
    re,
    run_cmd,
    subprocess,
    time,
)

from pathlib import Path
from typing import Any
from team_agent.messaging.tmux_prompt import detect_non_input_scrollback, non_input_scrollback_window

def _tmux_inject_text(
    target: str,
    text: str,
    submit_key: str,
    buffer_name: str,
    attempts: int = 3,
    provider: str = "fake",
    *,
    bypass_non_input_gate: bool = False,
) -> dict[str, Any]:
    # Round-5 follow-up: empty-text Enter path (used by trust auto-answer to
    # accept Codex's default `1. Yes, continue` choice with a plain Enter).
    # tmux rejects set-buffer / paste-buffer of an empty string, so the
    # buffer-paste route would leave the trust prompt stuck. Issue
    # `send-keys -t <target> <submit_key>` directly and bypass the buffer
    # path entirely.
    if text == "":
        proc = run_cmd(["tmux", "send-keys", "-t", target, submit_key], timeout=10)
        if proc.returncode != 0:
            return {
                "ok": False,
                "stage": "send-keys",
                "error": proc.stderr.strip() or "tmux send-keys failed",
                "attempts": [
                    {
                        "attempt": 1,
                        "submitted": False,
                        "verification": "send_keys_failed",
                        "submit_key": submit_key,
                    }
                ],
                "verification": "send_keys_failed",
            }
        return {
            "ok": True,
            "stage": "submitted",
            "visible": True,
            "submitted": True,
            "verification": "empty_text_send_keys",
            "submit_verification": f"{submit_key}_sent_direct",
            "turn_verification": "not_required",
            "attempts": [
                {
                    "attempt": 1,
                    "submitted": True,
                    "verification": "empty_text_send_keys",
                    "submit_key": submit_key,
                }
            ],
            "submit_attempts": [
                {"attempt": 1, "submitted": True, "verification": "send_keys"}
            ],
        }
    token_match = re.search(r"\[team-agent-token:([^\]]+)\]", text)
    token = token_match.group(1) if token_match else ""
    attempt_log: list[dict[str, Any]] = []
    last_verification = "not_checked"
    ready_timeout = _tmux_paste_ready_timeout(text)
    submit_settle_timeout = _tmux_submit_settle_timeout(text)
    text_bytes = _tmux_text_size(text)
    for attempt in range(1, max(attempts, 1) + 1):
        prepared = (
            {"ok": True, "verification": "non_input_gate_bypassed"}
            if bypass_non_input_gate
            else _prepare_tmux_pane_for_input(target)
        )
        if not prepared["ok"]:
            attempt_log.append(_prepare_failure_attempt(attempt, prepared))
            return {
                "ok": False,
                "status": "failed",
                "stage": prepared["stage"],
                "reason": prepared.get("reason"),
                "error": prepared.get("error"),
                "attempts": attempt_log,
                "verification": prepared["verification"],
                "detected": prepared.get("detected"),
                "pane_id": prepared.get("pane_id"),
                "pane_mode": prepared.get("pane_mode"),
                "pane_capture_tail": prepared.get("pane_capture_tail"),
            }
        baseline = _capture_tmux_pane_text(target)
        if not baseline["ok"]:
            return {
                "ok": False,
                "stage": "pre-paste-capture",
                "error": baseline.get("error"),
                "attempts": attempt_log,
                "verification": "pre_paste_capture_failed",
            }
        baseline_capture = baseline["capture"]
        buffered = _tmux_set_buffer_text(buffer_name, text)
        if not buffered["ok"]:
            return {"ok": False, "stage": buffered["stage"], "error": buffered.get("error"), "attempts": attempt_log}
        proc = run_cmd(["tmux", "paste-buffer", "-t", target, "-b", buffer_name, "-p"], timeout=10)
        deleted = _tmux_delete_buffer(buffer_name)
        if proc.returncode != 0:
            return {
                "ok": False,
                "stage": "paste-buffer",
                "error": proc.stderr.strip(),
                "attempts": attempt_log,
                "buffer_deleted": deleted.get("ok"),
                "buffer_delete_error": deleted.get("error"),
            }
        time.sleep(0.25)
        if token:
            visible, verification, capture_text = _wait_for_message_ready(
                target,
                token,
                ready_timeout,
                expected_text=text,
                baseline_capture=baseline_capture,
            )
        else:
            visible, verification, capture_text = True, "no_token", ""
        last_verification = verification
        attempt_entry = {
            "attempt": attempt,
            "visible": visible,
            "verification": verification,
            "buffer_method": buffered.get("method"),
            "buffer_name": buffer_name,
            "buffer_deleted": deleted.get("ok"),
            "text_bytes": buffered.get("text_bytes"),
            "ready_timeout_sec": ready_timeout,
        }
        if deleted.get("error"):
            attempt_entry["buffer_delete_error"] = deleted.get("error")
        if prepared.get("recovered_from_mode"):
            attempt_entry["recovered_from_mode"] = True
            attempt_entry["recovered_from_pane_mode"] = prepared.get("pane_mode")
        if prepared.get("warning_event"):
            attempt_entry["warning_event"] = prepared["warning_event"]
        attempt_log.append(attempt_entry)
        if not visible:
            time.sleep(0.2)
            continue
        submit = _submit_worker_prompt(
            target,
            capture_text,
            submit_key=submit_key,
            settle_timeout=submit_settle_timeout,
        )
        if not submit["ok"]:
            return {
                "ok": False,
                "stage": submit.get("stage", "submit"),
                "error": submit.get("error"),
                "attempts": attempt_log,
                "verification": verification,
                "submit_verification": submit.get("verification"),
                "submit_attempts": submit.get("attempts"),
            }
        submit_verification = _leader_submit_verification(submit.get("verification"), verification, submit_key)
        # Gap 42: paste+submit success is authoritative for delivery. The post-submit
        # turn-boundary probe is observation metadata only, never a delivery gate — a
        # busy / compacting recipient that has not yet shown a new prompt marker is
        # still a successful delivery. Real paste/submit failures are caught and
        # returned above; this point is only reached after submit reported ok.
        turn_visible, turn_verification, turn_capture = _wait_for_leader_new_turn(
            target,
            text,
            token,
            provider=provider,
            timeout=2.0,
        )
        if not turn_visible:
            turn_verification = "not_yet_observed"
        return {
            "ok": True,
            "stage": "submitted",
            "visible": True,
            "submitted": True,
            "verification": verification,
            "submit_verification": submit_verification,
            "turn_verification": turn_verification,
            "attempts": attempt_log,
            "submit_attempts": submit.get("attempts"),
        }
    return {
        "ok": False,
        "stage": "visible-check",
        "error": f"visible token not found after {max(attempts, 1)} attempts: {last_verification}",
        "attempts": attempt_log,
        "verification": last_verification,
    }


def _leader_submit_verification(submit_verification: str | None, verification: str, submit_key: str) -> str | None:
    if submit_verification != "enter_sent_without_placeholder_check":
        return submit_verification
    if verification == "capture_contains_token":
        return f"{submit_key}_sent_after_visible_token"
    if verification == "capture_contains_message_fragment":
        return f"{submit_key}_sent_after_visible_fragment"
    return submit_verification


def _wait_for_leader_new_turn(
    target: str,
    expected_text: str,
    token: str,
    provider: str,
    timeout: float,
) -> tuple[bool, str, str]:
    deadline = time.monotonic() + max(timeout, 0.0)
    last = "not_checked"
    last_capture = ""
    while True:
        capture = _capture_tmux_pane_text(target)
        if capture["ok"]:
            capture_text = capture["capture"]
            last_capture = capture_text
            if _capture_has_leader_new_turn(capture_text, expected_text, token, provider):
                return True, "leader_new_turn_boundary_verified", capture_text
            last = "leader_new_turn_boundary_missing"
        else:
            last = f"capture_failed: {capture.get('error')}"
        if time.monotonic() >= deadline:
            return False, last, last_capture
        time.sleep(0.1)


def _capture_has_leader_new_turn(capture_text: str, expected_text: str, token: str, provider: str) -> bool:
    if provider == "fake":
        return True
    lines = capture_text.splitlines()
    marker_indexes = [index for index, line in enumerate(lines) if re.match(r"^\s*[❯›>]\s*", line)]
    for index in marker_indexes:
        window = "\n".join(lines[index : index + 12])
        if token and token in window:
            return True
        if _leader_turn_contains_message_fragment(window, expected_text):
            return True
    return False


def _leader_turn_contains_message_fragment(capture_text: str, expected_text: str) -> bool:
    haystack = re.sub(r"\s+", "", capture_text)
    for line in expected_text.splitlines():
        compact = re.sub(r"\s+", "", re.sub(r"\[team-agent-token:[^\]]+\]", "", line))
        if len(compact) >= 18 and compact in haystack:
            return True
    return False


def _tmux_text_size(text: str) -> int:
    return len(text.encode("utf-8"))


def _tmux_paste_ready_timeout(text: str) -> float:
    size = _tmux_text_size(text)
    return min(
        TMUX_PASTE_MAX_READY_TIMEOUT,
        max(TMUX_PASTE_MIN_READY_TIMEOUT, size / TMUX_PASTE_BYTES_PER_SECOND),
    )


def _tmux_submit_settle_timeout(text: str) -> float:
    size = _tmux_text_size(text)
    return min(
        TMUX_SUBMIT_MAX_SETTLE_TIMEOUT,
        max(TMUX_SUBMIT_MIN_SETTLE_TIMEOUT, size / TMUX_SUBMIT_BYTES_PER_SECOND),
    )


def _tmux_set_buffer_text(buffer_name: str, text: str) -> dict[str, Any]:
    size = _tmux_text_size(text)
    if size >= TMUX_STDIN_BUFFER_THRESHOLD:
        proc = _runtime_tmux_load_buffer_stdin(buffer_name, text)
        return {
            "ok": proc.returncode == 0,
            "stage": "load-buffer",
            "method": "stdin_load_buffer",
            "text_bytes": size,
            "error": proc.stderr.strip() if proc.returncode != 0 else None,
        }
    proc = run_cmd(["tmux", "set-buffer", "-b", buffer_name, text], timeout=10)
    return {
        "ok": proc.returncode == 0,
        "stage": "set-buffer",
        "method": "set_buffer_arg",
        "text_bytes": size,
        "error": proc.stderr.strip() if proc.returncode != 0 else None,
    }


def _tmux_delete_buffer(buffer_name: str) -> dict[str, Any]:
    proc = run_cmd(["tmux", "delete-buffer", "-b", buffer_name], timeout=10)
    return {
        "ok": proc.returncode == 0,
        "stage": "delete-buffer",
        "error": proc.stderr.strip() if proc.returncode != 0 else None,
    }


def _tmux_load_buffer_stdin(buffer_name: str, text: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["tmux", "load-buffer", "-b", buffer_name, "-"],
        input=text,
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )


def _prepare_tmux_pane_for_input(target: str) -> dict[str, Any]:
    mode_result = _pane_mode(target)
    if not mode_result["ok"]:
        return {
            "ok": False,
            "stage": "pane-mode-check",
            "verification": "pane_mode_check_failed",
            "error": mode_result.get("error") or "tmux pane mode check failed",
        }
    capture_result = _pane_capture_tail(target, lines=30)
    if not capture_result["ok"]:
        return {
            "ok": False,
            "stage": "pane-tail-capture",
            "verification": "pane_tail_capture_failed",
            "error": capture_result.get("error") or "tmux capture-pane failed",
        }
    pane_mode = _normalize_pane_mode(mode_result.get("pane_mode"))
    capture_tail = str(capture_result.get("capture") or "")
    detected = detect_non_input_scrollback(capture_tail)
    if detected:
        return _non_input_refusal(target, pane_mode, capture_tail, detected)
    if not pane_mode:
        return {"ok": True, "verification": "pane_input_ready"}
    cancel = _pane_mode_cancel(target, pane_mode)
    if not cancel["ok"]:
        return _non_input_refusal(
            target,
            pane_mode,
            capture_tail,
            f"tmux_{pane_mode}",
            error=cancel.get("error") or "tmux pane mode cancel failed",
            verification="pane_mode_cancel_failed",
            warning_event=cancel.get("warning_event"),
        )
    warning_event = cancel.get("warning_event")
    deadline = time.monotonic() + 1.5
    while True:
        check = _pane_mode(target)
        if not check["ok"]:
            return {
                "ok": False,
                "stage": "pane-mode-check",
                "verification": "pane_mode_recheck_failed",
                "error": check.get("error") or "tmux pane mode recheck failed",
            }
        if not _normalize_pane_mode(check.get("pane_mode")):
            result = {
                "ok": True,
                "verification": "pane_input_ready_after_mode_cancel",
                "recovered_from_mode": True,
                "pane_mode": pane_mode,
            }
            if warning_event:
                result["warning_event"] = warning_event
            return result
        if time.monotonic() >= deadline:
            return _non_input_refusal(
                target,
                pane_mode,
                capture_tail,
                f"tmux_{pane_mode}",
                error=f"tmux pane stayed in {pane_mode} after cancel",
                verification="pane_mode_still_active_after_cancel",
                warning_event=warning_event,
            )
        time.sleep(0.1)


def _pane_mode(target: str) -> dict[str, Any]:
    proc = run_cmd(["tmux", "display-message", "-p", "-t", target, "#{pane_mode}"], timeout=5)
    if proc.returncode != 0:
        return {"ok": False, "error": proc.stderr.strip() or "tmux pane mode check failed"}
    return {"ok": True, "pane_mode": proc.stdout.strip()}


def _pane_capture_tail(target: str, lines: int = 30) -> dict[str, Any]:
    capture = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{lines}", "-t", target], timeout=5)
    if capture.returncode != 0:
        return {"ok": False, "capture": "", "error": capture.stderr.strip() or "tmux capture-pane failed"}
    return {"ok": True, "capture": capture.stdout}


def _pane_mode_cancel(target: str, pane_mode: str) -> dict[str, Any]:
    mode = _normalize_pane_mode(pane_mode)
    warning_event = None
    if mode == "copy-mode":
        args = ["tmux", "send-keys", "-t", target, "-X", "cancel"]
    elif mode in {"tree-mode", "view-mode"}:
        args = ["tmux", "send-keys", "-t", target, "q"]
    elif mode == "client-mode":
        args = ["tmux", "send-keys", "-t", target, "d"]
    else:
        args = ["tmux", "send-keys", "-t", target, "-X", "cancel"]
        warning_event = "pane_mode_unknown_cancel_attempted"
    cancel = run_cmd(args, timeout=10)
    if cancel.returncode != 0:
        return {
            "ok": False,
            "error": cancel.stderr.strip() or f"tmux {mode or 'unknown'} cancel failed",
            "warning_event": warning_event,
        }
    result = {"ok": True, "mode": mode, "args": args}
    if warning_event:
        result["warning_event"] = warning_event
    return result


def _normalize_pane_mode(mode: Any) -> str:
    value = str(mode or "").strip()
    if value == "0":
        return ""
    if value == "1":
        return "copy-mode"
    return value


def _non_input_refusal(
    target: str,
    pane_mode: str,
    capture_tail: str,
    detected: str,
    *,
    error: str | None = None,
    verification: str = "recipient_pane_in_non_input_mode",
    warning_event: str | None = None,
) -> dict[str, Any]:
    result = {
        "ok": False,
        "status": "failed",
        "stage": "pre-paste-pane-state",
        "reason": "recipient_pane_in_non_input_mode",
        "error": error or "recipient_pane_in_non_input_mode",
        "verification": verification,
        "detected": detected,
        "pane_id": target,
        "pane_mode": pane_mode,
        "pane_capture_tail": non_input_scrollback_window(capture_tail) or _last_lines(capture_tail, 10),
    }
    if warning_event:
        result["warning_event"] = warning_event
    return result


def _prepare_failure_attempt(attempt: int, prepared: dict[str, Any]) -> dict[str, Any]:
    entry = {
        "attempt": attempt,
        "visible": False,
        "verification": prepared["verification"],
    }
    for key in ("reason", "detected", "pane_id", "pane_mode", "pane_capture_tail", "warning_event"):
        if key in prepared:
            entry[key] = prepared[key]
    return entry


def _last_lines(text: str, count: int) -> str:
    lines = text.splitlines()
    return "\n".join(lines[-count:])















