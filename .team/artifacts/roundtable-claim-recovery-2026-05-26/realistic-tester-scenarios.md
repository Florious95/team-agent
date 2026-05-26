# Realistic Tester Scenarios: claim-leader Recovery After Exhaustion

Role: `realistic-tester` / External Realistic Acceptance Tester
Date: 2026-05-26
Worktree: `/tmp/team-agent-public-0.2.2-slice-1`

## Scope

This file enumerates user-facing scenarios where retry exhaustion plus later `claim-leader --confirm` can cause silent loss, duplicate notification, partial recovery, or delayed-too-long recovery. It is intentionally not a fix proposal. I did read-only research only: the second roundtable kickoff brief, the latest Mac mini failure diagnosis, and the relevant queue/delivery surfaces.

The known real-machine failure to explain is:

- worker reported result `res_8362140758fd`;
- leader was ambiguous, with candidate panes `%533` and `%534`;
- scheduled-event retries `3`, `4`, and `5` all failed with `reason=ambiguous`;
- user claimed pane `%533`;
- `claim-D.stdout` returned `requeued_watchers: []`;
- no post-claim `leader_receiver.deliver_attempt`, no `leader_notification_log` row, and no result envelope in pane `%533`.

The prior duplicate-delivery arm is treated as closed by `945948b`: injection-boundary dedupe keyed by `(result_id, leader_session_uuid)` remains an input constraint here.

## Realistic Evidence Streams

For every scenario below, the real-machine acceptance evidence should come from:

- `events.jsonl`: `mcp.report_result_notify_queued`, `leader_receiver.delivery_failed`, `coordinator.scheduled_retry`, `leader_receiver.claim_applied`, `leader_receiver.deliver_attempt`, `leader_receiver.submitted`, `leader_notification.dedupe_skip`, and any recovery-specific event.
- SQLite snapshots: `scheduled_events`, `result_watchers`, `leader_notification_log`, and `results`.
- Pane captures: the claimed leader pane must show the result envelope exactly once, or show an explicit recovery-failed incident if delivery truly cannot proceed.
- Command output: `claim-leader --confirm --json` must say what it revived or refused to revive; `requeued_watchers=[]` alone is not sufficient when abandoned scheduled events exist.

## Scenarios

### 1. Scheduled-event-only exhaustion, then claim

- What the user did: User sends work fire-and-forget. Worker calls `report_result`. The leader is ambiguous because the user has two resumed panes. The user reads the ambiguous incident for a few seconds, then runs `team-agent claim-leader --confirm`.
- Where the framework gave up: Path A only. `_notify_leader_of_report_result` created a `scheduled_events` row. `_fire_due_scheduled_events` consumed all retry attempts under `reason=ambiguous`; the final row is `status=failed`. No `result_watchers` row exists because this was a report-result notification, not a watched task.
- State at claim moment:
  ```yaml
  leader_receiver:
    old_pane_id: "%old"
    ambiguous_candidates: ["%D", "%E"]
  scheduled_events:
    - kind: send
      target: leader
      status: failed
      payload.content: "Result id: res_1"
      result_json.reason: ambiguous
      owner_team_id: current
  result_watchers: []
  leader_notification_log:
    no row for [res_1, uuid-A]
  ```
- Expected outcome: The claim wakes the abandoned scheduled result and the claimed pane receives `res_1` exactly once. The result should not require a new worker action or manual inbox polling.
- Failure mode if recovery layer is wrong: Silent loss. This is the observed `945948b` Mac mini failure: durable result exists, claim succeeds, but the claimed pane receives zero result notifications.

### 2. Result-watcher exhaustion, scheduled-event still has budget

- What the user did: User sends a task with result watching enabled and the worker also reports normally. Ambiguity starts while the result is being collected. The watched-result path exhausts first, but a scheduled leader notification still has a pending retry.
- Where the framework gave up: Path B (`result_watchers`) reached `delivery_exhausted` or `notify_failed` retry limit. Path A (`scheduled_events`) still has one pending retry or `retry_scheduled`.
- State at claim moment:
  ```yaml
  result_watchers:
    - watcher_id: watch_1
      result_id: res_1
      status: delivery_exhausted
      notified_message_id: null
  scheduled_events:
    - id: 8
      status: pending or retry_scheduled
      payload.content: "Result id: res_1"
  leader_notification_log:
    no row for [res_1, uuid-A]
  ```
- Expected outcome: The user sees one notification for `res_1` after claim. It does not matter which surviving path delivers it, but the other path must converge through the injection-boundary dedupe gate and not inject a second copy.
- Failure mode if recovery layer is wrong: Partial recovery or duplicate. If only scheduled events are revived, the watcher remains stale and may later re-fire after a restart. If both are revived without dedupe, the user sees duplicate result envelopes.

### 3. Both scheduled-event and result-watcher exhaust before claim

