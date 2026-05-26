# Test-Engineer Scenarios: Claim-Leader Recovery After Exhaustion

Date: 2026-05-26
Role: test-engineer / Runtime Test Engineer
Scope: user-visible failure scenarios where a worker result has exhausted one or more delivery retry budgets while the leader pane is ambiguous, then the user runs `team-agent claim-leader --confirm`.

This file lists scenarios only. It does not propose the recovery design. The duplicate-delivery arm is treated as closed by `945948b`; every expected outcome below composes with the approved injection-boundary dedupe key `(result_id, leader_session_uuid)`.

## Read-Only Observations From Current Code

- `report_result` queues a scheduled `kind="send"` event for the leader notification, with `max_attempts=3`.
- `_fire_due_scheduled_events` marks the current scheduled row `retry_scheduled` while it creates the next retry row; when `attempt >= max_attempts`, the current row becomes `failed`.
- Result watcher retry has a separate budget: `notify_result_watchers` marks a watcher `delivery_exhausted` after `_RESULT_DELIVERY_MAX_ATTEMPTS = 5`.
- `retry_result_deliveries` only acts on watcher rows with `status == "notify_failed"` and a non-empty `result_id`.
- `requeue_after_claim_leader` scans result watchers for the claimed team and skips rows with any `notified_message_id`. It does not scan failed scheduled events.
- `attach_leader` has a separate `requeue_delivery_exhausted_watchers` path; `claim-leader` uses `requeue_after_claim_leader`.
- The `945948b` gate lives at `_send_to_leader_receiver`, via `leader_notification_log`. It prevents duplicate visible injection but does not itself discover abandoned obligations.

## Scenario 1 - Path A Scheduled Event Exhausted, No Watcher Fired

- What the user did: I told a worker to do work, the worker reported a result while my leader pane was ambiguous, waited until retries stopped, then ran `team-agent claim-leader --confirm`.
- Where the framework gave up: Path A, the scheduled `kind="send"` event created by `_notify_leader_of_report_result`, consumed all attempts and ended as `status=failed`; Path B never matched a result watcher, or no watcher existed because the report was fire-and-forget.
- The moment claim-leader fires: `leader_receiver` moves from ambiguous/stale pane to the claimed pane; `scheduled_events` contains only dead rows for `Result id: res_A1`; `result_watchers` has no eligible row for that result; `leader_notification_log` has no row for `(res_A1, uuid_A)`.
- Expected outcome: the user sees the result on the newly claimed pane exactly once.
- Failure mode if recovery layer is wrong: silent loss if recovery only scans watchers; delayed-too-long if it waits for a dead scheduled row that will never become due again; duplicate if the dead scheduled row and a late-created watcher are both revived independently.
- Cheap-gate test surface: construct a failed scheduled event containing `Result id: res_A1`, no watcher rows, then simulate claim and assert exactly one post-claim deliverable obligation is observable.

## Scenario 2 - Path B Watcher Exhausted, Scheduled Event Still Has Budget

- What the user did: I waited through several failed result-watcher deliveries, then claimed the correct leader pane while the original scheduled notification still had a retry remaining.
- Where the framework gave up: Path B, `result_watchers.status=delivery_exhausted`; Path A still has a pending scheduled retry row with attempt `< max_attempts`.
- The moment claim-leader fires: watcher row has `result_id=res_B1`, `notified_message_id=null`; scheduled row is still `pending`; no `leader_notification_log` row exists.
- Expected outcome: the user sees one result notification after claim, not one from watcher recovery and another from the scheduled retry.
- Failure mode if recovery layer is wrong: duplicate if both paths are revived but not collapsed by the injection-boundary gate; silent loss if watcher exhaustion makes claim skip and the scheduled row is later marked failed before the pane is valid.
- Cheap-gate test surface: one exhausted watcher plus one pending scheduled event for the same result; run a deterministic post-claim recovery/tick sequence and count submitted result payloads.

## Scenario 3 - Both Scheduled Event And Watcher Exhaust Simultaneously

