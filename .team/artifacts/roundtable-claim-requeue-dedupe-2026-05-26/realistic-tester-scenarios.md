# Realistic Tester Scenarios: Gap 26 x Gap 32 claim-leader exactly-once

Role: `realistic-tester` / External Realistic Acceptance Tester
Date: 2026-05-26
Worktree: `/tmp/team-agent-public-0.2.2-slice-1`

## Scope

This file enumerates user-facing scenarios where the duplicate leader-notification class can manifest. It is intentionally not a fix proposal. I did read-only research only: kickoff brief, the latest Mac mini failure diagnosis, and the delivery entry points needed to label paths.

The real failure to explain is:

- user creates two candidate leader panes and runs `team-agent claim-leader --confirm`;
- worker result `res_c4673bac2340` is submitted twice to the winning pane `%526`;
- both submissions are visible and successful: `msg_14a48d4f5d52` and `msg_75f8c3cd14dd`;
- `claim-D.stdout` reported `requeued_watchers: []`, which matters because the live path was a scheduled send from `report_result`, not a `result_watchers` requeue.

## Read-only path map used for scenario labels

- Worker `report_result` user path:
  `messaging.results.report_result` -> `_notify_leader_of_report_result` -> `MessageStore.add_scheduled_event(kind="send")` -> coordinator `_fire_due_scheduled_events` -> `deliver_stored_message` -> `_send_single_message_unlocked` -> `_send_to_leader_receiver` -> `leader_receiver.deliver_attempt` / `leader_receiver.submitted`.
- Watch-result path:
  `send --watch-result` creates `result_watchers`; later coordinator `_collect_results_and_notify_watchers` -> `notify_result_watchers` -> `claim_leader_notification` -> `_deliver_result_to_watcher` -> `deliver_stored_message`.
- Claim path:
  `claim_leader` updates `team_owner.owner_epoch` and `leader_receiver`, emits `leader_receiver.claim_applied`, then calls `requeue_after_claim_leader`, which scans `result_watchers` only.
- Direct leader injection path:
  `_send_to_leader_receiver` creates a leader message and emits `leader_receiver.deliver_attempt`; it has no inherent result-level dedupe unless the caller already gated it.

## Scenarios

### 1. Stable single-pane worker result

- User-visible action: User asks `worker_a` to do one task; `worker_a` calls `report_result(status=success)` while the original leader pane remains open and valid.
- Worker state: `worker_a` is finishing a normal turn and returns to idle after the MCP call.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%A"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%A"
    status: attached
  result_watchers:
    none for this report_result-only path
  scheduled_events:
    one pending send with content containing "Result id: res_1"
  ```
- Code path triggered: `report_result` scheduled-event path, then coordinator `_fire_due_scheduled_events`, then `deliver_stored_message`, then `_send_to_leader_receiver`.
- Expected outcome and exact count: Leader pane `%A` shows exactly one result notification for `res_1`; events contain exactly one `leader_receiver.deliver_attempt` and one `leader_receiver.submitted` whose payload contains `Result id: res_1`.
- Failure mode if classification is wrong: If this is treated as a normal leader message and not as a result notification, a retry or restart can create a second leader message for the same `result_id` because there is no result-level canonical notification record on the scheduled-event path.

### 2. Stable single-pane watch-result delivery

- User-visible action: Leader sends a task with result watching enabled; worker reports success; leader expects the final result to appear once.
- Worker state: Worker received the task, completed it, and wrote a stored result.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%A"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%A"
    status: attached
  result_watchers:
    - watcher_id: "watch_1"
      task_id: "msg_task"
      agent_id: "worker_a"
      status: pending
      result_id: null or "res_1"
      notified_message_id: null
      owner_team_id: "current"
  ```
- Code path triggered: `notify_result_watchers` -> `claim_leader_notification` -> `_deliver_result_to_watcher` -> `deliver_stored_message`.
- Expected outcome and exact count: Exactly one canonical notification for `res_1`; exactly one watcher row carries the canonical `notified_message_id`; any duplicate watcher for the same `(team, result_id)` is marked superseded or deduped without a second visible pane injection.
- Failure mode if classification is wrong: If dedupe is scoped per `watcher_id` rather than per user-visible `result_id`, duplicate watcher rows can each produce one visible result notification, which is user-visible duplication even though each watcher "succeeded" once.

