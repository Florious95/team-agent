//! 0.5.62 P0 RED — an ambient tmux tuple is observation, not leader authority.
//!
//! User-visible contract:
//! - no ambient tuple selects the workspace-derived managed leader path;
//! - a complete tuple may select the direct provider path only when the caller
//!   controlling tty, live pane and requested workspace agree; redirected
//!   stdin is not an authority source;
//! - a live but foreign historical pane must fail loudly with the catalog
//!   reason whose required fact set matches what was actually observed, without
//!   starting a provider, switching to managed mode, or changing canonical
//!   state, leader registry or message store;
//! - the same workspace-mismatch facts and a copyable recovery action reach the
//!   independent `diagnose --json` and default `doctor --json` public callers;
//! - messages that failed in the attach window are either physically retried
//!   after attach or remain explicitly visible as attach-window debt.
//!
//! No real provider is started. The public CLI process runs against a PATH
//! shim, a deterministic pane/tty fixture, and a hermetic HOME.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use serial_test::serial;
use sha2::{Digest, Sha256};
use team_agent::message_store::MessageStore;
use team_agent::model::pane_authority_refusal as refusal_catalog;
use team_agent::state::persist::save_runtime_state;
use team_agent::transport::Transport;

const TEAM: &str = "pane-authority";
const AMBIENT_PANE: &str = "%ambient";
const GOOD_PANE: &str = "%good";
const RECOVERY_ACTION_DIRECTION_CANARY_ENV: &str = "TEAM_AGENT_TEST_BREAK_RECOVERY_ACTION_FOR";

#[test]
#[serial(env)]
fn a1_no_ambient_tuple_uses_workspace_derived_managed_path() {
    let case = Case::new("a1-no-ambient");
    case.set_mode("managed");

    let output = case.run(&["codex", "--json", "--", "--contract-canary"], None);
    let value = json_stdout("A1 managed launch", &output);
    let tmux_log = case.tmux_log();
    let expected_endpoint = team_agent::tmux_backend::TmuxBackend::for_workspace(&case.workspace)
        .tmux_endpoint()
        .expect("workspace tmux endpoint");

    assert_eq!(value["ok"], json!(true), "A1 output={value}");
    assert_eq!(
        value["mode"],
        json!("managed_tmux_client"),
        "ambient absence must select the managed path; output={value}"
    );
    assert!(
        tmux_log.contains(&expected_endpoint),
        "managed path must use the product's workspace-derived endpoint; \
         expected={expected_endpoint:?} tmux_log={tmux_log:?}"
    );
    assert!(
        contains_tmux_operation(&tmux_log, "new-session"),
        "positive control: managed path must really create its test-owned leader pane; \
         tmux_log={tmux_log:?}"
    );
}

#[test]
#[serial(env)]
fn a2_complete_matching_tuple_allows_direct_provider_path() {
    let case = Case::new("a2-matching");
    case.set_mode("matching");

    let output = case.run_with_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );
    let value = json_stdout("A2 matching ambient launch", &output);

    assert_eq!(value["ok"], json!(true), "A2 output={value}");
    assert_eq!(
        value["mode"],
        json!("exec_provider"),
        "a live pane whose tty and workspace match remains a legal direct-provider launch; \
         output={value}"
    );
    assert_eq!(
        case.provider_launches(),
        1,
        "positive control: the matching direct-provider path must actually reach the shim"
    );
    let state = fs::read_to_string(case.state_path()).expect("matching branch writes state");
    assert!(
        state.contains(AMBIENT_PANE),
        "matching branch must bind the verified ambient pane; state={state}"
    );
}

#[test]
#[serial(env)]
fn a2_redirected_stdin_does_not_hide_a_matching_controlling_tty() {
    let case = Case::new("a2-controlling-matches");
    case.set_mode("matching");

    let probe = case.run_with_distinct_controlling_and_stdin_ttys(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
        PaneTtySource::Controlling,
    );
    let value = json_stdout_even_on_error("A2 controlling tty authority", &probe.output);

    probe.assert_distinct_ttys_and_observed_controlling("A2 controlling authority positive");
    assert!(
        probe.output.status.success() && value["ok"] == json!(true),
        "A2 RED signature: matching controlling tty must be accepted even when stdin is another \
         tty; status={} output={value}",
        probe.output.status
    );
    assert_eq!(
        value["mode"],
        json!("exec_provider"),
        "a redirected stdin must not hide a matching process controlling tty; output={value}"
    );
    assert_eq!(
        case.provider_launches(),
        1,
        "positive control: matching controlling tty must reach the provider shim exactly once"
    );
}

#[test]
#[serial(env)]
fn a3_redirected_stdin_tty_cannot_impersonate_the_controlling_tty() {
    let case = Case::new("a3-stdin-cannot-impersonate");
    case.seed_preexisting_state_registry_and_store();
    let durable_before = case.durable_snapshot();
    assert!(
        durable_before.db.as_ref().is_some_and(|db| {
            db.user_version > 0
                && db
                    .row_counts
                    .iter()
                    .any(|(table, count)| table == "messages" && *count >= 1)
        }),
        "existing-store positive control: schema and at least one durable message row must exist"
    );
    case.set_mode("matching");

    let probe = case.run_with_distinct_controlling_and_stdin_ttys(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
        PaneTtySource::Stdin,
    );
    let value = json_stdout_even_on_error("A3 redirected stdin refusal", &probe.output);

    probe.assert_distinct_ttys_and_observed_controlling("A3 redirected stdin negative");
    assert_typed_prelaunch_refusal(
        &probe.output,
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneTtyMismatch,
        None,
        "A3 redirected stdin",
    );
    assert_tty_mismatch_facts(&value, "A3 redirected stdin");
    assert_no_provider_or_managed_spawn(&case, "A3 redirected stdin");
    case.assert_durable_unchanged(
        &durable_before,
        "A3 redirected stdin must be refused before state/registry/store access",
    );
}

#[test]
#[serial(env)]
fn durable_snapshot_distinguishes_read_only_from_same_byte_rewrite() {
    let case = Case::new("snapshot-three-state-canary");
    case.seed_preexisting_state_registry_and_store();
    let unopened = case.durable_snapshot();
    let state_path = case.state_path();
    let state_bytes = fs::read(&state_path).expect("read-only observation canary");
    let read_without_write = case.durable_snapshot();
    assert_eq!(
        read_without_write, unopened,
        "snapshot canary: read-only observation must not look like a durable write"
    );

    let replacement = state_path.with_extension("snapshot-canary-tmp");
    fs::write(&replacement, &state_bytes).expect("write same-byte replacement canary");
    fs::rename(&replacement, &state_path).expect("atomically replace state canary");
    let rewritten = case.durable_snapshot();
    assert_ne!(
        rewritten, unopened,
        "snapshot canary: a same-byte atomic rewrite must be detected by inode/times/tree facts"
    );
    let before_state = unopened
        .workspace_tree
        .iter()
        .find(|entry| entry.relative_path.ends_with("state.json"))
        .expect("state before rewrite");
    let after_state = rewritten
        .workspace_tree
        .iter()
        .find(|entry| entry.relative_path.ends_with("state.json"))
        .expect("state after rewrite");
    assert_eq!(
        (before_state.bytes.as_ref(), before_state.sha256),
        (after_state.bytes.as_ref(), after_state.sha256),
        "snapshot canary: content intentionally stays equal so metadata must expose the rewrite"
    );
    assert_ne!(
        before_state.inode, after_state.inode,
        "snapshot canary: atomic replacement must change the state inode"
    );
}

#[test]
#[serial(env)]
fn a3_missing_controlling_tty_never_falls_back_to_redirected_stdin() {
    let case = Case::new("a3-no-controlling-tty");
    let durable_before = case.durable_snapshot();
    assert!(
        !case.workspace.join(".team").exists(),
        "fresh-workspace positive control: no runtime/store directory may pre-exist"
    );
    case.set_mode("matching-slow");

    let probe = case.run_with_stdin_tty_but_no_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );
    let value = json_stdout_even_on_error("A3 missing controlling tty refusal", &probe.output);

    probe.assert_no_controlling_tty_observed("A3 missing controlling tty negative");
    assert_typed_prelaunch_refusal(
        &probe.output,
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::CallerControllingTtyUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::CallerControllingTty,
            refusal_catalog::CallerControllingTtyUnavailableCause::NoControllingTty.as_str(),
        )),
        "A3 missing controlling tty",
    );
    assert_no_provider_or_managed_spawn(&case, "A3 missing controlling tty");
    case.assert_durable_unchanged(
        &durable_before,
        "A3 missing controlling tty must not create state, registry, DB or store directories",
    );
    assert!(
        !case.workspace.join(".team").exists(),
        "A3 RED signature: a fresh invalid request must leave DB and its new store directory absent"
    );
}

#[test]
#[serial(env)]
fn a2_verified_ambient_authority_is_observed_once_and_then_reused() {
    let case = Case::new("a2-single-authority-observation");
    case.seed_preexisting_state_registry_and_store();
    case.set_mode("matching-then-foreign");

    let output = case.run_with_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );
    let value = json_stdout_even_on_error("A2 single authority observation", &output);
    let observation_count = case.list_panes_count();

    assert!(
        output.status.success() && value["ok"] == json!(true),
        "A2 RED signature: a verified ambient authority snapshot must survive later state \
         assembly without a second live observation; observations={observation_count} \
         status={} output={value}",
        output.status
    );
    assert_eq!(
        value["mode"],
        json!("exec_provider"),
        "the once-verified direct-provider branch must remain selected; output={value}"
    );
    assert_eq!(
        observation_count, 1,
        "A2 RED signature: authority must be observed once and the same verified snapshot reused"
    );
    assert_eq!(
        case.provider_launches(),
        1,
        "positive control: the single-observation legal branch must reach the provider shim"
    );
}

#[test]
#[serial(env)]
fn a3_foreign_live_tuple_fails_with_typed_reason_and_copyable_action() {
    let case = Case::new("a3-typed");
    case.set_mode("foreign-workspace");

    let output = case.run_with_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );
    let value = json_stdout_even_on_error("A3 foreign ambient refusal", &output);

    assert_typed_prelaunch_refusal(
        &output,
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        "A3 foreign ambient",
    );
    assert_workspace_mismatch_facts(&value, "A3 foreign ambient");
}