- What the user did: I was away while the leader pane stayed ambiguous; by the time I claim, every automatic retry budget for the result has been consumed.
- Where the framework gave up: Path A final scheduled row is `failed`; Path B watcher row is `delivery_exhausted`.
- The moment claim-leader fires: `result_watchers=[{status: delivery_exhausted, result_id: res_C1, notified_message_id: null}]`; `scheduled_events` has final `failed` or prior `retry_scheduled` rows; no leader log row; result row is stored.
- Expected outcome: claiming the leader pane wakes the abandoned result and delivers it exactly once.
- Failure mode if recovery layer is wrong: silent loss if both queues are terminal and neither is considered recoverable; duplicate if both terminal queues are independently reset and each sends.
- Cheap-gate test surface: build both terminal states before claim; assert the post-claim state produces exactly one `leader_receiver.deliver_attempt` with `res_C1`.

## Scenario 4 - delivery_exhausted Watcher With notified_message_id NULL

- What the user did: I claim a new leader pane after the framework says it stopped trying to notify me about a worker result.
- Where the framework gave up: watcher delivery reached `_RESULT_DELIVERY_MAX_ATTEMPTS` and stored `status=delivery_exhausted`, with no `notified_message_id`.
- The moment claim-leader fires: watcher is scoped to the team, has `result_id=res_D1`, `notified_message_id=null`, and may have `completed_at` earlier than the ambiguity incident timestamp.
- Expected outcome: if the result has not appeared on any leader pane, claim recovery delivers it to the new pane exactly once.
- Failure mode if recovery layer is wrong: silent loss if `delivery_exhausted` is treated as permanently terminal; delayed-too-long if it remains terminal until a manual attach-leader path runs.
- Cheap-gate test surface: one exhausted watcher with null notified id; claim should create one observable recovery attempt.

## Scenario 5 - delivery_exhausted Watcher With Legacy notified_message_id Set

- What the user did: I claim after exhaustion, but an older partial attempt left a `notified_message_id` on the watcher even though I never saw the result.
- Where the framework gave up: watcher row ended `delivery_exhausted` and carries `notified_message_id=msg_old` from legacy behavior or a partial attempt; no matching `leader_notification_log` row may exist.
- The moment claim-leader fires: watcher has `result_id=res_E1`, `status=delivery_exhausted`, `notified_message_id=msg_old`; message `msg_old` may be `failed`, `ambiguous`, `injected_unverified`, or missing.
- Expected outcome: if `msg_old` was not a verified visible leader notification, the user sees one recovery notification after claim; if it was verified visible for the same `leader_session_uuid`, the user sees no duplicate.
- Failure mode if recovery layer is wrong: silent loss if any non-null `notified_message_id` blocks recovery; duplicate if verified delivered evidence is ignored.
- Cheap-gate test surface: parameterize `msg_old` statuses and leader log presence; assert the user-visible result count, not just watcher status.

## Scenario 6 - Ambiguous For 30 Minutes, Result Arrived Early

- What the user did: I leave the machine with ambiguous leader candidates; a worker reports one minute after ambiguity starts; I claim thirty minutes later.
- Where the framework gave up: scheduled retry budget and possibly watcher budget both exhaust long before claim.
- The moment claim-leader fires: result and watcher timestamps are close to the ambiguity incident but far earlier than claim; final state may be `scheduled_events.failed` and watcher `delivery_exhausted`.
- Expected outcome: the old but still undelivered result appears on the claimed pane exactly once.
- Failure mode if recovery layer is wrong: silent loss if recovery filters by a short age window or only considers events near claim time; delayed-too-long if it waits for another unrelated coordinator progress event.
- Cheap-gate test surface: freeze timestamps so result is 29 minutes old relative to claim; assert age alone does not make an undelivered result unrecoverable.

## Scenario 7 - Ambiguous For 30 Minutes, Result Arrived Just Before Claim

