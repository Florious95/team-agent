//! E7 second-slice RED contract: host leader registry behavior.
//!
//! References:
//! - design `.team/artifacts/host-leader-registry-design.md` §14 steps 3-6.
//! - design `.team/artifacts/host-leader-registry-design.md` §11 tests 1, 4-10.
//! - CR red-line spirit from `.team/artifacts/phase-dx-invariant-review.md`:
//!   registry is a derived discovery index; send/read paths must revalidate
//!   canonical workspace/team state and must not become identity writers.
//!
//! Contract: successful binding hooks register leader entries under isolated
//! `$HOME/.team-agent/leaders`; leaders/send consume that host index only as
//! discovery, then validate canonical state before delivery or refusal.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;
use serde_json::{json, Value};
use serial_test::{file_serial, serial};
use sha2::{Digest, Sha256};

#[allow(dead_code)]
#[path = "../src/app_server_test_support.rs"]
mod app_server_test_support;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[serial(env)]
#[file_serial(tmux)]
fn e7_binding_success_hooks_write_schema_v1_registry_entries_atomically() {
    let mut failures = Vec::new();

    for verb in ["attach-leader", "claim-leader", "takeover"] {
        if let Err(error) = assert_tmux_binding_registers(verb) {
            failures.push(error);
        }
    }

    assert!(
        failures.is_empty(),
        "E7 second-slice RED: claim-leader/attach-leader/takeover success hooks must write host leader registry entries after canonical binding succeeds:\n{}",
        failures.join("\n\n")
    );
}

#[test]
#[serial(env)]
fn e7_app_server_binding_success_hook_writes_registry_entry() {
    let case = RuntimeCase::new("app-server-bind", "alpha");
    case.seed_state_without_receiver("alpha");
    let fake = app_server_test_support::FakeAppServer::start(
        "e7-app-bind",
        app_server_test_support::FakeAppServerScript::happy(
            "thread-live",
            "session-live",
            case.workspace.to_str().expect("workspace utf8"),
        ),
    );

    let output = case.run_cli(vec![
        "attach-app-server-leader".into(),
        "--workspace".into(),
        case.workspace_arg(),
        "--team".into(),
        "alpha".into(),
        "--socket".into(),
        fake.endpoint().to_string(),
        "--thread-id".into(),
        "thread-live".into(),
        "--json".into(),
    ]);
    let body = json_output(&output, "attach-app-server-leader --json");
    assert_json_ok(
        &output,
        &body,
        "attach-app-server-leader should succeed before registry check",
    );

    let entries = registry_entries(&case.home);
    assert_eq!(
        entries.len(),
        1,
        "E7 RED: attach-app-server-leader success must write exactly one registry file; entries={entries:?} body={body}"
    );
    let entry = &entries[0].1;
    assert_registry_schema(
        entry,
        &case.workspace,
        "alpha",
        "codex_app_server",
        "attach-app-server-leader",
    );
    assert_eq!(
        entry.pointer("/channel/socket").and_then(Value::as_str),
        Some(fake.endpoint()),
        "E7 RED: app-server registry channel must preserve socket endpoint; entry={entry}"
    );
    assert_eq!(
        entry.pointer("/channel/thread_id").and_then(Value::as_str),
        Some("thread-live"),
        "E7 RED: app-server registry channel must preserve thread_id; entry={entry}"
    );
    assert_eq!(
        entry.get("owner_epoch").and_then(Value::as_u64),
        state_owner_epoch(&case.workspace, "alpha"),
        "E7 RED: app-server registry owner_epoch must match canonical state after attach; entry={entry}"
    );
    assert_no_tmp_registry_files(&case.home);
}

