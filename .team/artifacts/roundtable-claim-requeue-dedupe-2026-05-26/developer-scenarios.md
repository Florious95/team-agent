# Developer Scenarios - Claim-Leader Requeue and Result Notification Exactly-Once

Role: developer / Runtime Implementation Developer
Date: 2026-05-26
Scope: read-only scenario enumeration for Gap 26 intersect Gap 32. No fix proposal.

## Code Map Used

- `src/team_agent/messaging/results.py:191-227` persists a `result_id` from `report_result`, then queues leader notification through `_notify_leader_of_report_result`.
- `src/team_agent/messaging/results.py:247-260` creates a scheduled `send` event containing the result text and `Result id: ...`.
- `src/team_agent/messaging/scheduler.py:41-83` fires scheduled `send` events. It checks `delivered_result_message(...)` before delivery, then calls `deliver_stored_message` or `send_message`. This is not the same gate as `claim_leader_notification`.
- `src/team_agent/messaging/results.py:430-447` collects stored results and invokes `notify_result_watchers`, then `retry_result_deliveries`.
- `src/team_agent/messaging/result_delivery.py:43-124` is the watcher notification path with atomic `claim_leader_notification`.
- `src/team_agent/message_store/result_watchers.py:108-180` is the current SQLite `BEGIN IMMEDIATE` claim gate for `(owner_team_id, result_id)` among watcher rows only.
- `src/team_agent/messaging/result_delivery.py:413-490` is `requeue_after_claim_leader`, which scans `result_watchers`, marks eligible watchers `notify_failed`, and immediately calls `retry_result_deliveries`.
- `src/team_agent/leader/__init__.py:361-428` applies `claim-leader`, updates `leader_receiver`, bumps `owner_epoch`, and invokes `requeue_after_claim_leader`.
- `src/team_agent/messaging/leader.py:54-190` creates a fresh leader message row and emits `leader_receiver.deliver_attempt` / `leader_receiver.submitted` for every successful direct leader injection.

Current real-flow evidence: Mac mini run `0.2.2-slice-1-20260525T181523Z` on `bad6484` delivered `res_c4673bac2340` twice to pane `%526` as `msg_14a48d4f5d52` and `msg_75f8c3cd14dd`; `claim-D.stdout` reported `requeued_watchers: []`, so the duplicate came from scheduled leader notification delivery, not from `requeue_after_claim_leader` watcher requeue.

## Scenario 1 - Sequential Single-Pane Report Result, Scheduled Send Only

- **User-visible action:** A worker calls `report_result`; the leader pane is already valid and no `send --watch-result` watcher exists.
- **Worker state:** Worker completed one task and successfully returned from the MCP `report_result` tool. Coordinator is running normally.
- **State/store snapshot:** `team_owner.leader_session_uuid = U1`; `leader_receiver = {pane_id: %L, leader_session_uuid: U1, owner_epoch: 0}`; `scheduled_events` has one pending `kind=send` row whose content includes `Result id: R1`; `result_watchers` has no row for `R1`.
- **Code path triggered:** `report_result` -> `_notify_leader_of_report_result` -> `MessageStore.add_scheduled_event` (`results.py:191-260`); later `_fire_due_scheduled_events` -> `deliver_stored_message` -> `_send_to_leader_receiver` (`scheduler.py:41-83`, `leader.py:54-190`).
- **Expected outcome and exact count:** User sees exactly 1 leader notification for `R1`, with exactly 1 `leader_receiver.deliver_attempt` and 1 `leader_receiver.submitted`.
- **Failure mode if classification is wrong:** If this is treated as a watcher-only scenario, the UPSERT in `claim_leader_notification` is never consulted. Duplicate scheduled rows, duplicate coordinator ticks, or retry rows can each create a fresh leader message and produce 2+ visible notifications.

## Scenario 2 - Sequential Watched Send Result, Watcher Path Only

