# Gap 26 ∩ Gap 32 Roundtable — Scenarios from claude-runtime-developer

**Date**: 2026-05-26
**Role**: claude-runtime-developer / Runtime Semantics Developer
**Context**: 4th recurrence of duplicate leader notification (Mac mini real-flow). Three prior fixes I authored on this code path failed to hold:
- `edac6b3` Stage 11.9 — first claim-requeue scan (status-whitelist filter)
- `9f52048` Stage 11.10 — broadened scan + per-watcher CAS re-fetch
- `bad6484` Stage 11.12 — atomic UPSERT (`BEGIN IMMEDIATE` in `claim_leader_notification`) + sentinel/promote pattern
- (Mac mini real-flow STILL produced duplicate on `bad6484`.)

**Goal of this file**: enumerate every user-facing scenario where the duplicate-delivery class of failure can manifest, so the roundtable can map fixes to the full failure surface — not just the case my last 3 commits patched. **Read-only research only**. No source edits, no E2E, no state mutations.

**Format per scenario**:
- **Name** + severity tag
- **User-visible action** — what the user does
- **Worker state** — what the worker(s) are doing
- **state.json snapshot** — relevant fields at the moment of the race
- **Code path triggered** — concrete functions / file:line where the race manifests
- **Expected outcome** — exact count of `leader_receiver.deliver_attempt` events and `notification_dedupe_skip` events
- **Failure mode** — what duplicate looks like when classification is wrong

---

## 0. Index (20 scenarios)

