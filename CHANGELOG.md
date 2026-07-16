# Changelog

## 0.5.44

- **Fix: debt sweep B — B1 canonical target resolution + B2 wrapper subtree provider evidence + B3 cross-provider env isolation + B4 bang gate harness.** B1: canonical target resolution now correctly resolves checked_paths entries against the current workspace rather than drifting to prior values during takeover scenarios. B2: wrapper subtree provider detection gains positive-evidence signals for node/bash-wrapped codex processes, preventing provider_not_foreground false negatives. B3: team-agent coordinator now strips inherited CLAUDE_CODE_SESSION_ID and related CLAUDE_CODE_* env vars when spawning workers, preventing cross-provider session identity leakage confirmed via ps eww on the remote-control fleet. B4: bang gate harness executes real private-socket tmux + real add-agent with success/failure dual-outcome assertions (add_agent.rollback event, old session survives, mcp.server_exit absent); --contract-check retained as lightweight self-check. Governance: StateWriteIntent::LeaderBindingRestoreNonTargetTeams typed route for scoped-claim saves; s1a 5/5 green, BASELINE 70 unchanged.

## 0.5.43

- **Chore: debt sweep A — short socket path + exact teardown + D-j mutation-in-place + fmt + CI fixes.** S1: tmux socket paths now use a deterministic short path (<100 chars, fail-closed) preventing "File name too long" errors on macOS sandboxed temp dirs. S2: teardown contract covers exact-owned and foreign-session scenarios with explicit Drop ordering and loud escape paths; blanket kill is prohibited. S4: eprintln cleanup (remove 3), packaging serialization anchored via env vars ×4, line-count gate made self-contained. S5 (D-j): clear_stale helper is pure mutation (zero I/O); attempt_due_recoveries uses exactly one load with sequence load→mutate→save→collect on the same Value; allowlist row migrated to containing function. S6: cargo fmt --all (253 files, no logic change). Backlog observations D-j/D-k/D-l/D-m all consumed — backlog reaches zero for the first time.

## 0.5.42

- **Chore: S1b writer cluster migration — CoordinatorTick + ClaimLeader → StateRepository (governance phase 5).** Migrates the CoordinatorTick and ClaimLeader write paths to route through StateRepository, reducing external direct-write points from 44 to 41 (BASELINE 73→70). All three migration points are byte-identical to prior direct calls; no helper, save_hook, or load/retry/cache semantics changed. Completes the five-phase governance arc (G0→S1a→C1→C2→S1b). Ledger contract: governance_writer_cluster_count=70 (5/5 cases).

## 0.5.41

- **Fix: fault invisibility — RuntimeFreshness + worker wrapper detection + host_boot heartbeat + stale diagnose (S1-S5).** Adds a RuntimeFreshness signal derived from host boot time (linux /proc/uptime, macOS sysctl kern.boottime) emitted in every heartbeat payload. The status renderer gains a stale-issue diagnose path triggered by three independent conditions: host_boot_mismatch (coordinator from prior boot), worker_provider_exited (wrapper exit detected), and coordinator_unavailable (no positive evidence, not already UNKNOWN). Worker liveness now uses an eight-level positive-evidence sequence with marker-based detection preceding legacy pane_pid checks; marker absence → Unverifiable rather than alive. Canonical UNKNOWN takes precedence over coordinator_unavailable to avoid double-stacking with the 0.5.35 R4 guard. Consumes D-m backlog item (0.5.39). New contract: fault_invisibility_0541_contract.

## 0.5.40

- **Fix: build-before-destroy Slice3 — deferred teardown + snapshot rollback (P0 Slice3).** When a restart is requested while an existing team is live, the coordinator now builds the new session first (deferred mode) before tearing down the old one. A dirty-topology gate refuses the build if the old session is in an ambiguous state. On failure mid-build, the coordinator rolls back from a pre-build snapshot (double-write to state.agents + teams.<key>.agents using canonical team_key). If the new tmux server crashes mid-flight (session_disappeared), the coordinator stops rather than cascading a pop. The old teardown path (session_disappeared_after_spawn) is preserved byte-identical for non-deferred restarts. New contract: restart_build_before_destroy_0540_contract.

## 0.5.39

- **Fix: tmux blast-radius containment — private socket + wrapper lifecycle (P0 Slice1+Slice2+§11.1B).** Workers now spawn inside a private tmux socket (`tmux -L ta-<epoch>`) so that `kill-server` affects only the team-agent session tree, not the user's ambient tmux server. Added a wrapper lifecycle layer (Slice2) covering all daemon worker spawn sites (first/into/adaptive), ensuring pane death is detected when the wrapper exits. Thin Slice1 removes the legacy `kill-session` shape-fallback. B-classifier upgraded to pure-observation: `classify_tmux_server_error` emits diagnosis only, no recovery actions. D-k/D-l backlog items consumed (stagger reverse-guard contract + stale comment). New contracts: tmux_server_death_0539_contract + parallel_spawn_stagger_reverse_guard.

## 0.5.38

- **Feature: startup latency instrumentation + bounded parallel worker spawn (Step 1+2).** Added per-worker spawn timing telemetry emitted to events.jsonl (worker.spawn_timing with queued_at, spawn_start_at, spawn_end_at, elapsed_ms). Fresh/FreshAfterMissingRollout restarts now spawn workers concurrently via a bounded Condvar submission gate (max 4 in-flight), reducing wall-clock startup time from O(N) serial to O(⌈N/4⌉) parallel. Resumed restarts remain serial (session_disappeared semantic requires deterministic ordering). New contract: startup_latency_contract (856 lines, golden fixture regen for 5 existing fixtures).

## 0.5.37

- **Fix: recovery terminal idempotency — skip dispatch and clear stale due-time (R5 restart storm).** Terminal recovery intents (succeeded/blocked/exhausted) were re-dispatching on every tick because the due-time filter did not exclude them and the dispatch path did not clear next_retry_at. Fixed by: (1) clearing stale terminal next_retry_at at the top of attempt_due_recoveries before collecting; (2) restricting collect_due_recovery_agents to scheduled/running status only; (3) removing next_retry_at on write_recovery_intent_result for terminal transitions. New contract cases: r8 terminal idempotency (positive) + r8 non-terminal still dispatches (negative guard).

## 0.5.36

- **Fix: API error recovery — bounded retry, backpressure, copyable state (supermarket P2).** The coordinator now detects repeated API errors (429/5xx) via the abnormal exit watcher and enters a bounded retry/backpressure mode: exponential back-off with a configurable retry budget, a canary-first strategy for 429s, and explicit noop paths when recovery is not applicable. Recovery intent is persisted atomically post-save (outside the pre-save window) and never synthesizes hidden provider turns or SDK calls.
- New contract: api_error_recovery (8 cases covering R4 backpressure, R5 budget exhaustion, R6 pre-save isolation, R7 no-SDK invariant).

## 0.5.35

- **Fix: managed-leader Ctrl+C reentry (user-reported).** When a managed leader pane received Ctrl+C and restarted, the coordinator would not recognize it as the same logical leader, causing a new unmanaged binding and orphaning the prior team session. Added a thin detection gate (different_pane check + LEADER_SESSION_PREFIX + socket match) that routes matching restarts through ManagedReentry instead of fresh binding.
- **Fix: status renderer UNKNOWN canonical precedence (R1 renderer layer).** The HTML/summary renderer used legacy worker_state as the primary signal for UNKNOWN, occasionally overriding the canonical classification. Canonical unknown (worker_state==UNKNOWN || activity.status==Uncertain) now takes early-return precedence over legacy health fallback.
- New contract: managed_leader_provider_reentry_contract (4 cases).

## 0.5.34

- **Fix: fake READY is structural-non-busy, not idle (0534).** The fake READY marker used by fake_worker.rs was being classified as an idle prompt (has_idle_prompt=true), causing worker_state=PROBABLY_IDLE and agent_health=IDLE post-restart. This violated the unknown-never-idle discipline: READY is the fake worker's boot heartbeat, not proof of an idle shell. Fixed by introducing a distinct has_fake_ready_structural flag that returns Uncertain (not Idle), preserving latest_pane_signal_is_structural=Some(_) so the 0532b freshness gate still suppresses false-busy last_output_at. New contract cases: R1 READY-not-idle assertion + fake WORKING still BUSY guard + real idle prompt still IDLE guard.

## 0.5.33

- **Fix: tighten jsonl freshness boundary + fix fixture mtime (0532b).** The freshness classifier used a non-strict less-than-or-equal boundary when comparing transcript mtime against spawned_at, causing a transcript written at exactly spawned_at to be classified as "working" instead of "uncertain". Changed to strict less-than so only transcripts written strictly before spawn are treated as stale. Also corrected the contract fixture to set mtime to spawned_at+1s (strict post-spawn) to match the intended semantic.

## 0.5.32

- **Fix: restart recovery precision — clear per-agent activity on new cohort (0532).** When a restart creates a new cohort, per-agent activity records from the previous cohort were not cleared, causing the coordinator's tick loop to misattribute stale activity to the new agent. This produced false-positive "agent is healthy" readings immediately after restart (the new agent had not yet sent any activity), masking delayed-start failures and preventing the recovery watchdog from triggering. Per-agent activity is now explicitly zeroed at cohort promotion.
- **Chore: C2 command deletion — internalized repair surfaces removed (net -1117 lines).** The `repair-state`, `doctor --repair`, and related diagnostic-repair command surfaces have been deleted from the CLI. Their functionality is now internalized: the coordinator self-heals on startup and the `alive` predicate prevents unsafe saves. Externally-triggered repair is no longer needed. New contract: `c2_command_internalization_deletion_contract` (8 cases).

## 0.5.31

- **Fix: reboot recovery follow-up — complete topology rebuild and lease handoff (0531).** Completes the reboot recovery path introduced in 0.5.29: the adapter layer now surfaces the rebuilt topology to the lease subsystem, ensuring the leader lease is re-acquired against the new pane IDs after recovery. Previously, recovery rebuilt the internal topology but did not propagate it to the adapter, leaving the lease pointing at stale pane handles and causing `send` and `restart` to fail after the first successful `status`. Also fixes `rebuild.rs` to handle the case where the restart target's pane has been reassigned in the new tmux session. New contract: `reboot_recovery_followup_0531_contract` (4 cases).

## 0.5.29