#[test]
#[serial(env)]
#[file_serial(tmux)]
fn e7_shutdown_unregisters_matching_registry_entry_only_after_canonical_success() {
    let case = RuntimeCase::new("shutdown-unregister", "alpha");
    let _pane = case.start_leader_pane("worker-placeholder");
    case.seed_state_without_receiver("alpha");
    let stale_path = write_registry_entry(
        &case.home,
        &case.workspace,
        "alpha",
        "direct_tmux",
        json!({
            "pane_id": "%stale",
            "session_name": case.session_name,
            "window_name": "old-leader"
        }),
        0,
        "attach-leader",
    );

    let output = case.run_cli(vec![
        "shutdown".into(),
        "--workspace".into(),
        case.workspace_arg(),
        "--team".into(),
        "alpha".into(),
        "--json".into(),
    ]);
    let body = json_output(&output, "shutdown --team alpha --json");
    assert_json_ok(
        &output,
        &body,
        "shutdown should succeed before unregister check",
    );

    assert!(
        !stale_path.exists(),
        "E7 RED: successful canonical shutdown/unbind must unregister matching registry entry; path={} body={body}",
        stale_path.display()
    );
}

#[test]
#[serial(env)]
#[file_serial(tmux)]
fn e7_leaders_lists_live_stale_and_ambiguous_without_worker_rows() {
    let home = temp_home("leaders-list");
    let live_a = RuntimeCase::with_home("leaders-live-a", "alpha", home.clone());
    let live_b = RuntimeCase::with_home("leaders-live-b", "beta", home.clone());
    let pane_a = live_a.start_leader_pane("leader");
    let pane_b = live_b.start_leader_pane("leader");
    live_a.seed_state_with_receiver("alpha", &pane_a, 1);
    live_b.seed_state_with_receiver("beta", &pane_b, 1);
    write_registry_entry(
        &home,
        &live_a.workspace,
        "alpha",
        "direct_tmux",
        channel_for_pane(&live_a, &pane_a),
        1,
        "seed",
    );
    write_registry_entry(
        &home,
        &live_b.workspace,
        "beta",
        "direct_tmux",
        channel_for_pane(&live_b, &pane_b),
        1,
        "seed",
    );

    let first = run_cli_with_home(
        &home,
        &live_a.workspace,
        vec!["leaders".into(), "--json".into()],
    );
    let first_json = json_output(&first, "leaders --json initial");
    assert_json_ok(&first, &first_json, "leaders should enumerate registry");
    assert_leader_status(&first_json, "alpha", "LIVE");
    assert_leader_status(&first_json, "beta", "LIVE");
    assert!(
        !first_json.to_string().contains("worker_one"),
        "E7 RED: leaders output must not list workers/tasks/results from canonical state; output={first_json}"
    );

    live_b.kill_session();
    let stale = run_cli_with_home(
        &home,
        &live_a.workspace,
        vec!["leaders".into(), "--json".into()],
    );
    let stale_json = json_output(&stale, "leaders --json after kill");
    assert_json_ok(
        &stale,
        &stale_json,
        "leaders should still return JSON when one entry is stale",
    );
    assert_leader_status(&stale_json, "alpha", "LIVE");
    assert_leader_status(&stale_json, "beta", "STALE");
    assert!(
        leader_by_name(&stale_json, "beta")
            .and_then(|entry| entry.get("stale_reason"))
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty()),
        "E7 RED: killed target must be STALE with machine-readable stale_reason; output={stale_json}"
    );

    let live_c = RuntimeCase::with_home("leaders-live-c", "alpha", home.clone());
    let pane_c = live_c.start_leader_pane("leader");
    live_c.seed_state_with_receiver("alpha", &pane_c, 1);
    write_registry_entry(
        &home,
        &live_c.workspace,
        "alpha",
        "direct_tmux",
        channel_for_pane(&live_c, &pane_c),
        1,
        "seed",
    );

    let ambiguous = run_cli_with_home(
        &home,
        &live_a.workspace,
        vec!["leaders".into(), "--json".into()],
    );
    let ambiguous_json = json_output(&ambiguous, "leaders --json ambiguous");
    assert_json_ok(
        &ambiguous,
        &ambiguous_json,
        "leaders should expose ambiguous short names",
    );
    assert_ambiguous_name(&ambiguous_json, "alpha", 2);
}