- What the user did: I claim a leader pane just after a worker result arrives during an already long ambiguous period.
- Where the framework gave up: scheduled path may not yet be exhausted; watcher path may still be `pending`, but the ambiguity incident is old.
- The moment claim-leader fires: watcher may have `created_at` after incident timestamp but `result_id` may still be null if collection has not matched it; scheduled event has fresh `pending` row.
- Expected outcome: the recent result is delivered to the claimed pane exactly once without waiting for the full original retry budget.
- Failure mode if recovery layer is wrong: delayed-too-long if claim ignores active pending work and lets it continue through slow scheduled retries; silent loss if watcher has no `result_id` at claim time and the scheduled row later fails.
- Cheap-gate test surface: pending scheduled event plus pending watcher with/without `result_id`; claim should not strand either shape.

## Scenario 8 - Coordinator Restart Between Exhaustion And Claim

- What the user did: The team kept running, coordinator restarted after retries gave up, then I claimed a leader pane.
- Where the framework gave up: terminal scheduled event and/or watcher state was written before restart; in-memory retry intent is gone.
- The moment claim-leader fires: persisted SQLite rows are the only source of truth; coordinator memory contains no pending timers; result row may be marked collected.
- Expected outcome: persisted exhausted delivery state is recoverable and the user sees one result after claim.
- Failure mode if recovery layer is wrong: silent loss if recovery depended on in-memory scheduled retry state; duplicate if restart recreates a retry while claim also revives the same result.
- Cheap-gate test surface: write terminal DB rows, instantiate fresh store/runtime objects, then simulate claim.

## Scenario 9 - Team A Exhausted, Team B Claims

- What the user did: In one workspace I have Team A and Team B; Team A loses a result under ambiguity, while Team B runs `claim-leader --confirm`.
- Where the framework gave up: Team A's scheduled event or watcher exhausted; Team B may have no pending result at all.
- The moment claim-leader fires: workspace has `teams={team_A, team_B}`; Team A rows are `owner_team_id=team_A`; Team B claim updates only Team B `team_owner` and `leader_receiver`; `leader_notification_log` is shared but scoped by owner metadata.
- Expected outcome: Team B claim does not revive, deliver, suppress, or mutate Team A's abandoned result; Team A result remains recoverable when Team A's leader claims.
- Failure mode if recovery layer is wrong: cross-team leak if Team A result appears in Team B pane; silent loss if Team B's claim marks Team A result as handled; duplicate if both teams later recover the same row.
- Cheap-gate test surface: multi-team DB fixture with one exhausted Team A result and a Team B claim; assert per-team row and pane counts.

## Scenario 10 - Failed Scheduled Row Has No Matching Watcher Because Result Was Ad-Hoc

- What the user did: A worker sends a direct `report_result` not tied to a leader-created task watcher; I claim after the scheduled notification exhausts.
- Where the framework gave up: only Path A exists; scheduled event is `failed`; no result watcher was ever created.
- The moment claim-leader fires: result table contains `res_A2`; scheduled event payload contains `Result id: res_A2`; `result_watchers=[]`; no leader log row exists.
- Expected outcome: the user still sees the ad-hoc result once after claim.
- Failure mode if recovery layer is wrong: silent loss if the recovery layer assumes every result must have a watcher; duplicate if a synthetic watcher is created while the failed scheduled event is also replayed.
- Cheap-gate test surface: failed scheduled event plus stored result, no watcher; claim should expose a recoverable notification obligation.

## Scenario 11 - Watcher Pending With result_id NULL At Claim Moment

- What the user did: I claim immediately after the worker reports, before coordinator collection links the stored result to the watcher.
- Where the framework gave up: Path A may later exhaust; Path B has not fired because the watcher has no `result_id`.
- The moment claim-leader fires: watcher row is `status=pending`, `result_id=null`, `notified_message_id=null`; result row exists and scheduled event contains `Result id: res_P1`.
- Expected outcome: the result is not lost; after collection/recovery, the leader sees it exactly once.
- Failure mode if recovery layer is wrong: partial recovery if claim marks the watcher `notify_failed` but `retry_result_deliveries` skips it because `result_id` is null; later scheduled exhaustion then causes silent loss.
- Cheap-gate test surface: claim against a pending null-result watcher and a matching stored result/scheduled event; assert eventual single delivery.

## Scenario 12 - Claim Succeeds But Immediate Recovery Attempt Fails Again