- **Fix: P1 — recover from stale tmux topology after reboot (0529).** When the system reboots while a team is active, the tmux topology (pane IDs, session names) becomes stale. The recovery path now detects this condition and rebuilds the topology from the coordinator's persisted state, unblocking `status`, `send`, and `restart` commands that previously deadlocked waiting for pane acknowledgment from a dead tmux session. New contract: `reboot_tmux_recovery_deadlock_contract` (8 cases).
- **Fix: P2 — restart merges add-agent dynamic role source, prevents SaveConflict (0530).** When `restart` revives a worker that was originally started with `add-agent`, it now merges the dynamic role source (role file path / inline role) from the original add-agent invocation. Previously, restart reconstructed the worker spec from static team spec only, omitting the dynamic role, which caused a SaveConflict when the restarted worker's spec diverged from the persisted team state. New contract: `add_agent_restart_saveconflict_contract` (4 cases).

## 0.5.28

- **Fix: restart now gates on projection alias identity (supermarket case layer-2 root fix).** When `restart` resolves the target agent, it now validates that the projected alias resolves to the same canonical identity as the currently-registered entry, preventing a stale alias from silently routing a restart to the wrong agent. Previously, a projection alias that had drifted from the canonical agent identity could cause restart to revive a different (sibling or predecessor) agent without surfacing an error, leaving the team in an inconsistent state.
- **New: `stale_team_projection_alias_contract` (3/3).** Contracts cover: stale projection alias does not route restart to wrong agent, alias identity gate rejects mismatched projection, canonical-alias round-trip is stable after coordinator restart.

## 0.5.27

- **Fix: shutdown now stamps projected top-level team as terminated (supermarket case root-cause).** When `shutdown` kills a session, it now stamps the projected top-level team (the team whose coordinator owns the session) as non-alive in state, preventing stale topology conflicts on the next `add-agent` or `start`. Previously the team entry remained alive after the coordinator process was killed, causing save conflicts when sibling workers' stale state files referenced the now-dead socket. The stamp is written before the kill signal is sent, ensuring consistency even if the coordinator does not exit cleanly.
- **Fix: `lsof --cwd` timeout in shutdown is diagnostic-only, not a partial-shutdown signal.** If the `lsof` invocation used to enumerate cwd-holding processes times out, shutdown now records a diagnostic event and continues rather than treating the timeout as evidence of a partial shutdown. Previously a slow `lsof` could abort the shutdown sequence prematurely.
- **New: `shutdown_kill_plan` contract (guard + RED tier).** Contracts cover: shutdown stamps projected top-level team non-alive before kill, lsof-cwd timeout is diagnostic not a shutdown partial, shutdown kill plan records the projected team key, guard contract rejects shutdown that skips the stamp step.

## 0.5.26

- **Fix: `alive` predicate narrowed to exclude stale topology conflicts (supermarket case).** The `alive` check previously returned `true` for teams with a stopped coordinator but a running sibling worker whose state file still referenced the old coordinator socket. This caused save conflicts when `add-agent` or `shutdown` tried to write team state while a dead sibling's stale topology was still considered live. The predicate now requires both the coordinator and all referenced workers to be running under a consistent topology. A stopped coordinator stamps the team as non-alive regardless of sibling worker state.
- **Fix: explicit-team fallback for `purge-agent` and `add-agent` downstream failures.** When `add-agent` fails downstream (e.g., worker spawn error), the spec and state rollback now uses the explicit team key from the request rather than re-deriving it from the potentially-partially-written state, ensuring retry is clean. `purge-agent` help and dispatch are now consistent in how they handle the explicit-team argument.
- **New: `stale_team_saveconflict_contract` (6/6).** Contracts cover: dead sibling stale topology does not block live team save but running sibling still conflicts, shutdown session killed stamps team non-alive, legacy all-stopped team is not alive but empty bootstrap remains alive, repair-state does not treat stopped current as alive ambiguity, add-agent downstream failure rolls back spec and state cleanly, purge-agent help and dispatch are consistent.

## 0.5.25

- **Fix: team runtime state path now uses B3 team-key layout, not legacy runtime-root layout (Foundation-0 F0-3).** The path preview surfaced in `status`, `diagnose`, and MCP tool responses previously showed the old per-session snapshot root. The path is now computed from the canonical B3 team-key layout (`~/.team-agent/teams/<team-key>/`) so callers see the correct authority path without reading legacy locations. New contract: `b0_reader_hideone_audit_contract` (5/5), covering that product readers never consume a legacy snapshot without a diagnostic marker, and that stale snapshots cannot influence delivery target resolution, restart preflight, or readiness diagnosis.
- **Fix: alpha migration gate — Foundation-0 observability surface (Foundation-0 F0-4).** The alpha migration gate now surfaces structured observability: `a0_transition_response_names_message_identity_distinctly_from_task_identity` ensures the response fields that name a message and name a task remain distinct, preventing attribution collapse during the Foundation-0 migration window. Legacy 0.5.x workspaces load without destructive B1 conversion. Stale legacy snapshots are marked or reported and never consumed by product readers. New contract: `f0_alpha_migration_gate_contract` (4/4).

## 0.5.24

- **Fix: `report_result` attribution fallback is now bounded (Foundation-0 F0-1).** A newer, non-reportable direct-message turn blocks the old-task fallback path. Previously, if a newer turn arrived between task delivery and `report_result`, the attribution could fall back to an older task even though the newer turn had no reportable task. The fallback now stops at the newest non-reportable direct turn and emits a structured `attribution_bounded_warning` so operators can observe when bounding occurs. New contract: `a0_current_turn_attribution_contract` (5/5).
- **Fix: legacy per-session snapshot retired as an authority source (Foundation-0 F0-2).** The authority save path no longer writes to the legacy per-session snapshot location. Any legacy snapshot that still exists on disk is preserved but tagged with `_not_authoritative` metadata, and all read paths that previously used it as an authority source now treat it as diagnostic-only. This eliminates the class of stale-route bugs where an old snapshot overrode a live authority state. New contract: `b0_legacy_snapshot_nonauthority_contract` (3/3).
- **Doc: coordinator health conditional fields default to healthy when absent (0.5.23 wire doc debt).** The coordinator health wire format documents that fields introduced in 0.5.23 (`service_available`, `newer_daemon_preserved`) are absent when the corresponding condition is not triggered — callers must treat absence as the healthy/default state, not as an error.

## 0.5.23

- **Fix: coordinator health now uses two independent predicates: `service_available` and binary identity.** `service_available` checks that the coordinator process is alive, the wire protocol version is compatible, and the schema version is compatible — these are the conditions required to serve an MCP client. Binary identity (binary path + `PKG_VERSION`) is checked separately and governs rotation. A compatible newer coordinator daemon that passes `service_available` is now allowed to stay live and serve older-version MCP clients, fixing the worker→worker cross-version delivery rejection that occurred when a newer coordinator was incorrectly rotated out by an older caller.
- **Fix: `start_coordinator` direction guard — only same-version or newer callers may rotate the coordinator; older callers preserve a compatible newer daemon.** When `start_coordinator` is called by a caller whose binary version is older than the running coordinator, it writes a `newer_daemon_preserved` diagnostic event and returns without downgrading the coordinator. This eliminates the version ping-pong risk where alternating old and new callers could repeatedly rotate the coordinator.
- **Fix: binary drift at enqueue time writes a diagnostic event.** When a message is enqueued against a coordinator with a binary identity mismatch, a structured diagnostic event is recorded, giving operators visibility into drift without requiring a manual `diagnose` run.
- **Binary protocol and schema incompatibility remain hard fail-closed.** Only binary drift is relaxed to allow compatible service; wire protocol mismatches and schema version mismatches still fail immediately without delivery.
- **New: `coordinator_service_compat_contract` (5/5).** Contracts cover: newer daemon is service-compatible for same-protocol MCP send, older caller does not downgrade a newer daemon, newer caller still rotates an older daemon, MCP lifecycle reset cannot rotate a newer daemon down, protocol mismatch fails closed without side effects.
- **Updated: `loud_ensure_contract` (6/6).** New contract: old caller must not rotate a newer compatible coordinator (guard).

## 0.5.22

- **Fix: coordinator-dependent mutating commands now loudly ensure a live coordinator (`loud ensure`).** When `send` encounters a coordinator in `Missing` or `Stale` state (binary identity mismatch), it automatically spawns or rotates the coordinator before delivering the message. The response and lifecycle events declare the action loudly: `coordinator_auto_restarted` flag, previous coordinator state, new coordinator identity, and a `coordinator.ensure_restarted` event are all emitted so operators can audit the transition. If the coordinator cannot be brought up, the command remains fail-closed — no silent delivery against a dead coordinator.
- **Read-only commands are unchanged:** commands that do not mutate team state continue to report a dead coordinator without spawning, preserving the existing behavior.
- **Dirty topology and other guards remain higher priority than `loud ensure`:** the ensure path is bypassed when the topology guard would refuse the operation, so safety rails are not weakened.
- **Upgrade experience improvement:** after a CLI upgrade, users no longer need to manually run `team-agent restart` to rotate the coordinator to the new binary. The first mutating command (e.g. `send`) automatically ensures the coordinator is on the current binary.
- **New: `loud_ensure_contract` (5/5).** Contracts cover: mutating send ensures missing coordinator, mutating send rotates stale coordinator, read-only commands report dead coordinator without spawning, loud ensure does not bypass dirty topology refusal, explicit restart semantics remain unchanged.

## 0.5.21

> **Note:** 0.5.20 contains a coordinator shutdown timeout defect (daemon sleep not interrupted on SIGTERM). Users on 0.5.20 are advised to upgrade directly to 0.5.21.

- **Fix: coordinator shutdown no longer times out waiting for the daemon sleep to expire.** The coordinator daemon loop previously slept in a single uninterruptible sleep call, causing `shutdown` to block for up to the full tick interval (default 30 s) before the coordinator acknowledged the stop signal. The daemon sleep is now split into ≤100 ms slices, each checking the shutdown flag, so the coordinator responds to SIGTERM within one slice. The `coordinator.exit` event continues to record `reason=signal` for audit, unchanged.

*(Includes all fixes from 0.5.20: coordinator heartbeat sidecar, `doctor --gate orphans` workspace scoping.)*

## 0.5.20

