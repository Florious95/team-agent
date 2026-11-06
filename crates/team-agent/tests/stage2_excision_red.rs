//! Stage 2 dead-code excision gates. P3 = K2 (coordinator tick engine remnants:
//! steps/abnormal.rs, runtime detectors/observation, orphan, provider classify and
//! faults). P4 = K3 (attach-leader / takeover / rediscovery, plus the Stage 1
//! `PaneField::PaneTty` deferral), K4 (fork / session-verify shims and the dead
//! `ProviderAdapter` surface), K5 (plan orchestrator), K7 (CLI residue) and K8
//! (legacy packaging).
//!
//! `excise_*` cases are RED on the Stage 1 final baseline (0cd6e76e) and turn GREEN
//! only once the inventoried code is gone. `guard_*` cases are GREEN on the baseline
//! and must stay GREEN: they stop over-deletion of protected mixed / B-review /
//! current cases and of live kernels that merely sit in coordinator-, leader- or
//! provider-named files.
//!
//! Inventory authority: the P3/P4 rows of the dead-code audit, rebased onto 0cd6e76e
//! (`fixtures/stage2_excision_manifest.tsv`). An inventory row is not a deletion
//! licence by itself; chief rulings are recorded in the tables below.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = include_str!("fixtures/stage2_excision_manifest.tsv");

/// Whole files whose manifest rows are retained by chief ruling: `(file, reason)`.
const RETAINED_FILES: &[(&str, &str)] = &[(
    "src/provider/approvals/runtime_prompts.rs",
    "Stage 1 ruling #14 (SPLIT+FLAG): notification_awaiting_human_confirm_red keeps the parser/decision/\
     fingerprint/event assertions; the AwaitingHumanConfirmFact emitter is an independent casefile (handoff §2.5)",
)];

/// Manifest rows deliberately retained after a caller check: `(file, declaration, reason)`.
/// A retained row is skipped, not required: the developer may still remove it after
/// repointing its current callers.
const RETAINED_AFTER_REVIEW: &[(&str, &str, &str)] = &[
    (
        "src/provider/types.rs",
        "pub struct ApprovalFingerprint(pub String);",
        "fingerprint identity of the retained awaiting-human-confirm kernel (Stage 1 ruling #14)",
    ),
    (
        "src/leader/lease.rs",
        "pub(crate) fn claim_lease_no_incident(",
        "called by the handoff-protected claim_lease_no_incident_vacant_acquire_advances_epoch_and_writes_canonical_only; keep the wrapper or repoint the test to claim_lease_no_incident_with_target",
    ),
    (
        "src/leader/start.rs",
        "pub(crate) fn leader_start_plan_after_ambient_authority_check(",
        "exercised by five current tests outside the P3/P4 inventory (leader/tests/identity.rs, lifecycle/tests/cli_argv_routing_red.rs); repoint them before removing",
    ),
    (
        "src/provider/session/capture.rs",
        "fn pending_session_capture<F>(",
        "exercised by the current canonical_pending_capture_preserves_legacy_missing_boundary outside the inventory",
    ),
    (
        "src/provider/session/resume.rs",
        "SessionCaptureIncomplete,",
        "constructed by the retained restart readiness wrapper (lifecycle/restart/rebuild.rs); folding it into Other would change refusal_reason.wire() to session_unresumable. SessionDrift and from_legacy (no production caller) are still retired",
    ),
];

/// Protected cases whose retirement a reviewer has explicitly approved:
/// `(file, test, reason)`. Silent deletion is not accepted.
const ADJUDICATED_RETIREMENTS: &[(&str, &str, &str)] = &[];

/// Files that hold nothing but inventoried P3/P4 items (and their own tests).
const DEAD_FILES: &[&str] = &[
    // K2
    "src/coordinator/steps/abnormal.rs",
    "src/coordinator/steps/mod.rs",
    "src/coordinator/runtime_detectors.rs",
    "src/coordinator/runtime_observation.rs",
    "src/coordinator/orphan.rs",
    "src/provider/classify.rs",
    "src/provider/faults.rs",
    // K3
    "src/leader/rediscover.rs",
    "src/leader/rediscover/tests.rs",
    "src/leader/takeover.rs",
    "src/leader/incident.rs",
    "src/layout/recovery.rs",
    // K4
    "src/provider/adapters/claude_fork.rs",
    "src/provider/submit_now.rs",
    "src/provider/session/context_fork/claude.rs",
    // K5
    "src/lifecycle/launch/plan.rs",
    "src/lifecycle/restart/orchestrator.rs",
    // K8
    "src/packaging/migrate.rs",
    "src/packaging/repair.rs",
];