#[test]
#[serial(env)]
fn a3_missing_pane_id_keeps_its_catalog_reason_and_cause() {
    let missing_pane = Case::new("a3-missing-pane-id");
    missing_pane.set_mode("matching");
    let missing_pane_output = missing_pane.run_with_ambient_tuple(
        &["codex", "--json", "--", "--contract-canary"],
        Some(format!(
            "{},4242,0",
            missing_pane.endpoint.to_string_lossy()
        )),
        None,
    );
    let missing_pane_value =
        json_stdout_even_on_error("A3 missing TMUX_PANE", &missing_pane_output);
    assert_typed_prelaunch_refusal(
        &missing_pane_output,
        &missing_pane_value,
        refusal_catalog::PaneAuthorityRefusalReason::AmbientPaneIdUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::ObservedPaneId,
            refusal_catalog::AmbientPaneIdUnavailableCause::EnvironmentValueMissing.as_str(),
        )),
        "A3 missing TMUX_PANE",
    );
}

#[test]
#[serial(env)]
fn a3_malformed_tmux_tuple_keeps_its_catalog_reason_and_cause() {
    let malformed_tuple = Case::new("a3-malformed-tuple");
    malformed_tuple.set_mode("matching");
    let malformed_tuple_output = malformed_tuple.run_with_ambient_tuple(
        &["codex", "--json", "--", "--contract-canary"],
        Some("not-a-tmux-tuple".to_string()),
        Some(AMBIENT_PANE),
    );
    let malformed_tuple_value =
        json_stdout_even_on_error("A3 malformed TMUX tuple", &malformed_tuple_output);
    assert_typed_prelaunch_refusal(
        &malformed_tuple_output,
        &malformed_tuple_value,
        refusal_catalog::PaneAuthorityRefusalReason::AmbientTmuxEndpointUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::Endpoint,
            refusal_catalog::AmbientTmuxEndpointUnavailableCause::TmuxTupleFieldCountInvalid
                .as_str(),
        )),
        "A3 malformed TMUX tuple",
    );
}

#[test]
#[serial(env)]
fn a3_pane_query_failure_keeps_its_catalog_reason_and_cause() {
    let query_failed = Case::new("a3-pane-query-failed");
    query_failed.set_mode("query-failed");
    query_failed.make_tmux_query_unspawnable();
    let query_failed_output = query_failed.run_with_fake_bin_only(
        &["codex", "--json", "--", "--contract-canary"],
        Some(AMBIENT_PANE),
    );
    let query_failed_value =
        json_stdout_even_on_error("A3 pane query failed", &query_failed_output);
    assert_typed_prelaunch_refusal(
        &query_failed_output,
        &query_failed_value,
        refusal_catalog::PaneAuthorityRefusalReason::AmbientPaneWorkspaceUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::ObservedPaneWorkspace,
            refusal_catalog::AmbientPaneWorkspaceUnavailableCause::PaneQueryFailed.as_str(),
        )),
        "A3 pane query failed",
    );
}

#[test]
#[serial(env)]
fn a3_pane_not_found_keeps_its_catalog_reason_and_cause() {
    let pane_not_found = Case::new("a3-pane-not-found");
    pane_not_found.set_mode("pane-not-found");
    let pane_not_found_output = pane_not_found.run(
        &["codex", "--json", "--", "--contract-canary"],
        Some(AMBIENT_PANE),
    );
    let pane_not_found_value =
        json_stdout_even_on_error("A3 pane not found", &pane_not_found_output);
    assert_typed_prelaunch_refusal(
        &pane_not_found_output,
        &pane_not_found_value,
        refusal_catalog::PaneAuthorityRefusalReason::AmbientPaneWorkspaceUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::ObservedPaneWorkspace,
            refusal_catalog::AmbientPaneWorkspaceUnavailableCause::PaneNotFound.as_str(),
        )),
        "A3 pane not found",
    );
}

#[test]
#[serial(env)]
fn a3_missing_pane_current_path_keeps_its_catalog_reason_and_cause() {
    let current_path_missing = Case::new("a3-current-path-missing");
    current_path_missing.set_mode("current-path-missing");
    let current_path_missing_output = current_path_missing.run(
        &["codex", "--json", "--", "--contract-canary"],
        Some(AMBIENT_PANE),
    );
    let current_path_missing_value =
        json_stdout_even_on_error("A3 pane current_path missing", &current_path_missing_output);
    assert_typed_prelaunch_refusal(
        &current_path_missing_output,
        &current_path_missing_value,
        refusal_catalog::PaneAuthorityRefusalReason::AmbientPaneWorkspaceUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::ObservedPaneWorkspace,
            refusal_catalog::AmbientPaneWorkspaceUnavailableCause::CurrentPathMissing.as_str(),
        )),
        "A3 pane current_path missing",
    );
}

#[test]
fn refusal_signature_normalization_keeps_known_different_causes_distinct() {
    let unavailable = json!({
        (refusal_catalog::REASON_FIELD):
            refusal_catalog::PaneAuthorityRefusalReason::CallerControllingTtyUnavailable.as_str(),
        (refusal_catalog::PaneAuthorityRefusalField::CallerControllingTty.as_str()): {
            (refusal_catalog::AVAILABILITY_FIELD):
                refusal_catalog::PaneAuthorityFactAvailability::Unavailable.as_str(),
            (refusal_catalog::CAUSE_FIELD):
                refusal_catalog::CallerControllingTtyUnavailableCause::NoControllingTty.as_str(),
        },
    });
    let mismatch = json!({
        (refusal_catalog::REASON_FIELD):
            refusal_catalog::PaneAuthorityRefusalReason::PaneTtyMismatch.as_str(),
        (refusal_catalog::PaneAuthorityRefusalField::CallerControllingTty.as_str()): 101,
        (refusal_catalog::PaneAuthorityRefusalField::ObservedPaneTty.as_str()): 202,
    });
    let unavailable_signature =
        refusal_signature("catalog_payload", Some(1), &unavailable).expect("unavailable signature");
    let mismatch_signature =
        refusal_signature("catalog_payload", Some(1), &mismatch).expect("mismatch signature");

    eprintln!(
        "normalization direction canary raw: unavailable_input={unavailable} \
         unavailable_output={unavailable_signature:?} mismatch_input={mismatch} \
         mismatch_output={mismatch_signature:?}"
    );
    assert_ne!(
        unavailable_signature, mismatch_signature,
        "normalization direction canary: a lossy signature must retain reason identity and field \
         identity, so CallerControllingTtyUnavailable cannot collapse into PaneTtyMismatch; \
         unavailable_input={unavailable} unavailable_output={unavailable_signature:?} \
         mismatch_input={mismatch} mismatch_output={mismatch_signature:?}"
    );
}

#[test]
#[serial(env)]
fn a3_foreign_live_tuple_spawns_neither_provider_nor_managed_leader() {
    let case = Case::new("a3-zero-spawn");
    case.set_mode("foreign-workspace");

    let _ = case.run_with_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );
    let tmux_log = case.tmux_log();

    assert_eq!(
        case.provider_launches(),
        0,
        "A3 RED signature: authority refusal must happen before provider spawn"
    );
    for forbidden in [
        "new-session",
        "new-window",
        "attach-session",
        "switch-client",
    ] {
        assert!(
            !contains_tmux_operation(&tmux_log, forbidden),
            "A3 RED signature: a foreign tuple must not silently switch to Managed; \
             forbidden={forbidden} tmux_log={tmux_log:?}"
        );
    }
}

#[test]
#[serial(env)]
fn a3_foreign_live_tuple_leaves_state_and_leader_registry_byte_stable() {
    let case = Case::new("a3-zero-state");
    case.seed_preexisting_state_and_registry();
    case.set_mode("foreign-workspace");
    let state_before = fs::read(case.state_path()).expect("positive control: preexisting state");
    let registry_before = case.env.registry_entries();
    assert!(
        !state_before.is_empty() && !registry_before.is_empty(),
        "positive control: zero-write check must begin with both state and registry inventory"
    );

    let _ = case.run_with_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );

    let state_changed = fs::read(case.state_path()).ok().as_deref() != Some(&state_before);
    let registry_changed = case.env.registry_entries() != registry_before;
    assert!(
        !state_changed && !registry_changed,
        "A3 RED signature: failed ambient authority must leave both .team/runtime/state.json \
         and the complete host leader registry byte-stable; \
         state_changed={state_changed} registry_changed={registry_changed}"
    );
}

