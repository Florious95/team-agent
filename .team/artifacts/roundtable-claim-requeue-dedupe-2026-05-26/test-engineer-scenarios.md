# Test-Engineer Scenarios: Gap 26 x Gap 32 Claim-Requeue Dedupe

Date: 2026-05-26
Role: test-engineer / Runtime Test Engineer
Scope: user-facing scenarios where one logical worker result can become two leader-visible notifications around leader receiver rebinding, claim-leader, scheduled delivery, and result watcher retry.

This document intentionally lists scenarios only. It does not propose the product fix. The test-engineer angle is to make every scenario observable in a cheap gate where possible, and to mark where only Mac mini/provider UI evidence can prove what was actually visible to the leader.

## Entry-Point Inventory From Test Perspective

- `report_result` stores a result envelope, then `_notify_leader_of_report_result` writes a scheduled `kind="send"` event containing the formatted result text and `Result id: <id>`.
- Coordinator scheduled delivery uses `_fire_due_scheduled_events`; for `kind="send"` it calls `delivered_result_message(...)` and then either `deliver_stored_message(...)` or `send_message(...)`.
- Result watcher delivery uses `_collect_results_and_notify_watchers` -> `notify_result_watchers(...)` -> `claim_leader_notification(...)` -> `_deliver_result_to_watcher(...)`.
- `claim-leader --confirm` uses `claim_leader(...)` -> `requeue_after_claim_leader(...)` -> `retry_result_deliveries(...)`.
- `attach_leader(...)` can requeue `delivery_exhausted` watchers without immediately passing through `claim-leader`.
- The actual leader-pane injection event is emitted by `_send_to_leader_receiver(...)` as `leader_receiver.deliver_attempt`, followed by `leader_receiver.submitted` if tmux injection submits.
- Other non-result leader notifications, such as peer mirrors and idle alerts, also flow through the same leader receiver injection path and can confuse event-count based tests if they are not classified separately.

## Scenario 1 - Sequential Single-Pane Watcher Delivery

- User-visible action: I ask one worker to finish a task; the worker reports one result while my leader pane is still the originally attached pane.
- Worker state: worker is `RUNNING`, then calls `report_result` once and returns to idle; no crash or retry.
- State/store snapshot:
  - `team_owner.leader_session_uuid = uuid_A`
  - `team_owner.owner_epoch = 1`
  - `leader_receiver = {type: direct_tmux, pane_id: %100, provider: codex, leader_session_uuid: uuid_A, owner_epoch: 1}`
  - `result_watchers = [{watcher_id: watch_1, owner_team_id: team_A, task_id: task_1, agent_id: worker_1, status: pending, result_id: null, notified_message_id: null}]`
  - no scheduled retry event for this result is due at the same time.
- Code path triggered: `_collect_results_and_notify_watchers` -> `notify_result_watchers` -> `claim_leader_notification` -> `_deliver_result_to_watcher` -> `deliver_stored_message` -> `_send_to_leader_receiver`.
- Expected outcome and exact count: leader sees exactly 1 result notification for `result_id=res_1`; event log has exactly 1 `leader_receiver.deliver_attempt` with that result payload and exactly 1 `leader_receiver.submitted` for that result; watcher ends `status=notified` with one canonical `notified_message_id`.
- Failure mode if classification is wrong: if this baseline is treated as two independent routes, the test can normalize away a duplicate as "scheduled plus watcher"; if it is treated as no leader notification until a watcher exists, the scheduled-only path can be missed.
- Cheap-gate observability: pure fake store plus patched `_tmux_inject_text`; assert payload count by `result_id`, not by total `leader_receiver.deliver_attempt` count.

## Scenario 2 - Sequential Scheduled Report-Result Delivery With No Watcher

- User-visible action: A worker reports a status update or ad-hoc result that was not created by a leader task watcher; I still expect one leader notification.
- Worker state: worker is running normally; one `report_result` call succeeds and no `result_watchers` row matches it.
- State/store snapshot:
  - `team_owner.leader_session_uuid = uuid_A`
  - `leader_receiver.pane_id = %100`
  - `result_watchers = []` or only unrelated watchers for other tasks.
  - `scheduled_events = [{id: ev_1, kind: send, target: leader, owner_team_id: team_A, payload.content contains "Result id: res_2", status: pending}]`
