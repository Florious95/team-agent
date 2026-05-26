# Developer Scenarios: claim-leader Recovery After Exhaustion

Role: `developer` / Runtime Implementation Developer
Date: 2026-05-26
Worktree: `/tmp/team-agent-public-0.2.2-slice-1`
Observed HEAD during read-only analysis: `3aee4f3`

## Scope

This is scenarios only, not a fix proposal. I enumerated the user-visible failure shapes where a leader-bound result has already hit a retry give-up state, then `team-agent claim-leader --confirm` changes the binding. The duplicate-delivery gate from `945948b` remains an input constraint: leader result notifications dedupe at the injection boundary by `(result_id, leader_session_uuid)`.

The real failure that anchors these scenarios is Mac mini run `res_6096baac8fdf`, local run `0.2.2-slice-1-20260526T015116Z`: result `res_8362140758fd` was stored, its scheduled result notification exhausted three ambiguous-leader attempts, the user claimed pane `%533`, `claim-leader` returned `requeued_watchers: []`, and no post-claim `leader_receiver.deliver_attempt` or `leader_notification_log` row appeared.

## Code Path Map

- `src/team_agent/messaging/results.py:191-227`: `report_result` stores the result and asks `_notify_leader_of_report_result` to queue a leader notification.
- `src/team_agent/messaging/results.py:230-285`: `_notify_leader_of_report_result` creates a `scheduled_events` row with `kind='send'`, `max_attempts=3`, and content containing `Result id: <result_id>`.
- `src/team_agent/messaging/scheduler.py:41-109`: `_fire_due_scheduled_events` delivers pending scheduled sends, schedules retries while `attempt < max_attempts`, and marks the final failed attempt as `status='failed'`.
- `src/team_agent/messaging/scheduler.py:112-130`: `_schedule_send_retry` creates the next scheduled event; no durable link currently promotes terminal failed scheduled result sends into claim recovery.
- `src/team_agent/coordinator/lifecycle.py:250-323`: `coordinator_tick` fires scheduled events before result watcher collection.
- `src/team_agent/messaging/result_delivery.py:19-35`: `retry_result_deliveries` only considers `status='notify_failed'` watchers with `result_id`.
- `src/team_agent/messaging/result_delivery.py:38-128`: `notify_result_watchers` handles watcher delivery, peeks `leader_notification_log`, checks legacy `notified_message_id`, and marks exhausted watcher delivery when the watcher attempt count reaches 5.
- `src/team_agent/messaging/result_delivery.py:320-338`: `_mark_delivery_exhausted` writes `status='delivery_exhausted'`.
- `src/team_agent/messaging/result_delivery.py:404-482`: `requeue_after_claim_leader` scans `result_watchers` for the team and ignores scheduled event rows entirely.
- `src/team_agent/leader/__init__.py:361-428`: `claim_leader` updates `team_owner` and `leader_receiver`, then calls `requeue_after_claim_leader`; its returned `requeued_watchers` surface is blind to scheduled-only result sends.
- `src/team_agent/leader/__init__.py:19-55`: `attach_leader` requeues `delivery_exhausted` watchers globally, not scheduled-only result sends.
- `src/team_agent/messaging/leader.py:69-305`: `_send_to_leader_receiver` creates the leader message row, validates/rebinds the receiver, and only reaches `leader_receiver.deliver_attempt` after the injection-boundary dedupe gate has allowed the result.
- `src/team_agent/messaging/leader.py:170-230`: the injection-boundary gate writes or reads `leader_notification_log` before tmux injection is attempted.
- `src/team_agent/message_store/core.py:239-291`: scheduled events are durable, but terminal status is just `failed` with `result_json`.
- `src/team_agent/message_store/result_watchers.py:47-52`: retryable watchers are only `pending` and `notify_failed`.
- `src/team_agent/message_store/result_watchers.py:89-105`: `attach_leader` can move all `delivery_exhausted` watchers to `notify_failed`; it does not scope by team and does not include scheduled events.
- `src/team_agent/message_store/leader_notification_log.py:17-68`: the approved dedupe table atomically claims `(result_id, leader_session_uuid)` and stores `notified_message_id`.

## Scenario 01 - Scheduled Event Exhausts, No Watcher Exists

Category coverage: (a)

What the user did:
The worker finishes a normal task and calls `report_result` while the leader receiver is ambiguous. The user sees the ambiguous-candidates incident, waits long enough for the scheduled send retries to run out, then claims pane `%D`.

Where the framework gave up:
The report-result path created a `scheduled_events` row through `_notify_leader_of_report_result`. `_fire_due_scheduled_events` tried the send, got `reason='ambiguous'`, created retry rows, and finally marked the last row `status='failed'`. No `result_watchers` row exists for this fire-and-forget result notification.