#[test]
#[serial(env)]
fn b1_send_surfaces_typed_workspace_mismatch_and_recovery_action() {
    let case = Case::new("b1-send");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");

    let output = case.run(
        &[
            "send",
            "leader",
            "pane authority send canary",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        None,
    );
    let value = json_stdout_even_on_error("B1 send refusal", &output);

    assert!(
        has_typed_reason(&value, "PaneWorkspaceMismatch"),
        "B1 RED signature: send must preserve the known typed cause instead of collapsing it \
         to leader_not_attached; output={value}"
    );
    assert!(
        has_copyable_recovery_action(&value),
        "B1 RED signature: send must carry a copyable attach-leader/takeover action; output={value}"
    );
}

#[test]
#[serial(env)]
fn b2_diagnose_checks_live_workspace_even_when_state_says_attached() {
    let case = Case::new("b2-diagnose");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");

    let output = case.run(
        &[
            "diagnose",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        None,
    );
    let value = json_stdout_even_on_error("B2 diagnose", &output);

    assert_catalog_refusal_payload(
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        "B2 diagnose public surface",
    );
    assert_workspace_mismatch_facts(&value, "B2 diagnose public surface");
}

#[test]
#[serial(env)]
fn b2_doctor_independently_checks_live_workspace_even_when_state_says_attached() {
    let case = Case::new("b2-doctor");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");

    let output = case.run(
        &[
            "doctor",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        None,
    );
    let value = json_stdout_even_on_error("B2 doctor", &output);

    assert_catalog_refusal_payload(
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        "B2 doctor public surface",
    );
    assert_workspace_mismatch_facts(&value, "B2 doctor public surface");
}

#[test]
#[serial(env)]
fn b2_diagnose_single_snapshot_rejects_reread_race() {
    assert_single_snapshot_rejects_reread_race(
        RecoverySurface::Diagnose,
        "b2-diagnose-snapshot-race",
    );
}

#[test]
#[serial(env)]
fn b2_doctor_single_snapshot_rejects_reread_race() {
    assert_single_snapshot_rejects_reread_race(RecoverySurface::Doctor, "b2-doctor-snapshot-race");
}

#[test]
#[serial(env)]
fn b2_diagnose_single_snapshot_survives_later_read_failure() {
    assert_single_snapshot_survives_later_read_failure(
        RecoverySurface::Diagnose,
        "b2-diagnose-snapshot-failure",
    );
}

#[test]
#[serial(env)]
fn b2_doctor_single_snapshot_survives_later_read_failure() {
    assert_single_snapshot_survives_later_read_failure(
        RecoverySurface::Doctor,
        "b2-doctor-snapshot-failure",
    );
}

fn assert_single_snapshot_rejects_reread_race(surface: RecoverySurface, tag: &str) {
    let case = Case::new(tag);
    case.seed_foreign_attached_state();
    case.set_mode("snapshot-reread-changes");

    let output = surface.invoke(&case, AMBIENT_PANE);
    let value = json_stdout_even_on_error(
        &format!("{} single-snapshot reread race", surface.name()),
        &output,
    );
    let reads = case.workspace_observation_trace();
    assert_eq!(
        reads.len(),
        1,
        "{} SNAPSHOT_MODE1_REREAD_RACE RED signature: the public presenter must consume the \
         mismatch snapshot instead of observing pane workspace again; \
         workspace_observation_read_count={} expected=1 trace={reads:?} output={value}",
        surface.name(),
        reads.len()
    );
    assert_catalog_refusal_payload(
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        &format!("{} single-snapshot reread-race payload", surface.name()),
    );
    assert_workspace_mismatch_facts_match_case(
        &value,
        &case,
        &format!("{} single-snapshot reread-race payload", surface.name()),
    );
}

fn assert_single_snapshot_survives_later_read_failure(surface: RecoverySurface, tag: &str) {
    let case = Case::new(tag);
    case.seed_foreign_attached_state();
    case.set_mode("snapshot-reread-fails");

    let output = surface.invoke(&case, AMBIENT_PANE);
    let value = json_stdout_even_on_error(
        &format!("{} single-snapshot later-read failure", surface.name()),
        &output,
    );
    let reads = case.workspace_observation_trace();
    let payload_present = find_reason_object(
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
    )
    .is_some();
    assert!(
        reads.len() == 1 && payload_present,
        "{} SNAPSHOT_MODE2_RESOLVED_MISMATCH_SURVIVES_REREAD_FAILURE RED signature: a failed \
         presenter reread must not erase the already-observed mismatch payload; \
         workspace_observation_read_count={} expected=1 payload_present={payload_present} \
         trace={reads:?} output={value}",
        surface.name(),
        reads.len()
    );
    assert_catalog_refusal_payload(
        &value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        &format!("{} single-snapshot later-failure payload", surface.name()),
    );
    assert_workspace_mismatch_facts_match_case(
        &value,
        &case,
        &format!("{} single-snapshot later-failure payload", surface.name()),
    );
}

#[test]
#[serial(env)]
fn b2_single_snapshot_fixture_distinguishes_reread_race_and_later_failure() {
    let race = Case::new("b2-snapshot-race-canary");
    race.set_mode("snapshot-reread-changes");
    let race_first = race.run_tmux_shim(&["list-panes"]);
    let race_second = race.run_tmux_shim(&[
        "display-message",
        "-p",
        "-t",
        AMBIENT_PANE,
        "#{pane_current_path}",
    ]);
    assert!(
        race_first.status.success()
            && String::from_utf8_lossy(&race_first.stdout)
                .contains(&race.foreign_workspace.to_string_lossy().into_owned()),
        "snapshot fixture canary: mode1 first observation must be a real foreign-workspace \
         mismatch; stdout={} stderr={}",
        String::from_utf8_lossy(&race_first.stdout),
        String::from_utf8_lossy(&race_first.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&race_second.stdout).trim(),
        race.workspace_str(),
        "snapshot fixture canary: mode1 second observation must succeed with the changed matching \
         workspace"
    );
    assert_eq!(
        race.workspace_observation_trace().len(),
        2,
        "snapshot fixture canary: mode1 measurement must distinguish both observations"
    );

    let failure = Case::new("b2-snapshot-failure-canary");
    failure.set_mode("snapshot-reread-fails");
    let failure_first = failure.run_tmux_shim(&["list-panes"]);
    let failure_second = failure.run_tmux_shim(&[
        "display-message",
        "-p",
        "-t",
        AMBIENT_PANE,
        "#{pane_current_path}",
    ]);
    let failure_fallback = failure.run_tmux_shim(&["list-panes"]);
    assert!(
        failure_first.status.success()
            && String::from_utf8_lossy(&failure_first.stdout)
                .contains(&failure.foreign_workspace.to_string_lossy().into_owned()),
        "snapshot fixture canary: mode2 first observation must be a real foreign-workspace \
         mismatch; stdout={} stderr={}",
        String::from_utf8_lossy(&failure_first.stdout),
        String::from_utf8_lossy(&failure_first.stderr)
    );
    assert!(
        !failure_second.status.success() && !failure_fallback.status.success(),
        "snapshot fixture canary: mode2 later PaneCurrentPath and fallback list_targets reads \
         must both fail; query_status={} fallback_status={}",
        failure_second.status,
        failure_fallback.status
    );
    assert_eq!(
        failure.workspace_observation_trace().len(),
        3,
        "snapshot fixture canary: mode2 measurement must record the first observation plus both \
         forbidden later attempts"
    );
}

#[test]
#[serial(env)]
fn b2_matching_workspace_is_not_misdiagnosed_by_diagnose_or_doctor() {
    let case = Case::new("b2-public-positive");
    case.seed_foreign_attached_state();
    case.set_mode("matching");

    for (surface, args) in [
        (
            "diagnose",
            vec![
                "diagnose",
                "--workspace",
                case.workspace_str(),
                "--team",
                TEAM,
                "--json",
            ],
        ),
        (
            "doctor",
            vec![
                "doctor",
                "--workspace",
                case.workspace_str(),
                "--team",
                TEAM,
                "--json",
            ],
        ),
    ] {
        let output = case.run(&args, None);
        let value = json_stdout_even_on_error(surface, &output);
        assert_no_catalog_refusal(
            &value,
            &format!(
                "B2 positive control: a matching live workspace must not be reported by {surface}"
            ),
        );
    }
}

#[test]
#[serial(env)]
fn b3_recovery_action_removes_the_same_typed_error() {
    let case = Case::new("b3-action");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");
    let before = case.send_canary("before recovery");
    assert!(
        has_copyable_recovery_action(&before),
        "positive-control precondition: mismatch output must advertise a supported recovery action; \
         output={before}"
    );
    let recovery_argv = copyable_recovery_command(&before).unwrap_or_else(|| {
        panic!(
            "B3 RED signature: recovery guidance must contain a directly executable \
             team-agent attach-leader/takeover command; output={before}"
        )
    });
    let recovery_args = recovery_argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .collect::<Vec<_>>();

    case.set_mode("recovery");
    let attach = case.run(&recovery_args, Some(GOOD_PANE));
    assert!(
        attach.status.success(),
        "B3 RED signature: copying the advertised recovery command in a corrected leader \
         terminal must succeed; command={recovery_argv:?} status={} stdout={} stderr={}",
        attach.status,
        String::from_utf8_lossy(&attach.stdout),
        String::from_utf8_lossy(&attach.stderr)
    );

    let diagnose = case.run(
        &[
            "diagnose",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        Some(GOOD_PANE),
    );
    let diagnose_value = json_stdout_even_on_error("B3 post-action diagnose", &diagnose);
    assert!(
        !json_contains_string(&diagnose_value, "PaneWorkspaceMismatch"),
        "copying the suggested action must remove the original typed error; \
         diagnose={diagnose_value}"
    );
}

#[test]
#[serial(env)]
fn b3_per_surface_launcher_recovery_action_closes_independently() {
    assert_independent_surface_recovery_action_closes(RecoverySurface::Launcher);
}

#[test]
#[serial(env)]
fn b3_per_surface_diagnose_recovery_action_closes_independently() {
    assert_independent_surface_recovery_action_closes(RecoverySurface::Diagnose);
}

#[test]
#[serial(env)]
fn b3_per_surface_doctor_recovery_action_closes_independently() {
    assert_independent_surface_recovery_action_closes(RecoverySurface::Doctor);
}

// Per-surface expected state matrix:
// - A+B2 product tip: launcher=GREEN, diagnose=GREEN, doctor=GREEN.
// - A-only product tip: launcher=GREEN; diagnose=RED and doctor=RED at
//   `<surface> independent recovery precondition: public surface must expose
//   catalog reason PaneWorkspaceMismatch` because the B2 projection is absent.
// - B2-only product tip: launcher=RED at `launcher independent recovery
//   precondition: public surface must expose catalog reason
//   PaneWorkspaceMismatch` because the A projection is absent; diagnose=GREEN
//   and doctor=GREEN.
// - Catalog-only boundary: all three are RED at that same surface-local
//   independent recovery precondition.
//
// Direction canary protocol: record the three baseline states and RED
// signatures, then set RECOVERY_ACTION_DIRECTION_CANARY_ENV to one surface.
// The selected surface must turn RED from its owning product tip; both
// non-selected surfaces must retain their baseline state and, when RED, their
// exact reason/assertion-node/exit-code/field-identity signature.
// The selected mutation RED signature is `<surface> independent recovery
// precondition: catalog reason PaneWorkspaceMismatch must have an executable
// recovery projection`.
fn assert_independent_surface_recovery_action_closes(surface: RecoverySurface) {
    let case = Case::new(surface.independent_tag());
    case.seed_foreign_attached_state();
    case.set_mode("foreign-workspace");

    let before = surface.invoke(&case, AMBIENT_PANE);
    let mut before_value =
        json_stdout_even_on_error(&format!("{} before recovery", surface.name()), &before);
    apply_surface_local_recovery_action_canary(surface, &mut before_value);
    assert_catalog_refusal_payload(
        &before_value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        &format!("{} independent recovery precondition", surface.name()),
    );
    let recovery_argv = copyable_recovery_command(&before_value).unwrap_or_else(|| {
        panic!(
            "{} RED signature: its own refusal must contain an executable catalog recovery \
             command; output={before_value}",
            surface.name()
        )
    });
    let recovery_args = recovery_argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .collect::<Vec<_>>();

    case.set_mode("recovery");
    let attach = case.run(&recovery_args, Some(GOOD_PANE));
    assert!(
        attach.status.success(),
        "{} RED signature: copying its advertised action after correcting the terminal/pane \
         context must succeed; command={recovery_argv:?} status={} stdout={} stderr={}",
        surface.name(),
        attach.status,
        String::from_utf8_lossy(&attach.stdout),
        String::from_utf8_lossy(&attach.stderr)
    );

    let after = surface.invoke(&case, GOOD_PANE);
    let after_value =
        json_stdout_even_on_error(&format!("{} after recovery", surface.name()), &after);
    assert_no_reason(
        &after_value,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        &format!(
            "{} RED signature: the copied action plus corrected context must remove the \
             original refusal",
            surface.name()
        ),
    );
}

#[test]
#[serial(env)]
// INTEGRATION-TIP TOOTH: intentionally retains the cross-surface consistency
// check. Its prerequisite is an integration tip with the A launcher product
// present; it is not used to attribute an A-only or B2-only slice failure.
fn b3_each_public_surface_recovery_action_closes_its_original_refusal() {
    for surface in [
        RecoverySurface::Launcher,
        RecoverySurface::Diagnose,
        RecoverySurface::Doctor,
    ] {
        let case = Case::new(surface.tag());
        case.seed_foreign_attached_state();
        case.set_mode("foreign-workspace");

        let before = surface.invoke(&case, AMBIENT_PANE);
        let before_value =
            json_stdout_even_on_error(&format!("{} before recovery", surface.name()), &before);
        assert_catalog_refusal_payload(
            &before_value,
            refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
            None,
            &format!("{} recovery precondition", surface.name()),
        );
        let recovery_argv = copyable_recovery_command(&before_value).unwrap_or_else(|| {
            panic!(
                "{} RED signature: its own refusal must contain an executable catalog recovery \
                 command; output={before_value}",
                surface.name()
            )
        });
        let recovery_args = recovery_argv
            .iter()
            .skip(1)
            .map(String::as_str)
            .collect::<Vec<_>>();

        case.set_mode("recovery");
        let attach = case.run(&recovery_args, Some(GOOD_PANE));
        assert!(
            attach.status.success(),
            "{} RED signature: copying its advertised action after correcting the terminal/pane \
             context must succeed; command={recovery_argv:?} status={} stdout={} stderr={}",
            surface.name(),
            attach.status,
            String::from_utf8_lossy(&attach.stdout),
            String::from_utf8_lossy(&attach.stderr)
        );

        let after = surface.invoke(&case, GOOD_PANE);
        let after_value =
            json_stdout_even_on_error(&format!("{} after recovery", surface.name()), &after);
        assert_no_reason(
            &after_value,
            refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
            &format!(
                "{} RED signature: the copied action plus corrected context must remove the \
                 original refusal",
                surface.name()
            ),
        );
    }
}

#[test]
#[serial(env)]
fn c_attach_window_failures_are_retried_or_remain_user_visible() {
    let case = Case::new("c-attach-window");
    case.seed_foreign_attached_state();
    let message_ids = case.seed_attach_window_failures();
    let attempts_before = message_ids
        .iter()
        .map(|id| case.message_attempts(id))
        .collect::<Vec<_>>();

    case.set_mode("recovery");
    let attach = case.run(
        &[
            "attach-leader",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--pane",
            GOOD_PANE,
            "--provider",
            "codex",
            "--confirm",
            "--json",
        ],
        Some(GOOD_PANE),
    );
    let attach_value = json_stdout_even_on_error("C attach", &attach);
    assert_eq!(
        attach_value["ok"],
        json!(true),
        "C precondition: leader attach must complete; output={attach_value}"
    );

    let physically_retried = message_ids.iter().enumerate().all(|(index, id)| {
        let row = case.message_row(id);
        row.attempts > attempts_before[index]
            && matches!(
                row.status.as_str(),
                "submitted_pending_acceptance"
                    | "submitted_awaiting_receipt"
                    | "submitted_unverified"
                    | "visible"
                    | "delivered"
                    | "acknowledged"
            )
    });
    let status = case.run(
        &[
            "status",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
            "--detail",
        ],
        Some(GOOD_PANE),
    );
    let diagnose = case.run(
        &[
            "diagnose",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        Some(GOOD_PANE),
    );
    let status_value = json_stdout_even_on_error("C status", &status);
    let diagnose_value = json_stdout_even_on_error("C diagnose", &diagnose);
    let visible_debt = attach_window_debt_visible(
        &[&attach_value, &status_value, &diagnose_value],
        &message_ids,
    );

    assert!(
        physically_retried || visible_debt,
        "C RED signature: after attach, messages that failed in the attach window must either \
         cross a new physical-attempt boundary or remain user-visible as N=2 attach-window debt; \
         rows={:?} attach={attach_value} status={status_value} diagnose={diagnose_value}",
        message_ids
            .iter()
            .map(|id| case.message_row(id))
            .collect::<Vec<_>>()
    );
}

#[derive(Clone, Copy)]
enum RecoverySurface {
    Launcher,
    Diagnose,
    Doctor,
}

impl RecoverySurface {
    const ALL: [Self; 3] = [Self::Launcher, Self::Diagnose, Self::Doctor];

    const fn name(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Diagnose => "diagnose --json",
            Self::Doctor => "doctor --json",
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Launcher => "b3-launcher-action",
            Self::Diagnose => "b3-diagnose-action",
            Self::Doctor => "b3-doctor-action",
        }
    }

    const fn independent_tag(self) -> &'static str {
        match self {
            Self::Launcher => "b3-independent-launcher-action",
            Self::Diagnose => "b3-independent-diagnose-action",
            Self::Doctor => "b3-independent-doctor-action",
        }
    }

    const fn canary_key(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Diagnose => "diagnose",
            Self::Doctor => "doctor",
        }
    }

    fn invoke(self, case: &Case, pane: &str) -> Output {
        match self {
            Self::Launcher => {
                case.run_with_controlling_tty(&["codex", "--json", "--", "--contract-canary"], pane)
            }
            Self::Diagnose => case.run(
                &[
                    "diagnose",
                    "--workspace",
                    case.workspace_str(),
                    "--team",
                    TEAM,
                    "--json",
                ],
                Some(pane),
            ),
            Self::Doctor => case.run(
                &[
                    "doctor",
                    "--workspace",
                    case.workspace_str(),
                    "--team",
                    TEAM,
                    "--json",
                ],
                Some(pane),
            ),
        }
    }
}

fn apply_surface_local_recovery_action_canary(surface: RecoverySurface, value: &mut Value) {
    let Some(target) = std::env::var_os(RECOVERY_ACTION_DIRECTION_CANARY_ENV) else {
        return;
    };
    let target = target.to_string_lossy();
    assert!(
        RecoverySurface::ALL
            .iter()
            .any(|surface| surface.canary_key() == target),
        "direction canary target must name exactly one public surface; \
         env={RECOVERY_ACTION_DIRECTION_CANARY_ENV} value={target}"
    );
    if surface.canary_key() != target {
        return;
    }

    let removed = strip_workspace_mismatch_recovery_action(value);
    assert!(
        removed > 0,
        "{} direction canary setup must remove a real recovery action before the target \
         assertion; output={value}",
        surface.name()
    );
}

fn strip_workspace_mismatch_recovery_action(value: &mut Value) -> usize {
    match value {
        Value::Array(values) => values
            .iter_mut()
            .map(strip_workspace_mismatch_recovery_action)
            .sum(),
        Value::Object(object) => {
            let is_workspace_mismatch = object
                .get(refusal_catalog::REASON_FIELD)
                .and_then(Value::as_str)
                == Some(
                    refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch.as_str(),
                );
            let mut removed = 0;
            if is_workspace_mismatch {
                removed += usize::from(object.remove(refusal_catalog::ACTION_FIELD).is_some());
                removed += usize::from(object.remove(refusal_catalog::HINT_ACTION_FIELD).is_some());
            }
            removed
                + object
                    .values_mut()
                    .map(strip_workspace_mismatch_recovery_action)
                    .sum::<usize>()
        }
        _ => 0,
    }
}

struct Case {
    _endpoint_fixture: UnixSocketFixture,
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    foreign_workspace: PathBuf,
    fake_bin: PathBuf,
    endpoint: PathBuf,
    mode_path: PathBuf,
    tmux_log_path: PathBuf,
    provider_launch_log_path: PathBuf,
    pane_capture_path: PathBuf,
    workspace_string: String,
}

impl Case {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace("requested");
        let foreign_workspace = env.workspace("historical-foreign");
        let fake_bin = env.root().join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        let endpoint = hermetic_guard::short_tmux_socket(tag);
        let endpoint_fixture = UnixSocketFixture::bind(&endpoint);
        let mode_path = env.root().join("pane-mode");
        let tmux_log_path = env.root().join("tmux.log");
        let provider_launch_log_path = env.root().join("provider-launch.log");
        let pane_capture_path = env.root().join("pane-capture");
        let workspace_string = workspace.to_string_lossy().into_owned();

        write_executable(&fake_bin.join("tmux"), TMUX_SHIM);
        write_executable(&fake_bin.join("codex"), PROVIDER_SHIM);
        Self {
            _endpoint_fixture: endpoint_fixture,
            env,
            workspace,
            foreign_workspace,
            fake_bin,
            endpoint,
            mode_path,
            tmux_log_path,
            provider_launch_log_path,
            pane_capture_path,
            workspace_string,
        }
    }

    fn workspace_str(&self) -> &str {
        &self.workspace_string
    }

    fn state_path(&self) -> PathBuf {
        self.workspace.join(".team/runtime/state.json")
    }

    fn spawn_session_path(&self) -> PathBuf {
        self.env.root().join("spawn-session")
    }

    fn list_panes_count_path(&self) -> PathBuf {
        self.env.root().join("list-panes-count")
    }

    fn db_path(&self) -> PathBuf {
        self.workspace.join(".team/runtime/team.db")
    }

    fn set_mode(&self, mode: &str) {
        fs::write(&self.mode_path, mode).expect("set pane fixture mode");
    }

    fn make_tmux_query_unspawnable(&self) {
        write_executable(
            &self.fake_bin.join("tmux"),
            "#!/definitely/not/a/real/interpreter\n",
        );
    }

    fn run(&self, args: &[&str], ambient_pane: Option<&str>) -> Output {
        let mut command = self.command(args, ambient_pane);
        command.output().expect("run team-agent CLI")
    }

    fn run_tmux_shim(&self, args: &[&str]) -> Output {
        let mut command = self.command_for_program(&self.fake_bin.join("tmux"), args, None);
        command.output().expect("run tmux observation fixture")
    }

    fn run_with_fake_bin_only(&self, args: &[&str], ambient_pane: Option<&str>) -> Output {
        let mut command = self.command(args, ambient_pane);
        command.env("PATH", &self.fake_bin);
        command
            .output()
            .expect("run team-agent CLI with isolated failing tmux binary")
    }

    fn run_with_ambient_tuple(
        &self,
        args: &[&str],
        tmux: Option<String>,
        ambient_pane: Option<&str>,
    ) -> Output {
        let mut command =
            self.command_for_program(Path::new(env!("CARGO_BIN_EXE_team-agent")), args, None);
        if let Some(tmux) = tmux {
            command.env("TMUX", tmux);
        }
        if let Some(pane) = ambient_pane {
            command.env("TMUX_PANE", pane);
        }
        command
            .output()
            .expect("run team-agent CLI with raw ambient tuple")
    }

    fn run_with_controlling_tty(&self, args: &[&str], ambient_pane: &str) -> Output {
        let (master, slave, tty) = open_pty().expect("allocate controlling tty");
        let expected_tdev = fd_rdev(slave.as_raw_fd()).expect("measure controlling slave rdev");
        let measurement_path = self
            .env
            .root()
            .join(format!("tty-measurement-{}", self.list_panes_count()));
        let measurement_file =
            measurement_file(&measurement_path).expect("create controlling-tty measurement file");
        let measurement_fd = measurement_file.as_raw_fd();
        let mut command = self.command(args, Some(ambient_pane));
        command
            .env("TEAM_AGENT_TEST_PANE_TTY", tty)
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                write_final_process_tty_measurement(measurement_fd, expected_tdev, expected_tdev)?;
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn team-agent in pty");
        let output = child.wait_with_output().expect("wait for pty child");
        drop(master);
        drop(measurement_file);
        let measurement = read_tty_measurement(&measurement_path);
        measurement.assert_controlling_tty(expected_tdev, expected_tdev, "matching tty fixture");
        output
    }

    fn run_with_distinct_controlling_and_stdin_ttys(
        &self,
        args: &[&str],
        ambient_pane: &str,
        pane_tty_source: PaneTtySource,
    ) -> TtyProbe {
        let (controlling_master, controlling_slave, controlling_tty) =
            open_pty().expect("allocate controlling tty");
        let (stdin_master, stdin_slave, stdin_tty) =
            open_pty().expect("allocate redirected stdin tty");
        let controlling_fd = controlling_slave.as_raw_fd();
        let expected_tdev =
            fd_rdev(controlling_fd).expect("measure distinct controlling slave rdev");
        let stdin_rdev =
            fd_rdev(stdin_slave.as_raw_fd()).expect("measure redirected stdin slave rdev");
        let measurement_path = self.env.root().join("distinct-tty-measurement");
        let measurement_file =
            measurement_file(&measurement_path).expect("create distinct-tty measurement file");
        let measurement_fd = measurement_file.as_raw_fd();
        let pane_tty = match pane_tty_source {
            PaneTtySource::Controlling => &controlling_tty,
            PaneTtySource::Stdin => &stdin_tty,
        };
        let mut command = self.command(args, Some(ambient_pane));
        command
            .env("TEAM_AGENT_TEST_PANE_TTY", pane_tty)
            .stdin(Stdio::from(stdin_slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(controlling_fd, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                write_final_process_tty_measurement(measurement_fd, expected_tdev, stdin_rdev)?;
                Ok(())
            });
        }
        let child = command
            .spawn()
            .expect("spawn with distinct controlling and stdin ttys");
        let output = child
            .wait_with_output()
            .expect("wait for tty topology probe");
        drop(measurement_file);
        let measurement = read_tty_measurement(&measurement_path);
        drop((controlling_master, controlling_slave, stdin_master));
        TtyProbe {
            output,
            controlling_tty: Some(controlling_tty),
            stdin_tty,
            measurement,
        }
    }

    fn run_with_stdin_tty_but_no_controlling_tty(
        &self,
        args: &[&str],
        ambient_pane: &str,
    ) -> TtyProbe {
        let (stdin_master, stdin_slave, stdin_tty) =
            open_pty().expect("allocate redirected stdin tty");
        let stdin_rdev =
            fd_rdev(stdin_slave.as_raw_fd()).expect("measure no-control stdin slave rdev");
        let measurement_path = self.env.root().join("no-controlling-tty-measurement");
        let measurement_file =
            measurement_file(&measurement_path).expect("create no-control measurement file");
        let measurement_fd = measurement_file.as_raw_fd();
        let mut command = self.command(args, Some(ambient_pane));
        command
            .env("TEAM_AGENT_TEST_PANE_TTY", &stdin_tty)
            .stdin(Stdio::from(stdin_slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                write_final_process_tty_measurement(measurement_fd, u64::MAX, stdin_rdev)?;
                Ok(())
            });
        }
        let child = command
            .spawn()
            .expect("spawn with redirected stdin and no controlling tty");
        let output = child
            .wait_with_output()
            .expect("wait for no-controlling-tty probe");
        drop(measurement_file);
        let measurement = read_tty_measurement(&measurement_path);
        drop(stdin_master);
        TtyProbe {
            output,
            controlling_tty: None,
            stdin_tty,
            measurement,
        }
    }

    fn command(&self, args: &[&str], ambient_pane: Option<&str>) -> Command {
        self.command_for_program(
            Path::new(env!("CARGO_BIN_EXE_team-agent")),
            args,
            ambient_pane,
        )
    }

    fn command_for_program(
        &self,
        program: &Path,
        args: &[&str],
        ambient_pane: Option<&str>,
    ) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self.env.home())
            .env("PATH", self.test_path())
            .env("TEAM_AGENT_TEST_TMUX_LOG", &self.tmux_log_path)
            .env(
                "TEAM_AGENT_TEST_PROVIDER_LAUNCH_LOG",
                &self.provider_launch_log_path,
            )
            .env("TEAM_AGENT_TEST_PANE_MODE_FILE", &self.mode_path)
            .env("TEAM_AGENT_TEST_REQUESTED_WORKSPACE", &self.workspace)
            .env("TEAM_AGENT_TEST_FOREIGN_WORKSPACE", &self.foreign_workspace)
            .env("TEAM_AGENT_TEST_PANE_CAPTURE", &self.pane_capture_path)
            .env("TEAM_AGENT_TEST_SPAWN_SESSION", self.spawn_session_path())
            .env(
                "TEAM_AGENT_TEST_LIST_PANES_COUNT",
                self.list_panes_count_path(),
            )
            .env("TEAM_AGENT_TEST_PANE_TTY", "/dev/ttys-good");
        for key in hermetic_guard::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        if let Some(pane) = ambient_pane {
            command
                .env(
                    "TMUX",
                    format!("{},4242,0", self.endpoint.to_string_lossy()),
                )
                .env("TMUX_PANE", pane);
        }
        command
    }

    fn test_path(&self) -> String {
        format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn tmux_log(&self) -> String {
        fs::read_to_string(&self.tmux_log_path).unwrap_or_default()
    }

    fn workspace_observation_trace(&self) -> Vec<String> {
        self.tmux_log()
            .lines()
            .filter(|line| {
                line.split_whitespace().any(|part| part == "list-panes")
                    || line.contains("#{pane_current_path}")
            })
            .map(str::to_string)
            .collect()
    }

    fn provider_launches(&self) -> usize {
        fs::read_to_string(&self.provider_launch_log_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "launch")
            .count()
    }

    fn list_panes_count(&self) -> usize {
        fs::read_to_string(self.list_panes_count_path())
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0)
    }

    fn durable_snapshot(&self) -> DurableSnapshot {
        DurableSnapshot::capture(&self.workspace, self.env.home(), &self.db_path())
    }

    fn assert_durable_unchanged(&self, before: &DurableSnapshot, label: &str) {
        let after = self.durable_snapshot();
        assert_eq!(
            &after, before,
            "{label}; before={before:#?} after={after:#?}"
        );
    }

    fn seed_foreign_attached_state(&self) {
        let receiver = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": AMBIENT_PANE,
            "pane": AMBIENT_PANE,
            "tmux_socket": self.endpoint,
            "session_name": "historical-foreign-leader",
            "pane_tty": "/dev/ttys-historical",
            "owner_epoch": 7
        });
        let owner = json!({
            "pane_id": AMBIENT_PANE,
            "provider": "codex",
            "owner_epoch": 7,
            "claimed_via": "attach-leader"
        });
        let team = json!({
            "team_key": TEAM,
            "workspace": self.workspace,
            "session_name": "team-pane-authority",
            "tmux_endpoint": self.endpoint,
            "tmux_socket": self.endpoint,
            "leader_receiver": receiver,
            "team_owner": owner,
            "owner_epoch": 7,
            "agents": {}
        });
        let state = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "workspace": self.workspace,
            "session_name": "team-pane-authority",
            "tmux_endpoint": self.endpoint,
            "tmux_socket": self.endpoint,
            "teams": {TEAM: team},
            "agents": {}
        });
        save_runtime_state(&self.workspace, &state).expect("seed foreign attached state");
        MessageStore::open(&self.workspace).expect("initialize message store");
    }

    fn seed_preexisting_state_and_registry(&self) {
        let state = json!({
            "active_team_key": "preexisting",
            "teams": {
                "preexisting": {
                    "team_key": "preexisting",
                    "workspace": self.workspace,
                    "agents": {}
                }
            },
            "agents": {}
        });
        save_runtime_state(&self.workspace, &state).expect("seed preexisting state inventory");
        let registry_path = self.env.home().join(".team-agent/leaders/preexisting.json");
        fs::write(
            registry_path,
            serde_json::to_vec_pretty(&json!({
                "team_key": "preexisting",
                "workspace": self.workspace,
                "pane_id": "%preexisting"
            }))
            .expect("serialize preexisting registry"),
        )
        .expect("seed preexisting registry inventory");
    }

    fn seed_preexisting_state_registry_and_store(&self) {
        self.seed_preexisting_state_and_registry();
        let store = MessageStore::open(&self.workspace).expect("seed existing message store");
        store
            .create_message(
                None,
                "existing-worker",
                "leader",
                "existing durable inventory canary",
                None,
                false,
                Some("preexisting"),
            )
            .expect("seed existing durable row");
    }

    fn send_canary(&self, suffix: &str) -> Value {
        let output = self.run(
            &[
                "send",
                "leader",
                suffix,
                "--workspace",
                self.workspace_str(),
                "--team",
                TEAM,
                "--json",
            ],
            None,
        );
        json_stdout_even_on_error("send canary", &output)
    }

    fn seed_attach_window_failures(&self) -> Vec<String> {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        [2_i64, 4_i64]
            .into_iter()
            .enumerate()
            .map(|(index, attempts)| {
                let id = store
                    .create_message(
                        None,
                        "worker",
                        "leader",
                        &format!("attach-window canary {index}"),
                        None,
                        false,
                        Some(TEAM),
                    )
                    .expect("create attach-window message");
                let conn = team_agent::db::schema::open_db(store.db_path()).expect("open team db");
                conn.execute(
                    "update messages
                     set status = 'failed',
                         error = 'leader_not_attached',
                         delivery_attempts = ?2,
                         created_at = ?3,
                         updated_at = ?3
                     where message_id = ?1",
                    params![
                        id,
                        attempts,
                        if index == 0 {
                            "2026-07-27T14:35:09+08:00"
                        } else {
                            "2026-07-27T14:35:22+08:00"
                        }
                    ],
                )
                .expect("shape attach-window failure");
                id
            })
            .collect()
    }

    fn message_attempts(&self, message_id: &str) -> i64 {
        self.message_row(message_id).attempts
    }

    fn message_row(&self, message_id: &str) -> MessageRow {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = team_agent::db::schema::open_db(store.db_path()).expect("open team db");
        conn.query_row(
            "select status, delivery_attempts, error from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(MessageRow {
                    status: row.get(0)?,
                    attempts: row.get(1)?,
                    error: row.get(2)?,
                })
            },
        )
        .expect("read message row")
    }
}