#[test]
#[serial(env)]
#[file_serial(tmux)]
fn e7_send_to_leader_resolves_unique_refuses_ambiguous_and_never_misroutes_stale() {
    let home = temp_home("send-to-leader");
    let sender = RuntimeCase::with_home("sender", "sender", home.clone());
    sender.seed_state_without_receiver("sender");

    let target = RuntimeCase::with_home("send-live-target", "alpha", home.clone());
    let pane = target.start_leader_pane("leader");
    target.seed_state_with_receiver("alpha", &pane, 1);
    write_registry_entry(
        &home,
        &target.workspace,
        "alpha",
        "direct_tmux",
        channel_for_pane(&target, &pane),
        1,
        "seed",
    );

    let live_token = unique_token("E7_TO_LEADER_LIVE");
    let live = sender.run_cli(vec![
        "send".into(),
        "--workspace".into(),
        sender.workspace_arg(),
        "--to-leader".into(),
        "alpha".into(),
        live_token.clone(),
        "--sender".into(),
        "e7-test".into(),
        "--json".into(),
    ]);
    let live_json = json_output(&live, "send --to-leader alpha --json");
    assert_json_ok(
        &live,
        &live_json,
        "send --to-leader unique short name should deliver",
    );
    assert_eq!(
        live_json.get("resolved_via").and_then(Value::as_str),
        Some("host_leader_registry"),
        "E7 RED: send --to-leader must report registry resolution before canonical delivery; output={live_json}"
    );
    assert_eq!(
        live_json.get("delivered").and_then(Value::as_bool),
        Some(true),
        "E7 RED: unique live leader delivery must be honest delivered=true after physical injection; output={live_json}"
    );
    assert_pane_contains(
        &pane,
        &live_token,
        "unique live --to-leader token should reach target pane",
    );

    let duplicate = RuntimeCase::with_home("send-live-duplicate", "alpha", home.clone());
    let duplicate_pane = duplicate.start_leader_pane("leader");
    duplicate.seed_state_with_receiver("alpha", &duplicate_pane, 1);
    write_registry_entry(
        &home,
        &duplicate.workspace,
        "alpha",
        "direct_tmux",
        channel_for_pane(&duplicate, &duplicate_pane),
        1,
        "seed",
    );
    let ambiguous_token = unique_token("E7_TO_LEADER_AMBIG");
    let ambiguous = sender.run_cli(vec![
        "send".into(),
        "--workspace".into(),
        sender.workspace_arg(),
        "--to-leader".into(),
        "alpha".into(),
        ambiguous_token.clone(),
        "--sender".into(),
        "e7-test".into(),
        "--json".into(),
    ]);
    let ambiguous_json = json_output(&ambiguous, "send --to-leader ambiguous --json");
    assert_eq!(
        ambiguous_json.get("ok").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        ambiguous_json.get("reason").and_then(Value::as_str),
        Some("name_ambiguous"),
        "E7 RED: short-name collision must refuse with reason=name_ambiguous and candidates; output={ambiguous_json}"
    );
    assert!(
        ambiguous_json
            .get("candidates")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() >= 2),
        "E7 RED: ambiguous refusal must list candidates; output={ambiguous_json}"
    );
    assert_pane_not_contains(
        &pane,
        &ambiguous_token,
        "ambiguous send must not inject into first candidate",
    );
    assert_pane_not_contains(
        &duplicate_pane,
        &ambiguous_token,
        "ambiguous send must not inject into second candidate",
    );
    assert_eq!(
        message_count(&target.workspace, &ambiguous_token)
            + message_count(&duplicate.workspace, &ambiguous_token),
        0,
        "E7 RED: ambiguous send must not create target DB rows"
    );

    let stale = RuntimeCase::with_home("send-stale-target", "deadteam", home.clone());
    let stale_pane = stale.start_leader_pane("leader");
    stale.seed_state_with_receiver_status("deadteam", &stale_pane, 3, "down");
    write_registry_entry(
        &home,
        &stale.workspace,
        "deadteam",
        "direct_tmux",
        channel_for_pane(&stale, &stale_pane),
        3,
        "seed",
    );
    stale.kill_session();
    let stale_token = unique_token("E7_TO_LEADER_STALE");
    let stale_out = sender.run_cli(vec![
        "send".into(),
        "--workspace".into(),
        sender.workspace_arg(),
        "--to-leader".into(),
        "deadteam".into(),
        stale_token.clone(),
        "--sender".into(),
        "e7-test".into(),
        "--json".into(),
    ]);
    let stale_json = json_output(&stale_out, "send --to-leader stale --json");
    assert_eq!(stale_json.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        stale_json.get("reason").and_then(Value::as_str),
        Some("registry_stale"),
        "E7 RED: dead/down registry target must fail closed as registry_stale; output={stale_json}"
    );
    assert_pane_not_contains(
        &pane,
        &stale_token,
        "stale registry target must not misroute to another live leader",
    );
    assert_eq!(
        message_count(&stale.workspace, &stale_token),
        0,
        "E7 RED: registry_stale refusal must not create a target DB row"
    );
}