The moment claim-leader fires:
`state.team_owner` and `state.leader_receiver` are updated to `%D`; `leader_notification_log` has no row for the result; `result_watchers(owner_team_id=team)` returns no eligible row; `scheduled_events` contains only terminal failed rows with `Result id: <id>` inside `payload_json`.

Expected outcome:
The claimed pane receives the stored result exactly once, and there is visible/auditable evidence that the abandoned scheduled result was recovered.

Failure mode if recovery layer is wrong:
Silent loss. This is the observed `res_8362140758fd` shape: durable result row exists, the user claimed a leader, but no post-claim `leader_receiver.deliver_attempt` fired.

## Scenario 02 - Watcher Exhausts First, Scheduled Event Still Has Budget

Category coverage: (b)

What the user did:
The user sends work with result watching enabled, and the worker also reports through the normal report-result path. Ambiguity persists while both paths are active. The watcher attempt counter reaches its budget first; a scheduled event retry is still pending.

Where the framework gave up:
`notify_result_watchers` marks the watcher `delivery_exhausted`, while `_fire_due_scheduled_events` still has a pending or `retry_scheduled` row for the same `result_id`.

The moment claim-leader fires:
The watcher is terminal, the scheduled row is still live, and `leader_notification_log` has no row. Claim has one dead source and one pending source for the same user-visible result.

Expected outcome:
The user sees one result notification. It is acceptable for either source to win the actual injection, but the other source must converge on the same exactly-once outcome and not remain able to surprise the user later.

Failure mode if recovery layer is wrong:
Duplicate if claim requeues the watcher immediately and the already-pending scheduled retry later injects a second copy. Partial recovery if only the scheduled path succeeds and the stale watcher later reawakens after restart or attach.

## Scenario 03 - Both Scheduled Event and Watcher Exhaust Before Claim

Category coverage: (c)

What the user did:
The user leaves two leader panes open for several minutes. Multiple delivery paths try to report the same worker result while ambiguity remains unresolved. The user claims a pane only after all retry budgets are consumed.

Where the framework gave up:
The scheduled-event branch has terminal `status='failed'`. The watcher branch has terminal `status='delivery_exhausted'`. Neither row is selected by ordinary coordinator retry scans.

The moment claim-leader fires:
There are two abandoned durable representations of the same result. One is a failed scheduled event whose payload contains `Result id: <id>`. The other is a watcher row with the same `result_id`, no visible successful notification, and possibly `notified_message_id` null.

Expected outcome:
The result appears on the claimed pane exactly once. The framework can audit that two abandoned delivery records existed, but the user gets one canonical result envelope.

Failure mode if recovery layer is wrong:
Silent loss if neither terminal class is scanned. Duplicate if both classes are reanimated independently and both reach `_send_to_leader_receiver`.

## Scenario 04 - delivery_exhausted Watcher With NULL notified_message_id

Category coverage: (d)

What the user did:
The user waits for a watched task result while the leader receiver is ambiguous. Every watcher delivery attempt fails before any leader message is visible. The user claims one of the candidate panes after watcher exhaustion.

Where the framework gave up:
`_mark_delivery_exhausted` changed `result_watchers.status` to `delivery_exhausted`, preserved `notified_message_id=NULL`, and emitted `result_delivery_exhausted`.

The moment claim-leader fires:
`requeue_after_claim_leader` can see the watcher if it is team-scoped and at-or-after the incident timestamp. The watcher is not retryable through `retry_result_deliveries` until some claim/attach path changes its status back to `notify_failed`.

Expected outcome:
The claimed pane receives the result once because a null `notified_message_id` means there is no durable evidence of a successful pane delivery.

Failure mode if recovery layer is wrong:
Silent loss if `delivery_exhausted` is treated as final. Delayed-too-long if recovery writes only a state transition but does not cause an immediate retry or a bounded next tick.

## Scenario 05 - delivery_exhausted Watcher With Non-NULL notified_message_id

Category coverage: (e)

What the user did:
A watched result attempted delivery before claim. A legacy or partial path set `notified_message_id`, but the user never saw the result on a valid leader pane. The user then claims the intended pane.

Where the framework gave up:
The watcher is `delivery_exhausted` or otherwise terminal, and `notified_message_id` is non-null. `leader_notification_log` may be absent, or may contain a row whose pane/message never became visibly submitted.

The moment claim-leader fires:
`requeue_after_claim_leader` currently skips watchers with `notified_message_id`. `notify_result_watchers` also has a legacy canonical check that can treat an existing `notified_message_id` as a dedupe reason before the injection boundary is reached.