#[derive(Clone, Copy)]
enum PaneTtySource {
    Controlling,
    Stdin,
}

struct TtyProbe {
    output: Output,
    controlling_tty: Option<String>,
    stdin_tty: String,
    measurement: FinalProcessTtyMeasurement,
}

impl TtyProbe {
    fn assert_distinct_ttys_and_observed_controlling(&self, label: &str) {
        let controlling_tty = self
            .controlling_tty
            .as_deref()
            .expect("fixture must declare a controlling tty");
        assert_ne!(
            controlling_tty, self.stdin_tty,
            "{label}: stdin and controlling tty must be distinct"
        );
        assert!(
            self.stdin_tty.starts_with("/dev/"),
            "{label}: redirected stdin must be a real tty; stdin={:?}",
            self.stdin_tty
        );
        self.measurement.assert_controlling_tty(
            self.measurement.expected_tdev,
            self.measurement.stdin_rdev,
            label,
        );
    }

    fn assert_no_controlling_tty_observed(&self, label: &str) {
        assert!(
            self.controlling_tty.is_none(),
            "{label}: fixture must not configure a controlling tty"
        );
        assert!(
            self.stdin_tty.starts_with("/dev/"),
            "{label}: redirected stdin must still be a real tty; stdin={:?}",
            self.stdin_tty
        );
        self.measurement.assert_no_controlling_tty(label);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct FinalProcessTtyMeasurement {
    dev_tty_fd: i32,
    dev_tty_errno: i32,
    proc_pidinfo_bytes: i32,
    e_tdev: u32,
    expected_tdev: u64,
    stdin_rdev: u64,
}

impl FinalProcessTtyMeasurement {
    fn assert_controlling_tty(&self, expected_tdev: u64, stdin_rdev: u64, label: &str) {
        assert!(
            self.dev_tty_fd >= 0,
            "{label}: final product-caller process must open /dev/tty after TIOCSCTTY; \
             measurement={self:?}"
        );
        assert_eq!(
            self.expected_tdev, expected_tdev,
            "{label}: measurement must retain the configured controlling slave identity"
        );
        assert_eq!(
            self.stdin_rdev, stdin_rdev,
            "{label}: measurement must retain the stdin slave identity"
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                self.proc_pidinfo_bytes as usize,
                std::mem::size_of::<libc::proc_bsdinfo>(),
                "{label}: proc_pidinfo(PROC_PIDTBSDINFO) must read a complete measurement; \
                 measurement={self:?}"
            );
            assert_eq!(
                u64::from(self.e_tdev),
                expected_tdev,
                "{label}: TIOCSCTTY measurement positive control must report the configured \
                 controlling slave rdev, not /dev/tty's generic alias; measurement={self:?}"
            );
        }
    }

    fn assert_no_controlling_tty(&self, label: &str) {
        assert_eq!(
            (self.dev_tty_fd, self.dev_tty_errno),
            (-1, libc::ENXIO),
            "{label}: final product-caller process must observe open(/dev/tty)=ENXIO; \
             measurement={self:?}"
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                self.proc_pidinfo_bytes as usize,
                std::mem::size_of::<libc::proc_bsdinfo>(),
                "{label}: the negative measurement is usable only if proc_pidinfo can read the \
                 same process; measurement={self:?}"
            );
            assert_eq!(
                self.e_tdev,
                u32::MAX,
                "{label}: a session leader without TIOCSCTTY must retain NODEV, proving the known \
                 bad physical shape; measurement={self:?}"
            );
        }
    }
}