- What the user did: User leaves two leader panes open and walks away. Worker finishes. Both the direct report-result notification and a watcher-based notification try to deliver under ambiguity until their budgets are exhausted. The user claims a pane later.
- Where the framework gave up: Path A has a failed `scheduled_events` row; Path B has a `delivery_exhausted` watcher. Neither queue is retryable by ordinary coordinator ticks.
- State at claim moment:
  ```yaml
  scheduled_events:
    - status: failed
      payload.content: "Result id: res_1"
      result_json.reason: ambiguous
  result_watchers:
    - status: delivery_exhausted
      result_id: res_1
      notified_message_id: null
  leader_notification_log:
    no row for [res_1, uuid-A]
  ```
- Expected outcome: Claim recovers the result exactly once. Recovery can report that two abandoned sources existed, but it must create one canonical user-visible notification for `res_1`.
- Failure mode if recovery layer is wrong: Silent loss if neither dead source is scanned; duplicate if both dead sources are reanimated as independent notifications.

### 4. Exhausted watcher with NULL notified_message_id

- What the user did: User used a watched task. The framework tried to notify the leader several times, but every attempt failed before a visible submission because the leader remained ambiguous. User claims a pane after exhaustion.
- Where the framework gave up: Path B watcher row entered `delivery_exhausted`; `notified_message_id` remains `NULL`.
- State at claim moment:
  ```yaml
  result_watchers:
    - watcher_id: watch_1
      status: delivery_exhausted
      result_id: res_1
      notified_message_id: null
      owner_team_id: current
  scheduled_events:
    none for this result
  leader_notification_log:
    no row for [res_1, uuid-A]
  ```
- Expected outcome: Claim treats this as not delivered and revives it. The claimed pane shows `res_1` once, and evidence shows the recovery crossed the injection-boundary dedupe gate.
- Failure mode if recovery layer is wrong: Silent loss if `delivery_exhausted` is treated as terminal. Delayed-too-long if recovery waits for another coordinator tick that never considers exhausted watchers.

### 5. Exhausted watcher with notified_message_id set from legacy or partial state

- What the user did: User claims a pane after an old or partially updated watcher row says a message id exists, but the user never saw the result in any live leader pane.
- Where the framework gave up: Path B watcher is `delivery_exhausted` or `notified`, but `notified_message_id` is non-null due to a legacy path, pre-Stage-12 state, or a partial update before visible submission. `leader_notification_log` may or may not have a matching row.
- State at claim moment:
  ```yaml
  result_watchers:
    - watcher_id: watch_1
      result_id: res_1
      status: delivery_exhausted or notified
      notified_message_id: msg_old
  leader_notification_log:
    absent, or row [res_1, uuid-A] -> msg_old with uncertain visibility
  messages:
    msg_old status: failed, ambiguous, submitted_unverified, or missing
  ```
- Expected outcome: The user-visible truth wins. If `msg_old` was visibly submitted to this leader session, claim must not duplicate it. If `msg_old` never reached a valid pane, claim must recover `res_1` once and leave audit explaining why the stale message id did not block recovery.
- Failure mode if recovery layer is wrong: Silent loss if any non-null `notified_message_id` is treated as proof of delivery. Duplicate if every stale/non-null id is ignored without checking pane/message visibility.

### 6. Long ambiguous window: result arrived 1 minute before claim vs 29 minutes before claim

- What the user did: User gets an ambiguous-leader incident and delays claiming. Some results finish shortly before claim; others finished much earlier while the user was away.
- Where the framework gave up: Retry budgets for older scheduled events/watchers have exhausted. Newer results may still be pending or retry-scheduled.
- State at claim moment:
  ```yaml
  ambiguous_incident:
    ts: T0
  results:
    - res_recent completed_at: Tclaim - 1m
    - res_old completed_at: Tclaim - 29m
  scheduled_events/result_watchers:
    mixed pending, retry_scheduled, failed, delivery_exhausted
  leader_notification_log:
    no rows for the undelivered results
  ```
- Expected outcome: The claimed pane receives all leader-bound results that are still relevant and undelivered, exactly once each, or the claim command emits a clear incident listing any results it deliberately does not revive. A 30-minute user delay should not silently discard completed work.
- Failure mode if recovery layer is wrong: Partial recovery. A too-narrow incident timestamp drops older-but-undelivered results; a too-wide scan revives stale historical results the user already saw or no longer expects.

### 7. Coordinator restart between exhaustion and claim

- What the user did: User leaves the team ambiguous. The coordinator exits or the machine sleeps/restarts after retry exhaustion. User later returns, restarts/attaches/claims a leader pane.
- Where the framework gave up: The failed `scheduled_events` rows and/or exhausted watcher rows are persisted before restart. In-memory retry state is gone.
- State at claim moment:
  ```yaml
  coordinator:
    new pid after restart
  scheduled_events:
    - status: failed
      payload.content: "Result id: res_1"
  result_watchers:
    - status: delivery_exhausted or notify_failed
  leader_notification_log:
    no row for [res_1, uuid-A]
  ```