### 3. Claim-leader race with active scheduled retry

- User-visible action: User has two resumed leader panes after a window switch. Worker reports a result while the framework is still in ambiguous-candidate state. User chooses the intended pane with `team-agent claim-leader --confirm`.
- Worker state: Worker already called `report_result`; it is idle or acknowledging completion. The result has been stored, and retry events may already be scheduled.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%old"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%old"
    status: attached
  latest_ambiguous_incident:
    candidates: ["%D", "%E"]
  result_watchers:
    none for report_result-only path
  scheduled_events:
    - id: 3
      status: retry_scheduled or pending
      target: leader
      payload.content: "Result id: res_1"
      owner_team_id: current
  ```
- Code path triggered: ambiguous scheduled-event deliveries through `_fire_due_scheduled_events`; `claim_leader` -> `requeue_after_claim_leader` sees no watchers; scheduled retry then calls `deliver_stored_message`.
- Expected outcome and exact count: After the claim, the winning pane receives the stored result exactly once. Either a scheduled retry delivers it or a claim-triggered requeue delivers it, but the total count for `res_1` after `claim_applied` is exactly one `leader_receiver.deliver_attempt` and one `leader_receiver.submitted`.
- Failure mode if classification is wrong: This is the observed failure. If the scheduled-event result path is classified as separate from the result watcher dedupe contract, both paths can send their own leader message and the winner sees two visible notifications for the same `result_id`.

### 4. Claim-leader race with watch-result requeue

- User-visible action: Leader uses a task flow that registered a watcher, then opens two candidate leader panes and claims one while the worker result is waiting.
- Worker state: Worker completed and result is stored. Watcher is `pending` or `notify_failed`, not yet notified.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%old"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%old"
  latest_ambiguous_incident:
    candidates: ["%D", "%E"]
  result_watchers:
    - watcher_id: "watch_1"
      status: pending or notify_failed
      result_id: "res_1"
      notified_message_id: null
      owner_team_id: current
  ```
- Code path triggered: `claim_leader` -> `requeue_after_claim_leader` -> `retry_result_deliveries` -> `notify_result_watchers`; concurrent coordinator `_collect_results_and_notify_watchers` may also call `notify_result_watchers`.
- Expected outcome and exact count: Exactly one visible result on the claimed pane; if two callers race, one is the canonical notifier and all other watcher paths emit a dedupe signal without injecting into tmux.
- Failure mode if classification is wrong: If the dedupe unit is `(watcher_id, result_id)` instead of `(owner_team_id, result_id)`, two watcher rows or two callers can each think they own a legitimate attempt and both inject into the claimed pane.

### 5. Leader pane changes mid-delivery

- User-visible action: User closes the leader window or switches Ghostty/tmux pane while a result notification is being pasted or submitted.
- Worker state: Worker has already reported a result; the leader notification is in-flight.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%A"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%A"
    status: attached
  result_watchers or scheduled_events:
    current result "res_1" has no final canonical visible submission yet
  ```
- Code path triggered: `_send_to_leader_receiver` may emit `leader_receiver.deliver_attempt`; validation, tmux injection, or submit verification can fail; later rediscovery or scheduled retry calls `_send_to_leader_receiver` again.
- Expected outcome and exact count: If no text became visible, the next valid pane may receive one notification. If text became visible and submitted, the framework must not submit the same result again. From the user's perspective the final screen count for `res_1` is 0 if delivery never reached a valid leader, or 1 if it did.
- Failure mode if classification is wrong: If `deliver_attempt` is counted as "notified" before visibility, a result can be lost. If visibility/submission is ignored and only final DB promotion counts, a later retry can duplicate a result that the user already saw.

### 6. Two ambiguous incidents in close succession

- User-visible action: User first opens panes D/E and gets an ambiguous leader incident, then quickly closes one and opens F/G, creating a second ambiguous incident before claiming.
- Worker state: One worker result is pending notification across both incidents.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%old"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%old"
  ambiguous_incidents:
    - incident_id: "inc_1"
      candidates: ["%D", "%E"]
    - incident_id: "inc_2"
      candidates: ["%F", "%G"]
  result_watchers or scheduled_events:
    result_id: "res_1"
    notified_message_id: null or no scheduled-path equivalent
  ```
