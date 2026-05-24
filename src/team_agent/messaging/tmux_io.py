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

def _tmux_inject_text(
    target: str,
    text: str,
    submit_key: str,
    buffer_name: str,
    attempts: int = 3,
    provider: str = "fake",
) -> dict[str, Any]:
    token_match = re.search(r"\[team-agent-token:([^\]]+)\]", text)
    token = token_match.group(1) if token_match else ""
    attempt_log: list[dict[str, Any]] = []
    last_verification = "not_checked"
    ready_timeout = _tmux_paste_ready_timeout(text)
    submit_settle_timeout = _tmux_submit_settle_timeout(text)
    text_bytes = _tmux_text_size(text)
    for attempt in range(1, max(attempts, 1) + 1):
        prepared = _prepare_tmux_pane_for_input(target)
        if not prepared["ok"]:
            attempt_log.append({"attempt": attempt, "visible": False, "verification": prepared["verification"]})
            return {
                "ok": False,
                "stage": prepared["stage"],
                "error": prepared.get("error"),
                "attempts": attempt_log,
                "verification": prepared["verification"],
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
        turn_visible, turn_verification, turn_capture = _wait_for_leader_new_turn(
            target,
            text,
            token,
            provider=provider,
            timeout=2.0,
        )
        if not turn_visible:
            return {
                "ok": False,
                "stage": "turn-boundary-verification",
                "error": f"leader turn boundary not verified: {turn_verification}",
                "attempts": attempt_log,
                "verification": verification,
                "submit_verification": submit_verification,
                "turn_verification": turn_verification,
                "submit_attempts": submit.get("attempts"),
            }
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
    mode = run_cmd(["tmux", "display-message", "-p", "-t", target, "#{pane_in_mode}"], timeout=5)
    if mode.returncode != 0:
        return {
            "ok": False,
            "stage": "pane-mode-check",
            "verification": "pane_mode_check_failed",
            "error": mode.stderr.strip() or "tmux pane mode check failed",
        }
    if mode.stdout.strip() != "1":
        return {"ok": True, "verification": "pane_input_ready"}
    cancel = run_cmd(["tmux", "send-keys", "-t", target, "-X", "cancel"], timeout=10)
    if cancel.returncode != 0:
        return {
            "ok": False,
            "stage": "pane-mode-cancel",
            "verification": "pane_mode_cancel_failed",
            "error": cancel.stderr.strip() or "tmux copy-mode cancel failed",
        }
    deadline = time.monotonic() + 1.5
    while True:
        check = run_cmd(["tmux", "display-message", "-p", "-t", target, "#{pane_in_mode}"], timeout=5)
        if check.returncode != 0:
            return {
                "ok": False,
                "stage": "pane-mode-check",
                "verification": "pane_mode_recheck_failed",
                "error": check.stderr.strip() or "tmux pane mode recheck failed",
            }
        if check.stdout.strip() != "1":
            return {"ok": True, "verification": "pane_input_ready_after_mode_cancel", "recovered_from_mode": True}
        if time.monotonic() >= deadline:
            return {
                "ok": False,
                "stage": "pane-mode-cancel",
                "verification": "pane_mode_still_active_after_cancel",
                "error": "tmux pane stayed in copy-mode after cancel",
            }
        time.sleep(0.1)





