- Code path triggered: `_notify_leader_of_report_result` -> `store.add_scheduled_event`; later `_fire_due_scheduled_events` -> `delivered_result_message` -> `deliver_stored_message` -> `_send_to_leader_receiver`.
- Expected outcome and exact count: leader sees exactly 1 result notification for `res_2`; event log has exactly 1 submitted leader delivery for `res_2`; no watcher row is required to mark completion.
- Failure mode if classification is wrong: a dedupe model that only watches `result_watchers.notified_message_id` will not protect this path; a test that only constructs watchers will miss scheduled-only duplicate delivery.
- Cheap-gate observability: fake scheduled event row with no watcher; count submitted payloads containing `Result id: res_2`.

## Scenario 3 - Same Result Enters Both Scheduled Queue And Watcher Collection

- User-visible action: I dispatch a watched task; the worker calls `report_result`; the framework both queues the report-result notification and later collects the same result for the watcher.
- Worker state: worker completes once; no user-visible retry; framework has two internal obligations for the same `result_id`.
- State/store snapshot:
  - `leader_receiver.pane_id = %100`, `leader_session_uuid = uuid_A`, `owner_epoch = 1`
  - `result_watchers = [{watcher_id: watch_3, owner_team_id: team_A, task_id: task_3, agent_id: worker_1, status: pending, result_id: null, notified_message_id: null}]`
  - `scheduled_events = [{id: ev_3, kind: send, owner_team_id: team_A, payload.content contains "Result id: res_3", status: pending}]`
  - `results = [{result_id: res_3, task_id: task_3, agent_id: worker_1, collected: false}]`
- Code path triggered: scheduled path `_fire_due_scheduled_events` and watcher path `_collect_results_and_notify_watchers` -> `notify_result_watchers` can run in the same coordinator tick.
- Expected outcome and exact count: leader sees exactly 1 result notification for `res_3`; total `leader_receiver.submitted` with `Result id: res_3` is exactly 1 even if both internal paths inspect the same result.
- Failure mode if classification is wrong: the scheduled path can bypass `claim_leader_notification` while the watcher path uses it; each path can produce its own message id, creating the real duplicate class.
- Cheap-gate observability: construct both a due scheduled event and a matching pending watcher; run one coordinator tick; assert payload-level uniqueness by `result_id`.

## Scenario 4 - Claim-Leader Race With Active Scheduled Retry

- User-visible action: My original leader pane becomes ambiguous; I run `team-agent claim-leader --confirm` while a prior result notification retry is already due.
- Worker state: worker already reported the result; worker is idle or unavailable; delivery is being retried by framework, not by the worker.
- State/store snapshot:
  - before claim: `leader_receiver = {pane_id: %old, leader_session_uuid: uuid_A, owner_epoch: 1}`
  - after claim target: `%new` has matching `leader_session_uuid = uuid_A`
  - `team_owner.owner_epoch` advances from `1` to `2`
  - `result_watchers = [{watcher_id: watch_4, owner_team_id: team_A, status: notify_failed, result_id: res_4, notified_message_id: null}]`
  - `scheduled_events = [{id: ev_4, kind: send, owner_team_id: team_A, payload.content contains "Result id: res_4", status: pending or retry_scheduled}]`
- Code path triggered: `claim_leader` -> `requeue_after_claim_leader` -> `retry_result_deliveries`, racing with `_fire_due_scheduled_events`.
- Expected outcome and exact count: before claim, 0 submitted notifications to `%new`; after claim, exactly 1 submitted notification for `res_4` to `%new`; no submitted notification to `%old` after the claim is accepted.
- Failure mode if classification is wrong: the claim path and scheduled retry each believe they are the only valid redelivery path; user sees the same result twice in the newly claimed pane.
- Cheap-gate observability: use two threads or deterministic hooks to interleave claim requeue and scheduled event fire; assert `submitted=true` count is 1.

## Scenario 5 - Leader Pane Changes Mid-Delivery After Message Row Creation