- **Fix: coordinator tick now emits a persistent heartbeat sidecar (`coordinator_tick.json`).** Each tick writes an extended identity snapshot — `pid`, `boot_id`, binary path, `phase`, and tick start/end timestamps — to a sidecar file beside the coordinator socket. On exit, the coordinator appends a `coordinator.exit` event carrying `reason` (`startup_error` / `panic` / `signal` / `stop`). For SIGKILL-class deaths (where the exit handler cannot run), the heartbeat window boundaries are used to bound the estimated time of death, enabling post-mortem forensics on silent coordinator exits (the external-project silent-death incident that drove this change).
- **Fix: `doctor --gate orphans` now defaults to the current workspace scope.** The orphan scan previously identified any `team-agent` coordinator process as a potential orphan regardless of which workspace it belonged to. Coordinators belonging to other projects are now placed in an `ignored_foreign` list and excluded from gate evaluation and kill candidates — only coordinators whose workspace path matches the current workspace are assessed. This eliminates the `dirty_003` class of false-positive orphan flags where a live coordinator from an unrelated project (e.g., `future_auto`) was incorrectly gated.
- **New: `coordinator_lifecycle_forensics_contract` (5/5) and `doctor_orphans_scope_contract` (4/4).** Two new contract suites cover: heartbeat sidecar written on every tick, exit event appended on clean stop, SIGKILL death bounded by heartbeat window, and orphan scan correctly scopes to current workspace and places foreign coordinators in `ignored_foreign`.

## 0.5.19

- **Fix: `diagnose` now surfaces coordinator health as first-class issues.** Three new issue IDs are emitted when the coordinator is in a degraded state: `coordinator_unavailable` (process dead or socket unreachable), `coordinator_stale_identity` (binary path or version does not match current executable — same source of truth as the rotation check), and `coordinator_schema_incompatible` (message-store schema version mismatch). Each issue carries a `repair` suggestion (`restart` or `repair-state`) so operators can act without reading source code. This closes the "diagnose green but send broken" blind spot confirmed by an external-project incident where `diagnose` reported no issues while `send` was failing due to a stale coordinator identity.
- **Fix: `restart` compatibility flag now has semantic documentation.** The `--compat` / `bool` flag on `restart` previously had no description in `--help` or internal docs. The flag meaning (allow restart across coordinator schema versions without rotation guard) is now documented in both the CLI help text and the structured output.

## 0.5.18

- **Fix: coordinator version-aware rotation.** Coordinator metadata now embeds binary identity (`current_exe` path + `PKG_VERSION` as canonical truth). On startup, if the running coordinator's identity does not match the current binary or the metadata is absent (legacy), `rotation_required` is set, the stale process tree is stopped, and the current binary is spawned in its place. Same-version coordinators that are healthy are treated as idempotent — no rotation occurs.
- **Fix: `shutdown` now uses three-branch stop strategy.** Three shutdown paths are distinguished: bare coordinator (no workers), scoped-last-live (coordinator is the last live process in the team scope), and scoped-live-sibling (coordinator with surviving sibling workers). Each path carries a `coordinator_stop_reason` field for audit and diagnostics.
- **Fix: `restart`, `status`, and `collect` now return a structured coordinator object.** The coordinator section of these command outputs now includes `status`, `pid`, binary identity fields, and `rotation_reason`, giving operators full visibility into coordinator lifecycle state without inspecting raw process tables.
- **Fix: `phase_golden` normalizer now correctly token-substitutes debug build binary paths under custom `CARGO_TARGET_DIR`.** The golden normalizer's bin-path token substitution previously missed paths under non-default `CARGO_TARGET_DIR` layouts. The `<TEAM_AGENT_BIN>` token now matches the actual debug binary path in all CARGO_TARGET_DIR configurations, eliminating the environment-specific phase B–F flake. (Second-effect evidence: `--lib` 1573/0 zero-exemption on this machine after 0.5.18 lands.)
- **New: `coordinator_version_rotation_contract` (6/6).** Contracts covering: rotation triggered on binary mismatch, legacy coordinator without metadata triggers rotation, same-version healthy coordinator is idempotent, rotation stops old process tree before spawning new, post-rotation coordinator carries current identity, `coordinator_stop_reason` field is present in shutdown output.

## 0.5.17

- **Fix: test isolation — `HermeticTestEnv` enforces four isolation planes (HOME / registry / socket / env).** A new `HermeticTestEnv` helper in `tests/support/hermetic.rs` provides a per-test isolated HOME directory, a private leader-registry database, an independent socket path, and a clean `TEAM_AGENT_*` environment block. A compile-time static guard prevents tests that call the real registry or real HOME from compiling without opt-in annotation, making side-effect leakage a build error rather than a flaky failure.
- **Fix: `phase_golden` sequential-order pollution eliminated in matching environments.** With `HermeticTestEnv`, the phase B–F golden fixtures no longer share state with the host's active coordinator or with each other. In environments where all four isolation planes are satisfied, both `--lib` and `--tests` gates now pass with zero exemptions. (Note: environments with a custom `CARGO_TARGET_DIR` path not yet covered by the golden normalizer's bin-path token may still see B–F red — tracked as follow-up H5.)
- **Fix: `restart` auto-attach now registers as the fifth restart-source entry (`source=restart-auto-attach`).** The auto-attach path after `restart` was not emitting a registry entry, causing `team-agent status` to show a gap in the attach-source timeline. The entry is recorded as best-effort (failure does not abort restart) and carries `tmux_endpoint_source` for audit.
- **Fix: real HOME registry no longer polluted by test fixtures.** Tests that previously called `register_leader` or `claim_endpoint` against the real `~/.team-agent/registry.db` now operate against the hermetic fixture database, eliminating the class of failures where a test fixture collided with a live host session (e.g., `team-video-workflow` socket conflict).
- **New: `test_isolation_escape_contract` (6/6).** Six contracts verify: HOME isolation, registry isolation, socket isolation, env isolation, no cross-test state leakage, and static guard prevents unannotated real-registry calls.

## 0.5.16

- **Fix: `report_result` attribution now uses physical submit boundary (0.5.14/0.5.15 frozen bug — attribution race).** `current_turn` was armed at coordinator start, causing any `report_result` injected before the worker's physical submit window to steal attribution from an unrelated task. A `SubmitObserver` now arms `current_turn` only after a successful tmux Enter injection and before the validation poll, ensuring attribution is live only inside the true physical submit window. Fallback priority is: explicit `task_id` → current-turn (armed) → last-delivered → task-row lookup.
- **Fix: `target_resolved` no longer acts as an attribution source.** The `message_is_reportable` predicate previously included `target_resolved` messages, allowing resolution events to steal task attribution. `target_resolved` is now filtered out by `latest_reportable_message_from_db` which requires an independent predicate excluding this event type.
- **Fix: endpoint convergence is now persisted before the converged response is returned (0.5.14/0.5.15 frozen bug — persistence race).** `persist::preserve_latest_endpoint_convergence_fields` uses an epoch-aware monotonic guard (`owner_epoch` CAS) to prevent a stale coordinator `save` from rolling back a freshly written convergence marker. `lease::verify_persisted_topology_convergence` performs a read-back verification on both the state file and the in-memory view before returning `converged`, ensuring the caller never receives a false-converged signal.
- **New: `endpoint_convergence_persistence_contract` (3/3).** Contracts covering: convergence is persisted before response, stale coordinator save cannot roll back convergence, restart refused-dirty-topology does not boot coordinator.
- **New: `result_attribution_race_contract` (2/2).** Contracts covering: physical submit window `report_result` without `task_id` belongs to the direct message, `target_resolved` without physical submit does not steal a no-task report.

## 0.5.15

- **Fix: `restart` now uses the converged endpoint for both spawn and attach commands (0.5.14 frozen bug).** After a successful `claim-leader` or `takeover` that triggered endpoint convergence, `restart` still opened workers on the old socket because spawn and attach commands were rendered from three independent state reads. A single `ResolvedRestartContext` (field: `selected`, `transport`, `tmux_endpoint_source`) is now resolved once at restart entry and threaded through the entire path, eliminating the stale-socket false-green loop.
- **Fix: attach commands are now rendered from the same transport as spawn.** `tmux_backend::attach_command_for_transport_session` and `attach_command_for_runtime_state_session_or_workspace` are new helpers that derive the attach socket directly from the restart transport, ensuring spawn and attach always target the same tmux server.
- **New: `spawn_argv` lifecycle event includes `tmux_endpoint` and `tmux_endpoint_source` metadata.** The `spawn_agent_window` event now carries the socket path and its provenance (`"transport"`, `"state"`, or `"workspace"`) so operators and tests can audit which endpoint a worker was spawned on.
- **Fix: `phase_golden` normalizer now token-substitutes endpoint paths by value shape.** The `value_looks_like_endpoint_path` predicate identifies socket paths by their structure (prefix + hex segment) rather than by field name, so any new endpoint-carrying field in golden fixtures is automatically normalized to `<SOCKET>` without requiring golden updates.
- **New: `claim_endpoint_convergence_contract` RED A–D (8/8).** Four additional restart-context tests cover: attach commands use the converged endpoint, spawn records the converged endpoint in metadata, transport and attach share the same state-selected endpoint, and legacy per-team snapshot endpoints are ignored by restart.
- **New: `lifecycle_transport_resolver_batch1` guard (4/4).** Structural guard confirming `rebuild.rs` migrated out of the raw-call allowlist and the three single-source resolver symbols are present.

## 0.5.14

- **Fix: claim/takeover success path now converges the tmux endpoint when the old leader server is dead.** When `claim-leader --confirm` or `takeover --confirm` succeeds, the topology layer now evaluates an `EndpointConvergenceDecision` and, if the old server is confirmed dead, rebinds the team's canonical socket to the new endpoint before returning. The leader emits a `leader_receiver.tmux_endpoint_converged` lifecycle event to confirm the switch. Previously the old endpoint persisted in stored state, causing every subsequent `restart` to return `refused_dirty_topology` (tmux_endpoint_socket_conflict + leader_receiver_socket_mismatch) indefinitely — a deadlock that required manual intervention.
- **Fix: `refused_dirty_topology` exit eliminated from claim/takeover success path.** The `tmux_endpoint_socket_conflict` and `leader_receiver_socket_mismatch` issue IDs are no longer raised after a successful claim or takeover when the old server is dead; endpoint convergence at bind-time removes the conflict before restart sees it.
- **New: `claim_endpoint_convergence_contract` (4/4).** Four contract tests cover the convergence path: RED1 fixture setup, RED2 convergence decision emitted at claim time, RED3 subsequent restart exits cleanly, RED4 `leader_receiver.tmux_endpoint_converged` event is recorded.
- **Fix: R1 harness bypass in legacy bare-pane claim deadlock contract.** The harness now has a second gate for legacy bare-pane fixtures that lack a convergence spec, preventing the fixture from being skipped silently when the primary spec path is absent.