- **User-visible action:** Leader sends a task to a worker with `watch_result`; worker completes and stores a result.
- **Worker state:** Worker received the watched message, completed the task, and the result is uncollected when coordinator ticks.
- **State/store snapshot:** `team_owner.leader_session_uuid = U1`; `leader_receiver.pane_id = %L`; one `result_watchers` row `{watcher_id: W1, owner_team_id: current, status: pending, result_id: null, notified_message_id: null}`; no scheduled report-result send row for `R1` if the result was collected from disk rather than MCP `report_result`.
- **Code path triggered:** `_collect_results_and_notify_watchers` -> `notify_result_watchers` -> `claim_leader_notification` -> `_deliver_result_to_watcher` (`results.py:430-447`, `result_delivery.py:43-124`, `result_delivery.py:202-260`).
- **Expected outcome and exact count:** User sees exactly 1 notification for `R1`; exactly 1 watcher row becomes `notified` with a canonical `notified_message_id`; sibling watchers, if any, become `superseded`.
- **Failure mode if classification is wrong:** If retry logic bypasses `notify_result_watchers`, or if a watcher is requeued with a cleared `notified_message_id`, the same result can be sent again. If this scenario is mistakenly classified as scheduled-send-only, the watcher row remains retryable and later generates a second notification.

## Scenario 3 - Same Result Has Both Scheduled Report Notification and Watcher Notification

- **User-visible action:** Leader sends work with `watch_result`, and the worker reports via MCP `report_result` for the same task.
- **Worker state:** Worker completed successfully. The MCP result is accepted, and a watcher row from the original send also matches the same `(task_id, agent_id)`.
- **State/store snapshot:** `leader_receiver = {pane_id: %L, leader_session_uuid: U1, owner_epoch: 0}`; `scheduled_events` has pending `send` row for `R1`; `result_watchers` has `{status: pending, result_id: null or R1, notified_message_id: null}`.
- **Code path triggered:** Two families can run in the same coordinator cycle: `_fire_due_scheduled_events` creates and submits a leader message directly (`scheduler.py:41-83`), while `_collect_results_and_notify_watchers` can route the watcher through `notify_result_watchers` (`results.py:430-447`).
- **Expected outcome and exact count:** User sees exactly 1 canonical notification for `R1` total across both families, not one scheduled notification plus one watcher notification.
- **Failure mode if classification is wrong:** The scheduled path does not claim `result_watchers.notified_message_id`; the watcher path does not own scheduled event rows. If both paths independently check "no delivered message yet" before either submission is committed, both can inject, yielding 2 notifications with different message IDs.

## Scenario 4 - Claim-Leader Race With Active Scheduled Retry

- **User-visible action:** Two valid leader panes receive an ambiguous-candidates prompt. The user runs `team-agent claim-leader --confirm` in pane D while the coordinator is already retrying a scheduled result notification that failed earlier with `reason=ambiguous`.
- **Worker state:** Worker has already called `report_result`; `leader_notified=false` was returned because the notification is queued/retrying. Worker is no longer active in the delivery flow.
- **State/store snapshot:** Before claim: `leader_receiver = {pane_id: %old, leader_session_uuid: U1, owner_epoch: 0}`; `team_owner.leader_session_uuid = U1`; latest event is `leader_receiver.ambiguous_candidates` with candidates `%D` and `%E`; `scheduled_events` contains a retryable `kind=send` row with `Result id: R1`; `result_watchers` is empty or has no matching row. After claim: `leader_receiver = {pane_id: %D, owner_epoch: 1}`.
- **Code path triggered:** `claim_leader` updates binding and calls `requeue_after_claim_leader`, but that helper scans only `result_watchers` (`leader/__init__.py:361-428`, `result_delivery.py:413-490`). Concurrently or immediately after, `_fire_due_scheduled_events` delivers the scheduled `send` (`scheduler.py:41-83`).
- **Expected outcome and exact count:** User sees exactly 1 result notification for `R1` in the claimed pane `%D`. If scheduled delivery wins, claim requeue must not create another; if claim-side routing wins, scheduled retry must not create another.
- **Failure mode if classification is wrong:** This is the current real failure. Treating `claim-leader requeue` as watcher-only leaves scheduled rows outside the atomic UPSERT. Two scheduled retry rows, or a scheduled row plus a separate requeue path, can create two `leader_receiver.deliver_attempt` events after `claim_applied`.