#[test]
#[serial(env)]
#[file_serial(tmux)]
fn e7_send_to_leader_queues_e6_mailbox_when_team_live_but_leader_unattached() {
    let home = temp_home("send-to-leader-mailbox");
    let sender = RuntimeCase::with_home("sender-mailbox", "sender", home.clone());
    sender.seed_state_without_receiver("sender");
    let target = RuntimeCase::with_home("mailbox-target", "mailbox", home.clone());
    target.seed_state_without_receiver("mailbox");
    write_registry_entry(
        &home,
        &target.workspace,
        "mailbox",
        "direct_tmux",
        json!({"session_name": target.session_name, "window_name": "leader", "pane_id": "%stale"}),
        0,
        "seed",
    );

    let token = unique_token("E7_TO_LEADER_MAILBOX");
    let output = sender.run_cli(vec![
        "send".into(),
        "--workspace".into(),
        sender.workspace_arg(),
        "--to-leader".into(),
        "mailbox".into(),
        token.clone(),
        "--sender".into(),
        "e7-test".into(),
        "--json".into(),
    ]);
    let body = json_output(&output, "send --to-leader mailbox --json");
    assert_json_ok(
        &output,
        &body,
        "leader-unattached target should queue, not hard fail",
    );
    assert_eq!(
        body.get("status").and_then(Value::as_str),
        Some("queued_until_leader_attach"),
        "E7 RED: team live + leader unattached must reuse E6 offline mailbox status; output={body}"
    );
    assert_eq!(
        body.get("delivered").and_then(Value::as_bool),
        Some(false),
        "E7 RED: mailbox queue is not physical delivery; delivered must be false; output={body}"
    );
    assert_eq!(
        message_count(&target.workspace, &token),
        1,
        "E7 RED: mailbox path must write exactly one target team.db message row"
    );
}

#[test]
#[serial(env)]
#[file_serial(tmux)]
fn e7_registry_gc_prunes_unbound_stale_entries_without_deleting_live_entries() {
    let home = temp_home("registry-gc");
    let live = RuntimeCase::with_home("gc-live", "live", home.clone());
    let live_pane = live.start_leader_pane("leader");
    live.seed_state_with_receiver("live", &live_pane, 1);
    let live_path = write_registry_entry(
        &home,
        &live.workspace,
        "live",
        "direct_tmux",
        channel_for_pane(&live, &live_pane),
        1,
        "seed",
    );

    let stale = RuntimeCase::with_home("gc-stale", "stale", home.clone());
    stale.seed_state_without_receiver("stale");
    let stale_path = write_registry_entry(
        &home,
        &stale.workspace,
        "stale",
        "direct_tmux",
        json!({"session_name": stale.session_name, "window_name": "leader", "pane_id": "%stale"}),
        0,
        "seed",
    );

    let output = run_cli_with_home(
        &home,
        &live.workspace,
        vec!["leaders".into(), "--json".into()],
    );
    let body = json_output(&output, "leaders --json gc");
    assert_json_ok(
        &output,
        &body,
        "leaders --json should validate and prune terminal stale entries",
    );
    assert!(
        live_path.exists(),
        "E7 RED: registry GC must never delete a canonical-live registry entry; output={body}"
    );
    assert!(
        !stale_path.exists(),
        "E7 RED: registry GC must prune canonical-unbound terminal stale entries while preserving live entries; output={body} stale_path={}",
        stale_path.display()
    );
}