- User-visible action: While a result is being delivered, I move from one leader pane to another or the original pane becomes invalid; then I claim the new pane.
- Worker state: worker has finished; framework is between "created leader message row" and "submitted visible text".
- State/store snapshot:
  - `leader_receiver` starts as `%old`, validation fails with `leader_pane_missing` or `ambiguous`
  - matching candidate `%new` exists with `leader_session_uuid = uuid_A`
  - `result_watchers = [{watcher_id: watch_5, status: pending or notify_failed, result_id: res_5, notified_message_id: claim:<token> or null}]`
  - `messages = [{message_id: msg_attempt_5, recipient: leader, content contains res_5, status: accepted or ambiguous}]`
- Code path triggered: `_send_to_leader_receiver` creates a leader message and validates the receiver; then `_rediscover_leader_receiver` or `claim_leader` rebinds and requeues.
- Expected outcome and exact count: 0 visible submissions before successful rebind; exactly 1 submitted visible result for `res_5` after rebind; failed or ambiguous attempts may exist in the store but must not count as visible leader notifications.
- Failure mode if classification is wrong: if a non-visible attempt is treated as delivered, the user never sees the result; if a visible-but-unrecorded attempt is treated as failed, retry creates a duplicate.
- Cheap-gate observability: local fake can prove store classification; Mac mini is needed to prove whether a borderline tmux injection was actually visible before status write failed.

## Scenario 6 - Two Ambiguous Incidents In Close Succession

- User-visible action: I have two candidate leader panes, run claim once, then the environment emits another ambiguity event before or just after the first claim's requeue.
- Worker state: worker result is already stored; worker is not actively reporting again.
- State/store snapshot:
  - `leader_receiver.ambiguous_incidents = [inc_1 at T1, inc_2 at T2]`
  - `team_owner.owner_epoch` can move from `1` to `2`, and possibly `3` if the second claim is accepted.
  - `result_watchers = [{watcher_id: watch_6, owner_team_id: team_A, status: pending or notify_failed, result_id: res_6, notified_message_id: null before first claim}]`
  - scheduled send for `res_6` may still be pending.
- Code path triggered: `claim_leader` calls `requeue_after_claim_leader` once per accepted claim; coordinator may also process scheduled event between the two claims.
- Expected outcome and exact count: across both incidents and claims, leader sees exactly 1 result notification for `res_6`; if the final owner pane changes, the one visible notification should be in the final accepted leader pane or in the first accepted pane only if it was submitted before the second claim.
- Failure mode if classification is wrong: using incident id as the dedupe unit creates one notification per incident; using only current pane creates one notification per pane.
- Cheap-gate observability: fake event log with two `leader_receiver.ambiguous_candidates` records; run claim twice against same result and count submitted result payloads.

## Scenario 7 - Cross-Team Interleaving In One Workspace

- User-visible action: I run Team A and Team B in the same workspace; both teams receive worker results around the same time, possibly with equal generated `result_id` values in a test fixture.
- Worker state: worker_A and worker_B each report one result; both are legitimate and belong to different teams/leaders.
- State/store snapshot:
  - workspace state has `teams = {team_A, team_B}`
  - Team A: `leader_receiver.pane_id = %A`, `team_owner.leader_session_uuid = uuid_A`, `owner_epoch = 1`
  - Team B: `leader_receiver.pane_id = %B`, `team_owner.leader_session_uuid = uuid_B`, `owner_epoch = 1`
  - `result_watchers` include one row for `(owner_team_id=team_A, result_id=res_same)` and one for `(owner_team_id=team_B, result_id=res_same)`
- Code path triggered: any combination of `_fire_due_scheduled_events`, `notify_result_watchers`, and `requeue_after_claim_leader` scoped by `owner_team_id`.
- Expected outcome and exact count: Team A leader sees exactly 1 Team A result; Team B leader sees exactly 1 Team B result; total across workspace is exactly 2, one per owner team.
- Failure mode if classification is wrong: global `result_id` dedupe suppresses one team's legitimate result; missing team scoping sends both notifications to one leader pane or duplicates within one team.
- Cheap-gate observability: multi-team fake state with two receivers; assert counts per `(owner_team_id, result_id, target_pane)`.

## Scenario 8 - Coordinator Restart Mid-Delivery

- User-visible action: A coordinator process restarts while a result notification is in progress; I do not manually resend anything.
- Worker state: worker already reported; no additional worker activity after restart.
- State/store snapshot:
  - `leader_receiver.pane_id = %100`
  - `result_watchers = [{watcher_id: watch_8, status: pending or notify_failed, result_id: res_8, notified_message_id: claim:<token> or null}]`
  - `scheduled_events` may have `ev_8` in `pending`, `retry_scheduled`, or unknown state depending on crash point.
  - message row may exist for `res_8` with status `accepted`, `submitted`, or no row if crash happened earlier.
