from __future__ import annotations

from team_agent.messaging.deps import (
    TMUX_PASTE_BYTES_PER_SECOND,
    TMUX_PASTE_MAX_READY_TIMEOUT,
    TMUX_PASTE_MIN_READY_TIMEOUT,
    TMUX_STDIN_BUFFER_THRESHOLD,
    TMUX_SUBMIT_BYTES_PER_SECOND,
    TMUX_SUBMIT_MAX_SETTLE_TIMEOUT,
    TMUX_SUBMIT_MIN_SETTLE_TIMEOUT,
    _capture_has_pasted_content_prompt,
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

def _tmux_inject_text(target: str, text: str, submit_key: str, buffer_name: str, attempts: int = 3) -> dict[str, Any]:
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
        if token:
            pre_visible, pre_verification, pre_capture = _wait_for_message_ready(
                target,
                token,
                0.0,
                expected_text=text,
                allow_pasted_prompt=False,
            )
            if pre_visible:
                attempt_entry = {
                    "attempt": attempt,
                    "visible": True,
                    "verification": pre_verification,
                    "buffer_method": "preexisting_prompt",
                    "text_bytes": text_bytes,
                    "ready_timeout_sec": 0.0,
                    "preexisting_prompt": True,
                }
                if prepared.get("recovered_from_mode"):
                    attempt_entry["recovered_from_mode"] = True
                attempt_log.append(attempt_entry)
                submit = _submit_worker_prompt(
                    target,
                    pre_capture,
                    submit_key=submit_key,
                    settle_timeout=submit_settle_timeout,
                )
                if not submit["ok"]:
                    return {
                        "ok": False,
                        "stage": submit.get("stage", "submit"),
                        "error": submit.get("error"),
                        "attempts": attempt_log,
                        "verification": pre_verification,
                        "submit_verification": submit.get("verification"),
                        "submit_attempts": submit.get("attempts"),
                    }
                submit_verification = _leader_submit_verification(submit.get("verification"), pre_verification, submit_key)
                return {
                    "ok": True,
                    "stage": "submitted",
                    "visible": True,
                    "submitted": True,
                    "verification": pre_verification,
                    "submit_verification": submit_verification,
                    "attempts": attempt_log,
                    "submit_attempts": submit.get("attempts"),
                }
            if _capture_has_pasted_content_prompt(baseline_capture):
                attempt_log.append(
                    {
                        "attempt": attempt,
                        "visible": False,
                        "verification": "preexisting_unverified_pasted_content_prompt",
                        "text_bytes": text_bytes,
                        "ready_timeout_sec": 0.0,
                    }
                )
                return {
                    "ok": False,
                    "stage": "preexisting-input",
                    "error": "target pane already has an unverified pasted-content prompt; refusing to paste again to avoid duplicate messages",
                    "attempts": attempt_log,
                    "verification": "preexisting_unverified_pasted_content_prompt",
                }
        buffered = _tmux_set_buffer_text(buffer_name, text)
        if not buffered["ok"]:
            return {"ok": False, "stage": buffered["stage"], "error": buffered.get("error"), "attempts": attempt_log}
        proc = run_cmd(["tmux", "paste-buffer", "-t", target, "-b", buffer_name, "-p"], timeout=10)
        if proc.returncode != 0:
            return {"ok": False, "stage": "paste-buffer", "error": proc.stderr.strip(), "attempts": attempt_log}
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
            "text_bytes": buffered.get("text_bytes"),
            "ready_timeout_sec": ready_timeout,
        }
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
        return {
            "ok": True,
            "stage": "submitted",
            "visible": True,
            "submitted": True,
            "verification": verification,
            "submit_verification": submit_verification,
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

