Expected outcome:
The user-visible delivery state controls the outcome. If the old message was actually visible in the same leader session, claim must not duplicate it. If the old id came from a failed or partial attempt, claim must still recover the result once and audit why the stale id did not count.

Failure mode if recovery layer is wrong:
Silent loss if any non-null `notified_message_id` is treated as proof of user-visible delivery. Duplicate if all stale ids are ignored without checking message status, pane identity, and dedupe log.

## Scenario 06 - Ambiguous Window Lasts 30 Minutes

Category coverage: (f)

What the user did:
The leader sees an ambiguous-candidates prompt and steps away. Some workers finish 29 minutes before claim; another finishes 1 minute before claim. The user finally runs `claim-leader --confirm`.

Where the framework gave up:
Older result notifications have exhausted scheduled and/or watcher budgets. Newer notifications may still be pending, retry-scheduled, or not yet attempted. The incident timestamp is much older than some results and older than normal retry windows.

The moment claim-leader fires:
The database has a mixed set of terminal, live, and maybe never-attempted rows. The event log has one or more ambiguous incident events, and `claim_leader` chooses the latest one from the tail.

Expected outcome:
Every undelivered result that belongs to the current ambiguous interval is handled exactly once, including both the old and recent result, or the claim output/event log explicitly says which result was deliberately not revived.

Failure mode if recovery layer is wrong:
Partial recovery if an incident timestamp filter drops older-but-undelivered results. Duplicate or stale replay if an unbounded scan revives results from prior, already-resolved incidents.

## Scenario 07 - Coordinator Restart Between Exhaustion and Claim

Category coverage: (g)

What the user did:
The worker result exhausts delivery under ambiguity. Then the coordinator restarts, the Mac sleeps, or the process is relaunched. The user claims a leader pane after the restart.

Where the framework gave up:
The only remaining evidence is durable DB state: failed scheduled events, exhausted watcher rows, stored result rows, message rows, and event log history. In-memory retry counters and pending work are gone.

The moment claim-leader fires:
`claim_leader` runs in a fresh process. It can load state and the message store, but only code that explicitly scans durable terminal rows can find the abandoned result. `retry_result_deliveries` will not select `delivery_exhausted` without a requeue, and scheduled failed rows are not selected by `due_scheduled_events`.

Expected outcome:
Recovery is durable across coordinator restarts. The result appears once on the claimed pane, and the audit trail says it was recovered from persisted terminal state.

Failure mode if recovery layer is wrong:
Silent loss if recovery relied on process memory or pending-only queries. Duplicate if restart resets attempt counts and revives multiple stale delivery paths without consulting the injection-boundary dedupe gate.

## Scenario 08 - Cross-Team Claim With Shared leader_notification_log

Category coverage: (h)

What the user did:
The same workspace has Team A and Team B. Team A has an exhausted result while ambiguous. The user claims Team B's leader pane, or Team B independently resolves ambiguity.

Where the framework gave up:
Team A's scheduled event or watcher is terminal and scoped with `owner_team_id=teamA`. Team B has its own owner identity and claim event. `leader_notification_log` is a shared table with an `owner_team_id` column but the dedupe key is `(result_id, leader_session_uuid)`.

The moment claim-leader fires:
The Team B claim updates Team B state and may run recovery scans. Team A's abandoned rows must remain untouched unless the user claims Team A.

Expected outcome:
Team B claim does not revive Team A's result and does not suppress it. Team A claim later recovers Team A's result exactly once for Team A.

Failure mode if recovery layer is wrong:
Cross-team leak if Team A's result appears in Team B's pane. Silent loss if Team B writes a dedupe row or status transition that makes Team A think its result was already delivered.

## Scenario 09 - Claim Happens While a Scheduled Retry Is Pending

What the user did:
The user reacts quickly: a first scheduled attempt failed under ambiguity and scheduled a retry, but the retry has not fired yet. The user claims the intended pane before the next retry tick.

Where the framework gave up:
It has not fully given up yet. The first row is `retry_scheduled`; the next row is `pending`. Any watcher may also be pending or `notify_failed`.

The moment claim-leader fires:
There is a newly valid leader receiver and a pending scheduled send that will eventually become due. A claim-time recovery path may also try immediate delivery for the same `result_id`.

Expected outcome:
The result appears once soon after claim. The user should not wait for the full retry interval if claim resolved the blocker, but the pending scheduled retry must not create a second visible copy.

Failure mode if recovery layer is wrong:
Delayed-too-long if claim does nothing and the user waits for the scheduled interval. Duplicate if immediate claim recovery and the existing scheduled retry both pass through as independent deliveries.