fn measurement_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)
}

fn fd_rdev(fd: i32) -> io::Result<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() }.st_rdev as u64)
}

fn write_final_process_tty_measurement(
    output_fd: i32,
    expected_tdev: u64,
    stdin_rdev: u64,
) -> io::Result<()> {
    let dev_tty_fd = unsafe {
        libc::open(
            b"/dev/tty\0".as_ptr().cast(),
            libc::O_RDONLY | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    let dev_tty_errno = if dev_tty_fd == -1 {
        last_errno()
    } else {
        unsafe {
            libc::close(dev_tty_fd);
        }
        0
    };
    let (proc_pidinfo_bytes, e_tdev) = final_process_bsd_tty();
    let measurement = FinalProcessTtyMeasurement {
        dev_tty_fd,
        dev_tty_errno,
        proc_pidinfo_bytes,
        e_tdev,
        expected_tdev,
        stdin_rdev,
    };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!(measurement).cast::<u8>(),
            std::mem::size_of::<FinalProcessTtyMeasurement>(),
        )
    };
    let written = unsafe {
        libc::pwrite(
            output_fd,
            bytes.as_ptr().cast(),
            bytes.len(),
            0 as libc::off_t,
        )
    };
    if written != bytes.len() as isize {
        return Err(if written == -1 {
            io::Error::last_os_error()
        } else {
            io::Error::new(io::ErrorKind::WriteZero, "short tty measurement write")
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn final_process_bsd_tty() -> (i32, u32) {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    let e_tdev = if read == size as i32 {
        unsafe { info.assume_init() }.e_tdev
    } else {
        u32::MAX
    };
    (read, e_tdev)
}

#[cfg(not(target_os = "macos"))]
fn final_process_bsd_tty() -> (i32, u32) {
    (-1, u32::MAX)
}

#[cfg(target_os = "macos")]
fn last_errno() -> i32 {
    unsafe { *libc::__error() }
}

#[cfg(not(target_os = "macos"))]
fn last_errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

fn read_tty_measurement(path: &Path) -> FinalProcessTtyMeasurement {
    let bytes = fs::read(path).expect("read final-process tty measurement");
    assert_eq!(
        bytes.len(),
        std::mem::size_of::<FinalProcessTtyMeasurement>(),
        "final-process tty measurement must be complete; path={} bytes={}",
        path.display(),
        bytes.len()
    );
    unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<FinalProcessTtyMeasurement>()) }
}

#[derive(Debug, PartialEq, Eq)]
struct DurableSnapshot {
    workspace_tree: Vec<TreeEntry>,
    home_tree: Vec<TreeEntry>,
    db: Option<DbFacts>,
}

impl DurableSnapshot {
    fn capture(workspace: &Path, home: &Path, db_path: &Path) -> Self {
        let db = db_path.exists().then(|| DbFacts::capture(db_path));
        Self {
            workspace_tree: tree_snapshot(workspace),
            home_tree: tree_snapshot(home),
            db,
        }
    }
}

#[derive(PartialEq, Eq)]
struct TreeEntry {
    relative_path: PathBuf,
    kind: &'static str,
    bytes: Option<Vec<u8>>,
    sha256: Option<[u8; 32]>,
    device: u64,
    inode: u64,
    mode: u32,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl std::fmt::Debug for TreeEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TreeEntry")
            .field("relative_path", &self.relative_path)
            .field("kind", &self.kind)
            .field("bytes_len", &self.bytes.as_ref().map(Vec::len))
            .field("sha256", &self.sha256)
            .field("device", &self.device)
            .field("inode", &self.inode)
            .field("mode", &self.mode)
            .field("len", &self.len)
            .field("mtime", &self.mtime)
            .field("ctime", &self.ctime)
            .finish()
    }
}

fn tree_snapshot(root: &Path) -> Vec<TreeEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<TreeEntry>) {
        let metadata = fs::symlink_metadata(path).expect("snapshot durable metadata");
        let file_type = metadata.file_type();
        let (kind, bytes) = if file_type.is_dir() {
            ("directory", None)
        } else if file_type.is_file() {
            (
                "file",
                Some(fs::read(path).expect("snapshot durable file bytes")),
            )
        } else if file_type.is_symlink() {
            (
                "symlink",
                Some(
                    fs::read_link(path)
                        .expect("snapshot durable symlink")
                        .to_string_lossy()
                        .as_bytes()
                        .to_vec(),
                ),
            )
        } else {
            ("other", None)
        };
        let sha256 = bytes
            .as_ref()
            .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
        entries.push(TreeEntry {
            relative_path: path
                .strip_prefix(root)
                .expect("snapshot path must stay under root")
                .to_path_buf(),
            kind,
            bytes,
            sha256,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            len: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        });
        if file_type.is_dir() {
            let mut children = fs::read_dir(path)
                .expect("enumerate durable tree")
                .map(|entry| entry.expect("read durable tree entry").path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

#[derive(Debug, PartialEq, Eq)]
struct DbFacts {
    user_version: i64,
    schema: Vec<(String, String, String, Option<String>)>,
    row_counts: Vec<(String, i64)>,
}

impl DbFacts {
    fn capture(path: &Path) -> Self {
        let uri = format!("file:{}?immutable=1", path.to_string_lossy());
        let connection = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .expect("open existing DB immutable for durable snapshot");
        let user_version = connection
            .query_row("pragma user_version", [], |row| row.get(0))
            .expect("read DB user_version");
        let mut schema_statement = connection
            .prepare(
                "select type, name, tbl_name, sql
                 from sqlite_schema
                 order by type, name, tbl_name",
            )
            .expect("prepare schema inventory");
        let schema = schema_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .expect("query schema inventory")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect schema inventory");
        let table_names = schema
            .iter()
            .filter_map(|(kind, name, _, _)| (kind == "table").then_some(name.clone()))
            .collect::<Vec<_>>();
        let row_counts = table_names
            .into_iter()
            .map(|table| {
                let quoted = table.replace('"', "\"\"");
                let count = connection
                    .query_row(&format!("select count(*) from \"{quoted}\""), [], |row| {
                        row.get(0)
                    })
                    .unwrap_or_else(|error| panic!("count rows in {table}: {error}"));
                (table, count)
            })
            .collect();
        Self {
            user_version,
            schema,
            row_counts,
        }
    }
}

struct UnixSocketFixture {
    _listener: UnixListener,
    path: PathBuf,
}

impl UnixSocketFixture {
    fn bind(path: &Path) -> Self {
        let listener = UnixListener::bind(path).expect("bind live historical tmux endpoint");
        Self {
            _listener: listener,
            path: path.to_path_buf(),
        }
    }
}

impl Drop for UnixSocketFixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct MessageRow {
    status: String,
    attempts: i64,
    #[allow(dead_code)]
    error: Option<String>,
}

fn open_pty() -> io::Result<(File, File, String)> {
    let mut master = -1;
    let mut slave = -1;
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0_i8; 256];
    let name_result = unsafe { libc::ttyname_r(slave, name.as_mut_ptr(), name.len()) };
    if name_result != 0 {
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        return Err(io::Error::from_raw_os_error(name_result));
    }
    let tty = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    Ok((master, slave, tty))
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fixture");
}

fn json_stdout(label: &str, output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{label} must exit zero; status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    json_stdout_even_on_error(label, output)
}

fn json_stdout_even_on_error(label: &str, output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout must be JSON: {error}; status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_typed_prelaunch_refusal(
    output: &Output,
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
    unavailable: Option<(refusal_catalog::PaneAuthorityRefusalField, &str)>,
    label: &str,
) {
    assert!(
        !output.status.success() && value["ok"] == json!(false),
        "{label} RED signature: invalid ambient authority must fail loud at the public launcher \
         refusal node; \
         status={} output={value}",
        output.status
    );
    assert_catalog_refusal_payload(value, reason, unavailable, label);
}

fn assert_catalog_refusal_payload(
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
    unavailable: Option<(refusal_catalog::PaneAuthorityRefusalField, &str)>,
    label: &str,
) {
    assert!(
        refusal_catalog::PaneAuthorityRefusalReason::ALL.contains(&reason),
        "{label}: expected reason must come from the product catalog"
    );
    let object = find_reason_object(value, reason).unwrap_or_else(|| {
        panic!(
            "{label} RED signature: public surface must expose catalog reason {}; output={value}",
            reason.as_str()
        )
    });
    let mut expected_fields = reason
        .required_fact_fields()
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>();
    expected_fields.sort_unstable();
    let mut catalog_field_names = refusal_catalog::PaneAuthorityRefusalReason::ALL
        .iter()
        .flat_map(|reason| reason.required_fact_fields())
        .map(|field| field.as_str())
        .collect::<Vec<_>>();
    catalog_field_names.sort_unstable();
    catalog_field_names.dedup();
    let mut actual_fields = object
        .keys()
        .filter_map(|key| {
            catalog_field_names
                .contains(&key.as_str())
                .then_some(key.as_str())
        })
        .collect::<Vec<_>>();
    actual_fields.sort_unstable();
    assert_eq!(
        actual_fields,
        expected_fields,
        "{label} RED signature: reason→required-field identity must come from the one product \
         catalog; reason={} output={value}",
        reason.as_str()
    );
    for field in reason.required_fact_fields() {
        let field_name = field.as_str();
        let fact = object.get(field_name).unwrap_or_else(|| {
            panic!(
                "{label}: catalog-required field {field_name} is absent for {}; output={value}",
                reason.as_str()
            )
        });
        if unavailable
            .as_ref()
            .is_some_and(|(unavailable_field, _)| unavailable_field == field)
        {
            let expected_cause = unavailable
                .as_ref()
                .map(|(_, cause)| *cause)
                .expect("unavailable field has cause");
            assert_unavailable_fact(fact, expected_cause, label, field_name);
        } else {
            assert_available_fact(fact, label, field_name);
        }
    }
    assert_recovery_action(value, reason, label);
}

fn find_reason_object(
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
) -> Option<&serde_json::Map<String, Value>> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_reason_object(value, reason)),
        Value::Object(object) => {
            if object
                .get(refusal_catalog::REASON_FIELD)
                .and_then(Value::as_str)
                == Some(reason.as_str())
            {
                Some(object)
            } else {
                object
                    .values()
                    .find_map(|value| find_reason_object(value, reason))
            }
        }
        _ => None,
    }
}