## 0.5.13

- **Fix: claim/takeover guards now use full 5-tuple matching (E51 — sixth pane-identity case).** The `attach-leader` claim and worker takeover paths previously compared bare `pane_id` strings when deciding whether an existing binding should block promotion. A new `WorkerPaneBindingMatch` classifier now evaluates the full `(endpoint, session, window, pane_id, pane_pid)` tuple: only an exact-match live binding blocks; stale or legacy bare-pane records are classified as `DiagnoseOnly` and do not prevent the claim from succeeding.
- **Fix: stale and legacy bare-pane records are diagnose-only, not blockers.** Bindings recorded before 5-tuple validation was introduced (legacy) and bindings whose tmux pane no longer exists (stale) are surfaced by `diagnose` under the appropriate topology issue IDs but no longer cause `attach-leader` or takeover to return `refused_dirty_topology` indefinitely. This eliminates the deadlock where a stale legacy binding prevented any future leader from attaching.
- **Fix: `refused_dirty_topology` next-actions are now executable.** When a genuine dirty-topology refusal does occur (live split-brain), the returned `next_actions` list contains commands the operator can run immediately to resolve the conflict, rather than advisory prose.

## 0.5.12

- **Fix: stale worker pane now accepted to inbox then replayed after `start-agent` (B-case).** In 0.5.11, a blocked message to a stale worker pane was refused at the send layer — the message was never persisted, so there was nothing to replay when the correct pane re-attached. The send path now accepts the message and writes a `queued_pane_missing` row to the inbox while still refusing physical injection to the stale target. When `start-agent` subsequently refreshes the pane tuple, the same `message_id` is automatically replayed exactly once, completing the pane-identity family's replay-ability guarantee introduced in 0.5.11.

## 0.5.11

- **Fix: pane-identity family root cause — worker delivery, named addressing, and restart now all converge on `(endpoint, session, window, pane_id, pane_pid)` 5-tuple validation.** A bare `pane_id` was previously accepted as a delivery target without cross-checking the enclosing window and tmux socket endpoint, allowing messages to land on reused pane IDs from other sessions or sockets. All three delivery paths now reject a cached binding unless the full 5-tuple matches the live tmux state.
- **Fix: stale pane bindings are now fail-closed.** When the topology check finds that a cached pane is missing or belongs to a different window (`queued_pane_missing` / `stale`), the delivery path refuses rather than falling through to a silent drop. The E6 offline-mailbox path accepts the message to disk so it can be retried when the correct pane re-attaches.
- **Fix: restart refuses dirty topology before kill/spawn.** The restart path now emits a `refused_dirty_topology` lifecycle event and returns an error if the current tmux state conflicts with the stored binding (endpoint or session split-brain). The refusal happens before any `kill` or `spawn` command is issued, preventing workers from starting in an inconsistent topology.
- **New: `diagnose` topology audit.** `team-agent diagnose --topology` (and the `--gate` variants) audits the live tmux topology against stored bindings and reports findings under six issue IDs: `stale_endpoint`, `session_mismatch`, `window_mismatch`, `pid_drift`, `pane_missing`, `socket_split_brain`. The output is machine-readable JSON for CI and operator tooling.

## 0.5.10

- **Fix: `send --to-name` to a live team with unattached leader now queues to offline mailbox (real-machine wiring).** In 0.5.9 the offline mailbox path was implemented but not wired into the third-party `send --to-name` resolution path — live-team sends to an unattached leader still returned a delivery error. The wiring is now in place: when the target team is live but the leader receiver is not attached, the message is written to the offline mailbox with `delivery_status: queued_until_leader_attach` and a stable `message_id`. When the leader attaches, the mailbox is replayed exactly once.
- **Fix: `attach-leader` accepts `--provider fake` for explicit stub-mode binding.** The `attach-leader` command now recognises `--provider fake` as a first-class option for e2e harness and test scenarios that use a fake-provider leader pane. Previously fake-provider bindings could only be triggered implicitly; the explicit flag makes harness setup deterministic.
- **Fix: tmux pane queries in `attach-leader` routed through `TmuxBackend` API (N16/CP-1).** All tmux operations in the attach path now go through `TmuxBackend::for_tmux_endpoint`, eliminating raw `Command::new("tmux")` invocations without socket scoping.
- **Fix: fake-provider pane TTY echo suppression prevents double-delivery.** When `attach-leader` binds a fake-provider pane running `/bin/cat`, the terminal driver's echo bit caused every injected token to appear twice in the pane capture. `attach-leader` now calls `stty -echo` on the pane's TTY device at bind time, ensuring exactly-once delivery semantics in test harnesses.

## 0.5.9

- **DX: `report_result` integrity warnings for unverified success claims.** When `report_result` is called with `status: success` but the attached tests field contains no executed evidence (all entries are `not_run`, scalar values, or missing descriptions), the coordinator now accepts the call but returns a structured `warnings` array instead of treating the claim as verified. Callers can distinguish a verified success from a best-effort claim without parsing prose.
- **DX: `--to-name` failure uses a three-tier refusal taxonomy.** Resolution failures now return one of three structured error kinds: `team_key_not_found` (lists available team keys and notes if a spec name was given instead of a team key), `leader_not_attached` (distinguishes whether the caller is the owner team or a third party), or `workspace_no_state` (the target workspace has no coordinator state). Each kind carries enough context to act on without a follow-up `status` call.
- **DX: offline mailbox — `send --to-name` queues to disk when leader is unattached.** When the named target's leader is not currently attached, the message is written to an offline mailbox (`queued_until_leader_attach`) rather than failing. When the leader attaches (via `attach-app-server-leader` or equivalent), queued messages are replayed exactly once in arrival order. The response includes `delivery_status: queued_until_leader_attach` so callers know the message is safe without polling.
- **New command: `team-agent leaders`.** Discovers all host-leader registry entries for the current machine and classifies each as `LIVE`, `STALE`, or `AMBIGUOUS`. Useful for diagnosing multi-team or multi-workspace setups where multiple leader sockets may be registered. `--json` returns the full registry schema.
- **New flag: `send --to-leader`.** Sends to the unique live leader on the current machine by short name, without requiring the full `--to-name workspace::team/leader` address. Refuses with a structured error if the leader is ambiguous or stale, directing the caller to use `team-agent leaders` to resolve.

## 0.5.8

- **DX: `status` stale detection sourced from physical liveness, not lifecycle state.** When a worker's tmux pane is dead, `status` now marks that worker `stale: true` with `stale_reason: "pane_dead"` derived from a physical liveness probe — not from the coordinator's own lifecycle bookkeeping. This prevents stale agents from appearing live in status output when the coordinator has not yet observed the pane exit.
- **DX: `status` exposes `current_task` and heartbeat as structured best-effort observation.** The status response now includes a `current_task` field (the task currently assigned to the worker) and a heartbeat timestamp for each agent entry. These are best-effort observations from the coordinator's view — not authority — and are marked as such so callers can distinguish coordinator knowledge from ground truth.
- **DX: `--to-name` failure attaches structured advisory candidates.** When a named-address send fails to resolve the target, the error response now includes an `advisory_candidates` list of reachable named addresses on the same socket, ranked by recency. This lets the caller see what names are actually live without running a separate `status` call.
- **DX: session all-gone recovery chain surfaces structured short-circuit hint.** When all workers in a session have exited and `diagnose` or `status` detects the all-gone state, the response now includes a structured `recovery_hint` that describes the recommended recovery path (e.g., `restart --allow-fresh`) along with the team and session context — condensing the triage steps a human or agent would otherwise have to run manually.
- **DX: recovery-originated task assignment carries structured confirmation marker.** When a worker accepts a task via a recovery path (rather than a normal assign), the task envelope now includes a `recovery_marker` field identifying the recovery origin. This lets the receiving worker log and surface the recovery context without parsing the message text prefix.

## 0.5.7

- **DX: `remove-agent` rejection lists all required flags at once with a copyable command.** When `remove-agent` is refused because the worker is still running or the agent spec still exists, the error now includes every flag needed to complete the operation (`--force`, `--from-spec`, etc.) in a single copyable command line. Previously the error named flags one-at-a-time, requiring multiple retries to discover the full set.
- **DX: `send` default human output condensed to one line.** The default (non-`--json`) `send` output is now a single status line that reflects the actual delivery outcome. Fields that are permanently `None` for a given send shape are omitted rather than printed as `null`. The `--json` envelope is fully compatible with prior releases.
- **DX: `send` reminder text matches actual delivery status.** The harness reminder appended after a send now describes what actually happened (delivered, blocked, queued) rather than using a generic template that could contradict the status line above it.

## 0.5.6

- **Fix: `report_result` attributes to the current message turn as the primary source.** When a worker calls `report_result`, the coordinator now looks up the worker's current in-flight message turn first. If the turn is open and matches the worker's `agent_id`, the result is attributed to that message — superseding the previous fallback-first behaviour that could attribute results to a stale prior delivery. The injected result simultaneously arms the turn's `turn_open` marker so the next result call for the same worker correctly recognises a fresh turn.
- **Fix: historical backfill is capped to messages delivered after the latest known result.** The fallback path (used when no current turn is in flight) no longer considers messages delivered before the last confirmed result. This prevents a post-restart worker from having its fresh result silently attributed to an old, already-resolved delivery from a prior session.
- **Fix: `delivered` semantics remain strict throughout.** A message is marked `delivered` only when physical delivery to the tmux pane succeeds. The current-turn attribution fix does not relax this invariant — attribution and delivery status are updated independently, so attribution to the current turn does not imply delivery success.

## 0.5.5