- Code path triggered: first process enters `notify_result_watchers` or `_fire_due_scheduled_events`; second process restarts and runs coordinator tick, scheduled delivery, and retry delivery again.
- Expected outcome and exact count: after restart convergence, user sees exactly 1 visible result for `res_8`; if no visible delivery happened before crash, one retry is allowed; if visible delivery happened before crash, zero additional visible retries are allowed.
- Failure mode if classification is wrong: stale sentinel causes permanent loss; stale null state causes duplicate; scheduled event replay plus watcher retry produce two submitted messages.
- Cheap-gate observability: fake crash checkpoints can cover DB state; Mac mini is needed for a crash after text submission but before SQLite promotion.

## Scenario 9 - Worker Crashes After Successful report_result Before Framework Observes Notification

- User-visible action: A worker submits a result, then its process dies or resets; after recovery it may replay the same result envelope.
- Worker state: first worker instance completed `report_result`; second instance starts fresh or resumes and may emit the same `result_id`.
- State/store snapshot:
  - `results` table contains `res_9` once or twice depending on idempotent insert behavior.
  - `scheduled_events` may contain `ev_9a` and `ev_9b`, both with content containing `Result id: res_9`.
  - `result_watchers = [{watcher_id: watch_9, owner_team_id: team_A, status: pending, result_id: null or res_9, notified_message_id: null}]`
  - `leader_receiver.pane_id = %100`
- Code path triggered: `_notify_leader_of_report_result` can enqueue duplicate scheduled events; coordinator later runs `_fire_due_scheduled_events`; watcher collection may also run.
- Expected outcome and exact count: leader sees exactly 1 result notification for `res_9`; duplicate scheduled events or duplicate result rows should be observable as deduped/skipped, not as second visible notification.
- Failure mode if classification is wrong: classifying each scheduled event id as a unique user notification creates two leader messages for one logical result; classifying only watcher rows misses duplicate scheduled events.
- Cheap-gate observability: insert two due scheduled events with identical `Result id` and one pending watcher; run coordinator tick; count visible payloads.

## Scenario 10 - SQLite Busy Or DB Lock Timeout During Atomic Claim

- User-visible action: Two framework paths attempt to deliver the same result while SQLite is locked or slow; I should not see two results because one path hit a DB timing issue.
- Worker state: worker already reported; no model retry.
- State/store snapshot:
  - `result_watchers = [{watcher_id: watch_10, status: notify_failed, result_id: res_10, notified_message_id: null}]`
  - a concurrent transaction holds a lock over `result_watchers` or message-store write.
  - `leader_receiver.pane_id = %100`
- Code path triggered: `claim_leader_notification` uses `BEGIN IMMEDIATE`; loser may raise sqlite busy; scheduled path may still be in `_fire_due_scheduled_events`.
- Expected outcome and exact count: during the lock window, leader sees 0 or 1 visible result, never 2; after retry convergence, exactly 1 visible result for `res_10`.
- Failure mode if classification is wrong: an exception path falls back to direct `_send_to_leader_receiver` without the same classification, or releases a claim it did not own, allowing duplicate visible sends.
- Cheap-gate observability: patch store connection to raise busy at `BEGIN IMMEDIATE`; assert no direct injection occurs from the failed claim path.

## Scenario 11 - Successful Visible Injection But Promotion To Canonical Message Id Fails

- User-visible action: A result appears on my leader screen, but a DB error occurs before the watcher row records the real message id.
- Worker state: worker is done; no further worker action.
- State/store snapshot:
  - before delivery: `result_watchers = [{watcher_id: watch_11, status: notify_failed, result_id: res_11, notified_message_id: claim:<token>}]`
  - `_send_to_leader_receiver` returns `ok=true`, `message_id=msg_11`
  - `promote_leader_notification_id` fails or times out before replacing `claim:<token>` with `msg_11`.
  - scheduled event for same content may still be pending.