- Code path triggered: `_rediscover_leader_receiver` emits multiple ambiguous incidents; `claim_leader` uses the latest incident; scheduled retry or watcher retry may still be tied to the prior failed delivery sequence.
- Expected outcome and exact count: Only the final claimed pane receives the result, exactly once. Candidate incident notifications can appear once per candidate pane per incident, but the actual worker result `res_1` appears once total after the final claim.
- Failure mode if classification is wrong: If dedupe is keyed by `incident_id`, each incident can authorize a fresh result delivery. The user sees the same worker completion once for each ambiguity cycle.

### 7. Cross-team interleaving in one workspace

- User-visible action: User runs Team A and Team B in the same workspace family, with each team having its own leader panes. Both workers report around the same time, and one team enters claim-leader ambiguity.
- Worker state: `teamA.worker_a` and `teamB.worker_a` have both reported, or one has reported while the other is retrying.
- State.json snapshot:
  ```yaml
  teams:
    teamA:
      team_owner:
        leader_session_uuid: "uuid-A"
        owner_epoch: 1
      leader_receiver:
        pane_id: "%A"
      pending_result:
        result_id: "res_same_shape_or_collision"
        owner_team_id: "teamA"
    teamB:
      team_owner:
        leader_session_uuid: "uuid-B"
        owner_epoch: 0
      leader_receiver:
        pane_id: "%B"
      pending_result:
        result_id: "res_same_shape_or_collision"
        owner_team_id: "teamB"
  ```
- Code path triggered: `select_runtime_state`, `team_state_key`, `deliver_stored_message(team=owner_team_id)`, `notify_result_watchers(owner_team_id=...)`, and scheduled-event owner-team filtering.
- Expected outcome and exact count: Team A's leader receives Team A's result exactly once; Team B's leader receives Team B's result exactly once. No cross-team dedupe suppresses the other team's valid notification, and no cross-team retry duplicates into the wrong pane.
- Failure mode if classification is wrong: If dedupe is global by `result_id` only, one team's result can suppress the other. If owner-team scoping is missing on scheduled events, a retry from Team A can deliver into Team B's claimed pane or duplicate after Team B claim.

### 8. Coordinator restart mid-delivery

- User-visible action: User restarts the team, or the coordinator process exits and restarts, while a leader result notification is pending or being submitted.
- Worker state: Worker result is stored; worker may already be idle. No new worker action should be required.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%A"
    leader_session_uuid: "uuid-A"
  leader_receiver:
    pane_id: "%A"
  scheduled_events:
    - id: 3
      status: pending or retry_scheduled
      payload.content: "Result id: res_1"
  result_watchers:
    - watcher_id: "watch_1" optional
      notified_message_id: "claim:<sentinel>" or null
  ```
- Code path triggered: `start_coordinator` on restart; `_fire_due_scheduled_events`; `_collect_results_and_notify_watchers`; `claim_leader_notification` if a watcher path is used.
- Expected outcome and exact count: After restart, the leader sees `res_1` exactly once. A stale sentinel is either safely resolved to one canonical message or released only if no visible submission happened.
- Failure mode if classification is wrong: A restart can replay `pending` scheduled events and re-scan watcher rows, causing duplicate visible submissions; the opposite error is leaving a sentinel forever and never notifying the leader.

### 9. Worker exits after successful report_result before notification observed

- User-visible action: Worker reports success and then the provider pane exits, crashes, or is removed before the leader notification completes.
- Worker state: The worker's MCP call succeeded and returned a `result_id`; worker process is gone before the coordinator delivers to leader.
- State.json snapshot:
  ```yaml
  agents.worker_a:
    proc: missing or stopped
  leader_receiver:
    pane_id: "%A"
  scheduled_events:
    - status: pending
      payload.sender: worker_a
      payload.content: "Result id: res_1"
  result_watchers:
    may be absent for direct report_result
  ```
- Code path triggered: `report_result` already persisted the result and scheduled notification; later coordinator delivers independently of worker liveness.
- Expected outcome and exact count: Worker crash does not cause result loss, and it does not cause the framework to create a second synthetic result. Leader sees the stored `res_1` once.
- Failure mode if classification is wrong: If the framework interprets worker death as "report status unknown", it may create a second retry/result envelope or leave the original scheduled send pending after a successful visible delivery.

### 10. DB lock timeout during notification claim or scheduled-event marking

- User-visible action: Heavy concurrent runtime activity happens while result delivery is trying to serialize the notification: claim-leader, coordinator retry, status, and another mutator all touch the DB.
- Worker state: Result is stored; delivery is ready but one code path hits SQLite lock pressure.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%A" or newly claimed "%D"
    owner_epoch: 1
  leader_receiver:
    pane_id: "%D"
  result_watchers:
    - watcher_id: "watch_1"
      notified_message_id: null or "claim:<sentinel>"
  scheduled_events:
    - id: 3
      status: pending or retry_scheduled
  ```
