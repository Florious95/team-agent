# Selftest Comms and Idle Accuracy Contract

This contract covers `team-agent doctor --comms` and the bug-071 idle accuracy
rules. Implementers may read this document and the contract stubs only; the
acceptance tests are owned by the Module Contract Owner.

## Public Signatures

The public helper behind the CLI is:

```python
def run_comms_selftest(
    workspace: Path,
    *,
    team: str | None = None,
    gate: str | None = None,
    response_sla_sec: float = 20.0,
    probe_content: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]: ...
```

`driver` is an injectable boundary for tests. Production calls omit it and use
real tmux/runtime/message-store primitives. The helper returns stable JSON and
never writes probe text to the live leader pane.

The idle challenge helper is:

```python
def evaluate_idle_behavior(
    workspace: Path,
    *,
    agent_id: str,
    claimed_status: str,
    response_sla_sec: float = 20.0,
    token: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]: ...
```

## Feature A: doctor --comms

C1. `doctor --comms` extends the existing `team-agent doctor` surface. There is
no new top-level `team-agent selftest` command. `team-agent doctor --help`
documents `--comms`.

C2. `doctor --comms --json` and `doctor --gate comms --json` route to the same
helper. For the same workspace and same tick, their canonical JSON is byte
identical after timestamp/run-id fields are removed.

C3. Worker-to-leader probe content must include a unique token and must not
start with `Result id:`. If it does, the helper returns
`ok=false`, `error=probe_content_uses_result_prefix`, and does not use result
notification dedupe as a success signal.

C4. Any fallback leader delivery is a selftest failure. A messaging-compatible
`ok=true, status=fallback_log` response is translated by doctor to
`checks.worker_to_leader.status=fail`, top-level `ok=false`, and non-zero CLI
exit.

C5. A deduped leader notification is a selftest failure. Dedupe proves DB state,
not live render.

C6. Worker-to-leader pass requires the unique token to appear in the disposable
capture pane text. DB rows or `leader_receiver.submitted` events alone are not
enough.

C7. Disposable tmux sessions use only the prefix `ta-selftest-comms-<runid>`.
They must never reuse `quick-test-*`, `team-agent-*`, or live team session
names.

C8. Created disposable sessions are killed in a finally path. If the helper
raises or returns failure after creation, cleanup still runs and JSON includes
`checks.cleanup.status=killed` or `pass`.

C9. At startup, the helper sweeps stale `ta-selftest-comms-*` sessions from a
previous crash and emits `selftest.swept_stale`.

C10. Cleanup is a first-class check. JSON contains
`checks.cleanup.{status,killed_sessions}`. Any cleanup failure makes top-level
`ok=false`.

C11. Receiver binding may not reintroduce command-name whitelists. A live
external Claude pane whose `pane_current_command` is `2.1.154` is usable.

C12. Receiver binding may not require `TEAM_AGENT_LEADER_SESSION_UUID*`.
Pane equality between `team_owner.pane_id`, `leader_receiver.pane_id`, and the
caller pane is sufficient identity.

C13. The helper is read-only with respect to live owner state. It does not call
owner-population/first-bind mutators and does not persist the disposable
receiver. `state.json` `team_owner` and `leader_receiver` are byte-identical
before and after the run.

C13b. The helper is also read-only with respect to pre-existing user messages.
Running `doctor --comms` must not advance, submit, deliver, acknowledge, fail,
or otherwise mutate any pending/accepted message that existed before the run.
Only selftest-owned probe rows may be processed by the selftest.

## Four Ack Layers

Every matrix cell reports the four signals independently:

- `enqueue_ack`: durable message row accepted.
- `delivery_ack`: exact message id was submitted to the recipient pane and a
  `send.submitted` or `send.pending_delivered` event exists.
- `execution_ack`: the worker produced a bounded result or leader-bound message
  tied to the unique task/message token.
- `leader_notification_ack`: the execution result/leader-bound response reached
  the disposable capture receiver and not the live leader pane.

The JSON must not collapse these signals into one `ok` field.

## Matrix

A1. Worker idle, leader to worker: pass requires all four ack layers. This is
also the behavioral definition of idle: an idle worker can accept a probe and
produce a bounded execution ack.

A2. Worker working, leader to worker: default semantics are FIFO, not
preemption. While busy, the second message gets `enqueue_ack=pass`, a
`send.deferred_busy` event, and no paste into the busy pane. After busy clears,
the same message gets `delivery_ack=pass` through `send.pending_delivered`.
Execution ack is separate and may arrive later.

B1. Worker working, worker to leader: when the worker reaches a safe point, the
leader-bound response renders into the disposable capture receiver. It must not
fall back, dedupe, or appear in the live leader pane.

B2. Worker idle, worker to leader: after an idle challenge, the worker produces
a bounded leader-bound response that renders into the disposable capture
receiver and not the live leader pane.

No selftest path may test or perform preemption, Ctrl+C, or interrupt behavior.

## Feature B: Idle Accuracy

C14. The latest provider idle prompt remains the strongest signal. Pane delta or
old working scrollback must not override a fresh idle prompt. This is backed by
verbatim real pane captures for both Codex
(`tests/fixtures/idle_prompts/codex_idle.txt`) and Claude Code
(`tests/fixtures/idle_prompts/claude_code_idle.txt`); the first metadata line is
not part of the scrollback input.

C15. Active task is required before pane-delta `running` can be promoted to
`WORKING`. Without an active task, raw `running` plus pane delta may remain
`IDLE`.

C16. Gap 32 takeover stays on provider session-log facts. The
`idle_takeover_wiring` module must not import `agent_health` or
`approvals.status`, and its inputs must not grow `last_output_at` or
`activity_output_hash`.

C17. Stuck detection includes `WORKING` rows as well as `RUNNING`. A worker with
an active task and stale `last_output_at` must still emit stuck diagnostics
after Feature B introduces the `WORKING` label.

## Real-Machine Acceptance

A realistic tester must run one external-leader, real-worker flow:

1. `team-agent doctor --comms --workspace . --json` while one worker is idle.
2. A long-running worker task, then `doctor --comms` during busy state.
3. The A2 busy case shows durable enqueue and deferred delivery, not preempt.
4. Worker-to-leader probes render only in disposable capture panes.
5. No `ta-selftest-comms-*` sessions remain after success or failure.
6. `team-agent status --json` shows an active outputting worker as `WORKING` or
   `RUNNING`, not `IDLE`; after provider idle prompt returns, it shows `IDLE`.