- **Fix: `send` accepted ≠ delivered — semantic split with `delivery_status` field.** The response envelope now carries a `delivery_status` field (`delivered` / `blocked` / `queued_pane_missing` / …) separate from the top-level `ok` / `status`. `accepted` means the message entered the queue; `delivered` means it reached the tmux target. Callers can now distinguish physical delivery from queue acceptance without parsing `reason`.
- **Fix: `--watch-result` observes initial delivery, not just result collection.** When a worker window is missing at send time, `--watch-result` no longer registers a result watcher before the message has been physically delivered. The response carries `"channel": "delivery_blocked"` and no `watch` key until delivery succeeds.
- **Fix: tmux target missing classified as recoverable `blocked`, not permanent failure.** `queued_pane_missing` is now a typed blocked status. The message stays in the queue and is replayed when `start-agent` repairs the missing window.
- **Fix: `start-agent` repair replays blocked messages with the same `message_id` (idempotent).** When a worker window is repaired, queued messages are redelivered using their original `message_id`. Replaying the same id a second time is a no-op — the coordinator deduplicates by id, so retry-safe callers receive exactly-once semantics.
- **Fix: leader notification delivery enforces socket boundary exclusivity.** Leader receiver delivery now requires that the target pane's tmux socket matches the recorded `leader_receiver` socket. Cross-server delivery (fallback to a pane on a different tmux server) is refused when a socket is on record, preventing silent delivery to the wrong server.
- **Fix: `fallback_pane` boundary enforced with loud refusal.** When a `fallback_pane` would cross to a different tmux server than the recorded socket, delivery is refused with a structured error rather than silently delivering to the wrong pane.
- **Fix: pending leader notifications replayed after `attach-leader` re-bind.** When a leader receiver is re-attached (via `attach-app-server-leader` or equivalent), any leader-bound messages that failed delivery while the receiver was absent are now replayed. `status` exposes these as `pending_leader_notifications` so operators can observe the replay queue before and after attachment.

## 0.5.4

- **Fix: Codex session identity crossbind — same-cwd multi-worker capture corrected.** When multiple Codex workers start in the same working directory, session attribution previously relied on recency and could assign a session to the wrong worker. Sessions are now attributed by reading the embedded `TEAM_AGENT_AGENT_ID` identity marker from the rollout transcript head. Real-machine acceptance: 8 same-cwd Codex workers each captured a distinct session, with `attribution_confidence: high` for all eight.
- **Fix: `restart` identity preflight and postflight checks.** Before spawning workers, `restart` verifies that each worker's stored `session_id` maps to a rollout transcript embedding that worker's own `agent_id`. A mismatch is refused with a structured `refused_session_identity_mismatch` response naming the affected worker, the stored session UUID, and the embedded identity. After spawning, a postflight check confirms the resumed transcript's embedded identity matches. Both checks surface `identity_ok` / `embedded_agent_id` fields in restart event logs.
- **Fix: `restart --allow-fresh` clears poisoned Codex tuple atomically.** When a worker is decided `fresh_start` due to an identity mismatch, both the root and team-scoped `session_id` / `rollout_path` fields are now cleared before the fresh process is spawned. Previously, the poisoned tuple persisted in state and `diagnose` continued to report `session_identity_mismatch` even after a successful fresh start.
- **Fix: `diagnose` and `status` expose session identity mismatch.** When a crossbind is detected, `diagnose` exits with code 1 and reports `issue: session_identity_mismatch:<worker_id>` with the `expected_agent_id`, `embedded_agent_id`, poisoned session UUID, and a remediation hint (`restart --allow-fresh`). `status` surfaces the same poisoned tuple fields for operator visibility.

## 0.5.3 (Windows-native, Preview)

- **Windows-native transport (`--backend conpty`) real-machine surface.** Full end-to-end delivery on native Windows via ConPTY + named-pipe shim. `team-agent quick-start --backend conpty --team <name>` spawns the coordinator daemon, which owns the `windows-shim.exe` process for the (workspace, team) tuple. `team-agent send <agent> "<msg>"` returns `ok=true` with `message_status: "accepted"` and a `message_id`. `shutdown` reaps the shim via `platform::process::terminate_pid` and emits `conpty_shim` audit action. **Real-machine six-check on Windows 10.0.26200 (batch9-final = 81a360c): 5/6 PASS + 1/6 partial** (`peek` needs the coord daemon's shim connection which CLI processes can't share yet — a follow-up refinement).
- **Packaging still declares Windows as `PreviewCompileOnly`, NOT Native.** Constraint from Batch 7-9 gate reports: promotion to `Native` requires 6/6 real-machine PASS. `peek`'s partial status blocks that promotion; the 0.5.3 shipping surface is honest about being a Preview.
- **Subscription-provider gate deferred.** Windows real-machine `codex` (`codex-cli 0.120.0`) and `claude` (`2.1.170 Claude Code`) are installed but not authenticated on the SSH host. Real-provider `send`+`report_result` round-trip verification requires operator login; documented as "待用户" per leader constraint.
- **Bug: F4 (Phase 1b shim client-reuse race) fixed.** `windows-shim.exe`'s accept loop now treats `ERROR_PIPE_CONNECTED (0x80070217)` as success and calls `FlushFileBuffers` + `DisconnectNamedPipe` before recreating the pipe instance. Fixes the "shim dies after first Hello disconnect" symptom.
- **Bug: F5 coordinator daemon on Windows no longer falls back to tmux.** `run_daemon` derives `team_key` from `--team` CLI arg (Batch 9) or `state.active_team_key` fallback (Batch 7). Enables the daemon to build a `ConPtyBackend` instead of the tmux-fallback path that broke every MCP-facing operation.
- **Bug: F6 `state.transport.shim` block preserved across merges.** `state::persist::apply_persist_merge_contract` now calls `preserve_transport_shim(incoming, latest)`. Fixes the "shim block silently dropped by downstream state saves" symptom.
- **Bug: F7 daemon-shim coexistence gap fixed.** Shim ownership moved from `quick-start` (one-shot) to `coordinator::conpty_shim::ensure_shim_running` (daemon-owned). Windows shim + coord daemon spawns now set `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB` creation flags so both truly outlive their SSH-parent process trees.
- **Bug: F8 `quick-start` seed-state ordering fixed.** Coordinator daemon now receives `--team <name>` on its CLI (via `start_coordinator_with_team`), so `quick-start` no longer needs to pre-seed `state.active_team_key`. Fixes the "existing runtime, use restart" branch triggering on fresh Windows quick-start.
- **RED-first contracts locked.** 8 RED tests across Batches 6-9 guard the shim reuse race, the state-merge preserve, the coordinator ownership shape, the reconnect API, the C-3 stale event family, the detached-process spawn flag, the launch-flow seed-state absence, and the coordinator `--team` argument forwarding.
- **CR anchors held.** C-1 (no silent tmux fallback), C-3 (typed stale events for shim death), C-4 (sha256 CI==Remote reconciliation), C-6 (`platform.terminate_force_only` audit event) all preserved and real-machine verified.

## 0.5.2

- **Added: `--backend conpty` transport for Windows-native ConPTY sessions.** A new `--backend` flag lets you explicitly select the transport backend: `tmux` (default, unchanged behaviour) or `conpty` (Windows ConPTY via named pipe). The full chain ships: a portable named-pipe control protocol (`conpty-transport`), a ConPTY shim binary (`win-conpty-phase0`), and a ConPTY backend in the coordinator (`conpty` module). The factory (`transport_factory`) assembles the right backend from persisted state at startup; on non-Windows hosts it returns a typed `MuxUnavailable` rather than silently falling back. Covered by factory guard suite (C-1/C-4/C-5/C-6), pipe-token persistence guard, and ConPTY end-to-end fake-worker tests.
- **Added: `attach-app-server-leader` subcommand for app-server leader hosting.** Codex app-server sessions can now act as the Team Agent leader host. The new subcommand wires a live Codex app-server pane as the leader receiver, writing the transport-kind tuple atomically and advancing the epoch. MUST-12 is updated: delivery paths are read-only with respect to ownership; the sole ownership-mutation entry point is the explicit `attach-app-server-leader` CLI path.

**Note on CI baseline:** The historically-known red test `leader_bound_delivery_must_target_bound_leader_pane_not_missing_leader_window` was absent from the merged integration run (0 failures in --tests). CI hermetic run will confirm whether it remains absent or resurfaces; result will be noted in the release report.

## 0.5.1

- **Added: `send --to-name` resolves stable workspace/team/agent or leader names to the current live pane.** Routes a message to a live tmux pane by stable name (`<workspace>::<team>/<agent>` or `leader`) without needing to look up the pane ID. The MVP assumes a trusted local caller; no auth gate is applied. Design includes 7 architectural constraints and is covered by 13 unit tests; 12/12 real-machine send scenarios passed.

## 0.5.0

This release completes a six-phase internal refactor that was carried out entirely behind the existing public API surface. Every gate (Phase A–F + 12-item subscription gate + leader gate) passed before shipping.

**Six-phase refactor summary**

- **Phase A — Lifecycle transaction lock.** Coordinator state transitions (start/stop/restart) are now serialized through an explicit lock, eliminating the class of races where two concurrent operations would corrupt worker state.
- **Phase B — Persist merge contract contraction.** The persistence layer's merge surface was narrowed: callers can no longer accidentally clobber in-flight state by passing a partial struct. The contract is now a typed diff rather than a full replacement.
- **Phase C — Scanner split.** Provider session detection was split into per-provider scanner modules (Claude, Codex, Copilot). Each scanner owns its detection heuristics; the coordinator no longer carries interleaved provider-specific branches.
- **Phase D — Provider wire single-source.** All provider launch strings (binary names, env vars, session-id flags) are now defined in one place and injected at wire-up time. Duplicate definitions across start/restart/health paths have been removed.
- **Phase E — Abnormal step modularization.** The "abnormal tick" path (provider died unexpectedly) is now a standalone step module with its own entry/exit contract, making it independently testable and easier to extend.
- **Phase F — Abnormal-exit transcript-error single gate + recency + CLI 2.1.181 assistant-shape adaptation.** Detecting that a provider exited abnormally (vs. graceful stop) now goes through a single gate that checks transcript recency and is adapted to the Claude CLI 2.1.181 `assistant`-role message shape.

**Bug fixes**

- **Fixed: `doctor` failed to parse paths containing spaces.** The `doctor` subcommand now correctly tokenises space-embedded paths, preventing false-negative health checks on non-standard install locations.
- **Fixed: `reset` spawn failure is now fail-closed.** Previously, if a coordinator failed to spawn during `reset`, ownership was silently dropped. It now hard-fails and leaves the team in a safe stopped state.
- **Fixed: `restart` correctly rehydrates worker roles.** After a `restart`, workers that had a custom `--role` are now restored to that role instead of falling back to the default.
- **Fixed: `send --pane` argument parsing regression (introduced in 0.3.26).** Pane targeting was silently ignored after an internal refactor; this is now restored.

**MCP-CYCLE shape baseline**

The strict MCP-CYCLE shape is a known baseline as of 0.4.10. The functional PASS evidence chain (inbox → event → collect → notify) is complete and has not regressed in this release.

## 0.4.11