- Code path triggered: `claim_leader_notification` with `BEGIN IMMEDIATE`; `promote_leader_notification_id` or `release_leader_notification_claim`; scheduled event `mark_scheduled_event`.
- Expected outcome and exact count: On lock timeout, either no visible notification happens and retry remains pending, or one visible notification happens and all retry state converges to that canonical message id. There must not be a second visible submission after a lock error.
- Failure mode if classification is wrong: If a timeout is treated as "safe to retry" without knowing whether the first path already injected into tmux, retry can duplicate. If it is treated as "already delivered" before visibility, the user never sees the result.

### 11. Claim loser continues operating after owner_epoch advances

- User-visible action: Two candidate panes both try to claim. Pane D wins; pane E loses but then immediately runs a mutator or triggers delivery by a command.
- Worker state: Worker result is pending notification or retrying.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%D"
    leader_session_uuid: "uuid-A"
    owner_epoch: 1
  leader_receiver:
    pane_id: "%D"
  loser_pane:
    pane_id: "%E"
    expected_epoch: 0
  pending_result:
    result_id: "res_1"
  ```
- Code path triggered: `claim_leader` loser path returns `owner_epoch_advanced`; any later delivery should validate current `leader_receiver` and route to `%D` only.
- Expected outcome and exact count: Pane E receives at most a refusal/incident notice; it must not receive the worker result. Pane D receives `res_1` exactly once.
- Failure mode if classification is wrong: If losing pane activity is allowed to trigger a fresh delivery attempt with stale epoch, both D and E can display the same result, or D can display it twice after E's failed command schedules another retry.

### 12. Attach-leader after no-candidate state

- User-visible action: User closes all leader panes. Worker reports a result. Later the user attaches a new leader pane with the same leader session identity.
- Worker state: Worker already reported; result notification is queued or exhausted because no leader pane existed.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%old"
    leader_session_uuid: "uuid-A"
    owner_epoch: 0
  leader_receiver:
    pane_id: "%old"
    status: stale or missing
  result_watchers:
    status: notify_failed or delivery_exhausted
    notified_message_id: null
  scheduled_events:
    status: retry_scheduled or failed
  ```
- Code path triggered: `_refresh_leader_receiver_or_flag_rebind`, `attach_leader` or automatic single-candidate rebind, `retry_result_deliveries`, scheduled retry.
- Expected outcome and exact count: When the new leader pane is attached, the queued result is delivered once to that new pane. The user should not have to manually inspect fallback files to avoid duplication.
- Failure mode if classification is wrong: If no-candidate recovery and claim recovery use separate queues, both can redeliver when the leader returns, or neither owns the notification and the result is lost.

### 13. Peer message mirror intersects with result notification