fn assert_tmux_binding_registers(verb: &str) -> Result<(), String> {
    let case = RuntimeCase::new(&format!("bind-{verb}"), "alpha");
    let pane = case.start_leader_pane("leader");
    let second_pane = case.start_extra_pane("new-leader");
    match verb {
        "attach-leader" => {
            case.seed_state_without_receiver("alpha");
            let output = case.run_cli(vec![
                "attach-leader".into(),
                "--workspace".into(),
                case.workspace_arg(),
                "--team".into(),
                "alpha".into(),
                "--pane".into(),
                pane.clone(),
                "--provider".into(),
                "codex".into(),
                "--json".into(),
            ]);
            assert_command_succeeded(&output, "attach-leader")?;
        }
        "claim-leader" => {
            case.seed_state_without_receiver("alpha");
            let output = case.run_cli_with_env(
                vec![
                    "claim-leader".into(),
                    "--workspace".into(),
                    case.workspace_arg(),
                    "--team".into(),
                    "alpha".into(),
                    "--confirm".into(),
                    "--json".into(),
                ],
                &[("TMUX_PANE", pane.clone())],
            );
            assert_command_succeeded(&output, "claim-leader")?;
        }
        "takeover" => {
            case.seed_state_with_receiver("alpha", &pane, 4);
            let output = case.run_cli_with_env(
                vec![
                    "takeover".into(),
                    "--workspace".into(),
                    case.workspace_arg(),
                    "--team".into(),
                    "alpha".into(),
                    "--confirm".into(),
                    "--json".into(),
                ],
                &[("TMUX_PANE", second_pane.clone())],
            );
            assert_command_succeeded(&output, "takeover")?;
        }
        other => panic!("unknown verb {other}"),
    }

    let entries = registry_entries(&case.home);
    if entries.len() != 1 {
        return Err(format!(
            "{verb}: E7 RED expected exactly one registry file after successful binding; entries={entries:?}"
        ));
    }
    let entry = &entries[0].1;
    assert_registry_schema(entry, &case.workspace, "alpha", "direct_tmux", verb);
    if entry
        .pointer("/channel/pane_id")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(format!(
            "{verb}: registry channel must include pane_id; entry={entry}"
        ));
    }
    let state_epoch = state_owner_epoch(&case.workspace, "alpha");
    if entry.get("owner_epoch").and_then(Value::as_u64) != state_epoch {
        return Err(format!(
            "{verb}: registry owner_epoch must equal canonical state owner_epoch; entry={entry}; state_epoch={state_epoch:?}"
        ));
    }
    assert_no_tmp_registry_files_result(&case.home).map_err(|error| format!("{verb}: {error}"))?;
    Ok(())
}

fn assert_command_succeeded(output: &Output, label: &str) -> Result<Value, String> {
    let body = json_output_result(output).map_err(|error| format!("{label}: {error}"))?;
    if !output.status.success() || body.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "{label}: expected exit 0 and ok=true before registry assertion; code={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(body)
}

fn assert_registry_schema(
    entry: &Value,
    workspace: &Path,
    team_key: &str,
    transport_kind: &str,
    source: &str,
) {
    let required = [
        "schema_version",
        "delivery_name",
        "qualified_name",
        "stable_qualified_name",
        "aliases",
        "workspace",
        "workspace_hash",
        "workspace_short",
        "team_key",
        "transport_kind",
        "channel",
        "owner_epoch",
        "attached_at",
        "updated_at",
        "source",
        "status",
    ];
    let missing = required
        .iter()
        .filter(|field| entry.get(**field).is_none())
        .copied()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "E7 RED: registry entry must contain complete schema v1 fields; missing={missing:?}; entry={entry}"
    );
    assert_eq!(entry.get("schema_version").and_then(Value::as_u64), Some(1));
    assert_eq!(
        entry.get("delivery_name").and_then(Value::as_str),
        Some(team_key)
    );
    assert_eq!(
        entry.get("team_key").and_then(Value::as_str),
        Some(team_key)
    );
    assert_eq!(
        entry.get("workspace").and_then(Value::as_str),
        Some(workspace.to_string_lossy().as_ref()),
        "E7 RED: registry workspace must be canonical target workspace; entry={entry}"
    );
    assert_eq!(
        entry.get("workspace_hash").and_then(Value::as_str),
        Some(workspace_hash(workspace).as_str()),
        "E7 RED: registry workspace_hash must be sha256-prefix of canonical workspace; entry={entry}"
    );
    assert_eq!(
        entry.get("transport_kind").and_then(Value::as_str),
        Some(transport_kind),
        "E7 RED: registry transport_kind must match canonical leader channel; entry={entry}"
    );
    assert_eq!(
        entry.get("source").and_then(Value::as_str),
        Some(source),
        "E7 RED: registry source must record the successful binding hook; entry={entry}"
    );
}