## Scenario 5 - Claim-Leader Race With Active Watcher Retry

- **User-visible action:** User resolves an ambiguous leader state with `claim-leader --confirm` while a result watcher is already retryable.
- **Worker state:** Worker completed earlier. A watcher row exists and has not reached the leader.
- **State/store snapshot:** `team_owner.leader_session_uuid = U1`; `leader_receiver.pane_id = %old`; latest ambiguous incident candidates `%D`, `%E`; `result_watchers` row `{watcher_id: W1, owner_team_id: current, status: pending or notify_failed, result_id: R1, notified_message_id: null}`; `owner_epoch` moves `0 -> 1` on claim.
- **Code path triggered:** `claim_leader` -> `requeue_after_claim_leader` marks W1 `notify_failed`, then calls `retry_result_deliveries`; a natural coordinator tick can also call `retry_result_deliveries` (`result_delivery.py:24-40`, `result_delivery.py:413-490`).
- **Expected outcome and exact count:** User sees exactly 1 notification for `R1`; one path receives `claimed_by_you`, the other sees `already_notified_by` or skips after status is no longer retryable.
- **Failure mode if classification is wrong:** If a retry path calls `_deliver_result_to_watcher` without `claim_leader_notification`, or if `result_id` is absent when claiming, both retry threads can inject. If the sentinel is promoted late and a loser writes its own `notified_message_id`, future dedupe can point at the wrong message.

## Scenario 6 - Leader Pane Changes Mid-Delivery

- **User-visible action:** A result notification starts delivering to the bound pane, then the user closes or switches the leader pane before the paste/submit verification finishes.
- **Worker state:** Worker has already reported the result and is done; coordinator owns delivery.
- **State/store snapshot:** At start: `leader_receiver = {pane_id: %old, leader_session_uuid: U1, owner_epoch: 0}`; by retry time `%old` is missing or wrong-command; rediscovery finds either one `%new` candidate or multiple candidates; scheduled row or watcher row for `R1` is still not conclusively delivered.
- **Code path triggered:** `_send_to_leader_receiver` validates the pane, may call `_rediscover_leader_receiver`, and may emit `leader_receiver.rebind_applied` or `leader_receiver.ambiguous_candidates` (`leader.py:111-138`, `leader_panes.py:228-285`). Delivery may be from scheduled send or watcher retry.
- **Expected outcome and exact count:** If no content reached a visible leader screen, user should later see 1 notification on `%new`. If content was already submitted and visible on `%old`, user should not get an automatic duplicate on `%new`; the old message may be diagnostic-only if the pane vanished.
- **Failure mode if classification is wrong:** Treating all validation failures as "not delivered" can duplicate a result that was already visible before verification failed. Treating all partial attempts as delivered can drop a result that never reached any usable pane. The count error is either 0 or 2 visible notifications.

## Scenario 7 - Two Ambiguous Incidents In Close Succession For One Result

- **User-visible action:** The user creates two same-UUID leader panes, ambiguity fires; before or after the first claim attempt, pane topology changes and a second ambiguity incident is emitted for the same result.
- **Worker state:** Worker result `R1` is already queued; no new worker action occurs.
- **State/store snapshot:** `team_owner.leader_session_uuid = U1`; `leader_receiver` may move `%old -> %D`; `owner_epoch` may move `0 -> 1`; event log contains two `leader_receiver.ambiguous_candidates` incident IDs; `scheduled_events` may contain original retry and a later retry; watcher status may be `pending`, `notify_failed`, or absent.
- **Code path triggered:** `_rediscover_leader_receiver` -> `_broadcast_ambiguous_candidates` for each incident (`leader_panes.py:247-265`, `leader_panes.py:359-399`); `claim_leader` uses the latest incident in `_latest_ambiguous_incident`; scheduled retry still fires independently.
- **Expected outcome and exact count:** Ambiguous prompts may appear once per candidate per incident, but the result notification `R1` appears exactly 1 time total after a claim succeeds.
- **Failure mode if classification is wrong:** If dedupe is keyed by incident ID or `owner_epoch`, the same result can be considered fresh in incident 2 and delivered again. If only the latest incident is requeued, a scheduled row from incident 1 can still deliver after incident 2 claim.