- Code path triggered: `_deliver_result_to_watcher` success branch after `claim_leader_notification`, then `promote_leader_notification_id`; later retry path may inspect stale sentinel or null.
- Expected outcome and exact count: user-visible submitted result count is exactly 1; later recovery should not submit a second result merely because canonical id promotion failed.
- Failure mode if classification is wrong: treating "promotion failed" as "not visible" creates a second visible message; treating stale sentinel as a permanent delivered id can hide future needed diagnostics.
- Cheap-gate observability: fake promotion failure after successful injection; assert next retry does not produce a second submitted payload.

## Scenario 12 - Delivery Failure After Claim With No Visible Text

- User-visible action: A result delivery attempt fails before text appears, then the framework retries after claim-leader.
- Worker state: worker result is stored; worker no longer involved.
- State/store snapshot:
  - `result_watchers = [{watcher_id: watch_12, status: notify_failed, result_id: res_12, notified_message_id: claim:<token> during attempt}]`
  - `_tmux_inject_text` returns `ok=false` before submit.
  - after failure, watcher should be retryable and not carry a visible message id.
- Code path triggered: `notify_result_watchers` -> `_deliver_result_to_watcher` failure branch -> `release_leader_notification_claim`; later `retry_result_deliveries`.
- Expected outcome and exact count: 0 visible result notifications for the failed attempt; exactly 1 visible result after a later successful retry; total visible count for `res_12` is 1.
- Failure mode if classification is wrong: if the failed attempt is counted as delivered, user never sees the result; if a partially visible attempt is incorrectly labeled failed, user sees duplicates.
- Cheap-gate observability: fake injection failure before submit for local; Mac mini needed for provider cases where text is pasted but submit verification fails.

## Scenario 13 - Attach-Leader Requeues Exhausted Watcher While Claim-Leader Also Runs

- User-visible action: I manually attach a leader pane after result delivery exhausted, then also resolve a claim-leader ambiguity.
- Worker state: worker result is old; worker is not reporting again.
- State/store snapshot:
  - `result_watchers = [{watcher_id: watch_13, status: delivery_exhausted, result_id: res_13, notified_message_id: null}]`
  - `attach_leader` updates `leader_receiver.pane_id = %attach`
  - `claim_leader` may then update `leader_receiver.pane_id = %claim`, `owner_epoch += 1`
  - no new worker result row.
- Code path triggered: `attach_leader` -> `requeue_delivery_exhausted_watchers`; later `claim_leader` -> `requeue_after_claim_leader`; coordinator `retry_result_deliveries`.
- Expected outcome and exact count: one old result may be redelivered once to the current valid leader pane; total visible result notifications for `res_13` across attach plus claim is exactly 1.
- Failure mode if classification is wrong: attach requeue and claim requeue each reset retry budget and both deliver the same old result.
- Cheap-gate observability: call attach helper and claim helper against one exhausted watcher; assert only one submitted result payload after coordinator tick.

## Scenario 14 - Non-Result Leader Alert Interleaves With Result Requeue

- User-visible action: While a result is being requeued after claim-leader, the coordinator also sends an idle/stuck/deadlock alert to the leader.
- Worker state: result-producing worker is done; one or more other workers may be idle or stuck.
- State/store snapshot:
  - `result_watchers = [{watcher_id: watch_14, owner_team_id: team_A, result_id: res_14, status: notify_failed, notified_message_id: null}]`
  - alert message row exists or is created without `Result id: res_14`
  - `leader_receiver.pane_id = %100`
- Code path triggered: result path uses `retry_result_deliveries` or scheduled event; alert path uses `idle_alerts` or stuck detector -> `deliver_stored_message` -> `_send_to_leader_receiver`.
- Expected outcome and exact count: leader sees exactly 1 result notification for `res_14` and exactly 1 separate alert if the alert condition is valid; total leader messages may be 2, but result notifications for `res_14` must be 1.
- Failure mode if classification is wrong: tests that count all `leader_receiver.deliver_attempt` events flag false duplicates; product logic that dedupes all leader messages can suppress legitimate alerts.
- Cheap-gate observability: payload classification must use result id and message type, not only event name.

## Scenario 15 - Worker-To-Leader Direct Send Contains Result-Like Text

- User-visible action: A worker sends a normal leader message whose content includes the literal line `Result id: res_15`, but it is not a `report_result` envelope.
- Worker state: worker is still working or asking for help; no result row is stored for `res_15`.
- State/store snapshot:
  - `result_watchers` may include an unrelated pending watcher for another task.
  - `messages = [{sender: worker_1, recipient: leader, content contains "Result id: res_15", task_id: null or task_x}]`
  - `leader_receiver.pane_id = %100`