fn assert_json_ok(output: &Output, body: &Value, label: &str) {
    assert!(
        output.status.success() && body.get("ok").and_then(Value::as_bool) == Some(true),
        "{label}: expected exit 0 and ok=true; code={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_leader_status(body: &Value, name: &str, status: &str) {
    let entry = leader_by_name(body, name)
        .unwrap_or_else(|| panic!("E7 RED: leaders[] must include {name}; output={body}"));
    assert_eq!(
        entry.get("status").and_then(Value::as_str),
        Some(status),
        "E7 RED: leader {name} must have status {status}; entry={entry}; output={body}"
    );
}

fn assert_ambiguous_name(body: &Value, name: &str, min_candidates: usize) {
    let ambiguous = body
        .get("ambiguous_names")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("E7 RED: leaders JSON must expose ambiguous_names array; output={body}")
        });
    let item = ambiguous.iter().find(|item| {
        item.get("name").and_then(Value::as_str) == Some(name) || item.as_str() == Some(name)
    });
    let Some(item) = item else {
        panic!("E7 RED: ambiguous_names must include {name}; output={body}");
    };
    if let Some(candidates) = item.get("candidates").and_then(Value::as_array) {
        assert!(
            candidates.len() >= min_candidates,
            "E7 RED: ambiguous name {name} must include at least {min_candidates} candidates; item={item}"
        );
    }
}

fn leader_by_name<'a>(body: &'a Value, name: &str) -> Option<&'a Value> {
    body.get("leaders")
        .and_then(Value::as_array)?
        .iter()
        .find(|entry| {
            entry.get("name").and_then(Value::as_str) == Some(name)
                || entry.get("delivery_name").and_then(Value::as_str) == Some(name)
                || entry.get("team_key").and_then(Value::as_str) == Some(name)
        })
}

fn assert_pane_contains(pane: &str, token: &str, label: &str) {
    let text = capture_pane(pane);
    assert!(
        text.contains(token),
        "{label}; pane={pane} capture={text:?}"
    );
}

fn assert_pane_not_contains(pane: &str, token: &str, label: &str) {
    let text = capture_pane(pane);
    assert!(
        !text.contains(token),
        "{label}; pane={pane} token={token} capture={text:?}"
    );
}

fn json_output(output: &Output, label: &str) -> Value {
    json_output_result(output).unwrap_or_else(|error| {
        panic!(
            "{label}: {error}; code={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn json_output_result(output: &Output) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone()).map_err(|error| error.to_string())?;
    if stdout.trim().is_empty() {
        return Err("stdout was empty, expected JSON".to_string());
    }
    serde_json::from_str(stdout.trim())
        .map_err(|error| format!("stdout must be JSON: {error}; stdout={stdout:?}"))
}

fn run_cli_with_home(home: &Path, cwd: &Path, args: Vec<String>) -> Output {
    run_cli_with_home_env(home, cwd, args, &[])
}

fn run_cli_with_home_env(
    home: &Path,
    cwd: &Path,
    args: Vec<String>,
    env_pairs: &[(&str, String)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    command.args(args).current_dir(cwd).env("HOME", home);
    for key in [
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_PROVIDER",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_WORKSPACE",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TMUX",
        "TMUX_PANE",
    ] {
        command.env_remove(key);
    }
    for (key, value) in env_pairs {
        command.env(key, value);
    }
    command.output().expect("run team-agent")
}

fn registry_entries(home: &Path) -> Vec<(PathBuf, Value)> {
    let dir = registry_dir(home);
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .map(|path| {
            let value = serde_json::from_str::<Value>(
                &std::fs::read_to_string(&path).expect("read registry entry"),
            )
            .expect("registry entry json");
            (path, value)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn assert_no_tmp_registry_files(home: &Path) {
    assert_no_tmp_registry_files_result(home).expect("no tmp registry files");
}

fn assert_no_tmp_registry_files_result(home: &Path) -> Result<(), String> {
    let dir = registry_dir(home);
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let tmp_files = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".tmp") || name.ends_with(".tmp"))
        })
        .collect::<Vec<_>>();
    if tmp_files.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "E7 RED: registry writes must use temp+rename atomically and leave no .tmp residual files; tmp_files={tmp_files:?}"
        ))
    }
}