- Expected outcome: Recovery uses durable DB state, not in-memory attempt state. The claimed leader pane receives `res_1` once after claim, and the event log records that a previously exhausted delivery was revived.
- Failure mode if recovery layer is wrong: Silent loss after restart because only in-memory retry queues knew about the result. Duplicate if restart resets retry budgets without consulting `leader_notification_log`.

### 8. Cross-team claim while another team's delivery is exhausted

- What the user did: Same machine/workspace has Team A and Team B. Team A has an exhausted leader-bound result under ambiguity. User claims a Team B leader pane or Team B independently enters claim recovery.
- Where the framework gave up: Team A's scheduled event or watcher is failed/exhausted. Team B has its own `team_owner`, `owner_epoch`, and possibly its own ambiguous incident.
- State at claim moment:
  ```yaml
  teams:
    teamA:
      scheduled_events:
        - status: failed
          owner_team_id: teamA
          payload.content: "Result id: res_A"
    teamB:
      claim_leader running for owner_team_id: teamB
  leader_notification_log:
    shared table with owner_team_id column
  ```
- Expected outcome: Team B claim must not revive Team A's result. Team A claim later revives `res_A` once for Team A. Shared `leader_notification_log` must not cross-suppress or cross-revive.
- Failure mode if recovery layer is wrong: Cross-team partial or duplicate delivery. Team A's result appears in Team B's leader pane, or Team B's claim marks Team A's result as delivered and causes silent loss for Team A.

### 9. Claim happens before retry exhaustion

- What the user did: User reacts quickly to the ambiguous incident and claims a pane while the scheduled event still has retry budget.
- Where the framework gave up: It has not fully given up yet. One or more retry rows are pending or retry-scheduled; no failed terminal row exists.
- State at claim moment:
  ```yaml
  scheduled_events:
    - status: retry_scheduled
      next_attempt: 2 or 3
      payload.content: "Result id: res_1"
  result_watchers:
    optional pending/notify_failed
  leader_notification_log:
    no row for [res_1, uuid-A]
  ```
- Expected outcome: The user should not have to wait for the original retry interval if claim resolved the blocker. The result should deliver once soon after claim; any later natural retry should dedupe.
- Failure mode if recovery layer is wrong: Delayed-too-long if claim does nothing and the user waits for the next scheduled tick. Duplicate if claim triggers immediate delivery and the still-pending retry later injects again.

### 10. No-candidate stale leader, then attach/claim recovery

- What the user did: User closes the only leader pane, worker reports a result, retry budget exhausts because no usable leader exists, then the user opens a new pane and attaches or claims it.
- Where the framework gave up: Scheduled-event or watcher retries failed with `leader_pane_missing`, `leader_not_attached`, or `rebind_required`, not `ambiguous`.
- State at claim moment:
  ```yaml
  leader_receiver:
    old_pane_id: "%dead"
    validation: missing
  scheduled_events:
    - status: failed
      result_json.reason: leader_pane_missing or rebind_required
      payload.content: "Result id: res_1"
  result_watchers:
    maybe delivery_exhausted
  ```
- Expected outcome: Once the user creates a valid leader pane, the abandoned result is delivered exactly once or an explicit "recover failed" incident is shown. The recovery rule should not be limited only to two-candidate ambiguity if the user-visible promise is "result appears when leader is available again."
- Failure mode if recovery layer is wrong: Silent loss for ordinary window-close/reopen workflows; or duplicate if both attach-leader and claim-leader independently revive the same result.

### 11. Losing claim pane keeps operating after recovery

- What the user did: Two candidate panes both attempt claim. Pane D wins; pane E loses with `owner_epoch_advanced`. Pane E then runs a command or receives stale incident text.
- Where the framework gave up: A result delivery was exhausted before claim and is revived by the winning claim.
- State at claim moment:
  ```yaml
  team_owner:
    pane_id: "%D"
    owner_epoch: 1
  loser_pane:
    pane_id: "%E"
    expected_epoch: 0
  scheduled_events/result_watchers:
    revived for res_1
  leader_notification_log:
    row should be created for [res_1, uuid-A] only when delivery to %D is claimed
  ```
- Expected outcome: Pane D receives `res_1` exactly once. Pane E receives only a refusal/epoch-advanced response and cannot re-drive or suppress the recovery.
- Failure mode if recovery layer is wrong: Duplicate if loser-pane activity triggers another revival. Silent loss if the losing claim writes or reads state in a way that marks the result as recovered before pane D receives it.

### 12. Several results accumulate while leader is ambiguous