fn assert_available_fact(value: &Value, label: &str, field: &str) {
    let legal = match value {
        Value::String(value) => !value.trim().is_empty() && value != "unknown",
        Value::Number(_) => true,
        _ => false,
    };
    assert!(
        legal,
        "{label}: available catalog field {field} must use its scalar legal shape, never null, \
         empty, unknown, or an unavailable placeholder; value={value}"
    );
}

fn assert_unavailable_fact(value: &Value, expected_cause: &str, label: &str, field: &str) {
    let object = value.as_object().unwrap_or_else(|| {
        panic!(
            "{label}: unavailable catalog field {field} must use the typed availability+cause \
             shape; value={value}"
        )
    });
    assert_eq!(
        object
            .get(refusal_catalog::AVAILABILITY_FIELD)
            .and_then(Value::as_str),
        Some(refusal_catalog::PaneAuthorityFactAvailability::Unavailable.as_str()),
        "{label}: unavailable field {field} must use the catalog availability identity; \
         value={value}"
    );
    assert_eq!(
        object
            .get(refusal_catalog::CAUSE_FIELD)
            .and_then(Value::as_str),
        Some(expected_cause),
        "{label}: unavailable field {field} must state the catalog cause that produced the \
         missing observation; value={value}"
    );
    assert!(
        object.values().all(|value| {
            value
                .as_str()
                .is_none_or(|value| !value.trim().is_empty() && value != "unknown")
        }),
        "{label}: unavailable field {field} must not collapse to empty/unknown; value={value}"
    );
}

