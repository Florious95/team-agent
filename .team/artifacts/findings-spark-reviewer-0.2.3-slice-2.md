# Findings — spark-reviewer (Slice 2 supplement)

## Scope
- `f89ba47` `tests/test_gap38_repro_four_deliveries.py`
- `65d426f` `src/team_agent/messaging/leader_api_errors.py`, `tests/test_gap28_event_emission.py`
- `3fd1f1d` `src/team_agent/messaging/leader_panes.py`, `src/team_agent/messaging/delivery.py`, `tests/test_gap29_trust_auto_answer.py`
- `e7892cf` `src/team_agent/messaging/leader_api_errors.py`, `tests/test_gap28_event_emission.py`, `tests/test_gap38_repro_four_deliveries.py`
- `10078e7` `src/team_agent/messaging/delivery.py`, `src/team_agent/messaging/leader_api_errors.py`, `src/team_agent/messaging/leader_panes.py`, `tests/test_gap29_trust_auto_answer.py`, `tests/test_gap28_event_emission.py`
- `e7fb747` `src/team_agent/messaging/delivery.py`, `src/team_agent/messaging/scheduler.py`, `src/team_agent/messaging/leader_api_errors.py`, `tests/test_gap28_event_emission.py`, `tests/test_gap29_trust_auto_answer.py`
- `03b0c00` `src/team_agent/cli/parser.py`, `src/team_agent/cli/commands.py`, `src/team_agent/diagnose/orphan_cleanup.py`, `tests/test_gap18a_status_summary.py`, `tests/test_gap18b_doctor_gate_orphans.py`

> Items 1-3 were swept by `e7892cf` (clock advancement in scheduler-retry harness;
> per-thread deepcopy in parallel-threads harness; API-context prefix on the
> leader_api_errors patterns). Items 4-7 were swept by `10078e7` (bounded poll
> for trust-prompt dismissal; canonical-path workspace match; structured events
> on every refusal branch; multi-line sliding-window API context matching).
> Items 1-2 of sweep #3 were swept by the spark-sweep-3 bundle commit
> (retry_needed bounded-backoff scheduled consumer with terminal
> trust_auto_answer_exhausted event; window tail-preservation instead of
> wholesale drop on long diagnostic blocks).

Open findings at the time of writing are listed below.

## Findings

### [MEDIUM] status --summary silently ignores positional `agent`

Commit: `03b0c00`
File/line evidence: `src/team_agent/cli/parser.py:181-183`, `src/team_agent/cli/commands.py:90-98`
Description: `status` still accepts positional `agent` but `--summary` path ignores it and always renders the full-team five-line summary via `runtime.status(...)` without passing agent filtering; scripts that pass an agent id and expect scoped output get misleading results with no validation error.
Suggested fix shape: reject `agent` when `--summary` is used, or explicitly document/implement an agent-scoped summary mode before emitting output.

### [MEDIUM] Health state classification drops non-modeled states to `running`

Commit: `03b0c00`
File/line evidence: `src/team_agent/cli/commands.py:240-254`
Description: `_agent_summary_counts` maps every non-handled health status to `running`, so states like `blocked`/`awaiting_approval` (from runtime health sync) are triaged as healthy, which can hide stalled/abnormal workers in the compact summary row used for quick incident triage.
Suggested fix shape: add explicit branches for non-modeled health states (e.g., `blocked`, `awaiting_approval`, `error`/`interrupted`/`missing`) and keep an `unknown` bucket instead of folding to `running`.

### [LOW] `doctor --fix` exists without requiring `--gate`

Commit: `03b0c00`
File/line evidence: `src/team_agent/cli/parser.py:314-327`, `src/team_agent/cli/commands.py:207-214`
Description: `--fix`/`--confirm` are accepted on `doctor` globally but have effect only in the new `--gate orphans` path; when passed without `--gate` the intent is silently ignored and the user gets legacy output, which can mask CI/test misconfiguration and produce false confidence.
Suggested fix shape: validate these flags (e.g., `if args.fix and not args.gate: error`) or move them into the same gate-specific argument group.

### [MEDIUM] Trust-retry state transitions are not atomic with scheduled-event lifecycle

Commit: `e7fb747`
File/line evidence: `src/team_agent/messaging/delivery.py:218-233`, `src/team_agent/messaging/delivery.py:307-325`, `src/team_agent/messaging/scheduler.py:115`, `src/team_agent/coordinator/lifecycle.py:283-285`
Description: `trust_retry` transitions from normal retry detection to scheduled retry insert then `failed`, and `trust_retry` execution sets status back to `accepted` before `_deliver_pending_message`; if coordinator crashes between status writes and scheduled-event completion, next tick can re-deliver immediately via `_deliver_pending_messages` and bypass backoff, creating duplicate retries and extra `send` attempts outside the intended interval.
Suggested fix shape: persist message-status change and scheduling atomically (same sqlite transaction or single helper), and avoid writing `accepted` directly before the message is guaranteed to be owned by the scheduler fire (for example mark with a retry-in-flight state and only move to `accepted` when due-event context is executing).

### [LOW] Exact boundary coverage for leader-api window tail slicing is not locked in