/// K2: tick engine, runtime detectors, abnormal-exit writer, classify/fault facts.
const K2_VOCABULARY: &[&str] = &[
    "TickStepGroup",
    "TickError",
    "TickStopReason",
    "DEFAULT_TICK_INTERVAL_SEC",
    "BACKOFF_MAX_SEC",
    "MarkerStore",
    "WholeTeamGoneReport",
    "WholeTeamGoneClass",
    "TeamPresenceSnapshot",
    "detect_whole_team_gone",
    "should_orphan_self_terminate",
    "observe_runtime",
    "detect_compaction",
    "detect_session_drift",
    "detect_leader_api_error",
    "CapturedRuntimeFact",
    "LeaderCaptureFact",
    "RuntimeObservationResults",
    "CompactionResult",
    "SessionDriftResult",
    "LeaderApiError",
    "AbnormalExitGate",
    "AbnormalExitDecision",
    "refresh_abnormal_watch_liveness",
    "abnormal_watch_agents",
    "read_fault_facts",
    "claude_fault_fact",
    "codex_fault_fact",
    "ClassifyResult",
    "LifecycleFact",
    "extract_lifecycle_facts",
    "CoordinatorStart",
    "coordinator_start_status_wire",
    "coordinator_service_available",
    "coordinator_issue_id",
    "DeadlockAlert",
    "IdleAlert",
    "TurnStateClassifier",
    "WakeWatchState",
    "RereadDecision",
];

/// K3: attach-leader / takeover / rediscovery and the PaneTty field it alone needed.
const K3_VOCABULARY: &[&str] = &[
    "attach_leader",
    "cmd_attach_leader",
    "AttachLeaderArgs",
    "attach_leader_args",
    "TakeoverArgs",
    "cmd_takeover",
    "AttachLeaderTarget",
    "attach_leader_targets",
    "attach_leader_to_state",
    "validate_attach_target",
    "live_bound_pane_blocking_other_attach",
    "TargetScanLiveness",
    "try_readopt_leader_pane",
    "rediscover_leader_receiver_with_identity",
    "rediscover_leader_receiver_with_owner_identity",
    "rediscover_leader_receiver_from_targets_with_identity",
    "LeaderIncident",
    "recover_leader_pane",
    "evaluate_takeover_reminder",
    "TakeoverReminderResult",
    "is_idle_for_takeover",
    "probe_ambient_leader_pane_info",
    "populate_team_owner_from_env",
    "apply_first_time_leader_binding",
    "worker_sender_bypasses_owner_gate",
    "leader_start_plan",
    "prepare_leader_start",
    "quiet_fake_leader_pane_echo",
    "PaneTty",
];

/// K4: fork / session-verify materialization shims and dead provider fact types.
const K4_VOCABULARY: &[&str] = &[
    "materialize_claude_fork",
    "ClaudeForkMaterialization",
    "materialize_copilot_fork",
    "CopilotForkMaterialization",
    "verify_context_fork",
    "verify_claude_fork",
    "verify_copilot_fork",
    "observe_context_fork",
    "ContextForkProof",
    "ContextForkOutcome",
    "ContextForkTermination",
    "ContextBackingSnapshot",
    "context_fork_convergence_deadline",
    "flush_explicit_queue",
    "FlushReport",
    "GROK_SEND_NOW_MARK",
    "GROK_SEND_NOW_MAX_ENTERS",
    "forward_one",
    "drain_scoped_pending_egress",
    "require_legacy_role",
    "declares_contract",
    "copilot_auth_hint",
    "claude_auth_hint",
    "choose_internal_mcp_approval_choice",
    "approval_choice_keys",
    "require_active_state_providers",
    "FaultFact",
    "NoPingReason",
    "AgentRuntimeStatus",
    "ClassifySource",
    "RemindResult",
    "StatusPatterns",
    "submit_to_bound_thread",
    "AppServerSubmit",
];

/// K5: the plan orchestrator and its persisted plan state.
const K5_VOCABULARY: &[&str] = &[
    "PlanState",
    "PlanStage",
    "PlanCondition",
    "PlanProgress",
    "PlanStatus",
    "PlanId",
    "start_plan",
    "halt_plan",
    "plan_status",
    "advance_plan_state",
    "save_plan_state",
    "read_plan_state",
    "plan_state_path",
    "plan_lock_path",
    "plan_state_dir",
    "plan_state_json",
    "InvalidPlan",
    "InvalidPlanId",
    "DangerousApprovalRequired",
    "RollbackFailed",
    "save_team_runtime_snapshot",
];

/// K7: CLI residue with no dispatch route.
const K7_VOCABULARY: &[&str] = &[
    "SEND_REMINDER",
    "KillDecision",
    "sessions_to_kill",
    "SummaryBucket",
    "SummaryCounts",
    "InteractionCounts",
    "NamedWorkspaceTransport",
    "resolve_name_with_workspace_transports",
    "consume_leader_inbox_summary",
    "render_inbox_summary",
    "parse_inbox_entries",
    "attach_lease_value",
    "attach_requeued_exhausted_watchers",
    "project_requeued_exhausted_watchers",
    "PiDoctorFacts",
    "PiDoctorInput",
    "pi_doctor_facts",
    "PiBackingStatus",
    "PiMcpStatus",
    "is_known_subcommand",
    "parse_cli_provider",
    "DevInternal",
];