## Scenario 10 - Post-Claim Injection Claim Row Exists but tmux Injection Fails

What the user did:
The user claims a pane after exhaustion. Recovery attempts to deliver the result to the newly claimed pane, but the pane is in copy mode, at an approval prompt, stale, or otherwise cannot accept injected text.

Where the framework gave up:
The pre-claim delivery gave up under ambiguity. The post-claim delivery reaches `_send_to_leader_receiver`. At `leader.py:181-192`, `leader_notification_log` may claim `(result_id, leader_session_uuid)` before `_tmux_inject_text` runs at `leader.py:247-253`. If tmux injection then fails, the log row can exist without a visible result.

The moment claim-leader fires:
The user sees a successful claim, but the new delivery attempt cannot complete. Future watcher retries may peek `leader_notification_log` and infer that the result was already notified.

Expected outcome:
The user either sees the result exactly once, or sees an explicit recover-failed event while the result remains recoverable. A pre-injection dedupe claim must not make a failed injection look delivered.

Failure mode if recovery layer is wrong:
Silent loss if the log row suppresses all future retries even though the claimed pane never received the result. Duplicate if the recovery ignores the log entirely and later attempts race.

## Scenario 11 - Claim Recovery Raises After State Binding Succeeds

What the user did:
The user claims a pane, and the claim writes the new `leader_receiver` successfully. During recovery, the message store is locked, corrupt, or a delivery helper raises.

Where the framework gave up:
The original scheduled event or watcher was exhausted. Then `claim_leader` has already saved the new state and emitted `leader_receiver.claim_applied`, but `requeue_after_claim_leader` or an immediate retry throws or returns nothing.

The moment claim-leader fires:
State says the leader is bound to the new pane. The user sees `claim-leader` succeed or partly succeed. The abandoned result is still terminal in the store, and no visible pane notification exists.

Expected outcome:
The user-visible claim result and event log distinguish "binding succeeded" from "delivery recovery still pending/failed"; the result is not silently considered handled.

Failure mode if recovery layer is wrong:
Partial recovery. The user believes claim fixed the team, but the completed result remains unreachable until manual database inspection or a new worker report.

## Scenario 12 - Multiple Results Accumulate While Ambiguous

What the user did:
Several workers finish while the leader is ambiguous. The user claims a pane after some delivery budgets have exhausted and others remain pending.

Where the framework gave up:
The store contains multiple result ids across scheduled events, result watchers, and message rows. Some rows are terminal, some pending. A few rows may refer to the same result id via different paths.

The moment claim-leader fires:
Recovery must classify rows by distinct result id, owner team, visibility state, and leader session UUID. The old code surface returns only `requeued_watchers`, which is not enough to describe scheduled-only results or per-result outcomes.

Expected outcome:
Each undelivered result appears exactly once on the claimed pane, and the order is stable enough for the user to correlate worker outcomes.

Failure mode if recovery layer is wrong:
Partial recovery if only the latest result or only watcher-backed results are delivered. Duplicate if multiple abandoned rows for one result become separate pane injections.

## Scenario 13 - attach-leader Used Instead of claim-leader

What the user did:
The result exhausts while the old leader receiver is missing or ambiguous. Instead of using `claim-leader`, the user runs `attach-leader` to point at a pane.

Where the framework gave up:
The original delivery may have failed as a scheduled event, a watcher, or both. `attach_leader` currently calls `requeue_delivery_exhausted_watchers`, which only covers watcher rows and does not inspect failed scheduled result sends.

The moment attach/claim recovery fires:
For watcher-only exhaustion, attach can reset status to `notify_failed`. For scheduled-only exhaustion, attach has no matching recovery surface. In multi-team state, `requeue_delivery_exhausted_watchers` also does not take `owner_team_id`.

Expected outcome:
The user gets the same "completed result is not lost" behavior regardless of whether the recovery trigger is claim or attach, with team scope preserved.

Failure mode if recovery layer is wrong:
Silent loss for scheduled-only report_result paths. Cross-team or stale replay if attach globally requeues watchers that belong to another team.

## Scenario 14 - Loser Pane Attempts Claim After Winner Recovered

What the user did:
Two candidate leader panes race. Pane `%D` wins claim and starts recovery. Pane `%E` runs `claim-leader --confirm` shortly afterward and loses because the owner epoch advanced.

Where the framework gave up:
The original result had exhausted before any claim. Winner recovery may be in progress or already delivered. The loser claim enters `_claim_lost_race`.

