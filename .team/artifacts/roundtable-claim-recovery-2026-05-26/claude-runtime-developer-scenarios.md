# Claim-Leader Recovery-After-Exhaustion (Silent-Loss Arm) — Scenarios from claude-runtime-developer

**Date**: 2026-05-26
**Role**: claude-runtime-developer / Runtime Semantics Developer
**Severity**: HIGH (message loss)
**Context**: The duplicate-delivery arm closed by commit `945948b` (Stage 12 injection-boundary dedupe gate). This roundtable addresses the **different** failure shape that surfaced post-fix on Mac mini real-flow: **the result envelope is silently lost when retry budgets exhaust under ambiguous-leader, then claim-leader fires too late to recover**.

**My role's lens**: I authored the requeue / recovery layer across Stage 11.9-11.12 (`requeue_after_claim_leader`, `retry_result_deliveries`, the watcher-table UPSERT now retired, and the legacy compat fallback that still gates against `notified_message_id`). The silent-loss arm is in **my code**. This doc enumerates the failure surface so the roundtable can pick a recovery design — not a "tighter retry" patch.

**Brief format** per scenario:
- **Name** + severity tag
- **What the user did** — concrete UI action
- **Where the framework gave up** — exact code path that emitted the terminal failure
- **State at claim moment** — relevant DB / state.json snapshot
- **Expected outcome** — what the user should see after claim
- **Failure mode if recovery layer is wrong** — what silent-loss looks like

---

## 0. Index (14 scenarios across 8 categories)