## Scenario 8 - Cross-Team Interleaving In The Same Workspace

- **User-visible action:** Team A and Team B share a workspace. Both have active leaders; a worker in Team A reports a result while Team B also has scheduled/retry activity.
- **Worker state:** Team A worker completed. Team B workers may be idle or reporting unrelated results.
- **State/store snapshot:** Workspace state has `teams = {A, B}`; Team A `team_owner.leader_session_uuid = UA`, `leader_receiver.pane_id = %A`; Team B `team_owner.leader_session_uuid = UB`, `leader_receiver.pane_id = %B`; Team A scheduled row or watcher has `owner_team_id = A`; legacy rows may have `owner_team_id = null`; `owner_epoch` differs per team.
- **Code path triggered:** `select_runtime_state` and `team_state_key` choose the team; scheduled events and watcher queries include `owner_team_id` filters in some paths (`scheduler.py:41-83`, `result_watchers.py:32-63`).
- **Expected outcome and exact count:** Team A leader sees exactly 1 Team A result; Team B leader sees 0 copies. Team B activity must not satisfy or suppress Team A dedupe.
- **Failure mode if classification is wrong:** Because `owner_team_id is null` is treated as legacy-open in several queries, a legacy watcher/message can be visible to both teams. Result ID collision is unlikely but not impossible in tests; content-based `delivered_result_message` can dedupe against the wrong team or fail to dedupe a cross-team duplicate.

## Scenario 9 - Coordinator Restart Mid-Delivery

- **User-visible action:** The runtime coordinator restarts after a result notification is queued or after `leader_receiver.deliver_attempt`, but before the scheduled event row or watcher row is marked complete.
- **Worker state:** Worker already returned from `report_result`; user expects the framework to finish delivery after restart.
- **State/store snapshot:** `scheduled_events` row for `R1` may still be `pending` or `retry_scheduled`; a `messages` row may be `accepted`, `target_resolved`, or `submitted`; `result_watchers` may be `pending`, `notify_failed`, or already contain a sentinel `claim:<uuid>`; `leader_receiver` remains bound to `%L` with same UUID and epoch.
- **Code path triggered:** New coordinator tick runs `_deliver_pending_messages`, `_fire_due_scheduled_events`, then `_collect_results_and_notify_watchers` (`coordinator/lifecycle.py` tick order). Message delivery claims are per-message (`MessageStore.claim_for_delivery`), while result notification dedupe is split between scheduled content scan and watcher UPSERT.
- **Expected outcome and exact count:** If the pre-restart attempt was not visible, one retry should complete; if it was visible/submitted, restart should not replay it. User sees 1 total notification for `R1`.
- **Failure mode if classification is wrong:** A `target_resolved` message that was already pasted but not marked `submitted` can be abandoned while a fresh scheduled retry injects a second copy. A sentinel `claim:<uuid>` left by a crashed watcher path can suppress all future retries or be mistaken for a canonical visible message.

## Scenario 10 - Worker Crashes After Successful report_result Return Boundary Is Unclear