- Code path triggered: `send_message` with target leader -> `_send_to_leader_receiver`, bypassing `notify_result_watchers`.
- Expected outcome and exact count: user sees exactly 1 normal worker-to-leader message; it must not create or satisfy a result-notification watcher unless product semantics explicitly classify it as a result.
- Failure mode if classification is wrong: content-only `Result id:` scanning can suppress a later real result notification or can count a normal message as a duplicate result delivery.
- Cheap-gate observability: create direct leader message with result-like content and a real result with same id; assert classification remains explicit.

## Scenario 16 - Peer Mirror To Leader Contains Result-Like Content

- User-visible action: Worker A sends worker B a peer message, and the framework mirrors that peer traffic to the leader; the peer content happens to include result-like text.
- Worker state: worker A and B are running; neither is calling `report_result` for this content.
- State/store snapshot:
  - `result_watchers` for task may be pending, but no stored result row matches the peer message.
  - `leader_receiver.pane_id = %100`
  - peer message content contains `Result id: res_16`.
- Code path triggered: `_mirror_peer_message_to_leader` -> `_send_to_leader_receiver`, plus any normal peer delivery path.
- Expected outcome and exact count: leader sees exactly 1 peer mirror message if mirroring is enabled; result notification count for `res_16` remains 0 until a real result envelope exists.
- Failure mode if classification is wrong: event-level dedupe can hide peer mirrors; content-level result dedupe can mistake the mirror for the canonical result notification.
- Cheap-gate observability: use fake peer send with mirror enabled and no result row; count classified result notifications separately from mirror notifications.

## Scenario 17 - Same Result Retried After Owner Epoch Advances

- User-visible action: I claim a leader pane, then a stale retry from the previous owner epoch fires.
- Worker state: worker result is complete; retry comes from old scheduled event or old watcher state.
- State/store snapshot:
  - before claim: `team_owner.owner_epoch = 1`, `leader_receiver.pane_id = %old`
  - after claim: `team_owner.owner_epoch = 2`, `leader_receiver.pane_id = %new`
  - stale scheduled event was created under epoch 1, content has `Result id: res_17`
  - watcher for `res_17` may now be `notified` or `notify_failed`.
- Code path triggered: `claim_leader` requeue plus `_fire_due_scheduled_events` for an event that was enqueued before the epoch changed.
- Expected outcome and exact count: user sees exactly 1 result notification for `res_17`; if it was already visible in `%old` before epoch 2, it must not be resent to `%new` unless the product classifies that old-pane visibility as not belonging to the current leader session.
- Failure mode if classification is wrong: per-pane or per-epoch dedupe can intentionally create one notification per owner epoch; global result-only dedupe can skip a needed notification if the prior pane was not user-visible.
- Cheap-gate observability: local store can prove event scoping; real machine is needed to decide old-pane visibility during a live window switch.

## Scenario 18 - Claim-Leader Refused But Retry Still Fires

- User-visible action: I run `claim-leader --confirm` from a pane whose UUID does not match, so the claim is refused; the framework still has a pending result retry.
- Worker state: worker result is complete; no new worker action.
- State/store snapshot:
  - `team_owner.leader_session_uuid = uuid_A`
  - caller pane has `leader_session_uuid = uuid_B`
  - `leader_receiver.pane_id = %old`, `owner_epoch = 1`
  - `result_watchers = [{watcher_id: watch_18, status: notify_failed, result_id: res_18, notified_message_id: null}]`
  - scheduled retry may be due.
- Code path triggered: `claim_leader` returns refused before `requeue_after_claim_leader`; coordinator scheduled retry may independently run.
- Expected outcome and exact count: refused claim produces 0 result notifications to the caller pane; the legitimate existing leader pane may receive exactly 1 retry for `res_18`.
- Failure mode if classification is wrong: a refused claim still requeues to the wrong pane, creating a second or cross-owner result notification.
- Cheap-gate observability: fake claim with UUID mismatch; assert no `leader_receiver.claim_requeue` and no target change before retry.

## Scenario 19 - Duplicate report_result Calls With The Same result_id

