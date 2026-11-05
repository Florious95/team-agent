//! Stage 1 dead-code excision gates: P1 private leaves/scaffolding plus P2 =
//! K1 (legacy SQLite message engine, MessageStore, FIFO waiter/watchers) and
//! K10 (legacy inject/submit transport trait surface).
//!
//! `excise_*` cases are RED on the 0.5.129 baseline (df796be5) and turn GREEN
//! only once the inventoried code is gone. `guard_*` cases are GREEN on the
//! baseline and must stay GREEN: they stop over-deletion of current/mixed test
//! cases and keep the current send path free of the retired engine.
//!
//! Inventory authority: the dead-code audit pinned at df796be5. Rows needing
//! fixture relocation (db/schema.rs DDL), supported-target adjudication
//! (ConPTY/Windows) or K4 ProviderAdapter methods (Stage 2) are out of scope.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = include_str!("fixtures/stage1_excision_manifest.tsv");

/// Manifest rows deliberately retained after a local caller check. Each entry is
/// `(file, declaration, reason)`; an empty reason is rejected.
const RETAINED_AFTER_REVIEW: &[(&str, &str, &str)] = &[
    (
        "src/tmux_backend.rs",
        "pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {",
        "small test seam still used by surviving current tests (incl. survivor-roster shutdown/tmux cases) from integration targets; must stay pub, cfg(test) would break them",
    ),
    (
        "src/tmux_backend.rs",
        "pub fn with_runner_for_workspace(runner: Box<dyn CommandRunner>, workspace: &Path) -> Self {",
        "small test seam still used by surviving current tests (incl. survivor-roster shutdown/tmux cases) from integration targets; must stay pub, cfg(test) would break them",
    ),
    (
        "src/tmux_backend.rs",
        "pub fn kill_server(&self) {",
        "inherent teardown used by surviving shutdown/isolation tests; not the removed Transport::kill_server",
    ),
];

/// Roster cases that a reviewer has explicitly retired as wholly obsolete.
/// Each entry is `(file, test, reason)`. Silent deletion is not accepted.
const ADJUDICATED_RETIREMENTS: &[(&str, &str, &str)] = &[];

/// Transport trait methods that only the retired inject/submit pipeline used.
const K10_TRANSPORT_METHODS: &[&str] = &[
    "supports_owned_native_input",
    "inject",
    "inject_with_submit_observer",
    "snapshot_enter_rescue",
    "rescue_enter_guarded",
    "rescue_enter",
    "set_session_env",
    "kill_server",
    "attach_session",
];

const P1_DELETED_FILES: &[&str] = &[
    "src/coordinator/steps/session_gate.rs",
    "src/coordinator/steps/delivery.rs",
    "src/coordinator/steps/runtime_prompts.rs",
    "src/coordinator/steps/health_sync.rs",
    "src/coordinator/steps/persist.rs",
    "src/messaging/tests/dup_inject.rs",
    "src/messaging/tests/leader_inject_acceptance.rs",
    "src/messaging/tests/main_preserved.rs",
    "src/messaging/enter_rescue_tests.rs",
    "src/messaging/delivery_consumption_tests.rs",
    "src/leader/tests/claim_leader_mailbox_flush_contract.rs",
    // P1b orphan subprocess harnesses (remove their worker registrations too).
    "src/coordinator/tests/s3_bounded.rs",
    "src/messaging/results/collection_tests.rs",
];