- **Fixed: starting a worker after a failed stop no longer opens a duplicate window.** If `stop` fails, Team Agent now refuses to `start` the same worker until the stop completes, preventing a second window from opening alongside the stale one.
- **Fixed: each `team-agent claude` launch now gets its own independent session.** Previously, a second launch could attach to an existing session instead of starting fresh; each invocation now creates a distinct session.
- **Improved: the generic teammate system prompt is leaner.** The runtime contract section was trimmed from 30 lines to 16, keeping only the essential MCP communication rules and making it easier for teammates to reach the key requirement: all replies must go through MCP tools.

## 0.4.10

- **Added: workers can now run at a configurable effort level.** Set `effort:` in a role's section of `TEAM.md` or the role document, and Team Agent passes the right flag to the provider. Supported values follow the provider's own effort scale.
- **Improved: worker lifecycle now tracks five distinct runtime states.** The new `WorkerRuntimeState` enum — `Pending`, `Starting`, `Running`, `Stopping`, `Stopped` — gives the coordinator and status commands a precise view of where each worker is in its lifecycle, replacing the previous boolean running/stopped split.
- **Improved: foreground workers now run in their own process group.** Phase 1 of the foreground process-group work launches each worker in a dedicated pgrp, so signals and cleanup target the right processes and do not accidentally affect the leader.
- **Fixed: `CLAUDE_EFFORT` is now scrubbed from the worker environment.** The env-export path previously could re-introduce variables that had already been unset; the scrub now runs after the full environment is assembled, so effort and other control variables do not leak into worker subprocesses.

## 0.4.9

- **Fixed: Claude environment variables no longer leak into spawned subprocesses.** The leader launcher now unsets all `CLAUDE_CODE_*` variables before starting workers, covering both the managed tmux path and the shell wrapper path. The ExecProvider in-tmux path also clears the Claude environment block, and the removal order is corrected to run after `envs()` is called — closing the leak that caused Claude workers to inherit the leader's session environment.
- **Fixed: E2E tests no longer leave coordinator processes running after a test completes.** `TestWorkspace::Drop` now precisely cleans up coordinator processes, preventing port/socket conflicts between test runs.

## 0.4.8

- **Fixed: Claude workers now start with a clean session, never inheriting the leader's session ID.** Previously, spawning a Claude worker could accidentally reuse the leader's Claude Code session environment, causing the worker to capture the leader's transcript instead of its own. Claude workers now launch in a fully isolated environment with all session-related variables cleared.
- **Fixed: `restart` now reliably resumes workers that had existing conversations.** A worker that had chatted before a shutdown is now brought back with its full context. A never-used worker (no transcript, no interaction) is correctly started fresh — no `--allow-fresh` flag required.
- **Fixed: session capture state is now set atomically with the session tuple.** The capture state and session ID are now written together, preventing a window where one was set but the other was not, which could cause misrouted messages.
- **Fixed: `reset` now clears the worker's spawn epoch**, so a restarted worker cannot accidentally be matched against stale spawn records from a previous lifecycle.
- **Fixed: the `transcript-ready` state machine is now consistent.** Workers only transition to the ready state once their transcript file is actually present and writable, preventing premature delivery attempts.
- **Fixed: status now rejects ambiguous multi-team targets.** If `--workspace` matches more than one team, `status` now returns `ok: false` with `reason: team_target_ambiguous` instead of silently picking one.
- **Fixed: `start-agent` now clears the fresh tuple and guards against pending mismatches** when launching a worker that previously existed, preventing stale state from leaking into the new lifecycle.
- **Fixed: Claude child/team environment is fully cleaned on worker spawn.** All `CLAUDE_CODE_*` and team-scoped environment variables are unset before launching a Claude worker, closing the root cause of session bleed-through.

## 0.4.7

- **Improved: message delivery and provider wiring are now more reliable.** Provider-specific logic is unified in one place, so fixes and improvements apply consistently across Claude, Codex, Copilot, and Gemini. A bug where GeminiCli and Fake providers missed some delivery updates is now fixed.
- **Improved: `status --json` output is now compact by default.** The default output shows 7 top-level fields and 4 per-agent fields — just what you need at a glance. Use `status --detail` to get the full diagnostic view with all coordinator and message fields.
- **Added: `status` now reports a `ready` / `not_ready` summary.** A single field tells you whether the team is ready to work and, if not, lists the reasons why (for example, workers not yet spawned).
- **Improved: provider adapters are now split into per-provider files.** Claude, Codex, Copilot, and Fake adapters each live in their own file, making the codebase easier to navigate and extend.

## 0.3.28

- **Improved: teammate windows now use one layout manager.** Starting, adding, removing, and restarting teammates now share the same layout rules, so panes are placed more predictably and new layout changes do not need to be reimplemented in several places.
- **Fixed: pasted messages no longer rely on the old grace fallback path.** Team Agent now waits for the real submit signal instead of treating a briefly visible pasted token as good enough, which makes delivery confirmation stricter and more reliable.
- **Fixed: Copilot teammates keep the expected launch flags.** Copilot workers now preserve the intended bypass settings through the unified layout and launch path.

## 0.3.27

- **Improved: message delivery now uses one consistent path.** Team Agent no longer has a separate side path for one kind of pasted message, so delivery checks behave the same way across providers and avoid branch-specific surprises.
- **Fixed: leader delivery now verifies the real leader pane before sending.** This prevents rare cases where the system could mistake a teammate window for the leader and send messages back into the wrong place.
- **Fixed: Copilot teammates keep the expected bypass flags during launch and delivery checks.** Teams using Copilot now preserve the intended startup flags instead of drifting into a stricter or inconsistent prompt flow.

## 0.3.26

- **Fixed: on a fresh Mac, the `team-agent` command was not found after install.** The installer now places it in a directory already on your `PATH`, so it works immediately.
- **Fixed: shutting down a team could close the leader terminal.** Shutdown now only closes worker windows and leaves the leader intact.
- **Fixed: starting a new teammate could steal the leader terminal focus.** New workers now open in the background.
- **Fixed: in rare cases the framework could mistake a worker window for the leader, causing messages to loop back.** The framework now refuses to rebind the leader to a registered worker pane.
- **Improved: message delivery diagnostics.** When delivery is retried, the framework now logs what it saw on screen, making it easier to diagnose delivery issues. It also uses the proven token-consumption gate for the Codex paste-prompt path, so submit verification is more reliable.
- **Added: send messages directly to any tmux pane with `team-agent send --pane`.** This works across teams for cross-team communication.

## 0.3.25

- **Fixed: adding a teammate to a running team could leave it unusable and impossible to remove.** In some cases, add said the teammate already existed while remove said it was unknown, because the two commands were reading different copies of the team record. They now use the same record, so add, re-add, and remove agree with each other. A failed add no longer leaves a half-created teammate behind.
- **Fixed: a new teammate added to a running team might not receive messages.** The first message could land in the new teammate's input box without being submitted. Team Agent now verifies that the message is submitted and the teammate starts working.
- **Fixed: a new teammate could be opened on top of an existing teammate's window.** Adding a teammate used to split into another teammate's window in some cases. New teammates now always get their own window.
- **Fixed: an idle teammate could be wrongly shown as busy or stuck.** Older versions guessed busy or idle state from screen text, which could be fooled by leftover text from a completed task. Team Agent now uses the actual conversation record to decide whether a teammate is working or idle.
- **Fixed: a misleading "has blockers" warning right after install.** The installer self-check could run doctor in an empty folder and mistake "no project here" for a real blocker. It now separates real problems from an empty-folder check.

## 0.3.24

- **Fixed: after pasting text into a teammate's input box, pressing Enter could sometimes do nothing and the message was not actually sent.** Previously, Team Agent could paste the text and press Enter immediately, before the pasted content had fully appeared in the teammate's interface. That left the message sitting in the input box instead of sending it. Team Agent now waits until the pasted content is visible before pressing Enter, so pasted messages are delivered reliably.
- **More reliable message delivery and team bookkeeping.** Team Agent is steadier when a team is closed and reopened, when multiple teams share the same folder, or when windows move around. Message addressing and team state records now line up more consistently, reducing edge-case delivery failures.

## 0.3.23

- **Fixed: closing a team and reopening it could lose a teammate's conversation, forcing a fresh start.** Previously, after you shut a team down and restarted it, a teammate that had already been chatting could be wrongly judged as "nothing to resume" — even though its full conversation history was safely saved on disk. You were then pushed to start that teammate over from scratch, losing its context. Now the system recognizes the saved conversation and brings the teammate back where it left off, so a normal close-and-reopen keeps everyone's context.
- **Added guidance for running a team inside a team.** When you ask a teammate to start its own nested team, there's now a clear in-app guide covering where the child team should live (its own folder, never the parent's), how the parent and child talk to each other, and an important safety rule: a child team must never shut down the folder the main team is running in.

## 0.3.22

- **More reliable message delivery, especially when something goes wrong with one message.** Previously, a single problem message — one whose destination had gone away, or that couldn't be confirmed as delivered — could quietly hold up the whole batch of pending messages behind it, so other teammates' messages stopped arriving too. Now each message is handled on its own: one bad message is set aside and reported, and everything else keeps flowing. The system also double-checks that a message actually landed on the recipient's screen before calling it delivered, and if a teammate's window has moved, it finds the new location instead of sending into a dead one. The result is that normal team chatter keeps moving even when one delivery hits trouble.

## 0.3.21

- **Fixed: messages from a teammate could get stuck and never reach the lead.** Previously, if a teammate's message happened to contain certain common words, the system could mistake it for a "crossed-wires" session and hold the message back instead of delivering it. Now the system reliably tells apart a genuine session problem from ordinary chat, so normal messages no longer get blocked — and if a real session problem does happen, it now corrects itself automatically instead of staying stuck.

## 0.3.20

- **Fixed: closing, restarting, or adding/removing teammates could fail on teams opened a certain way.** If you connected to a team using a custom setup, these management commands used to error out and couldn't find the right window. They now work reliably no matter how the team was opened.
- **Fixed: restarting a team would sometimes hang, claim the team was gone, and quit on its own.** On those same custom setups, a restart could wrongly decide the team "no longer exists" and give up halfway, even though the team was running fine. Restart now finds the team correctly and no longer bails out by mistake.

## 0.3.19