- **User-visible action:** Worker calls `report_result`; the provider pane or process crashes immediately afterward, before the leader observes notification delivery.
- **Worker state:** From user perspective, worker is done. From runtime perspective, `report_result` may have persisted result and scheduled event, but the worker may not receive the tool response or may be reset/restarted.
- **State/store snapshot:** `results` has `R1`; `scheduled_events` has a pending send; `messages` to the worker may or may not be acknowledged; `result_watchers` may still be `pending`; `leader_receiver` is valid.
- **Code path triggered:** `report_result` persistence/scheduling (`results.py:191-260`), coordinator tick scheduled send (`scheduler.py:41-83`), possibly `_collect_results_and_notify_watchers` if the same result remains uncollected (`results.py:430-447`).
- **Expected outcome and exact count:** Leader sees exactly 1 result notification for `R1`; worker crash/reset should not cause a duplicate report or duplicate watcher notification. If the worker retries the same semantic result with a new `result_id`, that is a separate product classification and should be visibly correlated, not silently deduped as the same ID.
- **Failure mode if classification is wrong:** If "worker crashed" is treated as "result not delivered", reset/restart may cause the worker to report again; if the new envelope gets a new `result_id`, result-id-only dedupe cannot suppress duplicate user-visible summaries for the same task.

## Scenario 11 - SQLite Busy/Lock Timeout During Atomic Watcher Claim

- **User-visible action:** Worker result is being delivered while another coordinator, claim process, or diagnostic command holds the SQLite database lock.
- **Worker state:** Worker result is stored; delivery attempt is triggered by watcher retry or claim requeue.
- **State/store snapshot:** `result_watchers` row `{status: pending or notify_failed, result_id: R1, notified_message_id: null}`; another connection holds a write lock; scheduled row may also exist.
- **Code path triggered:** `claim_leader_notification` opens `BEGIN IMMEDIATE` (`result_watchers.py:134-180`). On lock failure, caller path may catch exception as notify failure or scheduled failure depending on entry point.
- **Expected outcome and exact count:** User sees either 0 now and 1 later after retry, or 1 now if the claim succeeds. User must not see 2 notifications due to one path falling back around the lock.
- **Failure mode if classification is wrong:** If lock failure causes a caller to bypass the UPSERT and call `_send_to_leader_receiver` directly, or if a scheduled event continues independently while a watcher claim later succeeds, both can inject. If the sentinel claim is written but delivery fails and release also fails, future retries may be blocked or deduped to a non-message token.

## Scenario 12 - attach-leader Requeues Exhausted Watcher While Scheduled Retry Also Exists

- **User-visible action:** After result delivery exhausts because the leader pane was stale, the user runs `team-agent attach-leader --pane %new`.
- **Worker state:** Worker already reported; result is stored; delivery exhausted earlier.
- **State/store snapshot:** `leader_receiver` is updated to `%new`; `result_watchers` may have `{status: delivery_exhausted, result_id: R1, notified_message_id: null or msg_old}`; `scheduled_events` may still contain retry rows for the same `Result id: R1`; `owner_epoch` unchanged unless claim-leader is used.
- **Code path triggered:** `attach_leader` calls `MessageStore.requeue_delivery_exhausted_watchers` and emits `result_watcher.requeued` (`leader/__init__.py:19-54`). Scheduled sends are handled later by `_fire_due_scheduled_events`.
- **Expected outcome and exact count:** After attach, user sees exactly 1 result notification on `%new` if no earlier visible notification exists; if `notified_message_id` points to an earlier visible message, attach should not auto-inject a duplicate.
- **Failure mode if classification is wrong:** Requeueing watcher state and scheduled-send retry state separately can create one watcher notification and one scheduled notification. Conversely, preserving a failed-attempt message ID as if it were visible can suppress the only legitimate retry.

## Scenario 13 - Direct Peer Mirror Or Non-Result Leader Message Contains A Result-Like Line

- **User-visible action:** A worker or coordinator sends a normal leader message that includes a copied `Result id: R1` line, for example a peer mirror or diagnostic summary.
- **Worker state:** No new result is being reported, or a previous result is being discussed.
- **State/store snapshot:** `messages` has a leader-bound non-result row whose content includes `Result id: R1`; `result_watchers` for `R1` may still be pending; `leader_receiver` is valid.
- **Code path triggered:** `_send_to_leader_receiver` direct injection for leader target (`leader.py:54-190`) or `_mirror_peer_message_to_leader` (`leader.py:28-51`); later `delivered_result_message` scans message content/status (`result_delivery.py:315-327`).
- **Expected outcome and exact count:** User sees the ordinary message once; it must not incorrectly count as the canonical result notification unless it is actually the canonical result delivery.
- **Failure mode if classification is wrong:** Content-only dedupe can suppress the real result because an earlier non-result message contained the same `Result id:` text. The inverse failure is also possible: if such messages are ignored entirely, a direct result-looking injection can coexist with the real result notification and produce duplicate user-visible result text.