fn write_registry_entry(
    home: &Path,
    workspace: &Path,
    team_key: &str,
    transport_kind: &str,
    channel: Value,
    owner_epoch: u64,
    source: &str,
) -> PathBuf {
    let dir = registry_dir(home);
    std::fs::create_dir_all(&dir).expect("create registry dir");
    let hash = workspace_hash(workspace);
    let workspace_short = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let entry = json!({
        "schema_version": 1,
        "delivery_name": team_key,
        "qualified_name": format!("{workspace_short}/{team_key}"),
        "stable_qualified_name": format!("{hash}/{team_key}"),
        "aliases": [],
        "workspace": workspace.to_string_lossy().to_string(),
        "workspace_hash": hash,
        "workspace_short": workspace_short,
        "team_key": team_key,
        "transport_kind": transport_kind,
        "channel": channel,
        "owner_epoch": owner_epoch,
        "attached_at": "2026-07-07T00:00:00Z",
        "updated_at": "2026-07-07T00:00:00Z",
        "source": source,
        "status": "attached"
    });
    let path = dir.join(format!("{}__{}.json", workspace_hash(workspace), team_key));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&entry).expect("serialize registry"),
    )
    .expect("write registry entry");
    path
}

fn registry_dir(home: &Path) -> PathBuf {
    home.join(".team-agent").join("leaders")
}

fn workspace_hash(workspace: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace).expect("canonical workspace");
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn state_owner_epoch(workspace: &Path, team_key: &str) -> Option<u64> {
    let state = team_agent::state::persist::load_runtime_state(workspace).ok()?;
    state
        .pointer(&format!("/teams/{team_key}/owner_epoch"))
        .or_else(|| state.get("owner_epoch"))
        .and_then(Value::as_u64)
}