- **Fixed: reconnecting to a team could fail and not find the lead's window.** In some cases, rejoining a team failed because it remembered the wrong place to connect. It now finds the right place automatically and remembers it, so reconnecting works on the first try.
- **Improved: a misplaced startup option now gives you a clear message.** If you put a team's startup option in the wrong spot, it used to be silently ignored, leaving you guessing. Now it tells you right in the terminal exactly what's wrong instead of quietly dropping it.

## 0.3.18

- **Fresh teams created with `team-agent quick-start` are now correctly managed-topology by default again** (0.3.17 regression fix): the `is_external_leader` topology marker is now written explicitly on team creation (`false` for `quick-start`, `true` only for `--external-leader` opt-in or external attach), and every consumer (status, lifecycle, shutdown) reads it through a single helper. A missing marker is consistently interpreted as the managed default everywhere, so 0.3.17 teams could mis-classify themselves and `team-agent status` could falsely show ready when the managed leader receiver hadn't attached yet — both fixed. Managed teams now report a `degraded` / not-attached status when the leader receiver is missing, instead of silently saying ready.
- **`team-agent reset-agent --discard-session` actually starts a fresh worker context now**: previously a discard could leave a stale provider session id behind because the save path backfilled it from an in-flight capture. The save path is now tombstone-aware (it does not backfill a session after a discard), the next launch reports `start_mode = Fresh` with a new `session_id`, and the JSON output now carries structured `start_mode` / `discarded` / `new_session_id` so operators can verify the reset actually took.

## 0.3.17

- **`team-agent attach-leader` finds the leader pane on the first try in multi-team workspaces**: `--team` is now threaded through the lifecycle command port into the attach path, the attach logic selects team-scoped state and probes the tmux endpoint recorded in `leader_receiver.tmux_socket` / `tmux_socket` before falling back to the workspace default socket, and the endpoint that actually produced the pane is persisted back so subsequent attaches go straight to it. When the leader pane cannot be located, the error message now lists every endpoint that was searched instead of failing opaquely.
- **`--external-leader` opt-out is now fully wired** for `team-agent codex` / `claude` / `copilot`: the flag is recognised at the managed-launcher entry, persisted on the team as `is_external_leader = true`, suppresses the managed `:leader` window creation, and is honored across dispatch / restart / status. CLI usage is clear about putting `--external-leader` before any `--` provider passthrough separator, and contracts now cover the no-tmux / real-shape paths.

## 0.3.16

- **`team-agent <provider>` is now structurally unkillable from your terminal**: managed launchers (`team-agent codex`, `team-agent claude`, `team-agent copilot`) now create the leader provider process **inside** the team's tmux session and attach your terminal to it as a tmux client. Closing the terminal — or even `kill`-ing it from outside — only detaches the client; the leader provider and the rest of the team keep running. Same semantic as `tmux new-session -t ...` but with no manual tmux invocation. Status output, the team-agent skill, and `attach-leader` output all carry the `:leader` window pointer so re-attaching from anywhere is one command.
- **`--external-leader` opt-out keeps the old topology** for users who already drive the leader from their own terminal stack: pass `--external-leader` to `team-agent codex` / `claude` / `copilot` and the launcher behaves like before — the leader provider stays on the terminal you started it from, and the framework only treats it as the leader (no managed-session take-over). `state.is_external_leader` carries the choice forward through dispatch / restart / status; nested teams keep separate is_external_leader values so a parent and child team can each use the topology that fits.
- **`team-agent shutdown` no longer reports a false `partial`** when the coordinator hits a "late success" race (it finished cleanup just after the shutdown timer started reporting). The shutdown result classifier now folds the late-success outcome into the success bucket — the JSON / `lifecycle.shutdown` event now say `ok` when the coordinator did in fact stop cleanly.

## 0.3.15

- **Adaptive layout is on by default**: `team-agent quick-start` now lays out worker windows automatically — up to 3 panes per window (332 split for 8 workers becomes 3+3+2), agents added or restarted later land in the same adaptive layout instead of arriving as orphan windows, and the leader pane is never touched. The whole team shares one tmux socket and one window tree, so opening a second team in the same workspace doesn't fight the first.
- **`report_result` is now delivered to the leader even when the worker's MCP channel is down**: a worker that finishes after losing its MCP session no longer leaves the leader staring at an apparently-stuck worker. The runtime now records the final result through a leader-side fallback delivery path (`team-agent collect` continues to be the authoritative state-update path) and surfaces it with two new diagnostic subcommands and a single noisy audit event so missing report_results stop being silent. `team-agent diagnose` will point at the fallback record when MCP delivery was lost.
- **Verified RS recovery runbook is now a Team Agent skill reference**: `skills/team-agent/references/recovery-runbook.md` documents the supported recovery moves (Transport closed, leader pane re-attach, coordinator restart, etc.) for both human operators and Team Agent leaders. The skill description points at it so the runbook gets pulled in automatically when symptoms match.
- **`team-agent add-agent` no longer dead-locks or leaves a ghost role**: the runtime-state path is now the single source of truth for add/start-agent (no second-source write that could go stale), and a failure mid-add cleanly rolls back instead of leaving an agent record without a launched session.
- **`team-agent restart --allow-fresh` is no longer sticky across restarts**: a fresh session id is now persisted as the new expected session id, so the next restart resumes against the new session instead of asking for `--allow-fresh` again every cycle.

## 0.3.14

- **Copilot leader trust prompt is now actually auto-answered** in `team-agent copilot` interactive sessions: the trust-prompt handler that already existed in the worker tick is now also wired into the leader passthrough path on the current `TMUX_PANE`, so a fresh `team-agent copilot` boot no longer parks on `Confirm folder trust / Do you trust the files in this folder?` — it advances to ready on its own (0.3.13 changelog claim is now actually delivered).
- **`team-agent shutdown` JSON / event result is honest about what got killed**: bare shutdown now performs the shared-socket tail cleanup BEFORE computing the report, and the report includes `killed_sessions` / `spared_sessions` for the shared socket. Previously the JSON/event was computed before the cleanup step ran, so `ok: false, status: partial` could be reported even after the socket had been fully cleaned (0.3.13 changelog claim now actually delivered for the bare path).
- **Last remaining owner-attribution corner closed**: the seventh seed point (`lifecycle/launch.rs:3704-3758 seed_launched_owner_from_env`) previously defaulted to `codex` when `TMUX_PANE` was set but no explicit `TEAM_AGENT_LEADER_PROVIDER` env was passed, masking the 0.3.13 E22 fallback. Codex default removed; the seed now uses only the explicit env or attributes the caller pane via the existing `leader::attribute_pane_provider()` (and leaves owner/receiver unset rather than guessing). A `team-agent copilot` leader started from a tmux pane now writes `team_owner.provider = copilot` end-to-end.
- **`team-agent restart` no longer brings up a zombie session under a refused-resume worker**: the restart entry path now treats refused-resume as a single-worker failure and isolates it from the rest of the team (G1 regression for the resume-atomicity contract); honest exit codes and per-worker failure events keep working alongside it.

## 0.3.13

- **Copilot is now end-to-end usable** as both a leader and a worker provider:
  - The Copilot startup `Confirm folder trust / Do you trust the files in this folder?` prompt is recognised and auto-confirmed by the framework, so a fresh Copilot leader/worker boots all the way to ready instead of staying parked on the trust screen.
  - The leader's owner / receiver attribution is correct under a Copilot leader — previously `leader.provider=copilot` but `leader_receiver.provider=codex` / `team_owner.provider=codex` because the quick-start seed path defaulted to `codex` when the launcher couldn't observe the leader yet. The unified attribution path (E22) now covers this sixth call site so the seeded receiver/owner provider matches the real leader provider.
  - `team-agent shutdown` on a Copilot-led team no longer misidentifies the leader pane — the rediscovery path that already understood codex/claude leaders now also understands `copilot`, so bare shutdown reliably spares the leader instead of killing it.
- **`team-agent restart` no longer takes the whole team down when one worker can't resume**: the spawn loop now isolates per-worker failures — a worker whose resume fails is marked failed and the other workers continue. `restart` exit code now reports `partial` when some workers came up and some didn't, instead of a single all-or-nothing `ok`/`fail`. A single failed `--resume` no longer kills the shared tmux session and starves the next spawn.
- **Nested team state is preserved on merge**: when a child team launches inside a workspace that already hosts a team, the parent team's `teams` map is preserved through the launched-state merge instead of being overwritten. A user-supplied `owner_team_id` is now ignored with a warning rather than silently honoured, so it cannot point a child team at the wrong owner.
- **Honest exit codes in two more places**: a `team-agent shutdown` against a workspace whose coordinator was already absent now reports `ok` cleanly instead of falsely reporting `partial`; `remove-agent` and friends now return `ok: false` envelopes on Refused outcomes instead of returning `ok: true` for a no-op.

## 0.3.12

- **Coordinator now detaches from the launching terminal**: the coordinator daemon process is spawned with `setsid` so it becomes its own session/process-group leader instead of inheriting the launcher's. Closing the terminal that ran `team-agent quick-start` (or disconnecting an SSH session) no longer sends `SIGHUP` to the coordinator and no longer kills it as a side effect — common cause of "coordinator died after I closed the window" on WSL/SSH workflows.

## 0.3.11

- **Message delivery now surfaces a degraded status when the coordinator is not alive**: `team-agent send` (and the internal MCP send path) previously returned `accepted` even when the coordinator was dead, leaving messages silently stuck. The send path now returns an explicit degraded status instead of falsely reporting acceptance, so the leader can tell immediately that the coordinator needs attention rather than waiting on a delivery that will never happen.
- **Coordinator crashes now leave a durable post-mortem marker**: a panic inside the coordinator tick is now caught and written to `coordinator.tick_panic` with the captured backtrace, so `team-agent diagnose` and on-disk inspection can see where the coordinator died instead of finding only an absent process. Combined with the 0.3.10 "no silent self-exit" change, every coordinator failure mode now produces an evidence trail.

## 0.3.10

