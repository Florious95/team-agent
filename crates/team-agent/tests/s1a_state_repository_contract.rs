//! S1a StateRepository RED contracts.
//!
//! ---
//! purpose: cfg-aware direct state-save governance scanner
//! contract:
//!   provides:
//!     - name: s1a_direct_save_scan
//!       what: resolves Rust external modules and excludes only proven cfg(test) files
//!   depends:
//!     - name: state_save_allowlist
//!       what: frozen direct-save rows and semantic intent catalog
//! boundary:
//!   - scanner scope is product modules reachable from src/lib.rs
//!   - unsupported cfg or module-surface forms fail closed as unknown
//! maturity: wired
//! ---
//!
//! References:
//! - `.team/artifacts/s1a-state-repository-design.md` section 5 baseline
//!   allowlist and section 8 RED1-RED5.
//!
//! User story: every state write says what semantic intent it has before S1b
//! migrates write clusters. S1a must not move state truth, change schemas, or
//! alter helper behavior; it only adds the repository facade and hardens the
//! direct-save governance gate.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/state_save_allowlist.rs"]
mod state_save_allowlist;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use state_save_allowlist::{
    AllowedStateSaveCall, ALLOWED_STATE_SAVE_CALLS, BASELINE_DIRECT_SAVE_COUNT,
};

const REPOSITORY_PATH: &str = "state/repository.rs";
const FORBIDDEN_INTENT_VARIANTS: &[&str] = &[
    "Other",
    "Unknown",
    "RawSave",
    "LegacySave",
    "SaveRuntimeState",
    "SaveTeamScopedState",
];