- What the user did: The team continues working while the leader is ambiguous. Workers A, B, and C each report results before the user claims a pane.
- Where the framework gave up: Multiple scheduled events/watchers are in mixed terminal states: some failed, some retry-scheduled, some delivery_exhausted, some not yet attempted.
- State at claim moment:
  ```yaml
  results:
    - res_A
    - res_B
    - res_C
  scheduled_events/result_watchers:
    mixed states by result_id
  leader_notification_log:
    no rows for undelivered result ids
  ```
- Expected outcome: The claimed pane receives each distinct result exactly once, in a stable order if possible. The user can tell which work finished; no result silently vanishes because another one recovered first.
- Failure mode if recovery layer is wrong: Partial recovery if only the latest result is revived; duplicate if every queue row is revived without collapsing by `(result_id, leader_session_uuid)`; delayed-too-long if a large backlog depends on ordinary tick pacing.

### 13. Fallback inbox exists but pane notification never happened

- What the user did: During ambiguity, failed deliveries wrote fallback inbox entries. User later claims the intended leader pane and expects normal pane notification.
- Where the framework gave up: `_fail_leader_delivery` wrote `leader-inbox.log`; scheduled event or watcher reached terminal state. No injection reached the leader pane.
- State at claim moment:
  ```yaml
  fallback:
    leader-inbox.log contains "Result id: res_1"
  scheduled_events:
    status: failed
  leader_notification_log:
    no row for [res_1, uuid-A]
  pane:
    claimed pane has only ambiguous incident text
  ```
- Expected outcome: Fallback audit does not count as pane delivery. Claim should still deliver `res_1` once to the pane, while preserving the fallback file as historical evidence.
- Failure mode if recovery layer is wrong: Silent loss if "fallback written" is treated as delivered. Duplicate if fallback is rendered into the pane more than once alongside the recovered result.

### 14. Recovery after model/tool approval stalls the leader pane

- What the user did: User claims a pane, but the pane is at a provider prompt or MCP approval prompt rather than a clean input prompt. The result had already exhausted under ambiguity.
- Where the framework gave up: The original delivery exhausted under ambiguity. The recovery attempt after claim may fail at pane-readiness or injection verification.
- State at claim moment:
  ```yaml
  leader_receiver:
    pane_id: "%D"
    pane_current_command: claude.exe or codex
    pane state: trust prompt / MCP approval / copy-mode / session tree
  scheduled_events/result_watchers:
    exhausted result res_1
  leader_notification_log:
    should remain absent until a real visible injection is claimed
  ```
- Expected outcome: If the recovered notification cannot be injected, the framework shows or records a clear recovery-failed incident and keeps the result recoverable. It must not silently mark it delivered.
- Failure mode if recovery layer is wrong: Silent loss if recovery consumes the result before actual pane visibility. Delayed-too-long if no one tells the user the claimed pane is not input-ready. Duplicate if multiple readiness retries later inject after a partial attempt.

### 15. Same result after explicit takeover to a different leader session UUID

- What the user did: Original owner never claims; another user intentionally runs takeover with confirmation, creating a different leader session UUID. The abandoned result still belongs to the team.
- Where the framework gave up: Delivery exhausted under the old owner session. `leader_notification_log` may have no row for old UUID, or a row for old UUID if a partial/visible delivery happened before takeover.
- State at claim/takeover moment:
  ```yaml
  team_owner:
    leader_session_uuid: uuid-B
    claimed_via: takeover
  exhausted_delivery:
    result_id: res_1
    original_leader_session_uuid: uuid-A
  leader_notification_log:
    maybe row [res_1, uuid-A], no row [res_1, uuid-B]
  ```
- Expected outcome: If the result was never visible to the old owner, the new owner receives it once. If it was visible to the old owner, policy must be explicit: either replay once for the new UUID with audit, or do not replay with an explicit "already delivered to previous owner" incident. It must not disappear silently.
- Failure mode if recovery layer is wrong: Duplicate across users without audit, or silent loss because the old UUID's state blocks the new owner from seeing unfinished work.

### 16. User runs status/doctor/stuck-list after exhaustion but before claim

- What the user did: Seeing no result, user runs `team-agent status`, `doctor`, or `stuck-list` before claiming a pane.
- Where the framework gave up: A result notification has already exhausted, but no active retry remains.
- State at claim moment:
  ```yaml
  scheduled_events:
    failed result notification exists
  result_watchers:
    delivery_exhausted or absent
  status surfaces:
    may or may not expose "unnotified result"
  ```
- Expected outcome: Diagnostic commands should reveal that there is an undelivered leader-bound result and should not mutate recovery state. After claim, the result still delivers once.
- Failure mode if recovery layer is wrong: The diagnostic read accidentally suppresses or revives the result; or status says the team is healthy while a completed result is invisible.

## Scenario Count

16 scenarios enumerated.