- **`team-agent restart` no longer resurrects dead worker sessions (#264, E20)**: the resume gate was only running a transcript-existence check for `codex` workers — `claude`, `claude_code`, and `copilot` workers were promoted to `Resumed` purely on the presence of a stored `session_id`, even when the provider had no live transcript to resume against, producing a zombie window that looked alive but was attached to nothing. The transcript-existence check now runs for every provider that has a known transcript root, so a stale session id falls through to refused-or-fresh-by-policy instead of being silently honored.
- **Restart readiness gate (E20 C②)**: after relaunch, `restart` now waits for each worker to reach a ready state before reporting the team restart as complete; partial relaunches no longer silently look successful while leaving workers stuck mid-startup.
- **Coordinator no longer silently self-exits** on transient errors and **messages no longer silently disappear** in delivery race conditions — both paths now surface failures through the event log instead of swallowing them.
- **`--dangerously-bypass-approvals-and-sandbox` / `--dangerously-skip-permissions` inheritance is correct for internal MCP tools (#232)**: when the leader is started with a bypass flag, internal Team Agent MCP tools (orchestrator send/result/etc.) inherit the bypass correctly so they no longer trigger approval prompts that the user already opted out of.
- **`team-agent shutdown` under a Copilot-launched leader no longer kills the leader window**: the leader-pane rediscovery path now recognises `copilot` as a valid leader command (it previously only looked for `codex`/`claude`), so the shutdown-protection extension correctly identifies and spares the Copilot leader.

## 0.3.9

- **`team-agent restart` standard usage actually works (RED-2-STILL, P0 hotfix)**: the restart entry gate now resolves the team workspace through `canonical_run_workspace` before checking for a spec, so the documented form `team-agent restart .` (or `team-agent restart <workspace>`) finds the spec under `.team/runtime/<team_key>/` instead of falsely reporting "no spec" because it was looking at the raw workspace argument. The second-layer guard from 0.3.8 (067f78f) is preserved and still catches deeper misses.
- **`team-agent copilot` is now a first-class entry command**: `team-agent copilot [args...]` launches or attaches a tmux-managed Copilot leader the same way `team-agent codex` / `team-agent claude` do; the provider mapping, help text, and candidate-list now surface Copilot. Copilot leader processes are also started with `COPILOT_DISABLE_TERMINAL_TITLE=1` so the leader pane's tmux/terminal title stays stable instead of being repeatedly overwritten by Copilot.
- **Task results no longer double-deliver to the leader**: `report_result` previously took both a direct-inject path AND the normal delivery path, so a leader could see the same completion reported twice. The direct-inject path is now gated to deliver-failure only (`if !outcome.ok`), so a successful result is delivered exactly once via the normal path.

## 0.3.8

- **Shutdown safety (E12, P0)**: a bare `team-agent shutdown` now spares the leader pane by reading the state.json leader anchor and gating shared-socket `kill_server` — closing a three-time recurrence where shutdown would kill the leader window itself. The runtime now refuses to issue a server-wide kill against a socket carrying a registered leader anchor for any team.
- **Copilot end-to-end is now actually usable** (three pieces together):
  - `npx @team-agent/installer install` now installs the `team-agent` skill into the Copilot skill directory by delegating to a single canonical Rust path (`team-agent install-skill`); JS-side install code is gone — one source of truth, no double-install drift.
  - Copilot worker session attribution is correct under restart: when a worker has an `expected_session_id`, the runtime point-queries the `~/.copilot/session-store.db` sqlite by id (sessions.id == expected) instead of grabbing the latest-by-cwd row, so a worker no longer accidentally inherits the leader's Copilot session when both share the same cwd. When `expected_session_id` is unset, the runtime refuses to fall back to "latest at cwd" — it returns empty rather than promote a session that could belong to anyone.
  - Copilot leader / worker provider binding no longer crosses streams under leader + worker started in the same workspace.
- **Restart spec gate (RED-2, P0)**: `team-agent restart` now reads the runtime spec from `selected.spec_path` (the read-order B canonical path under `.team/runtime/<team_key>/`) instead of the user-visible workspace directory — so restart correctly finds the spec even when the user-visible directory was never populated, and behaves the same as quick-start about where the team spec lives.
- **`team-agent attach-leader`** is wired and surfaces the canonical attach command in human-readable output; `quick-start` ready output now includes the `attach_commands` table so attaching the leader pane does not require digging into runtime state.
- **Leader pane env preflight (RED-4 batch)**: a stale or invalid `TMUX_PANE` env coming into quick-start/restart is now caught at entry — empty pane probe output, absent env, invalid pane references are handled in turn; transient unknowns are degraded rather than promoted to fatal; warnings are persisted into team logs for after-the-fact diagnosis.
- Multi-socket transport probe: the tmux backend now scans all standard socket roots (`/tmp/tmux-<uid>`, `$TMPDIR/tmux-<uid>`) instead of a single hard-coded `/tmp` root, so workspaces using a non-default `TMPDIR` can be located.

## 0.3.7

- Runtime team spec is now single-sourced under `.team/runtime/<team_key>/` instead of being copied into user-visible workspace paths; `team-agent restart` rebuilds the runtime spec from your role docs each time (so edits to `agents/<role>.md` take effect on restart without manual regen), and `team-agent add-agent` no longer copies the role file into the platform directory — fixes a self-truncation bug where add-agent would zero out the source role file.
- Claude worker session attribution is now reliable for interactive workers (primary path verifies against the provider's project transcript; a fallback path covers the case where the transcript hasn't landed yet); `team-agent restart` with a worker that has no stored session id now refuses by default rather than silently spawning a blank worker — pass `--allow-fresh` only when you explicitly accept a fresh context.
- `npx @team-agent/installer install` now also installs the `team-agent` skill for Copilot, so a Copilot leader can actually use Team Agent skills after install (was missing before); CLI error-path guidance is unified across commands to the three-line `ERROR / ACTION / LOG` form.
- Coordinator tick degraded-monitor pattern (N36 face decouple): runtime-prompts / sync-agent-health / runtime captures each catch their own errors, emit a `*.failed` event, and continue — so a single capture failure no longer kills the whole tick or blocks delivery. `provider.classify.unsupported` emits at most once per check_key state-change (P4 dedup) instead of looping.
- Status `runtime` block: `team-agent status --json` now carries `runtime: { coordinator: { ok }, undelivered, hint? }`. `hint` only appears when the coordinator is down AND there is an undelivered backlog (anti-nag).
- Lifecycle quick-start validates the active leader pane env at entry; a stale `TMUX_PANE` pointing at a Dead pane now fails closed with the N38 three-line `ERROR / ACTION / LOG` form instead of silently retargeting.
- Leader terminal protection (B5) cross-socket coverage: protected-pane extension now reads three sources (session-prefix scan + `state.json` leader anchor + cross-socket query via the user's tmux endpoint), so leader panes on the user's default tmux socket survive workspace-socket shutdown.
- COPILOT v2 phase-1: argv + windowtitle on launch (`Q4 P0`); MCP residual scan + sqlite session capture + BYOK env wiring; `caps.fork=false` is an honest `CapabilityUnsupported` refusal (no silent fresh-spawn fallback); per-worker model override via role `model:`.
- Doctor legacy compatibility: a legacy `team_invalid` spec no longer hard-errors `doctor`; it reports a degraded status row and lets diagnosis continue.
- Add-agent / remove-agent / start-agent semantic distinction documented in SKILL.md (runtime add = `add-agent` one command, do not shutdown/restart to add a worker); subscription-tier `team-agent profile init` prelude added; private tmux socket + `attach_commands` documented; provider capability matrix table covers claude/codex/copilot/gemini_cli/fake.
- B-2 ownership: 2 weak-locked black-box contracts left intentionally pending in `tests/`, tracked for the next fixup batch.

## 0.3.6

- Hotfix: Linux x64 binary was built against the runner's glibc 2.39, leaving it unusable on every distro shipping older glibc (Ubuntu 22.04 / 20.04, Debian 12, WSL defaults, etc.). The release pipeline now targets `x86_64-unknown-linux-musl` with full static linking — no GLIBC_ version symbols and runs on any modern Linux. Inherits the same fix for Linux users on 0.3.0 → 0.3.5 (all earlier `cli-linux-x64` packages were affected; upgrade is required to run on Linux).
- Release pipeline: pre-publish assertion that the Linux artifact is statically linked AND carries zero `GLIBC_` version symbols (reverting the target back to `linux-gnu` will trip the gate before any package reaches npm).
- Installer: `npx @team-agent/installer install` now runs a `team-agent --version` post-install smoke and exits non-zero with a three-line `ERROR/ACTION/LOG` diagnostic when the freshly installed binary fails to load — catches loader-level failures (glibc mismatch, cpu mismatch, corrupted download) at install time instead of on first use.
- CI: workflow checkout actions bumped to `v5` (no behavior change).
- Re-includes ef7ab3d's permission-tier downgrade real-machine coverage.

## 0.3.5

- Faithful-port fixes vs Python 0.2.11 (#264 D1-D9): codex `developer_instructions` triple escaping, MCP `tool_timeout_sec=600`, profile `--profile`/`codex_config` injection, worker `TEAM_AGENT_ID` env, fresh-launch cwd=workspace, identity-first system prompt, `runtime.fast` codex toggle, `team-{name}` session fallback, real `unset` of profile keys in the worker shell.
- State safety (A0): the per-team roster preserve now survives an active-team-key flip between a writer's load and save; session-capture fields are monotonic across stale-snapshot saves.
- Leader terminal protection (B5): a bare `shutdown` spares `team-agent-leader-*` sessions and the tmux server carrying them; the leader pane process tree joins the shutdown protection set.
- Fixed-failure batch (A-1..A-8): `collect` reports the real coordinator status; the takeover reminder honors the arm gate; `watch`/`status` panels read live store data (team filter, latest results, agent detail); a missing leader receiver no longer reads as attached; watcher retries go through the real delivery path; MCP `stuck_cancel`/`send`/`fork` contract fixes; coordinator start refuses incompatible `team.db` schemas.
- Tick & shutdown performance (P1-P7, PERF-6): bounded transcript tail reads (128KB), head-bounded session-capture reads + candidate cap 300, steady-state ticks no longer rewrite `state.json` (tick counter moved to its own metadata file), change-driven compaction events, one pane snapshot per tick, one process-table snapshot + batched TERM/grace/KILL in shutdown (ps forks 10-15 -> 4), orphaned coordinator self-termination, `tick_error` signature dedup.
- Observability (swallow batches 1-4): probe/query/exit/config failures now emit events with non-null errors instead of silent empty results; corrupt provider-config JSON fails explicitly and never rewrites the user's file; MCP scope validation fails closed when the runtime state is unreadable.
- New provider: GitHub Copilot CLI (subscription-tier A-layer: BYOK env, auth hints, per-worker instructions overlay, sqlite session capture).
- Internal: constitution anchors N36-N39 and MUST-17 codified.

## 0.3.4

- Changed the default team display backend to `none`; set `display_backend: adaptive` in `TEAM.md` to opt in to adaptive display windows.