- What the user did: I claim a pane, but the newly claimed pane is briefly not ready or tmux injection fails during the immediate recovery attempt.
- Where the framework gave up: the original budget was already exhausted; the claim-triggered attempt also fails.
- The moment claim-leader fires: watcher is revived to `notify_failed` and recovery tries to deliver; `_send_to_leader_receiver` or tmux injection returns failure; scheduled event remains terminal.
- Expected outcome: the user gets explicit evidence that recovery tried and failed, and the result remains recoverable; it must not vanish silently.
- Failure mode if recovery layer is wrong: silent loss if the post-claim failure returns only in CLI output and no durable retry state remains; delayed-too-long if no next retry is scheduled.
- Cheap-gate test surface: patch delivery to fail after claim; assert durable failure evidence and a recoverable row/state remain.

## Scenario 13 - Leader Notification Log Row Exists But No Visible Submission Happened

- What the user did: I claim after an earlier attempt reserved the `(result_id, leader_session_uuid)` gate but failed before visible submission.
- Where the framework gave up: injection-boundary dedupe row was inserted, but the actual pane submission failed or never happened; watcher or scheduled event then exhausted.
- The moment claim-leader fires: `leader_notification_log` has `(res_L1, uuid_A, msg_old)`; message `msg_old` is not verified submitted; watcher may be `delivery_exhausted` or scheduled event `failed`.
- Expected outcome: because the user never saw the result, recovery should produce one visible notification or a clear leader-visible failure, not a silent dedupe skip.
- Failure mode if recovery layer is wrong: silent loss if the dedupe row is treated as proof of user-visible delivery; duplicate if the row is ignored even when the old submission was visible.
- Cheap-gate test surface: seed leader log and a non-submitted message status; assert recovery distinguishes log reservation from visible delivery.

## Scenario 14 - Leader Notification Log Row Pruned Before Late Claim

- What the user did: I claim more than a day after a result notification attempt exhausted.
- Where the framework gave up: original delivery terminal state is old; coordinator maintenance may prune `leader_notification_log` rows older than 24 hours.
- The moment claim-leader fires: failed scheduled/watcher rows may still exist, but dedupe log row may be missing even if the result was previously visible.
- Expected outcome: if the result was never visible, the user sees it once; if it was visible before pruning, recovery should not create an unexplained duplicate without audit evidence.
- Failure mode if recovery layer is wrong: duplicate after log pruning; silent loss if old terminal rows are ignored because their dedupe row is gone.
- Cheap-gate test surface: time-shifted leader log plus terminal rows; verify classification for old-visible versus old-not-visible fixtures.

## Scenario 15 - Multiple Results Exhaust Before One Claim

- What the user did: Several workers finish while my leader pane is ambiguous; I claim once after all retry budgets are exhausted.
- Where the framework gave up: multiple scheduled events and watcher rows independently reach terminal states.
- The moment claim-leader fires: rows for `res_M1`, `res_M2`, and `res_M3` exist with a mix of `failed`, `delivery_exhausted`, `pending`, and `notify_failed`; no result has a verified leader log row.
- Expected outcome: each distinct result appears once on the claimed leader pane; total visible result count equals the number of undelivered results.
- Failure mode if recovery layer is wrong: partial recovery if only the first eligible watcher is retried; duplicate if scheduled and watcher paths both revive per result; delayed-too-long if recovery throttles without evidence.
- Cheap-gate test surface: table-driven fixture with three result ids and mixed terminal states; assert exact per-result counts.

## Scenario 16 - Refused Claim From Wrong Pane While Result Is Exhausted

- What the user did: I accidentally run `claim-leader --confirm` from a pane whose leader UUID does not match the team owner, while a result is exhausted.
- Where the framework gave up: original result delivery exhausted before this refused claim.
- The moment claim-leader fires: `claim_leader` returns `uuid_mismatch` or owner refusal; team `leader_receiver` should not move; exhausted result remains bound to the original team owner context.
- Expected outcome: no result is delivered to the wrong pane; the result remains recoverable when the legitimate owner claims.
- Failure mode if recovery layer is wrong: cross-owner leak if refused claim triggers recovery; silent loss if refused claim marks the result handled; delayed-too-long if legitimate claim later sees no work.
- Cheap-gate test surface: refused claim fixture plus exhausted result; assert no mutation of recovery state except a refusal audit.