- User-visible action: A worker or provider retries the MCP call and submits the same result envelope twice with the same `result_id`.
- Worker state: one logical completion; two API calls due to retry, crash recovery, or model/tool uncertainty.
- State/store snapshot:
  - `results = [{result_id: res_19, envelope_hash: H}, {result_id: res_19, envelope_hash: H}]` or second insert rejected/merged.
  - `scheduled_events` may have two send rows with `Result id: res_19`.
  - `result_watchers = [{watcher_id: watch_19, task_id: task_19, agent_id: worker_1, status: pending, notified_message_id: null}]`
- Code path triggered: `report_result` twice -> `_notify_leader_of_report_result` twice; coordinator scheduled delivery and watcher collection later.
- Expected outcome and exact count: user sees exactly 1 result notification for `res_19`; duplicate API calls should produce audit evidence but not a second leader-visible result.
- Failure mode if classification is wrong: dedupe at watcher level passes, but each scheduled event creates a separate direct leader message.
- Cheap-gate observability: call the result API twice with fixed `result_id` under fake store; count submitted payloads.

## Scenario 20 - Scheduler Dedupe Sees Prior Message With Non-Final Status

- User-visible action: A result text was pasted or partially submitted, but the message row status is not one of the final delivered statuses when a scheduled retry fires.
- Worker state: worker already reported; no retry from worker.
- State/store snapshot:
  - `messages = [{message_id: msg_20a, recipient: leader, task_id: task_20, content contains "Result id: res_20", status: injected_unverified or failed}]`
  - `scheduled_events = [{id: ev_20, kind: send, payload.content contains "Result id: res_20", status: pending}]`
  - `result_watchers = [{watcher_id: watch_20, status: notify_failed, result_id: res_20, notified_message_id: null or msg_20a}]`
- Code path triggered: `_fire_due_scheduled_events` -> `delivered_result_message` scans only selected message statuses; if it does not find `msg_20a`, it calls `deliver_stored_message`.
- Expected outcome and exact count: if `msg_20a` was not visible, retry should create exactly 1 visible result; if `msg_20a` was visible despite non-final status, retry should create 0 additional visible results. In both cases the final visible count is exactly 1.
- Failure mode if classification is wrong: status-based dedupe diverges from real pane visibility; either duplicate user-visible text or lost notification.
- Cheap-gate observability: local can verify status classification; Mac mini must validate real visibility for `injected_unverified` and failed-submit edge cases.

## Scenario 21 - Orchestrator Advances On Result While Leader Notification Also Queues

- User-visible action: A plan stage reports success; the orchestrator advances to the next stage while the same result notification is queued for the leader.
- Worker state: worker reports `status=success`; orchestrator immediately processes the envelope.
- State/store snapshot:
  - `result_watchers = [{watcher_id: watch_21, task_id: stage_1, agent_id: worker_1, status: pending, result_id: null, notified_message_id: null}]`
  - `scheduled_events` includes the result notification for `res_21`.
  - orchestrator state records stage 1 as complete and may enqueue stage 2 work.
  - `leader_receiver.pane_id = %100`
- Code path triggered: `report_result` -> `_notify_leader_of_report_result` scheduled send, then `_orchestrator_advance_on_report_result`; coordinator later runs result watcher and scheduled delivery.
- Expected outcome and exact count: leader sees exactly 1 notification for the stage 1 result; separate stage 2 dispatch messages may appear but are not counted as duplicate result notifications.
- Failure mode if classification is wrong: stage advancement messages can be counted as duplicate results, or result dedupe can suppress an unrelated stage 2 dispatch.
- Cheap-gate observability: fake orchestrator plan with one result and one next-stage dispatch; classify by result id and message purpose.

## Scenario 22 - Result Notification And Claim Broadcast Share One Human Turn

- User-visible action: The framework tells me there are ambiguous leader candidates and, after I claim one, a worker result arrives in the same apparent provider turn.
- Worker state: worker result arrives while claim broadcast is waiting for human action.
- State/store snapshot:
  - event log has `leader_receiver.ambiguous_candidates` with `incident_id=inc_22`.
  - `leader_receiver.pane_id` is stale before claim, then `%new` after claim.
  - `result_watchers = [{watcher_id: watch_22, status: pending, result_id: res_22, notified_message_id: null}]`
  - scheduled result notification may be due at or after `incident.ts`.