/// K8: legacy binary installer / migrate / repair packaging.
const K8_VOCABULARY: &[&str] = &[
    "InstallReport",
    "InstallOptions",
    "platform_support",
    "PlatformSupport",
    "ReleaseTarget",
    "DoctorOptions",
    "AtomicReplaceOutcome",
    "MigrationOutcome",
    "repair_schema",
    "diagnose_path",
    "PathDiagnostic",
    "PathHint",
    "UninstallOutcome",
    "UninstallOptions",
    "default_skill_source",
    "doctor_gate_blockers",
    "BlockerSource",
];

/// `ProviderAdapter` methods no production path calls. Native providers go through
/// `build_command_plan` / `build_resume_command_plan` / `fork_plan` (or the contract
/// leaf / Pi lifecycle materializer) instead.
const DEAD_ADAPTER_METHODS: &[&str] = &[
    "version",
    "auth_hint",
    "build_command",
    "recover_session_id",
    "build_resume_command",
    "fork",
    "install_mcp",
    "status_patterns",
    "validate_model",
];

/// `ProviderAdapter` methods that carry the live native command surface.
const LIVE_ADAPTER_METHODS: &[&str] = &[
    "provider",
    "caps",
    "is_installed",
    "build_command_with_permissions",
    "build_command_plan",
    "capture_session_id",
    "capture_session_candidates",
    "session_is_resumable",
    "build_resume_command_with_context",
    "build_resume_command_plan",
    "fork_with_context",
    "fork_plan",
    "mcp_config",
    "requires_worker_mcp_handshake",
    "handle_startup_prompts",
    "handle_startup_prompts_outcome",
];

/// Live kernels that share a file or a name prefix with retired code: `(file, item)`.
/// Each must still be declared (as fn/struct/enum/trait/const) in that file.
const LIVE_NEIGHBOURS: &[(&str, &str)] = &[
    // Legacy-daemon upgrade retirement and read-only legacy observation (not the tick).
    ("src/coordinator/health.rs", "retire_legacy_coordinator"),
    ("src/coordinator/health.rs", "retire_legacy_coordinator_for_status"),
    ("src/coordinator/health.rs", "legacy_coordinator_value"),
    ("src/coordinator/health.rs", "stop_coordinator"),
    ("src/coordinator/observation.rs", "read_coordinator_heartbeat"),
    ("src/coordinator/observation.rs", "probe_host_boot_id"),
    ("src/cli/status_port/runtime.rs", "compute_runtime_freshness"),
    ("src/cli/status_port/runtime.rs", "coordinator_health_value"),
    // abnormal_exit_watch read side (the writer retired in Stage 1).
    ("src/state/abnormal_watch.rs", "needs_compaction"),
    ("src/state/abnormal_watch.rs", "compact_after_merge"),
    ("src/state/abnormal_watch.rs", "observation_identity"),
    ("src/state/abnormal_watch.rs", "matches_observation"),
    // Native input recognition.
    ("src/transport/native_input.rs", "NativeInputBinding"),
    ("src/transport/native_input.rs", "InputFrame"),
    ("src/transport/native_input.rs", "inspect_input"),
    // Approval parser kept by Stage 1 ruling #14.
    ("src/provider/approvals/parsing.rs", "extract_approval_prompt"),
    ("src/provider/approvals/parsing.rs", "active_approval_control_index"),
    // Current leader binding (claim-leader) and app-server attach.
    ("src/leader/lease.rs", "claim_leader"),
    ("src/leader/lease.rs", "claim_lease_no_incident_with_target"),
    ("src/leader/lease.rs", "write_lease_dual_state"),
    ("src/leader/lease.rs", "attach_app_server_leader"),
    // Live remainder of partially retired files.
    ("src/provider/adapters/copilot_fork.rs", "copilot_home"),
    ("src/provider/session/context_fork/outcome.rs", "ContextForkPendingFailure"),
    ("src/provider/session/context_fork/outcome.rs", "transition_pending_context_fork"),
    ("src/contract_runtime/forward.rs", "FrameworkContext"),
    ("src/packaging/install.rs", "install_skill"),
    ("src/provider/adapter.rs", "get_adapter"),
];

/// Integration-crate test seams on `TmuxBackend` (Stage 1 ruling): they stay `pub`.
const PUBLIC_TMUX_SEAMS: &[&str] = &[
    "pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {",
    "pub fn with_runner_for_workspace(runner: Box<dyn CommandRunner>, workspace: &Path) -> Self {",
    "pub fn kill_server(&self) {",
];