The moment loser claim fires:
State has `owner_epoch=1` and `leader_receiver.pane_id='%D'`. The abandoned result may already have a `leader_notification_log` row for the winning delivery, or may still be in a recovery-pending state.

Expected outcome:
The losing pane must not revive, suppress, or reroute the abandoned result. Pane `%D` is the only pane that can receive the recovered result for that owner epoch.

Failure mode if recovery layer is wrong:
Duplicate if loser claim also triggers recovery. Silent loss if loser claim marks the result as already handled while winner delivery is still pending or failed.

## Scenario 15 - Legacy or NULL owner_team_id Rows During Same-Workspace Multi-Team

What the user did:
The user upgraded from older state. An exhausted scheduled event or watcher was created before strict `owner_team_id` partitioning, so the row has `owner_team_id=NULL`. The user claims a pane for one team in a multi-team workspace.

Where the framework gave up:
The old row is terminal, contains a result id, and may be readable by multiple team-scoped queries because legacy `NULL` rows are treated as open access in several message-store paths.

The moment claim-leader fires:
The team owner identity is known, but the abandoned row lacks strong team ownership. The stored result envelope may still include enough task/agent context to identify its team, or it may not.

Expected outcome:
The correct team receives its undelivered result exactly once, and the other team is not affected. If the legacy row cannot be safely classified, the user gets an explicit recovery-required incident instead of silent loss or cross-team replay.

Failure mode if recovery layer is wrong:
Cross-team leak if a legacy row is revived into the wrong leader pane. Silent loss if all legacy rows are ignored even though they contain the user's only durable result notification.

## Scenario 16 - Explicit Takeover Changes leader_session_uuid After Exhaustion

What the user did:
The original owner never resolves ambiguity. Another authorized user intentionally runs takeover or otherwise changes the stored team owner UUID after an exhausted result exists.

Where the framework gave up:
The abandoned delivery was tied to the old `leader_session_uuid`. `leader_notification_log` may be empty, or may contain a row for the old UUID if a partial delivery path reached the dedupe gate.

The moment takeover/claim recovery fires:
The active owner UUID is now different. The approved dedupe key treats `(result_id, old_uuid)` and `(result_id, new_uuid)` as distinct.

Expected outcome:
Policy must be explicit in the recovery outcome: either replay once to the new owner because the result was never visible to any current owner, or show a clear "already delivered to prior owner" incident if that is the intended boundary. The result must not simply vanish.

Failure mode if recovery layer is wrong:
Duplicate across owners without audit if both UUIDs receive the same result. Silent loss if an old UUID row suppresses delivery to the new legitimate owner.

## Scenario 17 - Fallback Inbox Has the Result but Pane Delivery Never Happened

What the user did:
During ambiguity or missing-pane failure, the system wrote a fallback leader inbox entry for the result. The user later claims a pane and expects the normal leader screen to catch up.

Where the framework gave up:
`_fail_leader_delivery` can preserve a message/fallback artifact while the scheduled event or watcher still enters terminal failure. No injection-boundary `leader_notification_log` row exists for the valid pane delivery.

The moment claim-leader fires:
There is durable evidence in a fallback file or message row, but not in the actual leader pane. A recovery classifier may mistake "stored somewhere" for "delivered to the leader screen."

Expected outcome:
Fallback evidence remains audit-only. Claim still delivers the result to the leader pane exactly once, unless there is concrete proof that the leader pane already saw it.

Failure mode if recovery layer is wrong:
Silent loss if fallback storage is treated as visible delivery. Duplicate if fallback content and recovered pane injection are both pushed as separate user-visible result messages.

## Scenario 18 - Result Arrives During claim-leader Critical Section

What the user did:
The user runs `claim-leader --confirm` at the same time a worker calls `report_result`. The old receiver is ambiguous when the result is queued, but the new receiver is committed before or during the first delivery attempt.

Where the framework gave up:
Depending on interleaving, the scheduled event may be created before the state update, after the state update, or between `claim_applied` and `requeue_after_claim_leader`. The result may not be included in the incident timestamp window that claim recovery uses.

The moment claim-leader fires:
State and DB transitions overlap: result row exists or is about to exist, scheduled event exists or is about to exist, and claim recovery may scan before the result's row is visible.

Expected outcome:
The result appears once after the receiver is bound. If the claim-time scan missed the just-arriving result, the ordinary scheduled path must still deliver it through the new binding without needing another manual action.

Failure mode if recovery layer is wrong:
Silent loss if the scheduled event captures the ambiguous old state and the claim recovery scan ran too early to see it. Duplicate if both the in-flight scheduled path and claim recovery deliver independently.

## Scenario Count

18 scenarios enumerated.