## Scenario 17 - Same Result Re-Reported After Exhaustion Before Claim

- What the user did: A worker, seeing no response, reports the same logical result again before I claim the leader pane.
- Where the framework gave up: first result notification exhausted; second `report_result` created another scheduled event and possibly another result row with the same or different `result_id`.
- The moment claim-leader fires: there may be two scheduled rows and one or two watcher rows; leader log has no visible delivery.
- Expected outcome: if the same `result_id` is reused, the user sees one notification; if a new `result_id` represents a second semantic result, the user sees distinct notifications with clear ids.
- Failure mode if recovery layer is wrong: duplicate if same result id is delivered from both attempts; silent loss if the second attempt causes the first exhausted row to be superseded without delivery.
- Cheap-gate test surface: parameterize same-id and new-id re-report fixtures; assert per-result visible counts.

## Scenario 18 - Non-Result Leader Messages Exhaust In The Same Window

- What the user did: While a result is lost under ambiguity, idle/stuck alerts or peer mirrors also try to notify the leader and fail.
- Where the framework gave up: non-result leader messages may have failed scheduled sends; result notification also exhausted.
- The moment claim-leader fires: failed rows include both result content with `Result id:` and non-result content; only result recovery is under this roundtable's exactly-once result invariant.
- Expected outcome: the abandoned result appears once; non-result alert behavior is handled by its own contract and must not mask the result.
- Failure mode if recovery layer is wrong: partial recovery if scanning all leader messages floods the pane with stale alerts and misses the result; false duplicate if test counts every leader message rather than result notifications.
- Cheap-gate test surface: mixed failed scheduled rows; classify by message purpose and `result_id`.

## Scenario 19 - Stored Result Was Collected But Watcher Was Superseded

- What the user did: I had multiple watchers for the same worker/task; the result arrived under ambiguity, one watcher superseded another, then claim occurs after exhaustion.
- Where the framework gave up: one watcher is `superseded`, the primary watcher is `delivery_exhausted` or missing a result id; scheduled event is failed.
- The moment claim-leader fires: result table has `res_S1`; watcher table has mixed `superseded` and terminal rows; no verified leader notification exists.
- Expected outcome: the user sees the result once, not zero and not one per watcher.
- Failure mode if recovery layer is wrong: silent loss if recovery scans only non-superseded rows and the primary is not eligible; duplicate if every historical watcher is revived.
- Cheap-gate test surface: two watchers for one result with one superseded; assert one visible result.

## Scenario 20 - DB Lock Or Write Failure While Marking Terminal State

- What the user did: I claim after the framework appeared to have given up, but one of the terminal-state writes failed during the exhausted retry sequence.
- Where the framework gave up: scheduled retry or watcher retry exhausted in memory, but SQLite write to `failed` or `delivery_exhausted` was interrupted; event log may show failure while DB row remains `pending`, `retry_scheduled`, or `notify_failed`.
- The moment claim-leader fires: persisted state and event log disagree; result may look active in one surface and dead in another.
- Expected outcome: the user sees one result after claim or receives explicit durable recovery-failed evidence; no silent disappearance.
- Failure mode if recovery layer is wrong: delayed-too-long if a stale non-due row is treated as active; duplicate if both event-log-derived and DB-derived recovery paths fire.
- Cheap-gate test surface: inject write failure into terminal marking, then reload store and simulate claim.

## Scenario 21 - Claim Recovery After Worker Was Removed Or Reset

- What the user did: The worker reported a result, delivery exhausted under ambiguity, then I removed or reset that worker before claiming the leader pane.
- Where the framework gave up: scheduled event or watcher is terminal; agent state/health row may be gone.
- The moment claim-leader fires: result row still references old `agent_id`; `state.agents` no longer contains that worker; watcher may be `delivery_exhausted`.
- Expected outcome: because the result was durably reported before removal/reset, the user sees it once after claim.
- Failure mode if recovery layer is wrong: silent loss if recovery requires a live agent/health row; duplicate if reset causes a new watcher and old terminal watcher to both revive.
- Cheap-gate test surface: remove agent state but keep result/watcher rows; claim should recover from durable result state.