/// Cases Stage 2 must keep (trim/repoint only): the 15 P3/P4 protected mixed cases,
/// the 21 current P3/P4 B-review cases, the 4 P3 semantic oracles (rulings: three
/// REPOINT, one KEEP), handoff-protected names, the Stage 1 #14 awaiting-human-confirm
/// pair, and current cases outside the inventory that reach retired items.
#[rustfmt::skip]
const SURVIVORS: &[(&str, &str, &str)] = &[
    ("src/transport_factory.rs", "legacy_tmux_endpoint_resolves_to_tmux_endpoint_backend", "mixed P3"),
    ("src/transport_factory.rs", "explicit_conpty_without_team_key_refuses_no_silent_fallback", "mixed P3"),
    ("tests/read_paths_batch3.rs", "batch3_read_only_factory_for_tmux_state_returns_tmux_backend_byte_equivalent", "mixed P3"),
    ("src/leader/tests/basics.rs", "leader_event_names_match_python_byte_for_byte", "mixed P4"),
    ("src/contract_runtime/registry.rs", "claude_native_argv_prompt_keeps_first_business_tool_bootstrap", "mixed P4"),
    ("tests/contract_config.rs", "mixed_contract_providers_route_every_cursor_name_without_migrating_state", "mixed P4"),
    ("tests/contract_config.rs", "cursor_mutation_malformed_declarations_and_api_profiles_refuse_without_writes", "mixed P4"),
    ("src/contract_runtime/registry.rs", "every_public_codex_name_selects_the_leaf_and_a_legacy_row_is_never_reclassified", "mixed P4"),
    ("src/contract_runtime/registry.rs", "bare_pi_keeps_its_legacy_meaning_and_canonical_key_selects_the_leaf", "mixed P4"),
    ("src/contract_runtime/registry.rs", "grok_server_key_is_generation_bound_and_has_no_inspector_fallback", "mixed P4"),
    ("src/contract_runtime/registry.rs", "cursor_has_no_kiro_control_or_tool_inspector_fallback", "mixed P4"),
    ("src/lifecycle/tests/core.rs", "add_agent_on_unowned_workspace_does_not_silently_add", "mixed P4"),
    ("src/cli/adapters.rs", "reminders_only_recommend_supported_result_commands", "mixed P4"),
    ("src/cli/emit.rs", "all_dispatch_kinds_have_exactly_one_spec", "mixed P4"),
    ("src/cli/emit.rs", "public_help_templates_and_cli_guidance_are_english", "mixed P4"),
    ("src/state/abnormal_watch/tests/mod.rs", "unknown_team_retains_history_until_observation_but_never_authorizes_reuse", "B-review P3"),
    ("src/transport_factory.rs", "explicit_conpty_with_team_key_resolves_to_conpty", "B-review P3"),
    ("src/transport_factory.rs", "cli_override_disagreeing_with_state_kind_refuses_unless_fresh", "B-review P3"),
    ("src/transport_factory.rs", "state_conpty_with_legacy_tmux_endpoint_emits_ignored_notice", "B-review P3"),
    ("src/transport_factory.rs", "state_conpty_without_legacy_emits_no_notice", "B-review P3"),
    ("src/transport_factory.rs", "conpty_without_pipe_ready_hint_emits_no_pipe_client_notice", "B-review P3"),
    ("src/transport_factory.rs", "conpty_resolve_emits_no_pipe_client_notice_after_c5_fix", "B-review P3"),
    ("tests/c5_factory_no_probe_backend_lazy_red.rs", "resolve_transport_windows_conpty_completes_fast_when_no_shim", "B-review P3"),
    ("tests/lifecycle_transport_resolver_batch1.rs", "batch1_state_transport_kind_conpty_constructs_conpty_via_factory", "B-review P3"),
    ("tests/runtime_observation_detectors_contract.rs", "observe_marks_codex_session_drift_without_vetoing_physical_send", "B-review P3"),
    ("src/leader/start.rs", "external_exec_provider_publishes_complete_binding_before_provider_exec", "B-review P4"),
    ("src/leader/start.rs", "default_exec_provider_persists_current_pane_binding_before_provider_exec", "B-review P4"),
    ("tests/claim_rework_binding_contract.rs", "isolated_production_seed_attach_register_restart_ok", "B-review P4"),
    ("tests/reclaim_neutral_red.rs", "explicit_takeover_live_owner_unconditional_replaces_live_owner_without_split_brain", "B-review P4"),
    ("src/lifecycle/tests/launch_spawn.rs", "quick_start_preserves_managed_leader_topology_without_inventing_a_leader_window", "B-review P4"),
    ("src/lifecycle/tests/cli_argv_routing_red.rs", "a13_all_provider_version_and_auth_auxiliary_argv_are_native", "B-review P4"),
    ("src/lifecycle/tests/pi_fork_session_inheritance_red.rs", "r12_pi_clone_and_non_pi_in_window_fork_compatibility_are_preserved", "B-review P4"),
    ("src/provider/tests/adapter.rs", "retired_worker_caps_are_neither_resumable_nor_forkable", "B-review P4"),
    ("tests/contract_config.rs", "every_codex_and_grok_name_selects_the_contract_leaf_without_opt_in", "B-review P4"),
    ("src/leader/tests/lease_api.rs", "lease_divergence_reads_restart_snapshot_for_special_session_names", "B-review P4"),
    ("src/lifecycle/tests/phase_golden.rs", "phase_golden_missing_registry_requires_index_publication_not_claim", "B-review P4"),
    ("tests/sweep_new_reds_254_real_red.rs", "rc6_shutdown_real_cli_fake_team_json_no_residuals_and_runner_survives", "P3 semantic: REPOINT (drop live-coordinator fixture)"),
    ("tests/sweep_new_reds_254_real_red.rs", "rc3_claim_child_leader_persists_under_live_coordinator_tick", "P3 semantic: REPOINT (drop live-coordinator fixture)"),
    ("tests/sweep_new_reds_254_real_red.rs", "tit_claim_child_leader_survives_live_tick_projection_race", "P3 semantic: REPOINT (drop live-coordinator fixture)"),
    ("tests/e2e/cases/shut_004_cleans_coordinator.rs", "shut_004_shutdown_cleans_coordinator_sidecar", "P3 semantic: KEEP"),
    ("src/leader/tests/lease_api.rs", "write_lease_dual_state_persists_canonical_root_only_no_legacy_side_effect", "handoff-protected"),
    ("src/leader/tests/lease_claim.rs", "claim_lease_no_incident_vacant_acquire_advances_epoch_and_writes_canonical_only", "handoff-protected"),
    ("src/leader/tests/terminal_and_replay_vs_rerender_invariants_red.rs", "restart_never_marks_delivered_as_accepted", "handoff-protected"),
    ("src/lifecycle/tests/pi_lifecycle_doctor_red.rs", "pi_stop_retains_backing_and_never_scans_by_process_name", "handoff-protected"),
    ("src/cli/mod.rs", "audit_write_failure_skips_destructive_kill", "handoff-protected"),
    ("src/event_log.rs", "rotation_at_threshold_shifts_archives_and_drops_oldest", "handoff-protected"),
    ("src/event_log.rs", "concurrent_appends_do_not_interleave", "handoff-protected"),
    ("src/contract_runtime/mcp.rs", "diagnostic_sink_failures_do_not_change_execution_results", "handoff-protected"),
    ("tests/notification_awaiting_human_confirm_red.rs", "awaiting_human_confirm_uses_active_tail_structural_prompt_and_dedupes_per_fingerprint", "Stage 1 ruling #14"),
    ("tests/notification_awaiting_human_confirm_red.rs", "awaiting_human_confirm_path_never_presses_keys_or_auto_elevates_restricted_workers", "Stage 1 ruling #14"),
    ("src/transport/tests/behavior.rs", "pane_field_variants_distinct", "current: trim PaneTty"),
    ("src/lifecycle/restart/rebuild.rs", "lifecycle_phase_wire_labels_remain_stable", "current: trim coordinator_start label"),
    ("src/provider/session/capture.rs", "canonical_pending_capture_preserves_legacy_missing_boundary", "current outside inventory"),
    ("src/leader/tests/identity.rs", "in_tmux_default_leader_uses_managed_client_and_external_opt_out_keeps_exec_provider", "current outside inventory"),
    ("src/leader/tests/identity.rs", "in_tmux_external_opt_out_keeps_exec_provider_across_server_boundary", "current outside inventory"),
    ("src/leader/tests/identity.rs", "leader_start_plan_external_leader_keeps_exec_provider_in_tmux", "current outside inventory"),
    ("src/lifecycle/tests/cli_argv_routing_red.rs", "a07_pi_explicit_invalid_arguments_remain_rejected_under_routing", "current outside inventory"),
];