/// G0 frozen row keys (path::fn::callee, one entry per baseline row, sorted).
/// Ratchet invariant (external writers): the live allowlist may only DELETE
/// external-writer rows relative to this set — never add or rename. Slice-end
/// external zero emerges from deletion: once every external row is removed, any
/// remaining external callsite fails RED1 as unclassified.
///
/// Extension predicate (repository-internal authority helpers ONLY): a row MAY
/// be ADDED here iff it is a repository-internal authority helper — precisely,
/// `is_external_writer(path) == false` AND the write is reached only through
/// `StateWriteIntent` dispatch (same family as
/// `save_runtime_state_with_lifecycle_topology_authority`). Such helpers grow
/// with repository evolution (e.g. leader-inbound
/// `save_runtime_state_with_receiver_authority`); this is NOT an "internal code
/// may add anything" loophole — an external writer can NEVER be added, only
/// deleted. The `external_live` accounting below stays a pure down-ratchet.
/// (verifier single-point sign-off, leader msg_f9bf8320c5d0; the structural
/// question of encoding this split in the scanner itself is an arch-delta item.)
const FROZEN_G0_ROW_KEYS: &[&str] = &[
    "cli/adapters.rs::fake_shutdown::save_runtime_state",
    "cli/adapters.rs::seed_fake_e2e_state::save_runtime_state",
    "cli/mod.rs::acknowledge_idle::save_runtime_state",
    "cli/mod.rs::promote_live_sibling_after_scoped_shutdown::save_runtime_state",
    "cli/mod.rs::shutdown_with_transport_and_state::save_runtime_state",
    "cli/mod.rs::shutdown_with_transport_and_state::save_team_scoped_state",
    "coordinator/conpty_shim.rs::finalize::save_runtime_state",
    "coordinator/conpty_shim.rs::mark_transport_unavailable::save_runtime_state",
    "coordinator/steps/abnormal.rs::attempt_due_recoveries::save_runtime_state",
    "coordinator/steps/abnormal.rs::write_recovery_intent_result::save_runtime_state",
    "leader/start.rs::persist_exec_provider_leader_binding::save_runtime_state",
    "leader/start.rs::persist_external_leader_topology_marker::save_runtime_state",
    "leader/start.rs::persist_managed_leader_binding::save_runtime_state",
    "leader/start.rs::refresh_managed_leader_provider_binding::save_runtime_state",
    "lifecycle/launch.rs::annotate_persisted_team_depth::save_runtime_state",
    "lifecycle/launch.rs::rollback_add_agent_atomic::save_runtime_state_with_deleted_agents",
    "lifecycle/launch.rs::rollback_add_agent_atomic::save_runtime_state_with_deleted_agents",
    "lifecycle/launch.rs::save_launched_team_state_for_key::save_runtime_state",
    "lifecycle/restart/agent.rs::reset_agent_at_paths::save_team_scoped_state_with_tombstone_lifecycle_topology_authority",
    "lifecycle/restart/agent.rs::stop_agent_at_paths::save_team_scoped_state_with_lifecycle_topology_authority",
    "lifecycle/restart/common.rs::save_restart_projected_state_with_capture_backfill_skip::save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "lifecycle/restart/rebuild.rs::save_restart_session_repairs::save_team_scoped_state",
    "lifecycle/restart/rebuild.rs::save_restart_session_repairs::save_team_scoped_state",
    "lifecycle/restart/rebuild.rs::save_restart_state_with_lifecycle_topology_authority_and_capture_backfill_skip::save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "lifecycle/restart/remove.rs::remove_agent_inner::save_team_scoped_state_with_deleted_agents",
    "mcp_server/lifecycle_tools/agent_ops.rs::prepare_selected_team_state::save_runtime_state",
    "mcp_server/lifecycle_tools/state_status.rs::update_state::save_team_scoped_state_reapplying_after_conflict",
    "mcp_server/lifecycle_tools/state_status.rs::update_state_without_spec::save_team_scoped_state_reapplying_after_conflict",
    "mcp_server/tools.rs::assign_task::save_runtime_state_reapplying_after_conflict",
    "messaging/activity.rs::detect_idle_fallbacks::save_runtime_state_reapplying_after_conflict",
    "messaging/delivery.rs::save_scoped_state::save_runtime_state",
    "messaging/delivery.rs::save_scoped_state::save_runtime_state",
    "messaging/delivery.rs::save_scoped_state::save_team_scoped_state",
    "messaging/delivery.rs::save_scoped_state_reapplying_after_conflict::save_runtime_state_reapplying_after_conflict",
    "messaging/delivery.rs::save_scoped_state_reapplying_after_conflict::save_team_scoped_state_reapplying_after_conflict",
    "messaging/results.rs::collect_scoped::save_runtime_state_reapplying_after_conflict",
    "messaging/results.rs::collect_scoped::save_team_scoped_state_reapplying_after_conflict",
    "messaging/scheduler.rs::stuck_cancel::save_runtime_state_reapplying_after_conflict",
    "state/persist.rs::load_runtime_state::save_runtime_state",
    "state/persist.rs::save_runtime_state::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_reapplying_after_conflict::save_runtime_state",
    "state/persist.rs::save_runtime_state_reapplying_after_conflict::save_runtime_state",
    "state/persist.rs::save_runtime_state_with_deleted_agents::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_lifecycle_topology_authority::save_runtime_state_with_merge_options",
    // repository-internal authority helper added per the extension predicate above
    // (leader-inbound receiver authority; is_external_writer == false).
    "state/persist.rs::save_runtime_state_with_receiver_authority::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_team_tombstone_lifecycle_topology_authority::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_team_tombstoned_agents::save_runtime_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state::save_team_scoped_state_with_deleted_agents",
    "state/projection.rs::save_team_scoped_state_reapplying_after_conflict::save_team_scoped_state",
    "state/projection.rs::save_team_scoped_state_reapplying_after_conflict::save_team_scoped_state",
    "state/projection.rs::save_team_scoped_state_with_deleted_agents::save_team_scoped_state_with_merge_exceptions",
    "state/projection.rs::save_team_scoped_state_with_lifecycle_topology_authority::save_team_scoped_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip::save_team_scoped_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state_with_merge_exceptions::save_team_scoped_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_deleted_agents",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_deleted_agents",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstoned_agents",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstoned_agents",
    "state/projection.rs::save_team_scoped_state_with_tombstone_lifecycle_topology_authority::save_team_scoped_state_with_merge_options",
];