fn message_count(workspace: &Path, token: &str) -> i64 {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return 0;
    }
    let Ok(conn) = Connection::open(db) else {
        return 0;
    };
    conn.query_row(
        "select count(*) from messages where content like ?1",
        [format!("%{token}%")],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

fn channel_for_pane(case: &RuntimeCase, pane: &str) -> Value {
    json!({
        "pane_id": pane,
        "session_name": case.session_name,
        "window_name": "leader",
        "tmux_socket": case.tmux_socket().unwrap_or_else(|| "default".to_string())
    })
}

fn capture_pane(pane: &str) -> String {
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn temp_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("ta-e7-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp home root");
    root.join("home")
}

fn unique_token(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{}", std::process::id(), n)
}

struct RuntimeCase {
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    session_name: String,
}

impl RuntimeCase {
    fn new(tag: &str, team_key: &str) -> Self {
        Self::with_home(tag, team_key, temp_home(tag))
    }

    fn with_home(tag: &str, _team_key: &str, home: PathBuf) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("ta-e7-case-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create case root");
        std::fs::create_dir_all(&home).expect("create home");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let workspace = std::fs::canonicalize(workspace).expect("canonical workspace");
        let session_name = format!(
            "ta-e7-{}-{}-{}",
            std::process::id(),
            n,
            tag.chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
                .collect::<String>()
        );
        Self {
            root,
            home,
            workspace,
            session_name,
        }
    }

    fn workspace_arg(&self) -> String {
        self.workspace.to_string_lossy().to_string()
    }

    fn run_cli(&self, args: Vec<String>) -> Output {
        run_cli_with_home(&self.home, &self.workspace, args)
    }

    fn run_cli_with_env(&self, args: Vec<String>, env_pairs: &[(&str, String)]) -> Output {
        run_cli_with_home_env(&self.home, &self.workspace, args, env_pairs)
    }

    fn start_leader_pane(&self, window: &str) -> String {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .output();
        let program = self.provider_program();
        let cwd = self.workspace_arg();
        let output = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &self.session_name,
                "-n",
                window,
                "-c",
                &cwd,
                &program,
            ])
            .output()
            .expect("tmux new-session");
        assert!(
            output.status.success(),
            "tmux new-session failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.pane_id_for(window)
    }

    fn start_extra_pane(&self, window: &str) -> String {
        let program = self.provider_program();
        let cwd = self.workspace_arg();
        let output = Command::new("tmux")
            .args([
                "new-window",
                "-d",
                "-t",
                &self.session_name,
                "-n",
                window,
                "-c",
                &cwd,
                &program,
            ])
            .output()
            .expect("tmux new-window");
        assert!(
            output.status.success(),
            "tmux new-window failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.pane_id_for(window)
    }

    fn pane_id_for(&self, window: &str) -> String {
        let target = format!("{}:{window}", self.session_name);
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", &target, "#{pane_id}"])
            .output()
            .expect("tmux display-message");
        assert!(
            output.status.success(),
            "tmux display-message failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn provider_program(&self) -> String {
        let bin = self.root.join("bin");
        std::fs::create_dir_all(&bin).expect("create provider bin dir");
        let codex = bin.join("codex");
        if !codex.exists() {
            std::os::unix::fs::symlink("/bin/cat", &codex).expect("symlink codex provider stub");
        }
        codex.to_string_lossy().to_string()
    }

    fn tmux_socket(&self) -> Option<String> {
        let output = Command::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                &self.session_name,
                "#{socket_path}",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn kill_session(&self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .output();
    }

    fn seed_state_without_receiver(&self, team_key: &str) {
        self.seed_state(team_key, None, 0, "alive");
    }

    fn seed_state_with_receiver(&self, team_key: &str, pane: &str, owner_epoch: u64) {
        self.seed_state(team_key, Some(pane), owner_epoch, "alive");
    }

    fn seed_state_with_receiver_status(
        &self,
        team_key: &str,
        pane: &str,
        owner_epoch: u64,
        status: &str,
    ) {
        self.seed_state(team_key, Some(pane), owner_epoch, status);
    }

    fn seed_state(&self, team_key: &str, pane: Option<&str>, owner_epoch: u64, status: &str) {
        let receiver = pane.map(|pane| {
            let tmux_socket = self.tmux_socket().unwrap_or_else(|| "default".to_string());
            json!({
                "pane_id": pane,
                "provider": "codex",
                "session_name": self.session_name,
                "window_name": "leader",
                "tmux_socket": tmux_socket,
                "transport_kind": "direct_tmux",
                "owner_epoch": owner_epoch
            })
        });
        let tmux_socket = self.tmux_socket();
        let owner = pane.map(|pane| {
            json!({
                "pane_id": pane,
                "provider": "codex",
                "owner_epoch": owner_epoch,
                "claimed_via": "claim-leader"
            })
        });
        let mut team = json!({
            "team_key": team_key,
            "status": status,
            "session_name": self.session_name,
            "team_dir": self.workspace_arg(),
            "workspace": self.workspace_arg(),
            "agents": {
                "worker_one": {
                    "provider": "fake",
                    "status": "idle"
                }
            }
        });
        if let Some(socket) = tmux_socket.clone() {
            team["tmux_socket"] = json!(socket);
        }
        if let Some(receiver) = receiver.clone() {
            team["leader_receiver"] = receiver;
            team["owner_epoch"] = json!(owner_epoch);
        }
        if let Some(owner) = owner.clone() {
            team["team_owner"] = owner;
        }
        let mut state = json!({
            "active_team_key": team_key,
            "team_key": team_key,
            "status": status,
            "session_name": self.session_name,
            "team_dir": self.workspace_arg(),
            "workspace": self.workspace_arg(),
            "transport": {"kind": "tmux"},
            "agents": {
                "worker_one": {
                    "provider": "fake",
                    "status": "idle"
                }
            },
            "teams": {
                team_key: team
            }
        });
        if let Some(socket) = tmux_socket {
            state["tmux_socket"] = json!(socket);
        }
        if let Some(receiver) = receiver {
            state["leader_receiver"] = receiver;
            state["owner_epoch"] = json!(owner_epoch);
        }
        if let Some(owner) = owner {
            state["team_owner"] = owner;
        }
        team_agent::state::persist::save_runtime_state(&self.workspace, &state)
            .expect("save runtime state");
    }
}

impl Drop for RuntimeCase {
    fn drop(&mut self) {
        self.kill_session();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