## Scenario 22 - Claim Recovery With Legacy Result Missing result_id

- What the user did: A legacy or malformed result notification lacks a parseable `Result id:` line, then delivery exhausts and I claim.
- Where the framework gave up: scheduled and/or watcher retry exhausted, but the injection-boundary dedupe key cannot be computed.
- The moment claim-leader fires: content is leader-bound and user-visible, but `result_id_from_text` returns null; watcher may have `result_id=null`.
- Expected outcome: the user gets one visible recovery or an explicit structured refusal saying the result cannot be safely recovered automatically.
- Failure mode if recovery layer is wrong: silent loss if null result id is skipped everywhere; duplicate if content without a key is retried repeatedly.
- Cheap-gate test surface: malformed notification fixture; assert explicit non-silent outcome rather than silent skip.

## Scenario 23 - Claim Recovery While Scheduled Retry Is Not Due Yet

- What the user did: I claim while the next scheduled retry is delayed for a future timestamp after previous failures.
- Where the framework gave up: scheduled path has not terminally exhausted, but it is sleeping; watcher path may have exhausted.
- The moment claim-leader fires: scheduled row is `pending` with `due_at` in the future; watcher is `delivery_exhausted` or `notify_failed`; leader pane is now valid.
- Expected outcome: the result should not wait unnecessarily for the old delay if claim is the recovery trigger; the user sees it once within the claim recovery window.
- Failure mode if recovery layer is wrong: delayed-too-long if recovery ignores future-due scheduled events; duplicate if immediate recovery fires and the old future event later fires too.
- Cheap-gate test surface: future-due scheduled event plus exhausted watcher; claim sequence followed by future tick must still show one total delivery.

## Scenario 24 - Claim Recovery After Successful Delivery To Old Pane But User Claims New Pane

- What the user did: The framework actually delivered the result to the old pane, but I did not see it and later claim a new pane.
- Where the framework gave up: no give-up occurred for the result; the system has a verified `leader_notification_log` row and submitted message, but the user action is motivated by perceived loss.
- The moment claim-leader fires: log row exists for `(result_id, same leader_session_uuid)` and message status is submitted; watcher may be `notified`.
- Expected outcome: no duplicate result is injected by default; diagnostics should make clear the result was already delivered to the old pane.
- Failure mode if recovery layer is wrong: duplicate if every claim replays old visible results; silent confusion if the user gets no explanation and assumes loss.
- Cheap-gate test surface: verified log row plus claim; assert zero new submissions and visible diagnostic/audit evidence.

## Coverage Summary

Scenario count: 24.

Minimum kickoff categories:

- (a) Path A scheduled-event exhausts first, Path B never fired: Scenarios 1 and 10.
- (b) Path B exhausts first, Path A still has budget: Scenarios 2 and 23.
- (c) Both paths exhaust simultaneously: Scenario 3.
- (d) `delivery_exhausted` watcher with `notified_message_id` null: Scenario 4.
- (e) `delivery_exhausted` watcher with `notified_message_id` set: Scenarios 5 and 13.
- (f) Ambiguous state lasts a long time: Scenarios 6 and 7.
- (g) Coordinator restart between exhaustion and claim: Scenario 8.
- (h) Cross-team shared table isolation: Scenario 9.

## Cheap-Gate Blind Spots

- Fake tmux can prove recovery attempts and event counts, but it cannot prove whether a provider-visible submission happened before the DB recorded it.
- Tests must distinguish "dedupe log row exists" from "user saw the result"; `leader_notification_log` alone is not enough evidence for silent-loss cases.
- Event-log-only classification is unsafe because scheduled rows, watcher rows, and leader log rows can disagree after crash or DB lock faults.
- Result recovery tests should count by `(owner_team_id, result_id, leader_session_uuid)` and then inspect pane/message visibility classification; counting `leader_receiver.deliver_attempt` alone misses silent loss and false duplicates.
- Long-delay behavior needs simulated clocks locally; Mac mini is still required to validate real provider ambiguity, claim timing, and whether visible pane text matched DB status.