/// Current/mixed cases that Stage 1 must not delete wholesale: the 29 confirmed
/// mixed overrides (all phases), 15 explicit roots missed by the upstream
/// classifier, the 14 P2 B-review cases and the 9 ignored-label keepers.
#[rustfmt::skip]
const SURVIVORS: &[(&str, &str, &str)] = &[
    ("src/messaging/presentation.rs", "critical_classes_force_leader_while_prose_is_ignored", "mixed-override P2"),
    ("src/messaging/tests/runtime.rs", "casefile_leader_send_is_durable_without_entering_leader_funnel", "mixed-override P2"),
    ("src/tmux_backend/tests.rs", "query_single_field_argv_and_nonzero_maps_to_none", "mixed-override P2"),
    ("src/transport/tests/behavior.rs", "pane_field_variants_distinct", "mixed-override P2"),
    ("tests/claude_provider_trust_prompt_red.rs", "claude_active_yes_trust_handler_sends_exactly_enter_not_digit_one", "mixed-override P2"),
    ("tests/conpty_end_to_end_fake_worker.rs", "phase1_bullet1_quick_start_equivalent_spawns_fake_worker", "mixed-override P2"),
    ("src/transport_factory.rs", "legacy_tmux_endpoint_resolves_to_tmux_endpoint_backend", "mixed-override P3"),
    ("src/transport_factory.rs", "explicit_conpty_without_team_key_refuses_no_silent_fallback", "mixed-override P3"),
    ("tests/read_paths_batch3.rs", "batch3_read_only_factory_for_tmux_state_returns_tmux_backend_byte_equivalent", "mixed-override P3"),
    ("src/leader/tests/basics.rs", "leader_event_names_match_python_byte_for_byte", "mixed-override P4"),
    ("src/contract_runtime/registry.rs", "claude_native_argv_prompt_keeps_first_business_tool_bootstrap", "mixed-override P4"),
    ("tests/contract_config.rs", "mixed_contract_providers_route_every_cursor_name_without_migrating_state", "mixed-override P4"),
    ("tests/contract_config.rs", "cursor_mutation_malformed_declarations_and_api_profiles_refuse_without_writes", "mixed-override P4"),
    ("src/contract_runtime/registry.rs", "every_public_codex_name_selects_the_leaf_and_a_legacy_row_is_never_reclassified", "mixed-override P4"),
    ("src/contract_runtime/registry.rs", "bare_pi_keeps_its_legacy_meaning_and_canonical_key_selects_the_leaf", "mixed-override P4"),
    ("src/contract_runtime/registry.rs", "grok_server_key_is_generation_bound_and_has_no_inspector_fallback", "mixed-override P4"),
    ("src/contract_runtime/registry.rs", "cursor_has_no_kiro_control_or_tool_inspector_fallback", "mixed-override P4"),
    ("src/lifecycle/tests/core.rs", "add_agent_on_unowned_workspace_does_not_silently_add", "mixed-override P4"),
    ("src/cli/adapters.rs", "reminders_only_recommend_supported_result_commands", "mixed-override P4"),
    ("src/cli/emit.rs", "all_dispatch_kinds_have_exactly_one_spec", "mixed-override P4"),
    ("src/cli/emit.rs", "public_help_templates_and_cli_guidance_are_english", "mixed-override P4"),
    ("src/cli/mod.rs", "audit_write_failure_skips_destructive_kill", "mixed-override P5"),
    ("src/event_log.rs", "rotation_at_threshold_shifts_archives_and_drops_oldest", "mixed-override P5"),
    ("src/event_log.rs", "concurrent_appends_do_not_interleave", "mixed-override P5"),
    ("src/model/enums.rs", "enums_serialize_to_exact_python_strings", "mixed-override P5"),
    ("src/model/paths.rs", "subdirs_match_python_golden", "mixed-override P5"),
    ("src/platform/process.rs", "signal_kind_and_termination_outcome_are_distinguishable_variants", "mixed-override P5"),
    ("src/contract_runtime/mcp.rs", "diagnostic_sink_failures_do_not_change_execution_results", "mixed-override P5"),
    ("src/lifecycle/tests/pi_lifecycle_doctor_red.rs", "pi_stop_retains_backing_and_never_scans_by_process_name", "mixed-override P5"),
    ("src/tmux_backend/tests.rs", "real_command_runner_enforces_golden_5s_timeout_on_hang", "omitted-root"),
    ("src/tmux_backend/tests.rs", "list_targets_is_live_pane_discovery_primitive_for_status_doctor", "omitted-root"),
    ("src/tmux_backend/tests.rs", "r1_caller_target_uuid_is_first_leader_session_uuid_precedence_seam", "omitted-root"),
    ("src/cli/tests/leader_watch.rs", "cli_shutdown_kills_team_session_real_teardown", "omitted-root"),
    ("src/cli/tests/leader_watch.rs", "leader_port_takeover_confirm_without_pane_refuses_caller_pane_missing", "omitted-root"),
    ("src/cli/tests/leader_watch.rs", "leader_port_claim_leader_no_pane_refuses_caller_pane_missing_family_a", "omitted-root"),
    ("src/leader/tests/lease_api.rs", "write_lease_dual_state_persists_canonical_root_only_no_legacy_side_effect", "omitted-root"),
    ("src/leader/tests/lease_claim.rs", "claim_lease_no_incident_vacant_acquire_advances_epoch_and_writes_canonical_only", "omitted-root"),
    ("src/lifecycle/tests/launch_spawn.rs", "quick_start_full_ready_real_spawn", "omitted-root"),
    ("src/lifecycle/tests/launch_spawn.rs", "spawn_launch_not_dry_run_is_no_longer_the_spawn_stub", "omitted-root"),
    ("src/lifecycle/tests/agent_ops.rs", "lanea_stop_agent_kills_window_and_marks_stopped", "omitted-root"),
    ("src/transport/tests/behavior.rs", "wezterm_list_targets_forward_registration_real_machine", "omitted-root"),
    ("tests/cli_human_surface_red.rs", "help_is_pure_on_bad_route", "omitted-root"),
    ("tests/cli_human_surface_red.rs", "printed_examples_reach_actual_parser_without_help_shortcut", "omitted-root"),
    ("tests/leader_launcher_ambient_pane_authority_red.rs", "b3_each_public_surface_recovery_action_closes_its_original_refusal", "omitted-root"),
    ("tests/reclaim_neutral_red.rs", "send_path_reclaim_neutral_cli_broadcast_fanout_and_mcp_do_not_claim", "P2 B-review"),
    ("tests/reclaim_neutral_red.rs", "transport_retry_reclaim_neutral_injection_and_pending_paths_do_not_write_owner", "P2 B-review"),
    ("tests/reclaim_neutral_red.rs", "explicit_claim_unconditional_live_caller_claims_vacant_or_dead_and_requeues_same_message_id", "P2 B-review"),
    ("tests/reclaim_neutral_red.rs", "dead_leader_send_does_not_autofix_and_tells_user_to_claim_or_takeover", "P2 B-review"),
    ("tests/reclaim_neutral_red.rs", "send_owner_gate_retreat_cases_do_not_auto_bind_or_open_cross_team_access", "P2 B-review"),
    ("tests/shipgate_033_final_red.rs", "tit16_scoped_child_shutdown_reaps_only_child_and_parent_still_delivers", "P2 B-review"),
    ("tests/sweep_new_reds_254_real_red.rs", "rc3_after_claim_child_worker_result_reaches_child_leader", "P2 B-review"),
    ("tests/sweep_new_reds_254_real_red.rs", "tit_fake_worker_auto_report_keeps_selected_team_when_active_sibling_differs", "P2 B-review"),
    ("tests/sweep_new_reds_254_red.rs", "mcp_send_canonicalize_owner_team_id_no_fallback", "P2 B-review"),
    ("src/conpty/backend.rs", "no_pipe_client_returns_honest_mux_unavailable_not_success", "P2 B-review"),
    ("src/tmux_backend/native_input_tests.rs", "native_input_runtime_endpoint_factory_preserves_workspace_and_native_capability", "P2 B-review"),
    ("src/tmux_backend/tests.rs", "leader_receiver_full_endpoint_liveness_list_and_inject_use_s_path_command_shape", "P2 B-review"),
    ("src/transport/tests/wire.rs", "send_keys_sequence_routes_through_transport", "P2 B-review"),
    ("tests/conpty_end_to_end_fake_worker.rs", "phase1_bullet2_status_reports_conpty_backend_and_pane_row", "P2 B-review"),
    ("src/lifecycle/tests/team_in_team_sibling_quick_start_red.rs", "dirty_sibling_quick_start_from_same_leader_pane_binds_receiver_and_delivers_to_leader", "ignored-18 keep-repoint-seam"),
    ("tests/sweep_new_reds_254_real_red.rs", "depth_real_nested_cli_persists_child_depth_and_refuses_grandchild", "ignored-18 keep-repoint-seam"),
    ("tests/sweep_new_reds_254_real_red.rs", "depth_independent_domain_teams_remain_roots_with_leader_named_workers", "ignored-18 keep-repoint-seam"),
    ("tests/sweep_new_reds_254_real_red.rs", "rc3_unclaimed_child_leader_requires_claim_without_sentinel", "ignored-18 keep-repoint-seam"),
    ("tests/shutdown_provider_escape_red.rs", "shutdown_real_tmux_list_targets_populates_pane_pid", "ignored-18 keep-repoint-seam"),
    ("tests/shutdown_provider_escape_red.rs", "shutdown_reaps_workspace_mcp_process_not_descended_from_pane_pid", "ignored-18 keep-current-test"),
    ("tests/shutdown_provider_escape_red.rs", "shutdown_reaps_provider_process_by_pgid_or_workspace_when_ppid_escaped", "ignored-18 keep-current-test"),
    ("tests/shutdown_provider_escape_red.rs", "shutdown_session_killed_requires_post_reap_has_session_false", "ignored-18 keep-current-test"),
    ("tests/shutdown_missing_state_reaps_processes_red.rs", "shutdown_reaps_discovered_processes_when_state_file_is_missing", "ignored-18 keep-repoint-seam"),
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

#[test]
fn excise_p1_scaffold_helper_only_and_orphan_modules_are_deleted() {
    let present: Vec<_> = P1_DELETED_FILES
        .iter()
        .filter(|file| crate_root().join(file).exists())
        .collect();
    assert!(
        present.is_empty(),
        "P1 empty/helper-only/orphan modules still present ({} of {}): {present:#?}",
        present.len(),
        P1_DELETED_FILES.len()
    );
}

#[test]
fn excise_p1_k1_k10_inventory_declarations_are_deleted() {
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
        if RETAINED_AFTER_REVIEW
            .iter()
            .any(|(f, d, _)| *f == file && *d == declaration)
        {
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
        "{} inventoried Stage 1 declarations remain [{}]; first 40:\n{}",
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
fn excise_message_store_engine_has_no_executable_reference() {
    let mut hits = Vec::new();
    for path in crate_rust_files(&["src", "tests"]) {
        let text = masked(&path);
        for at in word_offsets(&text, "MessageStore") {
            hits.push(format!("{}:{}", relative(&path), line_of(&text, at)));
        }
    }
    assert!(
        hits.is_empty(),
        "the retired SQLite MessageStore engine is still referenced by code ({} sites); first 40:\n{}",
        hits.len(),
        hits.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn excise_transport_trait_and_every_impl_drop_k10_inject_rescue_methods() {
    let mut hits = Vec::new();
    for path in crate_rust_files(&["src", "tests"]) {
        let text = masked(&path);
        for keyword in ["trait", "impl"] {
            for at in word_offsets(&text, keyword) {
                let header_end = at + text[at..].find(['{', ';']).unwrap_or(text.len() - at);
                let header = &text[at..header_end];
                let transport = names_trait(keyword, header, "Transport");
                let observer = names_trait(keyword, header, "SubmitObserver");
                if !transport && !observer {
                    continue;
                }
                if observer {
                    hits.push(format!(
                        "{}:{} {keyword} SubmitObserver",
                        relative(&path),
                        line_of(&text, at)
                    ));
                    continue;
                }
                let Some((open, close)) = brace_block(&text, at) else {
                    continue;
                };
                let body = &text[open..close];
                for method in K10_TRANSPORT_METHODS {
                    if declares_fn(body, method) {
                        hits.push(format!(
                            "{}:{} {keyword} Transport still declares `{method}`",
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
        "K10 retired transport methods remain ({} sites); first 40:\n{}",
        hits.len(),
        hits.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn excise_fifo_result_waiter_is_gone_from_messaging() {
    let hits: Vec<String> = crate_rust_files(&["src/messaging"])
        .into_iter()
        .flat_map(|path| {
            let text = masked(&path);
            word_offsets(&text, "mkfifo")
                .into_iter()
                .map(|at| format!("{}:{}", relative(&path), line_of(&text, at)))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        hits.is_empty(),
        "the FIFO result waiter still creates FIFOs: {hits:#?}"
    );
}

/// `wait --task` is backed by the retired FIFO/SQL waiter. Its refusal must stay
/// fail-closed and name the real C3 replacement, not internal jargon.
#[test]
fn excise_wait_refusal_names_the_public_inbox_replacement() {
    let workspace = std::env::temp_dir().join(format!(
        "stage1-wait-refusal-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["wait", "--task", "task-1", "--workspace"])
        .arg(&workspace)
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
    let _ = std::fs::remove_dir_all(&workspace);
    assert!(!output.status.success(), "wait must refuse: {text}");
    assert!(
        text.contains("retired") && text.contains("team-agent inbox"),
        "wait refusal must say it is retired and point to `team-agent inbox`: {text}"
    );
}

#[test]
fn guard_current_and_mixed_cases_are_not_deleted_wholesale() {
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
                .map(|source| word_offsets(&mask_rust(&source), test).is_empty())
                .unwrap_or(true)
        })
        .map(|(file, test, origin)| format!("{file}::{test} [{origin}]"))
        .collect();
    assert!(
        missing.is_empty(),
        "current/mixed cases disappeared without an adjudicated retirement entry; keep their live \
         assertions (trim only obsolete sub-assertions) or record the reason in ADJUDICATED_RETIREMENTS:\n{}",
        missing.join("\n")
    );
}

#[test]
fn guard_current_send_path_never_reacquires_the_retired_engine() {
    let mut files = crate_rust_files(&["src/station"]);
    files.extend(
        [
            "src/inbox_store.rs",
            "src/direct_delivery.rs",
            "src/cli/send.rs",
        ]
        .map(|f| crate_root().join(f)),
    );
    let mut hits = Vec::new();
    for path in files {
        let text = masked(&path);
        for token in ["rusqlite", "message_store", "MessageStore", "mkfifo"] {
            for at in word_offsets(&text, token) {
                hits.push(format!(
                    "{}:{} {token}",
                    relative(&path),
                    line_of(&text, at)
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "current send/inbox path depends on the retired engine: {hits:#?}"
    );
}

#[test]
fn guard_source_masker_ignores_comments_and_literals() {
    let text = mask_rust(
        "// MessageStore\nlet a = \"MessageStore\"; /* MessageStore /* nested */ */\n\
         let b = r#\"MessageStore\"#; let c = 'x'; let d = '\\''; let e = '\"';\n\
         fn f<'a>(_: &'a MessageStore) {}\n",
    );
    assert_eq!(word_offsets(&text, "MessageStore").len(), 1, "{text}");
    assert_eq!(line_of(&text, word_offsets(&text, "MessageStore")[0]), 4);
    assert!(declares_fn("fn inject(&self)", "inject"));
    assert!(!declares_fn(
        "fn reinject(&self) { self.inject() }",
        "inject"
    ));
}
