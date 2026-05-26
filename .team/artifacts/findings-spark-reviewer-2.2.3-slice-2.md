# Findings — spark-reviewer (Slice 2 follow-up)

## Scope
- `617a517` `src/team_agent/messaging/tmux_io.py`, `src/team_agent/messaging/leader.py`, `src/team_agent/messaging/trust_auto_answer.py`, `src/team_agent/messaging/leader_panes.py`, `tests/test_gap29_send_trust_prompt_integration.py`

## 2026-05-26 Review — 617a517 (`gap-29` canonical non-input gate)

### [MEDIUM] Leader-receiver trust-prompt branch lacks retry scheduling parity

Commit: `617a517`
File/line evidence: `src/team_agent/messaging/leader.py:254-266`, `src/team_agent/messaging/trust_auto_answer.py:21-37`, `src/team_agent/messaging/delivery.py:173-193`
Description: In the leader-receiver lane, a trust prompt that is still present after waiting (`_wait_for_trust_prompt_dismissal` timeout) is converted to a terminal failure envelope and immediately handed to `_fail_leader_delivery`. Unlike worker delivery (`_deliver_pending_message`), this path does not emit `trust_retry_needed`/`trust_retry_scheduled` events or enqueue `trust_retry`, so transient slow prompt-dismiss races become one-shot drops to fallback instead of bounded retries.
Suggested fix shape: on timeout in `retry_injection_after_trust_auto_answer`, mirror the worker behavior by scheduling a retry event (or reusing `_handle_trust_retry_needed`) with a bounded backoff and explicit terminal exhaustion event for repeated failures, then keep existing success/error semantics unchanged.

### [LOW] Leader receiver integration test does not cover post-detection timeout failure path

Commit: `617a517`
File/line evidence: `tests/test_gap29_send_trust_prompt_integration.py:136-237`
Description: The new leader-receiver integration scenario only exercises the happy path where the prompt disappears. There is no test for the `trust_prompt_not_dismissed_after_answer` branch, so regressions in `retry_injection_after_trust_auto_answer` timeout behavior or loss of diagnostics are currently uncaught.
Suggested fix shape: add a failure-path test that keeps capture in trust-prompt state longer than the dismissal timeout and asserts the resulting event status/queue behavior (retry vs terminal fallback) before/after applying the above fix.

## 2026-05-26 Review — 25b2249 (`gap-29` trust prompt detection above trailing blank scrollback)

### [LOW] Detection can still miss prompts buried beyond the fixed 30-line window

Commit: `25b2249`
File/line evidence: `src/team_agent/messaging/tmux_io.py:303-304`, `src/team_agent/messaging/tmux_io.py:370-374`, `src/team_agent/messaging/tmux_prompt.py:45-53`
Description: Pre-paste detection still relies on a fixed capture of 30 tmux lines and a 15 non-empty-line normalization window, so prompts separated from the tail by heavy output (e.g. compile spam or bursty progress logs) can remain undetected and proceed without refusal handling.
Suggested fix shape: add bounded fallback when no mode/prompt is detected—e.g. widen capture adaptively (e.g. full visible history) or trigger a secondary capture/retry when the first window is mostly non-prompt output.

### [LOW] ANSI strip only removes CSI sequences, leaving OSC/control escapes in the detection path

Commit: `25b2249`
File/line evidence: `src/team_agent/messaging/tmux_prompt.py:16`, `src/team_agent/messaging/tmux_prompt.py:50`
Description: `_ANSI_ESCAPE_RE` removes only `ESC[` CSI escapes; OSC sequences (`ESC]...BEL/ST`) and other terminal control payloads are not stripped and can fragment signature text in real Codex output before regex matching.
Suggested fix shape: switch to a terminal-control normalizer that strips OSC as well (or use a broad ANSI cleaner) before non-input detection.