## Scenario 14 - Legacy Or Malformed Result Without result_id

- **User-visible action:** An older runtime path or malformed integration stores or emits a result-like notification without a `result_id`.
- **Worker state:** Worker claims completion, but the result cannot participate in `result_id` keyed dedupe.
- **State/store snapshot:** `result_watchers` row has `result_id = null`; scheduled event content has no parseable `Result id:` line; `leader_receiver` is valid.
- **Code path triggered:** `notify_result_watchers` falls back to `delivered_result_message` only when `result_id_str` is false (`result_delivery.py:123-139`); scheduler `result_id_from_text` returns null and the scheduled path still calls delivery (`scheduler.py:47-83`).
- **Expected outcome and exact count:** User should see at most 1 completion notification for the task, but the system may need to classify this as legacy-open because exact `result_id` dedupe is unavailable.
- **Failure mode if classification is wrong:** If the design assumes all notifications have result IDs, legacy messages bypass the canonical gate and can duplicate. If legacy messages are globally deduped by task alone, distinct retries or multiple legitimate results for one task can be collapsed incorrectly.

## Scenario 15 - Same Result Resent To New Pane After User Already Saw It On Old Pane

- **User-visible action:** A leader pane receives `R1`, then the user opens another same-UUID leader pane and claims it for continued work.
- **Worker state:** Worker is done; result was already visible to the old pane.
- **State/store snapshot:** Before claim: `leader_receiver.pane_id = %old`; `messages` has submitted leader message containing `Result id: R1`; `result_watchers.notified_message_id = msg_old` if watcher path was used, or no watcher row if scheduled path was used; after claim `leader_receiver.pane_id = %new`, `owner_epoch = old + 1`.
- **Code path triggered:** `claim_leader` updates binding; `requeue_after_claim_leader` may scan watcher rows; scheduled retry path may run if an old scheduled event was not marked done; `delivered_result_message` checks messages by content/status.
- **Expected outcome and exact count:** Automatic delivery count after claim is 0, because user already saw `R1`; total lifetime visible count remains 1. A manual history/inbox view may show the old result, but the new pane should not receive a fresh automatic paste.
- **Failure mode if classification is wrong:** If dedupe is keyed by `(result_id, pane_id)` or `(result_id, owner_epoch)`, moving panes authorizes a second injection. That is precisely the user-visible duplicate class when a result was already submitted before rebind/claim.

## Scenario 16 - Same Result Never Reached Old Pane, Claim Should Redeliver Once

- **User-visible action:** A leader pane was stale or ambiguous when `R1` was first queued. The user later claims or attaches a valid pane.
- **Worker state:** Worker is done; no leader screen has seen the actual result.
- **State/store snapshot:** `leader_receiver` changes `%old -> %new`; previous leader message rows for `R1` are `ambiguous`, `failed`, or absent; no submitted/visible message contains `Result id: R1`; watcher row, if present, has `notified_message_id = null`.
- **Code path triggered:** Either `claim_leader` plus watcher requeue, scheduled retry after claim, or `attach_leader` requeue (`leader/__init__.py:19-54`, `leader/__init__.py:361-428`, `scheduler.py:41-83`).
- **Expected outcome and exact count:** User sees exactly 1 result notification on the newly valid pane. This is the legitimate redelivery case and must not be suppressed by the existence of failed/ambiguous message rows.
- **Failure mode if classification is wrong:** If failed/ambiguous messages are treated as delivered, the result is lost. If failed rows are ignored but two independent retry families both run, the user sees 2 notifications.

## Scenario Count

Total scenarios enumerated: 16.