/// Acceptance suites from earlier stages stay in place.
const SURVIVOR_FILES: &[&str] = &[
    "tests/stage1_excision_red.rs",
    "tests/stage1_preserve_invariants.rs",
    "tests/fixtures/stage1_excision_manifest.tsv",
    "tests/reclaim_neutral_red.rs",
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn crate_rust_files(subdirs: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for subdir in subdirs {
        rust_files(&crate_root().join(subdir), &mut files);
    }
    files.sort();
    files
}

fn relative(path: &Path) -> String {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Blank comments, string/char literal contents and raw strings, keeping
/// newlines so line numbers survive. Lifetimes and labels stay code.
fn mask_rust(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
        } else if (c == 'r' || (c == 'b' && next == Some('r')))
            && !chars
                .get(i.wrapping_sub(1))
                .is_some_and(|p| p.is_alphanumeric() || *p == '_')
            && {
                let start = if c == 'b' { i + 2 } else { i + 1 };
                let mut j = start;
                while chars.get(j) == Some(&'#') {
                    j += 1;
                }
                chars.get(j) == Some(&'"')
            }
        {
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            let mut hashes = 0;
            while chars[j] == '#' {
                hashes += 1;
                j += 1;
            }
            j += 1; // opening quote
            for _ in i..j {
                out.push(' ');
            }
            i = j;
            while i < chars.len() {
                if chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
                    for _ in 0..=hashes {
                        out.push(' ');
                    }
                    i += hashes + 1;
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }
        } else if c == '"' {
            out.push(' ');
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    out.push(' ');
                    if let Some(escaped) = chars.get(i + 1) {
                        out.push(blank(*escaped));
                    }
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    out.push(' ');
                    i += 1;
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }
        } else if c == '\'' {
            let close = if next == Some('\\') {
                (i + 3..chars.len().min(i + 13)).find(|&j| chars[j] == '\'')
            } else if chars.get(i + 2) == Some(&'\'') {
                Some(i + 2)
            } else {
                None
            };
            match close {
                Some(end) => {
                    for _ in i..=end {
                        out.push(' ');
                    }
                    i = end + 1;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn masked(path: &Path) -> String {
    mask_rust(&std::fs::read_to_string(path).unwrap())
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets of `word` occurrences delimited by non-identifier characters.
fn word_offsets(text: &str, word: &str) -> Vec<usize> {
    text.match_indices(word)
        .filter(|(at, _)| {
            !text[..*at].chars().next_back().is_some_and(is_ident)
                && !text[at + word.len()..].chars().next().is_some_and(is_ident)
        })
        .map(|(at, _)| at)
        .collect()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// Body span (`{` .. matching `}`) of the first brace after `from`.
fn brace_block(text: &str, from: usize) -> Option<(usize, usize)> {
    let open = from + text[from..].find('{')?;
    let mut depth = 0usize;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, open + offset));
                }
            }
            _ => {}
        }
    }
    None
}