Commit: `e7fb747`
File/line evidence: `src/team_agent/messaging/leader_api_errors.py:169-171`
Description: the tail-preserve behavior changed to trim only when `len(window) > _WINDOW_MAX_CHARS`, but there are no direct tests around exact `400` and `401` boundary windows. A future change from `>` to `>=` or off-by-one in the cap logic would silently alter recall and may only surface on wrapped multiline diagnostics.
Suggested fix shape: add focused tests for exact-boundary cases (`len(window)==400` and `==401`) around a valid error pattern to freeze this behavior.

## 2026-05-26 Conversion review — 0846973 (`pytest`→`unittest`)

### [NONE] No additional findings

Commit: `0846973`
File/line evidence: `tests/test_gap18a_status_summary.py`, `tests/test_gap18b_doctor_gate_orphans.py`
Description: Migration preserved assertion coverage and control flow from the prior pytest versions: the same fixture setup (`tmp_path`→`tempfile.TemporaryDirectory`), exception paths (`pytest.raises`→`assertRaisesRegex`), and CLI output assertions (`capsys.out`→`redirect_stdout`) were retained. No silent no-op patterns (e.g., patch scope leakage, missing teardown, or dropped assertions) were identified.

## 2026-05-26 Review — 339ad49 (`watch` MVP stream)

### [MEDIUM] `team` selector is accepted by CLI but not enforced in event or result streams

Commit: `339ad49`
File/line evidence: `src/team_agent/cli/parser.py:190-193`, `src/team_agent/cli/commands.py:103-107`, `src/team_agent/watch/__init__.py:35-39`, `src/team_agent/watch/__init__.py:83-89`, `src/team_agent/message_store/core.py:425-437`
Description: `watch --team` is threaded through CLI and `run_watch`, but `_collect_event_lines` ignores team entirely and `MessageStore.latest_results` ignores `owner_team_id` (it is a no-op). In a workspace with multiple teams, `watch` can therefore print events/results from unrelated teams, so the `--team` flag is effectively a no-op and risks cross-team noise.
Suggested fix shape: pass `team` into event filtering and add owner-team filtering in `latest_results` (or equivalent query) so all emitted lines are scoped to the selected team before rendering.

### [LOW] Rotation can cause in-memory watch cursors to skip unread events

Commit: `339ad49`
File/line evidence: `src/team_agent/watch/__init__.py:61-81`, `src/team_agent/events.py:12-18`, `tests/test_gap18c_watch.py:86-99`
Description: `watch` only tails `events.jsonl` and silently ignores archive segments. If the file rotates while the cursor still trails inside the previous segment, unread lines moved to `events.jsonl.N` are not replayed and are lost from stream output. The existing tests explicitly validate archived segments are ignored, so this behavior is currently accepted and can look like silent dropped notifications for long-lived watches.
Suggested fix shape: persist archived-segment offsets in the cursor and replay required tail segments across rotation, or surface a “log rotated, replay omitted” marker when not all events can be guaranteed delivered.

## 2026-05-26 Review — 4b479fd (`status --summary + doctor gate`)

### [NONE] No additional findings

Commit: `4b479fd`
File/line evidence: `src/team_agent/cli/commands.py:91-108`, `src/team_agent/cli/commands.py:240-266`, `tests/test_gap18a_status_summary.py:44-56`, `tests/test_gap18b_doctor_gate_orphans.py:108-114`
Description: The targeted fixes for summary-agent rejection, unknown bucket rendering, and `doctor --fix` gating align with the brief and close prior gaps without introducing new behavioral regressions on the touched surfaces. Existing tests include the new rejection branch and the `--fix` validation path.

## 2026-05-26 Review — cd08303 (final claude-rd Gap-29 deprecation/structured event)

### [LOW] Deprecation warning one-shot guard is not thread-safe

Commit: `cd08303`
File/line evidence: `src/team_agent/messaging/leader_panes.py:561-607`, `src/team_agent/messaging/leader_panes.py:616-620`
Description: `_SPEC_OPT_IN_DEPRECATION_WARNED` is a module-level bool flipped without synchronization. If multiple `attempt_trust_auto_answer` calls execute concurrently in the same process (same runtime session), several threads can observe `False` before any writes it, and stderr deprecation warning can be printed more than once, violating the stated one-shot guarantee. The structured event path remains per-call.
Suggested fix shape: guard the check/set with a lock (or atomic compare-and-set helper) and keep structured event emission outside the lock to preserve one-shot semantics under concurrency while still logging each yaml-driven decision.

## 2026-05-26 Review — b34c2a2 (`watch team filtering + rotation marker`)

### [NONE] No additional findings

Commit: `b34c2a2`
File/line evidence: `src/team_agent/message_store/schema.py:124-129`, `src/team_agent/message_store/core.py:422-437`, `src/team_agent/watch/__init__.py:70-106`, `tests/test_gap18c_watch.py:61-92`
Description: Reviewed the commit for schema migration, team-scoped filtering, rotation marker behavior, and test coverage. The commit adds `owner_team_id` migration for legacy DBs, applies owner-team filtering consistently for event and result watch paths, and adds explicit cursor-based rotation marker behavior with targeted tests. No additional cross-team leakage/regression risk was identified in the touched surface.