- User-visible action: Worker A sends a peer message to Worker B and then reports success to leader. The framework mirrors peer traffic to leader while also sending the result notification.
- Worker state: Worker A is mid-turn and uses both `send_message(to=worker_b)` and `report_result`; Worker B may acknowledge.
- State.json snapshot:
  ```yaml
  leader_receiver:
    pane_id: "%A"
  messages:
    - peer mirror content without "Result id: res_1"
    - report_result notification content with "Result id: res_1"
  result_watchers:
    optional depending on task dispatch
  ```
- Code path triggered: `_mirror_peer_message_to_leader` calls `_send_to_leader_receiver` directly; `report_result` also schedules a leader send.
- Expected outcome and exact count: The leader may see one peer mirror and one result notification because they are different user-visible events. The result notification for `res_1` still appears exactly once.
- Failure mode if classification is wrong: If all leader_receiver deliveries from one worker are deduped as the same event, a legitimate peer mirror can suppress a result. If mirror and result are both classified as independent retries for the same result, the result can duplicate.

### 14. Orchestrator stage completion and leader notification share one report_result

- User-visible action: User runs an overnight plan. Worker reports stage success; the orchestrator advances to the next stage while the leader should also receive one result notification.
- Worker state: Stage worker has completed and called `report_result`.
- State.json snapshot:
  ```yaml
  orchestrator:
    plan_id: "plan_1"
    current_stage_index: 0
    status: running
  leader_receiver:
    pane_id: "%A"
  scheduled_events:
    one leader notification for "Result id: res_1"
  result_watchers:
    may be absent unless the stage dispatch used watch-result
  ```
- Code path triggered: `report_result` stores result, schedules leader notification, then `_orchestrator_advance_on_report_result`.
- Expected outcome and exact count: The plan advances once, and the leader receives one notification for `res_1`. Stage advancement must not depend on the leader notification being delivered, and notification retry must not advance the stage a second time.
- Failure mode if classification is wrong: If the notification retry is mistaken for a new result event, the plan can advance twice or dispatch the next stage twice; if advancement and notification share a fragile lock, a delivery failure can incorrectly halt the plan.

### 15. Human claims pane while worker is still visibly working

- User-visible action: Worker is still generating output. The leader pane becomes ambiguous, user claims a pane, and the worker reports result seconds later.
- Worker state: Worker is `working` at claim time, then transitions to `report_result`.
- State.json snapshot:
  ```yaml
  team_owner:
    pane_id: "%D"
    leader_session_uuid: "uuid-A"
    owner_epoch: 1
    claimed_via: claim-leader
  leader_receiver:
    pane_id: "%D"
  result_watchers:
    no result yet at claim time
  scheduled_events:
    none until report_result
  ```
- Code path triggered: `claim_leader` with no current watcher to requeue; later `report_result` schedules a send to the already-claimed pane.
- Expected outcome and exact count: Claim itself should not create a phantom delivery. When the later result exists, it is delivered once to the claimed pane.
- Failure mode if classification is wrong: If claim-leader scans broad historical state rather than only real not-yet-delivered results, it may re-deliver an older result at claim time and then deliver the new result later, making the user see stale duplicates and possibly miss which worker just finished.

### 16. Fallback inbox plus later pane delivery

- User-visible action: Delivery to leader fails and writes the result to fallback inbox. User later restores or claims a pane.
- Worker state: Worker result is complete; no additional worker action occurs.
- State.json snapshot:
  ```yaml
  leader_receiver:
    pane_id: "%old"
    status: invalid or ambiguous
  fallback:
    leader-inbox.log contains "Result id: res_1"
  result_watchers or scheduled_events:
    retry still pending because fallback is not a visible pane submission
  ```
- Code path triggered: `_fail_leader_delivery` writes fallback; scheduled retry or claim requeue later calls `_send_to_leader_receiver`.
- Expected outcome and exact count: The user may have one durable fallback record and one visible pane notification. The visible pane notification count remains exactly one. The fallback record must not be treated as a visible delivery, but it must be auditable.
- Failure mode if classification is wrong: If fallback is treated as delivered, the user never sees the result in the leader pane. If fallback is ignored completely, a later recovery may create multiple visible submissions because each retry thinks no prior delivery artifact exists.

## Scenario count

16 scenarios enumerated.