fn declares_fn(body: &str, name: &str) -> bool {
    word_offsets(body, name).into_iter().any(|at| {
        body[..at].trim_end().ends_with("fn")
            && !body[..at]
                .trim_end()
                .trim_end_matches("fn")
                .ends_with(is_ident)
    })
}

/// `fn|struct|enum|trait|const|static|type NAME` anywhere in masked `text`.
fn declares_item(text: &str, name: &str) -> bool {
    word_offsets(text, name).into_iter().any(|at| {
        let before = text[..at].trim_end();
        ["fn", "struct", "enum", "trait", "const", "static", "type"]
            .iter()
            .any(|keyword| {
                before.ends_with(keyword)
                    && !before[..before.len() - keyword.len()].ends_with(is_ident)
            })
    })
}

/// `trait NAME` or `impl ... NAME for ...` headers (paths allowed before NAME).
fn names_trait(keyword: &str, header: &str, name: &str) -> bool {
    match keyword {
        "trait" => header
            .split_whitespace()
            .nth(1)
            .is_some_and(|declared| declared.trim_end_matches(':') == name),
        _ => word_offsets(header, "for").first().is_some_and(|for_at| {
            word_offsets(&header[..*for_at], name)
                .last()
                .is_some_and(|at| header[at + name.len()..*for_at].trim().is_empty())
        }),
    }
}

/// Every executable (masked) reference to one of `words` under src/ and tests/.
fn code_references(words: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for path in crate_rust_files(&["src", "tests"]) {
        let text = masked(&path);
        for word in words {
            for at in word_offsets(&text, word) {
                hits.push(format!("{}:{} {word}", relative(&path), line_of(&text, at)));
            }
        }
    }
    hits
}