A. Single-process concurrent paths (#1-#5)
B. Multi-process concurrent paths (#6-#9)
C. Pane / identity transitions (#10-#13)
D. Persistence / SQLite layer (#14-#16)
E. Lifecycle / restart (#17-#19)
F. Multi-team contamination (#20)

---

## A. Single-process concurrent paths

### #1 claim-leader + coordinator scheduled retry race — `[claim_requeue_meets_retry_tick] - [critical]`
- **User-visible action**: User in ambiguous candidate pane D runs `team-agent claim-leader --confirm`. Coordinator's regular tick fires within the same ~10ms window.
- **Worker state**: Worker has emitted `report_result` for `res_X`; watcher is in `pending` status (retries not yet exhausted); `notified_message_id IS NULL`.
- **state.json snapshot**: `leader_receiver.pane_id` = stale pre-claim; `team_owner.owner_epoch` = N (claim hasn't committed yet); watcher row `(team, res_X, pending, NULL)`.
- **Code path**: Thread A: `runtime.claim_leader` → `requeue_after_claim_leader` → marks watcher to `notify_failed` → calls `retry_result_deliveries` → `notify_result_watchers` → `claim_leader_notification` (BEGIN IMMEDIATE). Thread B (coordinator tick): `coordinator_tick` → `_collect_results_and_notify_watchers` → `retry_result_deliveries` → `notify_result_watchers` → `claim_leader_notification`.
- **Expected**: exactly 1 `leader_receiver.deliver_attempt` for `res_X`. Loser thread emits 1 `notification_dedupe_skip`.
- **Failure mode**: 2 `deliver_attempt` events with distinct `message_id`s in same coordinator burst. Leader pane shows "Duplicate notification". **This is the case my `bad6484` was supposed to fix and Mac mini real-flow shows it didn't.** Possible root cause: claim-leader runs in a DIFFERENT PROCESS from the coordinator daemon (CLI subprocess vs daemon long-running); SQLite file locks should still serialize, but if WAL mode or shared cache is involved, the SELECT inside `BEGIN IMMEDIATE` could see a stale snapshot if the locking discipline isn't airtight. **Needs roundtable: confirm BEGIN IMMEDIATE actually blocks cross-process SELECTs in this codebase's SQLite config.**

### #2 Two coordinator ticks overlap — `[overlapping_ticks] - [high]`
- **User-visible action**: None — pure runtime. Long-running tick A holds while tick B starts (e.g., tmux pane capture timeout = 5s; tick interval = 1s).
- **Worker state**: Worker has fresh `report_result`; watcher in `pending`.
- **state.json**: `coordinator.lock` may or may not be held depending on whether `_coordinator_tick_lock` exists (need to verify; my Phase F work didn't add a tick-level lock).
- **Code path**: Both ticks invoke `retry_result_deliveries` → `notify_result_watchers` → `claim_leader_notification`.
- **Expected**: exactly 1 `deliver_attempt`.
- **Failure mode**: 2 `deliver_attempt`. UPSERT should serialize them inside one process. If coordinator-tick is single-threaded (which I believe it is — coordinator_tick is called sequentially from a daemon loop), this scenario doesn't manifest. **But the failure exists in COMBINATION with #1**: tick A's UPSERT lands, tick A's deliver fires, tick A's promote is still in-flight when claim-leader (separate process) starts BEGIN IMMEDIATE; if BEGIN IMMEDIATE doesn't actually block on the in-flight commit, both proceed.

### #3 Worker re-submits report_result with same result_id — `[worker_report_result_retry] - [high]`
- **User-visible action**: Worker's MCP `report_result` call times out / retries (network blip, claude.exe restart mid-call). Worker sends two `report_result` with identical envelope content but possibly different idempotency keys.
- **Worker state**: Worker pane busy retrying MCP call.
- **state.json**: After first report_result, watcher_A created with result_id from `add_result`. Second report_result either: (a) creates a NEW result row + NEW watcher_B; or (b) idempotent-detects and reuses watcher_A — depends on whether `add_result` checks duplicates.
- **Code path**: `mcp_server/tools.py:report_result` → `runtime.report_result` → `messaging/results.py:report_result` → eventually `notify_result_watchers`.
- **Expected**: exactly 1 leader-injected notification regardless of how many MCP calls happened.
- **Failure mode**: 2 watcher rows with 2 different `result_id`s (because `add_result` generates uuid each call). My UPSERT only dedupes WITHIN a single result_id; two different result_ids look unrelated → 2 deliveries. **Verify: does `add_result` have idempotency by envelope hash, or does each call generate a fresh result_id?** If fresh, the "exactly once per worker turn" contract breaks before reaching my code.

### #4 Multiple watchers for one result spawned by different paths — `[multi_path_watcher_spawn] - [high]`
- **User-visible action**: Worker reports a result that also triggers a peer-mirror copy (Phase G Gap 30 — worker→leader send copies result body) AND the canonical report_result path. Two watcher rows for the same (team, result_id) created via different code paths.
- **Worker state**: Worker A reporting to leader; worker B peer-mirroring same content.
- **state.json**: Two watcher rows: watcher_A (created by report_result path, agent_id=worker_A), watcher_B (created by peer-mirror, agent_id=worker_B but same result_id).
- **Code path**: `notify_result_watchers` is called for each watcher separately. My `_dedupe_watchers_for_result` picks one as primary per CALL — it doesn't dedupe ACROSS calls.
- **Expected**: 1 deliver_attempt per result_id (Gap 32 invariant).
- **Failure mode**: 2 calls to `notify_result_watchers`, each with its own primary. Both pass through `claim_leader_notification`. UPSERT SHOULD dedupe them. But if the watcher_A.result_id and watcher_B.result_id are equal AND both watcher rows exist with `notified_message_id IS NULL`, my SELECT clause `WHERE result_id = ? AND notified_message_id IS NOT NULL` finds nothing for the SECOND call IF the first call hasn't promoted past sentinel yet. The second call would see sentinel_A as canonical and dedupe correctly. **Should work — but verify via test.**

### #5 Manual `team-agent retry-result-delivery` CLI verb + tick overlap — `[manual_retry_verb_overlap] - [medium]`
- **User-visible action**: User notices a stuck delivery and runs a manual retry verb (if one exists — `attach-leader` triggers requeue today; future verb might do explicit retry).
- **Worker state**: Worker idle (already reported); watcher in `delivery_exhausted` or `notify_failed`.
- **state.json**: watcher with `notified_message_id IS NULL`.
- **Code path**: Manual CLI subprocess `runtime.retry_result_delivery` (hypothetical) → `notify_result_watchers`. Coordinator daemon's regular tick concurrent.
- **Expected**: exactly 1 deliver_attempt.
- **Failure mode**: Same as #1 — cross-process UPSERT race.

---

## B. Multi-process concurrent paths

### #6 Two coordinator processes both alive — `[double_coordinator] - [critical]`
- **User-visible action**: User restarted team agent improperly (kill -9 instead of clean shutdown). Old coordinator process not fully reaped; new one started.
- **Worker state**: Workers report; both coordinators see watcher in pending.
- **state.json**: Coordinator pid file may point at old or new; `coordinator.lock` if file-based may be stale.
- **Code path**: Two separate Python processes both running `coordinator_tick`. Each opens its own SQLite connection. Each invokes `notify_result_watchers` → `claim_leader_notification`.
- **Expected**: exactly 1 deliver_attempt.
- **Failure mode**: 2 deliveries. SQLite file-level locks should serialize the BEGIN IMMEDIATE, but if WAL mode is on, RESERVED locks are weaker than EXCLUSIVE. Two processes with their own connection caches could both fast-path the SELECT through stale cached pages. **Verify: is the codebase using WAL mode? Need to check `message_store/schema.py` for `journal_mode=WAL`.**

### #7 CLI claim-leader subprocess + coordinator daemon — `[cli_subprocess_meets_daemon] - [critical]`
- **User-visible action**: User runs `team-agent claim-leader --confirm` from pane D. This is a SHORT-LIVED Python subprocess. Coordinator daemon is a LONG-RUNNING separate process.
- **Worker state**: Worker just reported result_id=R; watcher pending.
- **state.json**: As above.
- **Code path**: CLI process: `runtime.claim_leader` → `requeue_after_claim_leader` → `retry_result_deliveries` → `notify_result_watchers` → claim_leader_notification (BEGIN IMMEDIATE in CLI process's connection). Daemon process: SAME tx via its own connection.
- **Expected**: exactly 1 deliver_attempt across both processes.
- **Failure mode**: This IS the Mac mini Scenario 3 failure on `bad6484`. **Roundtable focus**: BEGIN IMMEDIATE should serialize via SQLite file locks; if it doesn't, the failure is at the SQLite-config level (WAL vs DELETE journal, shared cache, busy_timeout, etc.), not in my Python code.

### #8 Two CLI claim-leader subprocesses from two candidate panes — `[double_claim_subprocess] - [medium]`
- **User-visible action**: Both candidate users in panes D and E run `claim-leader --confirm` within milliseconds of each other.
- **Worker state**: Worker reporting result.
- **state.json**: `team_owner.owner_epoch` not yet advanced for either; ambiguous incident still active.
- **Code path**: Both processes hit `runtime.claim_leader` → both pass `_latest_ambiguous_incident` check (incident still open) → both go for the `_runtime_lock("leader_receiver")` file lock → first wins, second gets `incident_already_claimed` or `owner_epoch_advanced` refusal. ONLY the winner runs `requeue_after_claim_leader`.
- **Expected**: 1 claim_applied event, 1 owner_epoch bump, 1 requeue burst from the winner. Loser refused cleanly.
- **Failure mode**: Both proceed past `_runtime_lock` if the lock is broken / not file-based. Both requeue. Both retry. UPSERT should still dedupe — but emergency cascade if locks don't serialize.

### #9 SSH tmux multi-attach — `[ssh_dual_attach] - [low]`
- **User-visible action**: User attaches to the same tmux session from two SSH clients. Both run claim-leader CLI from "their" pane (same actual pane underneath).
- **Worker state**: Same as #8.
- **state.json**: Same as #8.
- **Code path**: Same as #8 but both processes have IDENTICAL env (same pane_id via `TMUX_PANE`). Both see themselves as candidates.
- **Expected**: 1 claim_applied.
- **Failure mode**: As above. `_runtime_lock` should handle it.

---

## C. Pane / identity transitions

### #10 Leader pane changes mid-delivery — `[pane_changed_during_delivery] - [high]`
- **User-visible action**: User closes ghostty window holding pane %76 right as a `deliver_attempt` is mid-injection. New ghostty window opens with pane %648.
- **Worker state**: Coordinator was mid-inject to %76 when pane vanished. Injection fails (`leader_pane_missing`). Watcher mark goes to `notify_failed`, `notified_message_id` stays NULL.
- **state.json**: `leader_receiver.pane_id = %76` (stale); auto-rebind may or may not have fired yet.
- **Code path**: Failed inject → `notify_failed` → retry tick → `_rediscover_leader_receiver` finds new pane → next `notify_result_watchers` call → claim → inject to %648 → success.
- **Expected**: 1 successful deliver_attempt (to %648). The failed attempt to %76 emits `delivery_failed` but no real injection landed.
- **Failure mode**: Both injections landed (failed attempt to %76 ALSO succeeded because the pane was alive enough). User sees the result twice — once in stale window, once in new. **Verify: does `_send_to_leader_receiver` truly fail when pane vanishes mid-inject, or does it sometimes inject before failure detection?**

### #11 Auto-rebind concurrent with claim-leader — `[autorebind_meets_claim] - [medium]`
- **User-visible action**: User runs `claim-leader --confirm`. At the same moment, coordinator's `coordinator_tick` invokes `_rediscover_leader_receiver` (Gap 26 substrate) for a different observation that also finds a new candidate.
- **Worker state**: Result pending.
- **state.json**: `leader_receiver.pane_id` being mutated by two paths simultaneously.
- **Code path**: claim_leader writes leader_receiver under `_runtime_lock("leader_receiver")`. Auto-rebind in coordinator path writes under (different lock? Need to verify).
- **Expected**: 1 leader_receiver update wins; the other refuses or no-ops.
- **Failure mode**: Both writes land; final state.json has inconsistent fields. Subsequent delivery routes to whatever pane_id is in state at delivery time — could be either of the two writes. If the wrong one is bound, deliveries go to dead pane and the retry then re-delivers elsewhere → duplicate.

### #12 Leader pane crashed mid-injection + restart in same pane — `[claude_internal_crash_mid_inject] - [medium]`
- **User-visible action**: Claude process inside leader pane crashes mid-injection (rare). Wrapper restarts claude.exe in same pane (same pane_id, new PID).
- **Worker state**: Coordinator's `deliver_stored_message` returned `ok=True` per tmux capture (the injection text was visible briefly) but Claude crashed before processing it. Coordinator records `notified_message_id = real_msg_id_A`.
- **state.json**: Looks like delivery succeeded.
- **Code path**: Next tick: notify_result_watchers sees notified_message_id != null → dedupe_skip. User never sees the result; coordinator thinks it did.
- **Expected**: User awareness that the prior delivery was lost. Either re-deliver OR surface the loss.
- **Failure mode**: NO duplicate in this case (would be a different bug — silent loss). Mentioned because the symmetric: if claude restart triggers a SECOND notification request from the worker side, we'd then have duplicate. **Roundtable: how do we distinguish "delivered but consumer crashed" from "delivered and consumer processed"?**

### #13 Multiple workers ALL report SAME result_id — `[multi_worker_same_result] - [low]`
- **User-visible action**: Two workers conspire to call report_result with the same task_id+result content. Each creates its own watcher row.
- **Worker state**: Two workers active.
- **state.json**: Two watcher rows, both with same result_id (if upstream allows).
- **Code path**: Each path eventually invokes notify_result_watchers separately.
- **Expected**: 1 deliver_attempt per (team, result_id). UPSERT dedupes.
- **Failure mode**: Same as #4. If UPSERT race window leaks, 2 deliveries.

---

## D. Persistence / SQLite layer

### #14 BEGIN IMMEDIATE doesn't actually block in WAL mode — `[sqlite_wal_breakdown] - [critical]`
- **User-visible action**: None — pure persistence behavior.
- **Worker state**: As in #1.
- **state.json**: As in #1.
- **Code path**: Two BEGIN IMMEDIATE transactions in different processes/connections.
- **Expected behavior of SQLite**: BEGIN IMMEDIATE acquires RESERVED lock. Second BEGIN IMMEDIATE waits (returns SQLITE_BUSY without busy_timeout, or blocks with busy_timeout).
- **Code path realism**: I never verified the codebase's SQLite config. Need to check `message_store/schema.py` for:
  - `PRAGMA journal_mode` (DELETE vs WAL — affects locking)
  - `PRAGMA busy_timeout` (without it, BEGIN IMMEDIATE returns BUSY immediately)
  - `check_same_thread` in connect helper
  - Connection pooling / sharing
- **Failure mode**: BEGIN IMMEDIATE returns SQLITE_BUSY → Python raises sqlite3.OperationalError. My helper has try/except → ROLLBACK → re-raises. But the CALLER of claim_leader_notification doesn't handle this gracefully — might fall through to fast path that bypasses UPSERT? **VERIFY THIS PATH NOW**. *This is my #1 suspect for why bad6484 didn't hold.*

### #15 Two SQLite connections from same process see different snapshots — `[connection_cache_skew] - [medium]`
- **User-visible action**: None.
- **Worker state**: Two simultaneous notify_result_watchers calls within ONE Python process (e.g., from threading test).
- **state.json**: As in #1.
- **Code path**: Each call gets a fresh `store.connect()`. If MessageStore uses connection pool / cache, each may see stale snapshot until COMMIT.
- **Expected**: BEGIN IMMEDIATE serializes within one process.
- **Failure mode**: If `store.connect()` returns the SAME connection instance (singleton), nested BEGIN IMMEDIATE would error. If it returns fresh connections, they serialize at SQLite file lock. **Verify: how does MessageStore connect actually behave?**

### #16 Database file backup/restore during operation — `[sqlite_corruption_mid_run] - [low]`
- **User-visible action**: User backs up `.team/runtime/messages.db` while system is running (e.g., `cp messages.db messages.db.bak`).
- **Worker state**: Coordinator delivering result.
- **Code path**: Backup operation acquires its own SQLite lock; coordinator's BEGIN IMMEDIATE waits or fails.
- **Failure mode**: Inconsistent reads if backup is mid-write. Could produce a state where notified_message_id "appears" to be null when it shouldn't, leading to duplicate.

---

## E. Lifecycle / restart

### #17 Coordinator killed after sentinel write but before promote — `[orphan_sentinel] - [high]`
- **User-visible action**: User restarts team agent. The crash happened RIGHT after my UPSERT wrote `notified_message_id = "claim:abc..."` (sentinel) but BEFORE `promote_leader_notification_id` ran.
- **Worker state**: New coordinator starts; sees watcher row with `notified_message_id = "claim:abc..."`.
- **state.json**: Stale; coordinator pid file may not match.
- **Code path**: Next `notify_result_watchers` call for this result. UPSERT's SELECT finds the sentinel → returns `already_notified_by("claim:abc...")` → dedupe-skip. **Result never gets delivered** because the sentinel masquerades as a real delivery.
- **Expected**: Either re-deliver (treat orphan sentinels as null) or surface as stuck.
- **Failure mode**: Silent loss — opposite of duplicate, but still a Gap 32 invariant violation. Or, if next session generates a new sentinel and races with the orphan → 2 deliveries. **Roundtable: how do orphan sentinels get garbage-collected on restart?**

### #18 attach-leader requeues after orphan sentinel exists — `[attach_meets_orphan_sentinel] - [medium]`
- **User-visible action**: After #17, user runs `team-agent attach-leader --pane <new>`. attach_leader calls `requeue_delivery_exhausted_watchers` which preserves notified_message_id (per Gap 32 reversal of Phase D hotfix-3).
- **Worker state**: Watcher status flips back to notify_failed; sentinel preserved.
- **state.json**: notified_message_id still sentinel.
- **Code path**: Retry tick → notify_result_watchers → UPSERT sees sentinel → dedupe_skip. Result never delivers.
- **Expected**: Re-deliver to the newly attached pane.
- **Failure mode**: Silent loss again. Same root cause as #17.

### #19 SIGTERM during deliver_attempt — `[sigterm_mid_inject] - [medium]`
- **User-visible action**: User Ctrl-C's the team agent while a delivery is mid-injection.
- **Worker state**: Coordinator process getting torn down.
- **state.json**: Sentinel committed; deliver_stored_message may or may not have written its message row before SIGTERM.
- **Failure mode**: On restart, same orphan-sentinel problem (#17). If the message row was committed before SIGTERM (delivery side effect actually landed in tmux), but the watcher's promote step never ran, sentinel + visible tmux content = silent. If user re-runs claim-leader to "retry", UPSERT dedupes → result lost forever.

---

## F. Multi-team contamination

### #20 Two teams share workspace; sibling team's result_id collision — `[multi_team_result_id_collision] - [low]`
- **User-visible action**: Workspace has team alpha + team beta. Both teams running. By cosmic uuid coincidence, alpha generates result_id=X and beta generates result_id=X.
- **Worker state**: Both teams have workers reporting.
- **state.json**: Two watcher rows: (alpha, X, ...) and (beta, X, ...).
- **Code path**: `claim_leader_notification(owner_team_id="alpha", result_id="X", ...)` — my SELECT filters `WHERE result_id = ? AND (owner_team_id = ? OR owner_team_id IS NULL)`. The `OR owner_team_id IS NULL` clause means an unscoped watcher matches BOTH teams. If a watcher exists with owner_team_id=NULL and result_id=X, it could cross-suppress.
- **Expected**: Each team's notification fires independently.
- **Failure mode**: Cross-team dedupe. Alpha's delivery suppresses beta's because alpha's watcher with owner_team_id=NULL was set first. **Verify: do watchers ever have owner_team_id=NULL in multi-team workspaces?** If yes, my SELECT clause has a bug.

---

## Concerns / questions for the roundtable

1. **SQLite config verification (#14, #15)** — This is my #1 suspect for why `bad6484` BEGIN IMMEDIATE didn't hold. Need someone to run `PRAGMA journal_mode; PRAGMA busy_timeout` against `.team/runtime/messages.db` from the Mac mini E2E run and report the values. If journal_mode=WAL and busy_timeout=0, BEGIN IMMEDIATE in a 2nd connection returns SQLITE_BUSY immediately and my helper raises — but I don't think Mac mini real-flow showed that error in events.jsonl. Need to check whether the helper's `except: raise` actually propagated, or whether the CALLER swallowed it and fell through to a non-UPSERT path.

2. **Process boundary clarity (#6, #7)** — claim-leader CLI is a separate Python subprocess from the coordinator daemon. They share `messages.db` via filesystem. Are both processes always doing BEGIN IMMEDIATE in the dedupe path, or does one path bypass it? Audit every code path that emits `leader_receiver.deliver_attempt` and trace whether it goes through `notify_result_watchers` → `claim_leader_notification`.

3. **Orphan sentinel garbage collection (#17, #18, #19)** — My sentinel pattern creates a NEW failure mode (silent loss on restart). Even if duplicate delivery is fixed, this regression must be addressed. Options: (a) on coordinator startup, sweep watcher rows with `notified_message_id LIKE 'claim:%'` and clear them; (b) embed coordinator pid in sentinel and clear sentinels owned by dead pids; (c) attach TTL to sentinels and auto-clear on read if expired.

4. **add_result idempotency (#3)** — Does `MessageStore.add_result` deduplicate by envelope hash, or generate fresh uuid each call? If fresh, "exactly once per worker turn" is breakable at the upstream layer. My UPSERT only handles intra-result-id dedupe. **Roundtable: agreed contract for result_id stability.**

5. **Cross-team owner_team_id NULL semantic (#20)** — My `claim_leader_notification` SELECT uses `OR owner_team_id IS NULL` for compatibility with legacy unscoped watchers. In a multi-team workspace this could cross-suppress. Should NULL-team watchers be retired entirely, or scoped explicitly?

6. **The "different code paths" hypothesis (broad)** — If `bad6484`'s UPSERT serializes correctly within `notify_result_watchers` but Mac mini real-flow STILL shows duplicate, there must be ANOTHER code path that emits `leader_receiver.deliver_attempt` WITHOUT going through `notify_result_watchers`. Candidates to audit:
   - `messaging/leader.py:_deliver_leader_message` (any message addressed to leader)
   - `messaging/internal_delivery.py:deliver_stored_message` (orchestrator dispatch, idle-fallback)
   - `messaging/results.py:report_result` notification path that uses `_notify_leader_of_report_result` scheduled_event → coordinator delivers (separate from notify_result_watchers)
   - Phase D Gap 7 retry-by-result_id direct code paths
   - Worker → leader send_message with `task_id` set that the leader pane interprets as result content

   **If a non-UPSERT path is emitting deliver_attempt for a result already claimed via UPSERT, that's the actual root cause of the 4th recurrence.**

---

## Not yet covered (would expand if more time)

- (#a) Race between `_notify_leader_of_report_result` scheduled_event delivery (Phase D Gap 7) and the `notify_result_watchers` retry path. These are TWO separate notification mechanisms for results; if both fire for the same result_id, my UPSERT doesn't see the scheduled_event path.
- (#b) Worker peer-mirror copies result content as a regular message (worker B → leader). That regular message goes through `_deliver_leader_message`, not `notify_result_watchers`. If the canonical notification ALSO fires, leader sees both.
- (#c) The `result_id` extraction from message content via `result_id_from_text` in `result_delivery.py` — if a regular message contains the line `Result id: <id>`, the dedupe scan in `delivered_result_message` finds it; but UPSERT-based dedupe is keyed on watcher rows, not message rows.

---

**Word count**: ~3800 words. 20 scenarios across 6 categories. Wrote read-only in one focused turn. No fix proposal — scenarios only per the kickoff brief.