fn assert_workspace_mismatch_facts(value: &Value, label: &str) {
    use refusal_catalog::{
        PaneAuthorityRefusalField as Field, PaneAuthorityRefusalReason as Reason,
    };

    let object = find_reason_object(value, Reason::PaneWorkspaceMismatch)
        .unwrap_or_else(|| panic!("{label}: workspace mismatch object missing; output={value}"));
    let requested = object[Field::RequestedWorkspace.as_str()]
        .as_str()
        .expect("requested workspace legal shape");
    let observed = object[Field::ObservedPaneWorkspace.as_str()]
        .as_str()
        .expect("observed workspace legal shape");
    assert_ne!(
        requested, observed,
        "{label}: PaneWorkspaceMismatch is legal only when two observed workspace identities \
         actually differ; output={value}"
    );
    assert!(
        Path::new(requested).is_absolute() && Path::new(observed).is_absolute(),
        "{label}: both workspace facts must be self-locating absolute paths; output={value}"
    );
}

fn assert_workspace_mismatch_facts_match_case(value: &Value, case: &Case, label: &str) {
    use refusal_catalog::{
        PaneAuthorityRefusalField as Field, PaneAuthorityRefusalReason as Reason,
    };

    assert_workspace_mismatch_facts(value, label);
    let object = find_reason_object(value, Reason::PaneWorkspaceMismatch)
        .unwrap_or_else(|| panic!("{label}: workspace mismatch object missing; output={value}"));
    let expected_requested = case
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| case.workspace.clone());
    assert_eq!(
        object[Field::RequestedWorkspace.as_str()].as_str(),
        Some(expected_requested.to_string_lossy().as_ref()),
        "{label}: requested workspace must come from the original mismatch snapshot; \
         output={value}"
    );
    assert_eq!(
        object[Field::ObservedPaneId.as_str()].as_str(),
        Some(AMBIENT_PANE),
        "{label}: observed pane must come from the original mismatch snapshot; output={value}"
    );
    assert_eq!(
        object[Field::ObservedPaneWorkspace.as_str()].as_str(),
        Some(case.foreign_workspace.to_string_lossy().as_ref()),
        "{label}: observed pane workspace must come from the original mismatch snapshot; \
         output={value}"
    );
    assert_eq!(
        object[Field::Endpoint.as_str()].as_str(),
        Some(case.endpoint.to_string_lossy().as_ref()),
        "{label}: endpoint must come from the original mismatch snapshot; output={value}"
    );
}

fn assert_tty_mismatch_facts(value: &Value, label: &str) {
    use refusal_catalog::{
        PaneAuthorityRefusalField as Field, PaneAuthorityRefusalReason as Reason,
    };

    let object = find_reason_object(value, Reason::PaneTtyMismatch)
        .unwrap_or_else(|| panic!("{label}: tty mismatch object missing; output={value}"));
    let caller = object[Field::CallerControllingTty.as_str()]
        .as_u64()
        .expect("caller tty device identity");
    let pane = object[Field::ObservedPaneTty.as_str()]
        .as_u64()
        .expect("observed pane tty device identity");
    assert_ne!(
        caller, pane,
        "{label}: PaneTtyMismatch is legal only when both measured device identities exist and \
         differ; output={value}"
    );
}

fn assert_recovery_action(
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
    label: &str,
) {
    let object = find_recovery_object(value, reason).unwrap_or_else(|| {
        panic!(
            "{label}: catalog reason {} must have an executable recovery projection; \
             output={value}",
            reason.as_str()
        )
    });
    let mut expected_fields = reason
        .required_fact_fields()
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>();
    expected_fields.sort_unstable();
    let mut actual_fields = reason
        .required_fact_fields()
        .iter()
        .filter_map(|field| {
            object
                .contains_key(field.as_str())
                .then_some(field.as_str())
        })
        .collect::<Vec<_>>();
    actual_fields.sort_unstable();
    assert_eq!(
        actual_fields,
        expected_fields,
        "{label}: recovery projection must retain the same catalog-required fact identities; \
         reason={} output={value}",
        reason.as_str()
    );
    assert_eq!(
        object
            .get(refusal_catalog::ACTION_REQUIRED_FIELD)
            .and_then(Value::as_bool),
        Some(refusal_catalog::PaneAuthorityRecovery::REQUIRED.action_required),
        "{label}: recovery required bit must come from the shared catalog; output={value}"
    );
    let action = object
        .get(refusal_catalog::ACTION_FIELD)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let hint = object
        .get(refusal_catalog::HINT_ACTION_FIELD)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        parse_supported_recovery_command(&hint).is_some()
            || copyable_recovery_command(&Value::Object(object.clone())).is_some(),
        "{label}: recovery must contain a directly executable action from the supported catalog \
         action set; output={value}"
    );
    assert!(
        action.contains("terminal")
            && (action.contains("outside") || action.contains("outside of"))
            && (action.contains("tmux") || action.contains("pane")),
        "{label}: clean-terminal guidance must say that the new terminal is outside the current \
        tmux/pane, not merely suggest unsetting inherited variables; output={value}"
    );
}