- Code path triggered: ambiguous receiver validation in `_send_to_leader_receiver`, then `claim_leader` and `requeue_after_claim_leader`, plus coordinator scheduled send.
- Expected outcome and exact count: user may see one claim/broadcast instruction and exactly 1 result notification for `res_22`; the claim instruction is not a result notification.
- Failure mode if classification is wrong: event log tests count the broadcast and result as duplicate notifications, or product dedupe suppresses the result because a claim instruction already used the leader receiver path.
- Cheap-gate observability: event classification test must distinguish `leader_receiver.ambiguous_candidates` or claim instruction text from result notification text.

## Scenario 23 - Same Message Id Reused Or Observed Across Retry Evidence

- User-visible action: A result is retried after a receiver failure; evidence contains a prior `message_id` and a later canonical `message_id`.
- Worker state: worker has finished; framework retry only.
- State/store snapshot:
  - `result_watchers = [{watcher_id: watch_23, status: notify_failed, result_id: res_23, notified_message_id: msg_old or claim:<token>}]`
  - `messages` includes `msg_old` with content containing `res_23`, status not clearly final.
  - later delivery may create `msg_new`.
- Code path triggered: `leader_notified_message_id_for_result`, `delivered_result_message`, `_mark_watcher_dedupe_skip`, and scheduler dedupe each may interpret old message evidence differently.
- Expected outcome and exact count: leader-visible result text count is exactly 1; store may keep old failed evidence and one canonical final message id, but not two final submitted notifications.
- Failure mode if classification is wrong: tests compare only DB `message_id` count and miss two visible messages, or compare only pane text and miss a store state that will duplicate on the next restart.
- Cheap-gate observability: assert both pane-submission count and canonical-message state in the same test fixture.

## Scenario 24 - Result Requeue After remove-agent Or Worker GC

- User-visible action: I remove or reset the worker after it reports; a pending result notification still needs to reach the leader exactly once.
- Worker state: original worker may no longer exist in `state.agents`; result row and watcher still reference its old `agent_id`.
- State/store snapshot:
  - `agents` no longer contains `worker_removed`, or `agent_health` row was GC'd.
  - `result_watchers = [{watcher_id: watch_24, agent_id: worker_removed, owner_team_id: team_A, status: pending or notify_failed, result_id: res_24, notified_message_id: null}]`
  - `leader_receiver.pane_id = %100`
- Code path triggered: `notify_result_watchers`, `retry_result_deliveries`, scheduled event delivery; status/GC code may also scan state.
- Expected outcome and exact count: leader sees exactly 1 result notification for `res_24` if the result was durably stored before removal; no duplicate is caused by missing worker state or health GC.
- Failure mode if classification is wrong: missing worker state makes the framework create a new fallback watcher or scheduled send; duplicate delivery occurs, or result is dropped.
- Cheap-gate observability: fake state with removed agent but existing result/watcher; run coordinator tick.

## Coverage Summary

Scenario count: 24.

The minimum kickoff cases are covered as follows:

- Sequential single-pane delivery: Scenarios 1 and 2.
- Claim-leader race with active scheduled retry: Scenario 4.
- Leader pane changes mid-delivery: Scenario 5.
- Two ambiguous incidents in close succession: Scenario 6.
- Cross-team interleaving: Scenario 7.
- Coordinator restart mid-delivery: Scenario 8.
- Worker crash after successful `report_result` before observed notification: Scenario 9.
- Network or DB lock timeout during atomic UPSERT: Scenario 10.

## Blind Spots For Cheap Gates

- Fake tmux can prove how many injection functions were called, but it cannot prove whether a provider pane visibly received text when submit verification failed or the process crashed after paste.
- SQLite fault injection can prove no direct fallback call happens after a lock error, but it cannot fully reproduce macOS filesystem latency, tmux server timing, or provider UI state.
- Event-log counts alone are unsafe: peer mirrors, idle alerts, claim instructions, and result notifications can all emit `leader_receiver.deliver_attempt`.
- Content-only `Result id:` scanning is unsafe for classification tests because a normal worker message can contain that line. Cheap gates should tag fixtures with message purpose as well as content.
- Cross-team cases must assert by `owner_team_id` and target pane; a global pass/fail count can hide one team losing its result while another team receives duplicates.