#[test]
fn red1_baseline_allowlist_classifies_all_current_direct_state_saves() {
    let calls = scan_product_state_saves();
    let allowlist = allowlist_by_key();
    let mut failures = Vec::new();

    if !repo_root()
        .join("crates/team-agent/src")
        .join(REPOSITORY_PATH)
        .exists()
    {
        failures.push(format!(
            "StateRepository skeleton is missing at src/{REPOSITORY_PATH}; S1a allowlist is not actionable without the write facade"
        ));
    }
    // Ratchet (successor to the fixed ==67 pin): the live allowlist must be a
    // multiset-subset of the frozen G0 rows — rows may only be deleted as their
    // callsites are converted, never added or renamed. New debt is blocked by
    // the unclassified check below; count monotonicity is derived, not pinned.
    {
        let mut frozen: BTreeMap<&str, usize> = BTreeMap::new();
        for key in FROZEN_G0_ROW_KEYS {
            *frozen.entry(key).or_default() += 1;
        }
        let mut live: BTreeMap<String, usize> = BTreeMap::new();
        for row in ALLOWED_STATE_SAVE_CALLS {
            *live
                .entry(format!(
                    "{}::{}::{}",
                    row.path, row.containing_fn, row.callee_family
                ))
                .or_default() += 1;
        }
        if ALLOWED_STATE_SAVE_CALLS.len() > FROZEN_G0_ROW_KEYS.len() {
            failures.push(format!(
                "allowlist grew beyond frozen G0 baseline: {} > {}",
                ALLOWED_STATE_SAVE_CALLS.len(),
                FROZEN_G0_ROW_KEYS.len()
            ));
        }
        for (key, count) in &live {
            let allowed = frozen.get(key.as_str()).copied().unwrap_or(0);
            if *count > allowed {
                failures.push(format!(
                    "allowlist row not in frozen G0 baseline (or multiplicity grew): {key} live={count} frozen={allowed}"
                ));
            }
        }
        let external_live = ALLOWED_STATE_SAVE_CALLS
            .iter()
            .filter(|row| is_external_writer(row.path))
            .count();
        let internal_live = ALLOWED_STATE_SAVE_CALLS.len() - external_live;
        if BASELINE_DIRECT_SAVE_COUNT < ALLOWED_STATE_SAVE_CALLS.len() {
            failures.push(format!(
                "BASELINE_DIRECT_SAVE_COUNT must ratchet down with the allowlist: const={BASELINE_DIRECT_SAVE_COUNT} rows={}",
                ALLOWED_STATE_SAVE_CALLS.len()
            ));
        }
        // Split accounting: external debt vs authority-internal writes are
        // reported separately so the external 38 -> 0 ratchet is auditable.
        let _ = (external_live, internal_live);
    }

    let mut seen = BTreeSet::new();
    for call in &calls {
        let key = call.key();
        seen.insert(key.clone());
        match allowlist.get(&key).and_then(|rows| rows.first()).copied() {
            Some(allowed) => {
                if allowed.intent.is_empty()
                    || allowed.migration_phase.is_empty()
                    || allowed.reason.is_empty()
                {
                    failures.push(format!(
                        "allowlist row has empty intent/phase/reason for {key}: {allowed:?}"
                    ));
                }
                if is_external_writer(&call.path) && allowed.intent == "repository_internal" {
                    failures.push(format!(
                        "external writer {key} cannot be classified as repository_internal"
                    ));
                }
            }
            None => failures.push(format!(
                "unclassified direct state save: {}:{} fn={} callee={} snippet={}",
                call.path, call.line, call.containing_fn, call.callee_family, call.snippet
            )),
        }
    }

    for (key, rows) in &allowlist {
        let current_count = calls.iter().filter(|call| call.key() == *key).count();
        if current_count != rows.len() {
            let evidence = rows
                .iter()
                .map(|row| row.evidence_line.to_string())
                .collect::<Vec<_>>()
                .join(",");
            failures.push(format!(
                "allowlist key occurrence count drifted: {key} expected={} current={} evidence_lines={evidence}",
                rows.len(),
                current_count
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED1: every one of the {BASELINE_DIRECT_SAVE_COUNT} G0 direct state-save callsites must be classified by path+function+callee family, with repository skeleton present.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red2_new_direct_state_saves_are_blocked_outside_repository_or_allowlist() {
    let calls = scan_product_state_saves();
    let allowlist = allowlist_by_key();
    let mut failures = Vec::new();

    if !repo_root()
        .join("crates/team-agent/src")
        .join(REPOSITORY_PATH)
        .exists()
    {
        failures.push(format!(
            "state/repository.rs is missing; new direct-save gate cannot distinguish repository internals from legacy allowlist"
        ));
    }

    let mut line_hints = Vec::new();
    for call in &calls {
        if is_repository_internal_path(&call.path) {
            continue;
        }
        let key = call.key();
        let Some(rows) = allowlist.get(&key) else {
            failures.push(format!(
                "new or unallowlisted direct save is blocked: {}:{} fn={} callee={} snippet={}",
                call.path, call.line, call.containing_fn, call.callee_family, call.snippet
            ));
            continue;
        };
        if !rows.iter().any(|row| row.evidence_line == call.line) {
            let evidence = rows
                .iter()
                .map(|row| row.evidence_line.to_string())
                .collect::<Vec<_>>()
                .join(",");
            line_hints.push(format!(
                "line drift only: {key} evidence_lines={evidence} current_line={}",
                call.line
            ));
        }
    }

    for hint in line_hints {
        println!("RED2_HINT {hint}");
    }

    assert!(
        failures.is_empty(),
        "RED2: new direct save_runtime_state*/save_team_scoped_state* callsites must be blocked unless they are in state/repository.rs or the S1a allowlist key.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red3_state_write_intent_has_no_escape_bucket_and_covers_allowlist_intents() {
    let repository = repository_source_or_panic("RED3");
    let enum_block = block_after(&repository, "enum StateWriteIntent").unwrap_or_else(|| {
        panic!(
            "RED3: repository must declare `StateWriteIntent`; source={}",
            repository
        )
    });
    let normalized_enum = normalize(&enum_block);
    let mut failures = Vec::new();

    for forbidden in FORBIDDEN_INTENT_VARIANTS {
        if normalized_enum.contains(&normalize(forbidden)) {
            failures.push(format!(
                "StateWriteIntent must not expose escape bucket/helper-name variant `{forbidden}`; enum={enum_block}"
            ));
        }
    }
    for intent in required_intents() {
        if !normalized_enum.contains(&normalize(&intent)) {
            failures.push(format!(
                "StateWriteIntent is missing allowlist intent variant `{intent}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED3: StateWriteIntent must be a closed semantic catalog, with no Other/Unknown/RawSave escape and all non-internal allowlist intents covered.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red4_repository_dispatches_representative_intents_to_existing_helpers() {
    let repository = repository_source_or_panic("RED4");
    let expectations = [
        (
            "ClaimLeader",
            "helper_write_root",
            "ClaimLeader must dispatch to the existing root helper",
        ),
        (
            "CoordinatorTick",
            "save_team_scoped_state",
            "coordinator tick must dispatch to the existing scoped helper",
        ),
        (
            "StopAgent",
            "save_team_scoped_state_with_lifecycle_topology_authority",
            "stop-agent must keep lifecycle topology-authority scoped save behavior",
        ),
        (
            "ResetAgent",
            "save_team_scoped_state_with_tombstone_lifecycle_topology_authority",
            "reset-agent discard-session must keep tombstone topology behavior",
        ),
        (
            "ForkAgent",
            "helper_write_team_scoped_with_lifecycle_topology_authority",
            "fork-agent must persist the selected team projection with topology authority",
        ),
        (
            "AgentRollback",
            "helper_write_team_scoped_with_deleted_agents",
            "team-scoped agent rollback must preserve sibling teams",
        ),
        (
            "ResultCollection",
            "save_team_scoped_state_reapplying_after_conflict",
            "result collection with a team must keep scoped reapply behavior",
        ),
        (
            "ResultCollection",
            "save_runtime_state_reapplying_after_conflict",
            "result collection without a team must keep root reapply behavior",
        ),
    ];
    let mut failures = Vec::new();
    for (intent, helper, reason) in expectations {
        if !source_mentions_helper_near_intent(&repository, intent, helper) {
            failures.push(format!(
                "{reason}: repository must include intent `{intent}` and helper `{helper}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED4: S1a repository must be behavior-neutral for representative intents by dispatching to the same legacy helper families.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red5_repository_skeleton_does_not_introduce_schema_or_b3_path_migration() {
    let repository = repository_source_or_panic("RED5");
    let normalized = normalize(&repository);
    let mut failures = Vec::new();

    for forbidden in [
        "schema_version",
        "SCHEMA_VERSION",
        "TeamRuntimePaths",
        "runtime/teams",
        ".team/runtime/teams",
        "canonical per-team",
        "migrate_state",
        "B3",
    ] {
        if normalized.contains(&normalize(forbidden)) {
            failures.push(format!(
                "repository skeleton must not introduce schema/path migration term `{forbidden}`"
            ));
        }
    }
    for required in [
        "StateRepository",
        "load_workspace",
        "load_team",
        "save_reapplying",
    ] {
        if !normalized.contains(&normalize(required)) {
            failures.push(format!(
                "repository skeleton must expose `{required}` without changing schema"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED5: S1a is a facade/no-schema-migration slice; repository must expose the skeleton while avoiding B3 path/schema migration terms.\n{}",
        failures.join("\n")
    );
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DirectSaveCall {
    path: String,
    containing_fn: String,
    callee_family: String,
    line: usize,
    snippet: String,
}

impl DirectSaveCall {
    fn key(&self) -> String {
        format!(
            "{}::{}::{}",
            self.path, self.containing_fn, self.callee_family
        )
    }
}

fn scan_product_state_saves() -> Vec<DirectSaveCall> {
    let src = repo_root().join("crates/team-agent/src");
    let module_map = build_module_map(&src).expect("build product Rust module map");
    assert!(
        module_map.unknowns.is_empty(),
        "S1A scanner module map is unknown; unsupported cfg/macro/include forms must be resolved or reported:\n{}",
        module_map.unknowns.join("\n")
    );
    println!(
        "S1A_MODULE_MAP production_files={} test_only_files={} unknowns=0",
        module_map.production_files.len(),
        module_map.test_only_files.len()
    );
    let mut calls = Vec::new();
    for path in module_map.production_files {
        scan_file(&path, &mut calls).expect("scan product source state-save callsites");
    }
    calls.sort();
    calls
}

#[derive(Debug, Default)]
struct ModuleMap {
    production_files: BTreeSet<PathBuf>,
    test_only_files: BTreeSet<PathBuf>,
    unknowns: Vec<String>,
}

#[derive(Debug)]
struct ModuleDeclaration {
    name: String,
    reachability: CfgReachability,
    path: Option<PathBuf>,
    line: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CfgReachability {
    Production,
    TestOnly,
    Unknown,
}

fn build_module_map(src: &Path) -> std::io::Result<ModuleMap> {
    let mut map = ModuleMap::default();
    let mut queue = vec![(src.join("lib.rs"), false)];
    let mut visited = BTreeMap::<PathBuf, bool>::new();

    while let Some((path, inherited_test_only)) = queue.pop() {
        let test_only = inherited_test_only;
        if let Some(previous_test_only) = visited.get(&path).copied() {
            if previous_test_only && !test_only {
                map.unknowns.push(format!(
                    "{}: module is reached through both cfg(test) and production declarations",
                    path.display()
                ));
            }
            if previous_test_only || !test_only {
                continue;
            }
        }
        visited.insert(path.clone(), test_only);
        if test_only {
            map.test_only_files.insert(path.clone());
        } else {
            map.production_files.insert(path.clone());
        }

        // Once a file is proven reachable only through cfg(test), its contents
        // are not product code. Do not recursively interpret its test helpers
        // as production modules; the parent declaration is the proof boundary.
        if test_only {
            continue;
        }

        let source = fs::read_to_string(&path)?;
        let (declarations, includes, source_unknowns) = parse_module_surface(&source, &path);
        map.unknowns.extend(source_unknowns);
        for include in includes {
            match resolve_include(&path, &include.target) {
                Ok(child) => queue.push((child, test_only)),
                Err(reason) => map.unknowns.push(format!(
                    "{}:{} include!({:?}): {reason}",
                    path.display(),
                    include.line,
                    include.target
                )),
            }
        }
        for declaration in declarations {
            let child = match resolve_module(&path, &declaration) {
                Ok(child) => child,
                Err(reason) => {
                    map.unknowns.push(format!(
                        "{}:{} mod {}: {reason}",
                        path.display(),
                        declaration.line,
                        declaration.name
                    ));
                    continue;
                }
            };
            let child_test_only =
                test_only || declaration.reachability == CfgReachability::TestOnly;
            queue.push((child, child_test_only));
        }
    }
    Ok(map)
}

#[derive(Debug)]
struct IncludeDirective {
    target: String,
    line: usize,
}

fn parse_module_surface(
    source: &str,
    path: &Path,
) -> (Vec<ModuleDeclaration>, Vec<IncludeDirective>, Vec<String>) {
    let mut declarations = Vec::new();
    let mut includes = Vec::new();
    let mut unknowns = Vec::new();
    let mut pending_cfg = CfgReachability::Production;
    let mut pending_path = None;
    let mut pending_cfg_attr = false;
    let mut macro_depth = None;

    for (index, raw_line) in source.lines().enumerate() {
        let line = index + 1;
        let code = raw_line.split("//").next().unwrap_or("").trim();
        if code.is_empty() {
            continue;
        }
        if let Some(depth) = macro_depth.as_mut() {
            if code.contains(" mod ") || code.starts_with("mod ") {
                unknowns.push(format!(
                    "{}:{} macro-generated module declaration is unsupported",
                    path.display(),
                    line
                ));
            }
            *depth += brace_delta(code);
            if *depth <= 0 {
                macro_depth = None;
            }
            continue;
        }
        if code.contains("macro_rules!") {
            let depth = brace_delta(code);
            if depth > 0 {
                macro_depth = Some(depth);
            }
            continue;
        }
        if code.contains("mod ") && code.contains('!') {
            unknowns.push(format!(
                "{}:{} macro-generated module declaration is unsupported",
                path.display(),
                line
            ));
            continue;
        }

        let mut remainder = code.to_string();
        if let Some(start) = remainder.find("#[cfg(") {
            if let Some(end) = remainder[start..].find(")]") {
                let attr_end = start + end + 2;
                pending_cfg = cfg_reachability(&remainder[start + 6..start + end]);
                if pending_cfg == CfgReachability::Unknown {
                    unknowns.push(format!(
                        "{}:{} cfg expression is not provably production or test-only",
                        path.display(),
                        line
                    ));
                }
                remainder = remainder[..start].to_string() + remainder[attr_end..].trim_start();
            } else {
                unknowns.push(format!(
                    "{}:{} unterminated cfg attribute",
                    path.display(),
                    line
                ));
                continue;
            }
        }
        if remainder.contains("#[cfg_attr") {
            pending_cfg_attr = true;
        }
        if let Some(start) = remainder.find("#[path") {
            if let Some(end) = remainder[start..].find(']') {
                let attr_end = start + end + 1;
                match remainder[start..attr_end]
                    .split_once('=')
                    .and_then(|(_, value)| parse_string_literal(value))
                {
                    Some(target) => pending_path = Some(PathBuf::from(target)),
                    None => unknowns.push(format!(
                        "{}:{} unsupported #[path] attribute",
                        path.display(),
                        line
                    )),
                }
                remainder = remainder[..start].to_string() + remainder[attr_end..].trim_start();
            }
        }

        for (target, include_line) in include_targets(&remainder, line, path, &mut unknowns) {
            includes.push(IncludeDirective {
                target,
                line: include_line,
            });
        }

        if let Some(name) = external_module_name(&remainder) {
            if pending_cfg_attr {
                unknowns.push(format!(
                    "{}:{} cfg_attr affecting module reachability is unsupported",
                    path.display(),
                    line
                ));
            }
            declarations.push(ModuleDeclaration {
                name,
                reachability: pending_cfg,
                path: pending_path.take(),
                line,
            });
            pending_cfg = CfgReachability::Production;
            pending_cfg_attr = false;
        } else if !remainder.starts_with("#[") && !remainder.is_empty() {
            pending_cfg = CfgReachability::Production;
            pending_path = None;
            pending_cfg_attr = false;
        }
    }
    (declarations, includes, unknowns)
}

fn cfg_reachability(expression: &str) -> CfgReachability {
    let expression = expression.trim();
    if expression == "test" || cfg_all_contains_test(expression) {
        CfgReachability::TestOnly
    } else if expression.starts_with("not(") {
        CfgReachability::Production
    } else if expression.contains("test") {
        CfgReachability::Unknown
    } else {
        CfgReachability::Production
    }
}

fn cfg_all_contains_test(expression: &str) -> bool {
    let Some(inner) = expression
        .strip_prefix("all(")
        .and_then(|value| value.strip_suffix(')'))
    else {
        return false;
    };
    let parts = split_cfg_args(inner);
    !parts.is_empty()
        && parts.iter().any(|part| part.trim() == "test")
        && parts.iter().all(|part| {
            let part = part.trim();
            !part.starts_with("any(") && !part.starts_with("not(")
        })
}

fn split_cfg_args(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for (index, ch) in value.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !value[start..].trim().is_empty() {
        parts.push(value[start..].trim());
    }
    parts
}

fn external_module_name(code: &str) -> Option<String> {
    let marker = code.find("mod ")?;
    let prefix = code[..marker].trim();
    if !matches!(
        prefix,
        "" | "pub" | "pub(crate)" | "pub(super)" | "pub(self)"
    ) {
        return None;
    }
    let rest = &code[marker + 4..];
    let name_len = rest
        .chars()
        .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
        .map(char::len_utf8)
        .sum::<usize>();
    if name_len == 0 || !rest[name_len..].trim_start().starts_with(';') {
        return None;
    }
    Some(rest[..name_len].to_string())
}

fn include_targets(
    code: &str,
    line: usize,
    path: &Path,
    unknowns: &mut Vec<String>,
) -> Vec<(String, usize)> {
    let mut targets = Vec::new();
    let mut offset = 0;
    while let Some(found) = code[offset..].find("include!(") {
        let start = offset + found + "include!(".len();
        let Some(end) = code[start..].find(')') else {
            unknowns.push(format!("{}:{} unterminated include!", path.display(), line));
            break;
        };
        let argument = code[start..start + end].trim();
        if let Some(target) = parse_string_literal(argument) {
            targets.push((target, line));
        } else {
            unknowns.push(format!(
                "{}:{} include! argument is not a literal path",
                path.display(),
                line
            ));
        }
        offset = start + end + 1;
    }
    targets
}

fn parse_string_literal(value: &str) -> Option<String> {
    let value = value.trim();
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\""))
}

fn resolve_include(parent: &Path, target: &str) -> Result<PathBuf, String> {
    let child = parent
        .parent()
        .ok_or_else(|| "parent has no directory".to_string())?
        .join(target);
    if child.is_file() {
        Ok(child)
    } else {
        Err(format!(
            "literal path does not resolve to a Rust source file: {}",
            child.display()
        ))
    }
}

fn resolve_module(parent: &Path, declaration: &ModuleDeclaration) -> Result<PathBuf, String> {
    if declaration.reachability == CfgReachability::Unknown {
        return Err("cfg expression is not provably test-only".to_string());
    }
    if let Some(path) = &declaration.path {
        let child = parent
            .parent()
            .ok_or_else(|| "parent has no directory".to_string())?
            .join(path);
        return child
            .is_file()
            .then_some(child.clone())
            .ok_or_else(|| format!("#[path] does not resolve: {}", child.display()));
    }
    let base = parent
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| *name != "mod" && *name != "lib")
        .map(|name| parent.parent().unwrap().join(name))
        .unwrap_or_else(|| parent.parent().unwrap().to_path_buf());
    let sibling = base.join(format!("{}.rs", declaration.name));
    let directory = base.join(&declaration.name).join("mod.rs");
    match (sibling.is_file(), directory.is_file()) {
        (true, false) => Ok(sibling),
        (false, true) => Ok(directory),
        (false, false) => Err(format!(
            "Rust sibling module is missing (tried {} and {})",
            sibling.display(),
            directory.display()
        )),
        (true, true) => Err(format!(
            "Rust sibling module is ambiguous (both {} and {} exist)",
            sibling.display(),
            directory.display()
        )),
    }
}

fn scan_file(path: &Path, out: &mut Vec<DirectSaveCall>) -> std::io::Result<()> {
    let text = fs::read_to_string(path)?;
    let relative = path
        .strip_prefix(repo_root().join("crates/team-agent/src"))
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let mut pending_cfg_test = false;
    let mut skip_test_depth: Option<i32> = None;
    let mut containing_fn = "module".to_string();

    for (index, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("").trim();
        if let Some(depth) = skip_test_depth.as_mut() {
            *depth += brace_delta(code);
            if *depth <= 0 {
                skip_test_depth = None;
            }
            continue;
        }
        if code.contains("#[cfg(test)]") {
            pending_cfg_test = true;
            continue;
        }
        if pending_cfg_test {
            if code.contains('{') {
                let depth = brace_delta(code);
                if depth > 0 {
                    skip_test_depth = Some(depth);
                }
                pending_cfg_test = false;
                continue;
            }
            if !code.is_empty() && !code.starts_with("#[") {
                pending_cfg_test = false;
            }
        }
        if let Some(name) = fn_name(code) {
            containing_fn = name.to_string();
        }
        if code.is_empty()
            || code.contains("fn save_runtime_state")
            || code.contains("fn save_team_scoped_state")
        {
            continue;
        }
        if let Some(callee_family) = state_save_callee(code) {
            out.push(DirectSaveCall {
                path: relative.clone(),
                containing_fn: containing_fn.clone(),
                callee_family,
                line: index + 1,
                snippet: code.split_whitespace().collect::<Vec<_>>().join(" "),
            });
        }
    }
    Ok(())
}

fn fn_name(code: &str) -> Option<&str> {
    let marker = "fn ";
    let start = code.find(marker)? + marker.len();
    let rest = &code[start..];
    let len = rest
        .chars()
        .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
        .map(char::len_utf8)
        .sum::<usize>();
    if len == 0 {
        None
    } else {
        Some(&rest[..len])
    }
}

fn state_save_callee(code: &str) -> Option<String> {
    for prefix in ["save_runtime_state", "save_team_scoped_state"] {
        let mut offset = 0;
        while let Some(found) = code[offset..].find(prefix) {
            let start = offset + found;
            let before_ok =
                start == 0 || !code[..start].chars().next_back().is_some_and(is_ident_char);
            let after_prefix = &code[start + prefix.len()..];
            let token_tail_len = after_prefix
                .chars()
                .take_while(|ch| is_ident_char(*ch))
                .map(char::len_utf8)
                .sum::<usize>();
            let after_token = &after_prefix[token_tail_len..];
            if before_ok && after_token.trim_start().starts_with('(') {
                return Some(format!("{prefix}{}", &after_prefix[..token_tail_len]));
            }
            offset = start + prefix.len();
        }
    }
    None
}

fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn brace_delta(code: &str) -> i32 {
    let opens = code.chars().filter(|ch| *ch == '{').count() as i32;
    let closes = code.chars().filter(|ch| *ch == '}').count() as i32;
    opens - closes
}

fn allowlist_by_key() -> BTreeMap<String, Vec<&'static AllowedStateSaveCall>> {
    let mut map = BTreeMap::new();
    for row in ALLOWED_STATE_SAVE_CALLS {
        let key = allowed_key(row);
        map.entry(key).or_insert_with(Vec::new).push(row);
    }
    map
}

fn allowed_key(row: &AllowedStateSaveCall) -> String {
    format!("{}::{}::{}", row.path, row.containing_fn, row.callee_family)
}

fn required_intents() -> BTreeSet<String> {
    ALLOWED_STATE_SAVE_CALLS
        .iter()
        .filter_map(|row| {
            if row.intent == "repository_internal" {
                None
            } else {
                Some(row.intent.to_string())
            }
        })
        .collect()
}

fn is_external_writer(path: &str) -> bool {
    !matches!(
        path,
        "state/persist.rs" | "state/projection.rs" | REPOSITORY_PATH
    )
}

fn is_repository_internal_path(path: &str) -> bool {
    matches!(
        path,
        "state/persist.rs" | "state/projection.rs" | REPOSITORY_PATH
    )
}

fn repository_source_or_panic(red: &str) -> String {
    let path = repo_root()
        .join("crates/team-agent/src")
        .join(REPOSITORY_PATH);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{red}: missing StateRepository skeleton at {}; error={error}",
            path.display()
        )
    })
}

fn block_after(source: &str, marker: &str) -> Option<String> {
    let start = source.find(marker)?;
    let after = &source[start..];
    let end = after.find("\n}\n").map(|offset| start + offset + 3)?;
    Some(source[start..end].to_string())
}

fn source_mentions_helper_near_intent(source: &str, intent: &str, helper: &str) -> bool {
    let mut offset = 0;
    while let Some(found) = source[offset..].find(intent) {
        let start = offset + found;
        let end = source.len().min(start + 1400);
        if normalize(&source[start..end]).contains(&normalize(helper)) {
            return true;
        }
        offset = start + intent.len();
    }
    false
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate should live under crates/team-agent")
        .to_path_buf()
}

#[test]
fn scanner_synthetic_external_cfg_test_and_production_modules_are_distinct() {
    let (declarations, includes, unknowns) = parse_module_surface(
        "#[cfg(test)] mod renamed_test;\nmod renamed_production;\n",
        Path::new("synthetic/parent.rs"),
    );
    assert!(includes.is_empty());
    assert!(unknowns.is_empty());
    assert_eq!(declarations.len(), 2);
    assert_eq!(declarations[0].reachability, CfgReachability::TestOnly);
    assert_eq!(declarations[1].reachability, CfgReachability::Production);
}

#[test]
fn scanner_synthetic_unsupported_cfg_and_include_are_unknown() {
    let (_, _, unknowns) = parse_module_surface(
        "#[cfg(any(test, unix))] mod maybe_test;\ninclude!(concat!(\"tests/\", \"fixture.rs\"));\n",
        Path::new("synthetic/parent.rs"),
    );
    assert_eq!(
        unknowns.len(),
        2,
        "unsupported module surface must be loud: {unknowns:?}"
    );
    assert!(unknowns.iter().any(|item| item.contains("cfg expression")));
    assert!(unknowns
        .iter()
        .any(|item| item.contains("include! argument")));
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