fn assert_vocabulary_gone(cluster: &str, words: &[&str]) {
    let hits = code_references(words);
    let mut remaining: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| hits.iter().any(|hit| hit.ends_with(&format!(" {word}"))))
        .collect();
    remaining.dedup();
    assert!(
        hits.is_empty(),
        "{cluster}: retired items still referenced by code ({} sites; still alive: {remaining:?}); first 40:\n{}",
        hits.len(),
        hits.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

fn retained_row(file: &str, declaration: &str) -> bool {
    RETAINED_FILES.iter().any(|(f, _)| *f == file)
        || RETAINED_AFTER_REVIEW
            .iter()
            .any(|(f, d, _)| *f == file && *d == declaration)
}

fn run_cli(args: &[&str]) -> (std::process::Output, String, PathBuf) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let workspace = std::env::temp_dir().join(format!(
        "stage2-excise-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(args)
        .arg("--workspace")
        .arg(&workspace)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output, text, workspace)
}

/// Files under `<workspace>/.team`, except CLI error logs (an allowed refusal receipt).
fn state_files(workspace: &Path) -> Vec<PathBuf> {
    fn walk(at: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&workspace.join(".team"), &mut out);
    out.retain(|path| !path.starts_with(workspace.join(".team/logs")));
    out
}

// ───────────────────────────── excision (RED → GREEN) ─────────────────────────────

#[test]
fn excise_p3_p4_dead_modules_are_deleted() {
    let present: Vec<_> = DEAD_FILES
        .iter()
        .filter(|file| crate_root().join(file).exists())
        .collect();
    assert!(
        present.is_empty(),
        "P3/P4 files that hold only inventoried dead items are still present ({} of {}): {present:#?}",
        present.len(),
        DEAD_FILES.len()
    );
}

#[test]
fn excise_p3_p4_inventory_declarations_are_deleted() {
    for (file, reason) in RETAINED_FILES {
        assert!(!reason.trim().is_empty(), "retention of {file} needs a reason");
    }
    for (file, declaration, reason) in RETAINED_AFTER_REVIEW {
        assert!(
            !reason.trim().is_empty(),
            "retention of {file} `{declaration}` needs a reason"
        );
    }
    let mut remaining = Vec::new();
    let mut by_cluster = std::collections::BTreeMap::<String, (usize, usize)>::new();
    for row in MANIFEST
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
    {
        let fields: Vec<&str> = row.splitn(6, '\t').collect();
        assert_eq!(fields.len(), 6, "malformed manifest row: {row}");
        let [file, max_remaining, cluster, baseline_line, symbol, declaration] = [
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
        ];
        let file = file.trim_start_matches("crates/team-agent/");
        let max_remaining: usize = max_remaining.parse().unwrap();
        let tally = by_cluster.entry(cluster.to_string()).or_default();
        tally.0 += 1;
        if retained_row(file, declaration) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(crate_root().join(file)) else {
            continue;
        };
        let found = source
            .lines()
            .filter(|line| line.trim() == declaration)
            .count();
        if found > max_remaining {
            tally.1 += 1;
            remaining.push(format!(
                "{file}:{baseline_line} [{cluster}] {symbol} -> `{declaration}` (found {found}, allowed {max_remaining})"
            ));
        }
    }
    let summary: Vec<String> = by_cluster
        .iter()
        .map(|(cluster, (total, left))| format!("{cluster}: {left}/{total} remaining"))
        .collect();
    assert!(
        remaining.is_empty(),
        "{} inventoried Stage 2 declarations remain [{}]; first 40:\n{}",
        remaining.len(),
        summary.join(", "),
        remaining
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn excise_k2_tick_engine_and_detector_vocabulary_has_no_code_reference() {
    assert_vocabulary_gone("K2 coordinator tick engine", K2_VOCABULARY);
}

/// Stage 1 waived `PaneField::PaneTty` only until the K3 attach-leader callers go;
/// it is removed in the same change, with its fixture arms.
#[test]
fn excise_k3_attach_takeover_rediscovery_and_pane_tty_have_no_code_reference() {
    assert_vocabulary_gone("K3 attach-leader/takeover/rediscovery", K3_VOCABULARY);
    let transport = std::fs::read_to_string(crate_root().join("src/transport.rs")).unwrap();
    assert!(
        !transport.contains("#{pane_tty}"),
        "src/transport.rs still maps a PaneField to the tmux #{{pane_tty}} format"
    );
}

#[test]
fn excise_k4_fork_and_session_verify_shims_have_no_code_reference() {
    assert_vocabulary_gone("K4 fork/session-verify shims", K4_VOCABULARY);
}

#[test]
fn excise_k4_provider_adapter_trait_and_every_impl_drop_dead_methods() {
    let mut hits = Vec::new();
    for path in crate_rust_files(&["src", "tests"]) {
        let text = masked(&path);
        for keyword in ["trait", "impl"] {
            for at in word_offsets(&text, keyword) {
                let header_end = at + text[at..].find(['{', ';']).unwrap_or(text.len() - at);
                let header = &text[at..header_end];
                if !names_trait(keyword, header, "ProviderAdapter") {
                    continue;
                }
                let Some((open, close)) = brace_block(&text, at) else {
                    continue;
                };
                let body = &text[open..close];
                for method in DEAD_ADAPTER_METHODS {
                    if declares_fn(body, method) {
                        hits.push(format!(
                            "{}:{} {keyword} ProviderAdapter still declares `{method}`",
                            relative(&path),
                            line_of(&text, at)
                        ));
                    }
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "dead ProviderAdapter methods remain ({} sites); first 40:\n{}",
        hits.len(),
        hits.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn excise_k5_plan_orchestrator_has_no_code_reference() {
    assert_vocabulary_gone("K5 plan orchestrator", K5_VOCABULARY);
}

#[test]
fn excise_k7_cli_residue_has_no_code_reference() {
    assert_vocabulary_gone("K7 CLI residue", K7_VOCABULARY);
}

#[test]
fn excise_k8_legacy_packaging_has_no_code_reference() {
    assert_vocabulary_gone("K8 legacy packaging", K8_VOCABULARY);
}

/// `coordinator` is retired like `attach-leader` and `takeover`: its refusal must not
/// depend on argument parsing, `--help` must not hide it, and usage guidance must
/// never send the user back to the retired verb.
#[test]
fn excise_coordinator_verb_refuses_before_argument_parsing() {
    for args in [
        vec!["coordinator", "--json"],
        vec!["coordinator", "run"],
        vec!["coordinator", "start", "--json"],
        vec!["coordinator", "--help"],
        vec!["coordinator", "status", "--detail"],
        vec!["coordinator", "--no-such-flag"],
    ] {
        let json = args.contains(&"--json");
        let (output, text, workspace) = run_cli(&args);
        let leaked = state_files(&workspace);
        let _ = std::fs::remove_dir_all(&workspace);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {text}");
        assert!(
            text.contains("retired") && text.contains("schedule"),
            "{args:?} must say coordinator is retired and name `schedule`: {text}"
        );
        assert!(
            !text.contains("team-agent coordinator --help"),
            "{args:?} sends the user back to the retired verb: {text}"
        );
        assert!(leaked.is_empty(), "{args:?} wrote workspace state while refusing: {leaked:?}");
        if json {
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|error| panic!("{args:?}: stdout is not JSON ({error}): {text}"));
            assert_eq!(value["ok"], false, "{args:?}: {value}");
        }
    }
}

// ───────────────────────────── guards (GREEN → GREEN) ─────────────────────────────

#[test]
fn guard_protected_and_current_cases_are_not_deleted_wholesale() {
    for (file, test, reason) in ADJUDICATED_RETIREMENTS {
        assert!(
            !reason.trim().is_empty(),
            "retiring {file}::{test} needs a reason"
        );
    }
    let missing: Vec<String> = SURVIVORS
        .iter()
        .filter(|(file, test, _)| {
            !ADJUDICATED_RETIREMENTS
                .iter()
                .any(|(f, t, _)| f == file && t == test)
        })
        .filter(|(file, test, _)| {
            std::fs::read_to_string(crate_root().join(file))
                .map(|source| !declares_fn(&mask_rust(&source), test))
                .unwrap_or(true)
        })
        .map(|(file, test, origin)| format!("{file}::{test} [{origin}]"))
        .collect();
    let missing_files: Vec<&&str> = SURVIVOR_FILES
        .iter()
        .filter(|file| !crate_root().join(file).exists())
        .collect();
    assert!(
        missing.is_empty() && missing_files.is_empty(),
        "protected/current cases disappeared without an adjudicated retirement entry; keep their live \
         assertions (trim only the retired sub-assertions, repoint through real CLI/MCP/station APIs) or \
         record the ruling in ADJUDICATED_RETIREMENTS:\n{}\nmissing files: {missing_files:?}",
        missing.join("\n")
    );
}

#[test]
fn guard_live_kernels_beside_retired_code_survive() {
    let missing: Vec<String> = LIVE_NEIGHBOURS
        .iter()
        .filter(|(file, item)| {
            std::fs::read_to_string(crate_root().join(file))
                .map(|source| !declares_item(&mask_rust(&source), item))
                .unwrap_or(true)
        })
        .map(|(file, item)| format!("{file}: {item}"))
        .collect();
    assert!(
        missing.is_empty(),
        "live kernels that only share a file or a name with retired code were removed:\n{}",
        missing.join("\n")
    );
}

#[test]
fn guard_provider_adapter_keeps_its_native_command_surface() {
    let text = masked(&crate_root().join("src/provider/adapter.rs"));
    let at = word_offsets(&text, "trait")
        .into_iter()
        .find(|at| {
            let end = at + text[*at..].find('{').unwrap();
            names_trait("trait", &text[*at..end], "ProviderAdapter")
        })
        .expect("trait ProviderAdapter is declared in src/provider/adapter.rs");
    let (open, close) = brace_block(&text, at).unwrap();
    let body = &text[open..close];
    let missing: Vec<&&str> = LIVE_ADAPTER_METHODS
        .iter()
        .filter(|method| !declares_fn(body, method))
        .collect();
    assert!(
        missing.is_empty(),
        "ProviderAdapter lost live native-command methods: {missing:?}"
    );
}

#[test]
fn guard_public_tmux_backend_seams_stay_public() {
    let source = std::fs::read_to_string(crate_root().join("src/tmux_backend.rs")).unwrap();
    let missing: Vec<&&str> = PUBLIC_TMUX_SEAMS
        .iter()
        .filter(|seam| !source.lines().any(|line| line.trim() == **seam))
        .collect();
    assert!(
        missing.is_empty(),
        "integration crates use these TmuxBackend seams; they must stay pub and outside cfg(test): {missing:?}"
    );
}

#[test]
fn guard_manifest_targets_exist_at_the_pinned_baseline_shape() {
    let mut malformed = Vec::new();
    let mut rows = 0;
    for row in MANIFEST
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
    {
        rows += 1;
        let fields: Vec<&str> = row.splitn(6, '\t').collect();
        if fields.len() != 6
            || !fields[0].starts_with("crates/team-agent/src/")
            || fields[1].parse::<usize>().is_err()
            || !fields[2].starts_with('P')
            || fields[5].trim().is_empty()
            || fields[5] != fields[5].trim()
        {
            malformed.push(row.to_string());
        }
    }
    assert!(rows > 400, "manifest lost rows: {rows}");
    assert!(malformed.is_empty(), "malformed manifest rows: {malformed:#?}");
}

#[test]
fn guard_source_helpers_ignore_comments_and_literals() {
    let text = mask_rust(
        "// PaneTty\nlet a = \"PaneTty\"; /* PaneTty /* nested */ */\n\
         let b = r#\"PaneTty\"#; let c = 'x'; let d = '\\''; let e = '\"';\n\
         fn f<'a>(_: &'a PaneTty) {}\nenum E { PaneTtyMismatch }\n",
    );
    assert_eq!(word_offsets(&text, "PaneTty").len(), 1, "{text}");
    assert_eq!(line_of(&text, word_offsets(&text, "PaneTty")[0]), 4);
    assert!(declares_fn("fn fork(&self)", "fork"));
    assert!(!declares_fn("fn fork_plan(&self) { self.fork() }", "fork"));
    assert!(declares_item("pub(crate) struct InputFrame {", "InputFrame"));
    assert!(!declares_item("let x: InputFrame = y;", "InputFrame"));
    assert!(names_trait("impl", "impl ProviderAdapter for BasicProviderAdapter ", "ProviderAdapter"));
    assert!(names_trait("impl", "impl crate::provider::ProviderAdapter for X ", "ProviderAdapter"));
    assert!(!names_trait("impl", "impl From<ProviderAdapter> for X ", "ProviderAdapter"));
}