fn find_recovery_object(
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
) -> Option<&serde_json::Map<String, Value>> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_recovery_object(value, reason)),
        Value::Object(object) => {
            let is_matching_recovery = object
                .get(refusal_catalog::REASON_FIELD)
                .and_then(Value::as_str)
                == Some(reason.as_str())
                && object
                    .get(refusal_catalog::ACTION_REQUIRED_FIELD)
                    .and_then(Value::as_bool)
                    == Some(refusal_catalog::PaneAuthorityRecovery::REQUIRED.action_required)
                && (object.contains_key(refusal_catalog::ACTION_FIELD)
                    || object.contains_key(refusal_catalog::HINT_ACTION_FIELD));
            if is_matching_recovery {
                Some(object)
            } else {
                object
                    .values()
                    .find_map(|value| find_recovery_object(value, reason))
            }
        }
        _ => None,
    }
}

fn assert_no_catalog_refusal(value: &Value, label: &str) {
    for reason in refusal_catalog::PaneAuthorityRefusalReason::ALL {
        assert_no_reason(value, reason, label);
    }
}

fn assert_no_reason(
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
    label: &str,
) {
    assert!(
        find_reason_object(value, reason).is_none(),
        "{label}; unexpected_reason={} output={value}",
        reason.as_str()
    );
}

#[derive(Debug, PartialEq, Eq)]
struct RefusalSignature {
    assertion_node: String,
    exit_code: Option<i32>,
    reason: String,
    field_identities: Vec<String>,
}

fn refusal_signature(
    assertion_node: &str,
    exit_code: Option<i32>,
    value: &Value,
) -> Option<RefusalSignature> {
    refusal_catalog::PaneAuthorityRefusalReason::ALL
        .iter()
        .find_map(|reason| {
            find_reason_object(value, *reason).map(|object| {
                let mut field_identities = reason
                    .required_fact_fields()
                    .iter()
                    .filter_map(|field| {
                        object
                            .contains_key(field.as_str())
                            .then_some(field.as_str())
                    })
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                field_identities.sort();
                RefusalSignature {
                    assertion_node: assertion_node.to_string(),
                    exit_code,
                    reason: reason.as_str().to_string(),
                    field_identities,
                }
            })
        })
}

fn assert_no_provider_or_managed_spawn(case: &Case, label: &str) {
    assert_eq!(
        case.provider_launches(),
        0,
        "{label} RED signature: authority refusal must happen before provider spawn"
    );
    let tmux_log = case.tmux_log();
    for forbidden in [
        "new-session",
        "new-window",
        "attach-session",
        "switch-client",
    ] {
        assert!(
            !contains_tmux_operation(&tmux_log, forbidden),
            "{label} RED signature: invalid ambient authority must not fall back to Managed; \
             forbidden={forbidden} tmux_log={tmux_log:?}"
        );
    }
}

fn contains_tmux_operation(log: &str, operation: &str) -> bool {
    log.lines()
        .any(|line| line.split_whitespace().any(|part| part == operation))
}

fn has_copyable_recovery_action(value: &Value) -> bool {
    let text = value.to_string().to_ascii_lowercase();
    text.contains("team-agent attach-leader")
        || text.contains("team-agent takeover")
        || text.contains("clean terminal")
        || text.contains("unset tmux")
}

fn copyable_recovery_command(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(text) => text
            .split('`')
            .enumerate()
            .filter_map(|(index, candidate)| (index % 2 == 1).then_some(candidate))
            .find_map(parse_supported_recovery_command),
        Value::Array(values) => values.iter().find_map(copyable_recovery_command),
        Value::Object(values) => values.values().find_map(copyable_recovery_command),
        _ => None,
    }
}

fn parse_supported_recovery_command(command: &str) -> Option<Vec<String>> {
    let argv = command
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let supported = argv.first().is_some_and(|arg| arg == "team-agent")
        && argv
            .get(1)
            .is_some_and(|arg| arg == "attach-leader" || arg == "takeover");
    supported.then_some(argv)
}

fn has_typed_reason(value: &Value, expected: &str) -> bool {
    value.as_object().is_some_and(|object| {
        ["reason", "reason_code", "channel_reason"]
            .iter()
            .any(|key| object.get(*key).and_then(Value::as_str) == Some(expected))
    }) || value
        .as_array()
        .is_some_and(|values| values.iter().any(|value| has_typed_reason(value, expected)))
        || value.as_object().is_some_and(|object| {
            object
                .values()
                .any(|value| has_typed_reason(value, expected))
        })
}

fn json_contains_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value == expected,
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_string(value, expected)),
        Value::Object(values) => values
            .values()
            .any(|value| json_contains_string(value, expected)),
        _ => false,
    }
}

fn attach_window_debt_visible(values: &[&Value], message_ids: &[String]) -> bool {
    values
        .iter()
        .any(|value| value_names_attach_window_debt(value, message_ids.len() as u64))
}

fn value_names_attach_window_debt(value: &Value, expected_count: u64) -> bool {
    match value {
        Value::String(text) => {
            let text = text.to_ascii_lowercase();
            (text.contains("leader_not_attached") || text.contains("attach window"))
                && text.contains(&expected_count.to_string())
        }
        Value::Array(values) => values
            .iter()
            .any(|value| value_names_attach_window_debt(value, expected_count)),
        Value::Object(object) => {
            let local_text = Value::Object(object.clone())
                .to_string()
                .to_ascii_lowercase();
            let names_cause =
                local_text.contains("leader_not_attached") || local_text.contains("attach_window");
            let names_count = object.iter().any(|(key, value)| {
                (key == "count" || key == "failed_count" || key == "message_count")
                    && value.as_u64() == Some(expected_count)
            });
            (names_cause && names_count)
                || object
                    .values()
                    .any(|value| value_names_attach_window_debt(value, expected_count))
        }
        _ => false,
    }
}

const PROVIDER_SHIM: &str = r#"#!/bin/sh
if [ "${1-}" = "--version" ]; then
  printf 'codex 1.0.0\n'
  exit 0
fi
printf 'launch\n' >> "$TEAM_AGENT_TEST_PROVIDER_LAUNCH_LOG"
exit 0
"#;

const TMUX_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$TEAM_AGENT_TEST_TMUX_LOG"
mode=$(cat "$TEAM_AGENT_TEST_PANE_MODE_FILE")
last=
target=
previous=
spawn_session=
for arg in "$@"; do
  if [ "$previous" = "-t" ]; then
    target="$arg"
  fi
  if [ "$previous" = "-s" ]; then
    spawn_session="$arg"
  fi
  previous="$arg"
  last="$arg"
done

if [ "${1-}" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi

case " $* " in
  *" has-session "*)
    exit 1
    ;;
  *" new-session "*|*" new-window "*)
    if [ -n "$spawn_session" ]; then
      printf '%s\n' "$spawn_session" > "$TEAM_AGENT_TEST_SPAWN_SESSION"
    fi
    printf '%%managed\n'
    exit 0
    ;;
  *" list-panes "*)
    if [ "$mode" = "query-failed" ]; then
      printf 'fixture pane query failed\n' >&2
      exit 1
    fi
    count=0
    if [ -f "$TEAM_AGENT_TEST_LIST_PANES_COUNT" ]; then
      count=$(cat "$TEAM_AGENT_TEST_LIST_PANES_COUNT")
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$TEAM_AGENT_TEST_LIST_PANES_COUNT"
    if [ "$mode" = "snapshot-reread-fails" ] && [ "$count" -gt 1 ]; then
      printf 'fixture later list_targets read failed\n' >&2
      exit 1
    fi
    if [ "$mode" = "matching-slow" ]; then
      sleep 1
    fi
    if [ "$mode" = "matching" ] || [ "$mode" = "matching-slow" ] || \
       { [ "$mode" = "matching-then-foreign" ] && [ "$count" -eq 1 ]; }; then
      printf '%%ambient\tambient-leader\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4101\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    elif [ "$mode" = "matching-then-foreign" ]; then
      printf '%%ambient\thistorical-foreign-leader\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4102\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_FOREIGN_WORKSPACE"
    elif [ "$mode" = "foreign-workspace" ] || \
         [ "$mode" = "snapshot-reread-changes" ] || \
         [ "$mode" = "snapshot-reread-fails" ]; then
      printf '%%ambient\thistorical-foreign-leader\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4102\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_FOREIGN_WORKSPACE"
    elif [ "$mode" = "current-path-missing" ]; then
      printf '%%ambient\tambient-leader\t0\tcodex\t0\t%s\tcodex\t1\t\t1\t0\t4101\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY"
    elif [ "$mode" = "foreign" ]; then
      printf '%%ambient\thistorical-foreign-leader\t0\tcodex\t0\t/dev/ttys-historical\tcodex\t1\t%s\t1\t0\t4102\t\n' \
        "$TEAM_AGENT_TEST_FOREIGN_WORKSPACE"
    fi
    printf '%%good\trequested-leader\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4103\t\n' \
      "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    managed_session=managed-leader
    if [ -f "$TEAM_AGENT_TEST_SPAWN_SESSION" ]; then
      managed_session=$(cat "$TEAM_AGENT_TEST_SPAWN_SESSION")
    fi
    printf '%%managed\t%s\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4104\t\n' \
      "$managed_session" "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    exit 0
    ;;
  *" display-message "*)
    case "$last" in
      '#{pane_id}') printf '%s\n' "${target:-%good}" ;;
      '#{pane_current_command}') printf 'codex\n' ;;
      '#{pane_current_path}')
        if [ "$target" = "%ambient" ] && [ "$mode" = "snapshot-reread-fails" ]; then
          printf 'fixture later PaneCurrentPath read failed\n' >&2
          exit 1
        elif [ "$target" = "%ambient" ] && \
           { [ "$mode" = "foreign" ] || [ "$mode" = "foreign-workspace" ]; }; then
          printf '%s\n' "$TEAM_AGENT_TEST_FOREIGN_WORKSPACE"
        elif [ "$target" = "%ambient" ] && [ "$mode" = "current-path-missing" ]; then
          printf '\n'
        else
          printf '%s\n' "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
        fi
        ;;
      '#{pane_tty}')
        if [ "$target" = "%ambient" ] && [ "$mode" = "foreign" ]; then
          printf '/dev/ttys-historical\n'
        else
          printf '%s\n' "$TEAM_AGENT_TEST_PANE_TTY"
        fi
        ;;
      '#{session_name}') printf 'requested-leader\n' ;;
      '#{pane_width}') printf '120\n' ;;
      '#{pane_mode}') printf '0\n' ;;
      *) printf '%s\n' "${target:-%good}" ;;
    esac
    exit 0
    ;;
  *" set-buffer "*)
    printf '%s' "$last" > "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *" load-buffer "*)
    cat > "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *" capture-pane "*)
    [ -f "$TEAM_AGENT_TEST_PANE_CAPTURE" ] && cat "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *" send-keys "*" Enter"*|*" send-keys "*" Enter")
    : > "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#;