A. Both branches exhaust before claim (#1-#2)
B. One branch exhausts, the other pending (#3-#4)
C. Stage 12 dedupe-gate row blocks recovery (#5-#6)
D. Sentinel / claim-token leftover from retired pattern (#7)
E. Process boundary: coordinator dies mid-exhaustion (#8-#9)
F. Multi-result interleave (#10-#11)
G. Peer-mirror legitimately delivered, canonical lost (#12)
H. Mac-mini / network specific (#13-#14)

---

## A. Both branches exhaust before claim

### #1 Both Path A and Path B exhaust → claim arrives too late — `[both_exhausted_pre_claim] - [critical]`
- **What user did**: Worker reported `report_result(res_X, success)`. Leader pane went ambiguous (Gap 26 trigger — ghostty new window). User noticed silence, took ~3 minutes to decide which pane to claim, then ran `team-agent claim-leader --confirm` from pane D.
- **Where framework gave up**:
  - Path A (`messaging/results.py:_notify_leader_of_report_result`): scheduled_event fires 3 attempts via `_fire_due_scheduled_events` (max_attempts=3). Each attempt routes through `deliver_stored_message` → `_send_to_leader_receiver` → fails because no pane validates → scheduled_event marked `failed`.
  - Path B (`messaging/result_delivery.py:retry_result_deliveries`): watcher status pending → notify_failed → after 5 attempts (`_RESULT_DELIVERY_MAX_ATTEMPTS`) → `_mark_delivery_exhausted` → status `delivery_exhausted`.
- **State at claim moment**: watcher row `(status=delivery_exhausted, notified_message_id=NULL, error="retry budget exhausted")`; scheduled_event row `(status=failed, attempt=3)`; `leader_notification_log` empty (no claim ever landed); `result` row exists.
- **Expected outcome**: claim-leader rebinds receiver, scans the message store / watcher table / scheduled_events for un-notified obligations, requeues with FRESH BUDGET, retries delivery against the newly bound pane → leader sees the result.
- **Failure mode**: `requeue_after_claim_leader` currently scans only watchers WHERE `notified_message_id IS NULL` AND eligible status — it DOES re-mark exhausted watchers to `notify_failed` (Stage 11.10 broadening). retry triggers → notify_result_watchers → fast-path peek `leader_notification_log` empty → proceeds to deliver. **Should work in principle.** But if Stage 12 legacy-compat fallback finds a stale `delivered_result_message` scan match (a prior "Result id: …" message that got written but never injected because tmux failed), it dedupe-skips. **Verify: under exhausted-then-claim, does `delivered_result_message` find anything?** The previous failed attempts created message rows with status `failed`/`delivery_blocked` — `_DELIVERED_RESULT_MESSAGE_STATUSES` excludes those, so fallback returns None. Good. **Then the question is: does scheduled_event get re-fired?** No — its status is `failed` terminal. **THIS IS THE BUG.** Only the watcher path is requeued; the scheduled_event branch is dead. If the watcher had also been notified-skipped or never created, BOTH branches dead → silent loss.

### #2 Scheduled_event exhausts; watcher never existed — `[scheduled_only_exhausted] - [high]`
- **What user did**: Same as #1 but the worker called report_result via MCP only, no `watch_result` flag → no result watcher row was created. Only the scheduled_event branch exists.
- **Where framework gave up**: scheduled_event fired 3 attempts → marked `failed`. No retry path exists for scheduled_event after terminal failure.
- **State at claim moment**: zero rows in `result_watchers` for this result_id; one row in `scheduled_events` with `status=failed`.
- **Expected outcome**: claim recovery picks up the failed scheduled_event, re-queues with fresh budget against the new pane.
- **Failure mode**: `requeue_after_claim_leader` doesn't touch the `scheduled_events` table at all — it only scans `result_watchers`. Silent loss is guaranteed for results that only have a scheduled_event row. **THIS IS A SECOND BUG** — my requeue scope is too narrow.

---

## B. One branch exhausts, the other pending

### #3 Path A exhausts but Path B watcher still pending — `[path_a_dead_b_pending] - [high]`
- **What user did**: Same setup as #1 but the user claimed faster (~30 seconds). Scheduled_event already exhausted its 3 attempts (each ~2s apart), but watcher's 5-attempt budget hasn't burned yet — watcher status is `notify_failed` after a few attempts.
- **Where framework gave up**: Path A `failed`; Path B not yet exhausted.
- **State at claim moment**: scheduled_event `failed`; watcher `notify_failed` with attempts=2 or 3.
- **Expected outcome**: claim recovery sees watcher's remaining budget and lets it continue; Path A's dead state is ignored (Path B will deliver).
- **Failure mode**: works today for Path B (requeue flips to notify_failed and triggers retry, which goes through the new gate and delivers cleanly). But if user waits another minute, Path B also exhausts → falls into #1.

### #4 Path B exhausts but Path A scheduled_event still has retries — `[path_b_dead_a_pending] - [high]`
- **What user did**: Watcher created and burned through its 5 attempts (5s apart); scheduled_event's slower retry schedule (2s, 4s, max 5s) still has retries pending.
- **Where framework gave up**: Path B watcher `delivery_exhausted`; Path A scheduled_event row `pending` with attempt=2.
- **State at claim moment**: as above.
- **Expected outcome**: scheduled_event eventually fires after claim, claims via leader_notification_log, delivers.
- **Failure mode**: scheduled_event's next due_at could be in the past or near-future; `_fire_due_scheduled_events` picks it up. routes through gate. gate is empty → claim → deliver. Works. UNLESS — the user runs claim-leader BEFORE scheduled_event's next retry, AND `requeue_after_claim_leader` triggers an immediate watcher retry (via `retry_result_deliveries`) which ALSO routes through the gate. Now both paths race for the gate; only one wins. Other emits `leader_notification.dedupe_skip`. OK that's the duplicate-defence working. No silent loss IF either path succeeds. Silent loss IF the gate-claimer fails delivery (e.g., pane validation hiccup) — the loser has already dedupe-skipped and won't retry. **The Stage 12 gate's claim-on-attempt semantic blocks retries even when the original attempt failed.**

---

## C. Stage 12 dedupe-gate row blocks recovery

### #5 Stage 12 gate claim wrote a row, attempt failed, row never released — `[gate_orphan_block] - [critical]`
- **What user did**: ambiguous → user ran `claim-leader --confirm` → recovery requeue fired → notify_result_watchers fast-path peek → empty → claim via `claim_leader_notification_delivery` → row inserted → deliver_stored_message → `_send_to_leader_receiver` re-consults gate (idempotent INSERT OR IGNORE returns 0 → already_notified_by with OUR own row → suppresses inject as "dedupe").
- **Where framework gave up**: the gate's design assumes the caller that wins the claim ALSO succeeds at injection. If injection fails AFTER the gate claim (e.g., pane validation interceded, or `_tmux_inject_text` returned ok=false), the row stays in `leader_notification_log` with the failed `proposed_message_id`. Next retry → peek finds the row → fast-path dedupe → no delivery. Permanent silent loss.
- **State at claim moment**: `leader_notification_log` has one row `(result_id, uuid, proposed_message_id="msg_X_first_attempt", envelope_hash, …)`; watcher has `status=notified, notified_message_id=msg_X_first_attempt`; actual tmux pane shows no result text.
- **Expected outcome**: framework detects the gate claim came from a failed attempt and either releases the row or treats it as "needs-redelivery".
- **Failure mode**: **THIS IS THE MAIN BUG**. The dedupe gate has NO failure-release semantic. My commit `945948b` introduced this; it's the symmetric of Stage 11.12's orphan-sentinel problem I flagged in my prior scenarios doc question #3, just at the new table layer.

### #6 Gate row from scheduled_event branch blocks watcher-branch retry — `[cross_branch_gate_block] - [high]`
- **What user did**: same as #1 but the scheduled_event branch made it FURTHER than the watcher branch — scheduled_event claimed the gate row, then its tmux inject failed, then the watcher's retry tried.
- **Where framework gave up**: scheduled_event's failed inject left the gate row populated. Watcher's `retry_result_deliveries` → `notify_result_watchers` → peek finds the row → fast-path dedupe → no delivery. Watcher status flips to `notified` with the dead `proposed_message_id`. Both branches effectively silent.
- **Expected outcome**: gate row recognized as "failed claim" and released for retry.
- **Failure mode**: same as #5. Cross-branch contamination via the gate.

---

## D. Sentinel / claim-token leftover from retired pattern

### #7 Watcher row has leftover `notified_message_id` from Stage 11.12-era sentinel — `[stage_11_12_sentinel_orphan] - [medium]`
- **What user did**: User upgraded from 0.2.1 (or pre-0.2.2-slice-1 candidate) where Stage 11.12 sentinel pattern was in effect. Their on-disk watcher rows have `notified_message_id="claim:abc..."` from a crashed run. Now they run claim-leader.
- **Where framework gave up**: `requeue_after_claim_leader` scans for watchers WHERE `notified_message_id IS NULL` — these are EXCLUDED. retry never fires.
- **State at claim moment**: watcher row `(notified_message_id="claim:abc...", status=notify_failed)`.
- **Expected outcome**: framework recognizes `claim:*` prefix as a stale sentinel and treats the watcher as un-notified.
- **Failure mode**: silent loss. The retired sentinel pattern's orphans now leak through. **My Stage 12 commit retired the sentinel CODE but didn't migrate or clean up rows that already had `notified_message_id="claim:..."` set on disk.**

---

## E. Process boundary: coordinator dies mid-exhaustion

### #8 Coordinator killed during scheduled_event retry attempt — `[coordinator_oom_mid_retry] - [medium]`
- **What user did**: coordinator daemon got OOM-killed during the 2nd scheduled_event retry attempt. New coordinator spawned by next coordinator_tick observation, picks up the scheduled_event row.
- **Where framework gave up**: depends — if the kill happened after `store.mark_scheduled_event(..., "failed", ...)` but before the next `_schedule_send_retry` ran, the row is terminally `failed`. Recovery should re-queue but currently doesn't.
- **State at claim moment**: scheduled_event `failed`; watcher status depends on timing.
- **Expected outcome**: coordinator startup sweep re-queues `failed` scheduled_events that have an associated result still pending.
- **Failure mode**: no coordinator-startup sweep exists. Failed scheduled_events stay terminal. If user then runs claim-leader, the requeue only looks at watchers.

### #9 SIGTERM during claim-leader subprocess execution — `[claim_subprocess_sigterm] - [medium]`
- **What user did**: User hit Ctrl-C while `team-agent claim-leader --confirm` was mid-execution (after gate row written, before tmux inject).
- **Where framework gave up**: subprocess died with gate row populated. Coordinator daemon's next tick finds watcher in stale state.
- **State at claim moment**: gate row exists with our proposed_message_id; watcher status indeterminate.
- **Expected outcome**: coordinator detects orphan gate rows (e.g., by message_id not existing in `messages` table as actually-delivered) and releases them.
- **Failure mode**: silent loss until prune at 24h expires the row.

---

## F. Multi-result interleave

### #10 N results in-flight; claim-leader fires; some recover, some lost — `[partial_recovery] - [high]`
- **What user did**: Worker reported 4 results in rapid succession before ambiguity hit. After claim, user expects 4 leader notifications.
- **Where framework gave up**: requeue scan finds all 4 watchers, requeues them, fires `retry_result_deliveries`. Each goes through the gate. If 2 of them had prior failed-attempt rows in the log (per #5/#6), only the other 2 land. User sees 2.
- **State at claim moment**: 4 watcher rows pending; 2 stale gate rows from prior failed attempts.
- **Expected outcome**: all 4 redelivered against the new pane.
- **Failure mode**: 50% silent loss. User wonders where the other 2 went.

### #11 Same result_id but different envelope_hash (legitimate re-emit) — `[legit_reemit_during_recovery] - [medium]`
- **What user did**: Worker re-emitted `res_X` with updated content (e.g., new test results) after the first emit got into ambiguous state. Both have same result_id.
- **Where framework gave up**: the second emit's gate claim sees the row from the first emit with different hash → `leader_notification.legitimate_duplicate_suspected` → suppresses (per Stage 12 conservative-first-ship policy).
- **State at claim moment**: gate row from first failed attempt with hash_A; second emit attempts to claim with hash_B.
- **Expected outcome**: legitimate updates should reach the leader. If first attempt actually failed, the update is doubly important.
- **Failure mode**: silent suppression of the updated content. User sees neither the original (lost in ambiguous) nor the update (suppressed as suspected-duplicate). **This is the conservative-first-ship policy's cost.**

---

## G. Peer-mirror legitimately delivered, canonical lost

### #12 Peer-mirror reached leader pane; canonical notify branch silently lost — `[peer_mirror_only_delivered] - [medium]`
- **What user did**: Worker A's `report_result(res_X)` triggered peer-mirror copy to worker B AND the canonical leader notification. Peer-mirror happens via `_mirror_peer_message_to_leader` (calls `_send_to_leader_receiver` with peer-mirror content, NO `Result id:` line → bypasses gate). Canonical notification via Path A/B → exhausts → gate row leftover.
- **Where framework gave up**: gate blocks canonical retry per #5/#6. Peer-mirror succeeded with different content text.
- **State at claim moment**: leader pane has the peer-mirror message (looks like "Team Agent peer message from worker_a to worker_b for task_X: success"); leader pane lacks the canonical result-envelope formatting (no result_id, no test breakdown, no team_state.md update).
- **Expected outcome**: canonical notification ALSO lands so result_id is recorded for team_state.md update + audit trail.
- **Failure mode**: leader sees peer-mirror only. team_state.md doesn't update. Audit trail breaks. User thinks the result was delivered (peer-mirror text looks like a result) but coordinator's collect path never marks the result `collected`.

---

## H. Mac-mini / network specific

### #13 SSH disconnect during retry burst — `[ssh_drop_during_recovery] - [medium]`
- **What user did**: User SSH'd into Mac mini, ran claim-leader, then SSH dropped during the requeue's retry burst.
- **Where framework gave up**: depends on which retry was in-flight when the tmux pane became briefly unreachable.
- **State at claim moment**: indeterminate — some retries may have gate-claimed but their tmux inject got an EBADF or similar.
- **Expected outcome**: SSH reconnect resumes; framework re-detects pane and re-injects.
- **Failure mode**: same #5/#6 — gate rows from failed injects block retries.

### #14 ghostty window re-created during claim execution — `[ghostty_recreate_mid_claim] - [low]`
- **What user did**: User closed and reopened the ghostty window holding the claimed pane between claim_applied and requeue's deliver attempt. The pane_id changes again.
- **Where framework gave up**: requeue's deliver hits a missing pane → fails → could re-trigger Gap 26 rediscover, but the gate row already exists.
- **Expected outcome**: rediscover, then retry against new pane.
- **Failure mode**: gate row blocks. Silent loss.

---

## Concerns / questions for the roundtable

1. **Fundamental design question — gate-claim vs gate-record**: My Stage 12 gate writes the row AT CLAIM time (before inject). If inject fails, the row is orphan. The alternative is to write the row only AFTER successful inject — but that re-opens the race the gate was designed to close. The fix is probably: gate-claim at attempt-start AND a release-on-failure path. The brief explicitly retired my Stage 11.12 release/promote sentinel pattern, so the roundtable should resolve this tension.

2. **scheduled_event branch is dead after `failed` — no recovery hook**: `requeue_after_claim_leader` only touches `result_watchers`. The `scheduled_events` row stays `failed` forever. Scenarios #1, #2, #8 all hit this. Options: (a) requeue scope expands to also re-queue failed `scheduled_events` whose result is still pending; (b) coordinator startup sweep handles it; (c) merge the two branches into one path (eliminates the duality entirely — radical).

3. **Gate row staleness detection**: how does the framework decide a `leader_notification_log` row reflects a real delivery vs an orphan from a failed attempt? Options: (a) cross-check against `messages` table for status `submitted`/`visible`; (b) embed a confirmation step (caller MUST UPDATE row with `confirmed_at` after successful inject; absence of `confirmed_at` past N seconds → orphan); (c) embed inject pid/process id in the row; if pid is dead and row is old, orphan.

4. **Stage 11.12 sentinel orphans on disk** (#7) — even though I retired the code, watcher rows from prior runs may have `notified_message_id="claim:..."` set. Need a one-time migration or a runtime check that ignores `claim:*` prefix when reading the legacy compat fallback.

5. **Conservative-first-ship suppression of legitimate duplicates** (#11) — the brief said suppress and emit event so we can learn the false-suppression rate. If the roundtable agrees that legitimate re-emit is a real user need, flip to allow with audit.

6. **Peer-mirror gap** (#12) — peer-mirror messages bypass the gate (no `Result id:` line in their text). If the canonical notification is silently lost, the peer-mirror looks like a result but isn't one. Either peer-mirror should also carry result_id and route through gate, OR collect path should treat peer-mirror as not-sufficient.

7. **Bigger question for the roundtable**: should the recovery layer for "exhausted retries" be at the `coordinator_tick` startup-sweep level (find all stale obligations and re-queue) rather than at the `claim-leader` per-call level? The latter only helps when a human claims; the former helps automatically. Trade-off: coordinator sweep could mass-re-fire requests at a healthy leader pane, creating noise.

---

## Not yet covered

- (#α) Worker explicit retry semantics: if the worker's MCP `report_result` returns a timeout, the worker may retry → second `add_result` → new result_id → no dedupe with first. Different shape from #11.
- (#β) Cross-team contamination via shared `leader_notification_log` if `owner_team_id` is NULL on legacy rows (mirrors my Stage 11.12 #20 from the prior scenarios doc).
- (#γ) What happens when team_owner is taken over (uuid rotation) WHILE recovery is in flight? The new uuid means a fresh row in `leader_notification_log` (intended per scenario #c in Stage 12), but the old retries are still in-flight pointing at the OLD uuid's row.

---

**Scenario count**: 14 across 8 categories. ~3600 words. Read-only; no fix proposal. Roundtable consolidation when user wakes.
