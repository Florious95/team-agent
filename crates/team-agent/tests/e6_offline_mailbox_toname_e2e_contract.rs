//! E6 RED e2e contract: real CLI offline mailbox for `send --to-name <team>/leader`.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E6.
//! - design `.team/artifacts/offline-mailbox-toname-design.md` §§3, 6, 8, 11.
//! - real-machine escape evidence:
//!   `.team/evidence/0.5.9-subscription-gate-20260707T143241Z-4645/`.
//!
//! Contract: with a real tmux workspace, live fake worker, running coordinator,
//! and no attached leader, a third-party `send --to-name <ws>::<key>/leader`
//! queues one target `team.db` row as `queued_until_leader_attach`. Later
//! `attach-leader` replays that same message id exactly once to the new leader
//! pane.
//!
//! ---
//! purpose: E6 real-CLI mailbox contract with an isolated tmux fixture
//! contract:
//!   provides:
//!     - hermetic workspace, HOME, and exact tmux endpoint ownership
//!   depends:
//!     - tests/support/hermetic.rs
//! boundary:
//!   - never uses ambient serial-test locks or the default tmux server
//! arch:
//!   allowed_dependencies:
//!     - rusqlite
//!     - serde_json
//!     - serial_test
//!     - std
//!   read_closure: [messaging, coordinator]
//!   unresolved_disposition:
//!     - rule: unanchored_relative_path
//!       scope: task-scoped test-helper calls only
//!       disposition: bounded_unknown_no_production_edge
//!       rationale: required row/event/target/receipt probes remain test-local;
//!         the base/head stable-symbol set is compared in CR6 evidence.
//!   named_test_only_anchors:
//!     - OpenOptions::new: immutable process-owned evidence writer
//!     - serde_json::to_vec_pretty: deterministic failure-bundle encoding
//!     - std::env::var_os: optional evidence-dir override; default is process-owned
//!     - std::env::current_exe: child-process cleanup tooth launcher
//!     - std::fs::remove_file/remove_dir: exact cleanup of the process-owned bundle
//! maturity: wired
//! ---

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use serial_test::serial;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::{short_tmux_socket, HermeticTestEnv};

static COUNTER: AtomicU64 = AtomicU64::new(0);
const COMPILED_GATE_SHA: Option<&str> = option_env!("TEAM_AGENT_GATE_SHA");

#[test]
#[serial(env)]
fn e6_real_cli_live_team_unattached_leader_queues_then_attach_replays_once() {
    let case = E6Case::new("real-cli-mailbox");
    case.write_fake_team("twitter-autopub", "fake-worker");

    let quick_start = case.run_cli(
        case.target_workspace(),
        vec![
            "quick-start".into(),
            case.team_dir_arg(),
            "--workspace".into(),
            case.target_workspace_arg(),
            "--team".into(),
            case.team_key.clone(),
            "--no-display".into(),
            "--yes".into(),
            "--json".into(),
        ],
    );
    let quick_json = json_output(&quick_start, "quick-start fake team");
    assert_eq!(
        quick_json
            .pointer("/readiness/all_workers_spawned")
            .and_then(Value::as_bool),
        Some(true),
        "E6 e2e RED setup: fake worker must be spawned so target team is live even though quick-start may exit nonzero for leader_receiver_unbound; code={:?} output={quick_json}",
        quick_start.status.code()
    );

    let status = case.run_cli(
        case.target_workspace(),
        vec![
            "status".into(),
            "--workspace".into(),
            case.target_workspace_arg(),
            "--team".into(),
            case.team_key.clone(),
            "--detail".into(),
            "--json".into(),
        ],
    );
    let status_json = json_output(&status, "status after quick-start");
    assert_eq!(
        status_json
            .pointer("/coordinator/ok")
            .and_then(Value::as_bool),
        Some(true),
        "E6 e2e RED setup: coordinator must be running for the target team; status={status_json}"
    );
    assert_eq!(
        status_json
            .get("tmux_session_present")
            .and_then(Value::as_bool),
        Some(true),
        "E6 e2e RED setup: target tmux session must be live; status={status_json}"
    );
    assert!(
        status_json.get("leader_receiver").is_none()
            || status_json.get("leader_receiver").is_some_and(|v| {
                v.as_object()
                    .map(|object| object.is_empty())
                    .unwrap_or(false)
            }),
        "E6 e2e RED setup: leader must never have been attached before mailbox send; status={status_json}"
    );

    let token = unique_token("E6_REAL_CLI_MAILBOX");
    let to_name = format!(
        "{}::{}/leader",
        case.target_workspace().display(),
        case.team_key
    );
    let send = case.run_cli_as(
        case.sender_workspace(),
        vec![
            "send".into(),
            "--workspace".into(),
            case.sender_workspace_arg(),
            "--to-name".into(),
            to_name,
            token.clone(),
            "--json".into(),
        ],
        "third-party",
    );
    let body = json_output(&send, "third-party send --to-name unattached leader");
    let rows_after_send = message_rows(case.target_workspace(), &token);

    assert!(
        send.status.success() && body.get("ok").and_then(Value::as_bool) == Some(true),
        "E6 e2e RED: live target team with unattached leader must queue mailbox, not hard fail. \
         Expected ok=true/status=queued_until_leader_attach/message_id; got code={:?} output={body}; \
         rows_after_send={rows_after_send:?}; stderr={}",
        send.status.code(),
        String::from_utf8_lossy(&send.stderr)
    );
    assert_eq!(
        body.get("status").and_then(Value::as_str),
        Some("deferred"),
        "E6 e2e RED: unattached leader send must return presenter status=deferred; output={body}"
    );
    assert_eq!(
        body.get("deferred_reason").and_then(Value::as_str),
        Some("never_attached"),
        "E6 e2e RED: unattached leader send must explain deferred_reason=never_attached; output={body}"
    );
    assert_eq!(
        body.get("message_status").and_then(Value::as_str),
        Some("queued_until_leader_attach"),
        "E6 e2e RED: message_status must honestly stay queued until attach; output={body}"
    );
    assert_eq!(
        body.get("channel").and_then(Value::as_str),
        Some("leader_mailbox"),
        "E6 e2e RED: queued mailbox must identify channel=leader_mailbox; output={body}"
    );
    assert_eq!(
        body.get("delivered").and_then(Value::as_bool),
        Some(false),
        "E6 e2e RED: mailbox queue is not physical delivery; delivered must be false; output={body}"
    );
    let message_id = body
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("E6 e2e RED: queued mailbox response must include message_id; output={body}")
        })
        .to_string();

    assert_eq!(
        rows_after_send.len(),
        1,
        "E6 e2e RED: queued mailbox must create exactly one target team.db row; rows={rows_after_send:?}; output={body}"
    );
    let row = &rows_after_send[0];
    assert_eq!(
        row.message_id, message_id,
        "E6 e2e RED: DB row message_id must match CLI response"
    );
    assert_eq!(
        row.owner_team_id.as_deref(),
        Some(case.team_key.as_str()),
        "E6 e2e RED: mailbox row owner_team_id must be target runtime key"
    );
    assert_eq!(
        row.recipient.as_deref(),
        Some("leader"),
        "E6 e2e RED: mailbox row recipient must be leader"
    );
    assert_eq!(
        row.status.as_deref(),
        Some("queued_until_leader_attach"),
        "E6 e2e RED: mailbox row status must be queued_until_leader_attach before attach"
    );
    assert_eq!(
        row.delivery_attempts, 0,
        "E6 e2e RED: queued mailbox must not be claimed/delivered before leader attach"
    );
    assert_eq!(
        row.error, None,
        "E6 e2e RED: queued mailbox must not carry a failure before replay"
    );
    assert_eq!(
        row.delivered_at, None,
        "E6 e2e RED: queued mailbox must not carry a delivery timestamp before replay"
    );
    assert_event_for_message(
        case.target_workspace(),
        "leader_mailbox.queued_until_attach",
        &message_id,
    );

    let state = runtime_state(case.target_workspace());
    let session_name = state
        .pointer(&format!("/teams/{}/session_name", case.team_key))
        .and_then(Value::as_str)
        .expect("session name in state")
        .to_string();
    let tmux_socket = state
        .pointer(&format!("/teams/{}/tmux_socket", case.team_key))
        .or_else(|| state.pointer(&format!("/teams/{}/tmux_endpoint", case.team_key)))
        .and_then(Value::as_str)
        .expect("tmux socket in state")
        .to_string();
    let pane = case.start_leader_pane(&tmux_socket, &session_name);

    let attach = case.run_cli(
        case.target_workspace(),
        vec![
            "attach-leader".into(),
            "--workspace".into(),
            case.target_workspace_arg(),
            "--team".into(),
            case.team_key.clone(),
            "--pane".into(),
            pane.clone(),
            "--provider".into(),
            "fake".into(),
            "--confirm".into(),
            "--json".into(),
        ],
    );
    let attach_json = json_output(&attach, "attach-leader after mailbox queue");
    assert!(
        attach.status.success() && attach_json.get("ok").and_then(Value::as_bool) == Some(true),
        "E6 e2e RED setup: attach-leader must succeed so queued mailbox can replay; code={:?} output={attach_json} stderr={}",
        attach.status.code(),
        String::from_utf8_lossy(&attach.stderr)
    );

    let attached_state = runtime_state(case.target_workspace());
    let receiver = attached_state
        .pointer(&format!("/teams/{}/leader_receiver", case.team_key))
        .expect("attached leader receiver in state");
    assert_eq!(
        receiver.get("status").and_then(Value::as_str),
        Some("attached")
    );
    assert_eq!(
        receiver.get("pane_id").and_then(Value::as_str),
        Some(pane.as_str())
    );
    assert_eq!(
        receiver.get("session_name").and_then(Value::as_str),
        Some(session_name.as_str())
    );
    assert_eq!(
        receiver.get("tmux_socket").and_then(Value::as_str),
        Some(tmux_socket.as_str())
    );
    assert_eq!(
        receiver.get("window_name").and_then(Value::as_str),
        Some("leader")
    );
    assert_eq!(
        receiver.get("window_index").and_then(Value::as_str),
        Some("1")
    );
    assert!(
        receiver
            .get("pane_tty")
            .and_then(Value::as_str)
            .is_some_and(|tty| !tty.is_empty()),
        "E6 e2e RED: attached target must persist pane tty; receiver={receiver}"
    );

    // Injection-case revision (MUST-10 boundary): attach must wake the mailbox
    // and physically inject, but a physical submit is not a provider receipt —
    // the SAME row parks as injected_awaiting_receipt, never jumps straight
    // to delivered on transport success alone.
    let expected_status = if std::env::var_os("E6_FORCE_TERMINAL_STATUS").is_some() {
        "__forced_unexpected_status__"
    } else {
        "injected_awaiting_receipt"
    };
    let accepted = wait_for_message_status(
        case.target_workspace(),
        &message_id,
        expected_status,
        &case.team_key,
        &tmux_socket,
        &pane,
        &token,
        case.durable_evidence_dir(),
    );
    assert!(
        accepted,
        "E6 e2e RED: attach-leader must requeue the same message_id={message_id} and park it as \
         injected_awaiting_receipt after the physical inject; rows={:?}",
        message_rows(case.target_workspace(), &token)
    );
    let pane_text = wait_for_pane_token(&tmux_socket, &pane, &token);
    let token_count = pane_text.matches(&token).count();
    assert_eq!(
        token_count, 1,
        "E6 e2e RED: attach replay must inject queued mailbox token exactly once; pane={pane} token={token} count={token_count} capture={pane_text:?}"
    );
    let rows_after_attach = message_rows(case.target_workspace(), &token);
    assert_eq!(
        rows_after_attach.len(),
        1,
        "E6 e2e RED: attach replay must reuse the same row, not create duplicates; rows={rows_after_attach:?}"
    );
    assert_eq!(
        rows_after_attach[0].message_id, message_id,
        "E6 e2e RED: attach replay must preserve the original queued message_id"
    );
    let row = &rows_after_attach[0];
    assert_eq!(
        row.delivery_attempts, 1,
        "E6 e2e RED: attach replay must have exactly one winning claim; row={row:?}"
    );
    assert_eq!(
        row.error.as_deref(),
        Some("leader_receipt_source_unavailable"),
        "E6 e2e RED: receipt-source failure must be persisted on the same row; row={row:?}"
    );
    assert_eq!(
        row.delivered_at, None,
        "E6 e2e RED: physical injection must not infer a provider receipt; row={row:?}"
    );
    let receipt = delivery_token(case.target_workspace(), &message_id)
        .expect("delivery token for physically injected message");
    assert_eq!(receipt.unique_token, message_id);
    assert!(
        !receipt.injected_at.is_empty(),
        "delivery token must persist inject time"
    );
    assert_eq!(
        receipt.consumed_at, None,
        "source-unavailable receipt cannot be consumed"
    );
    assert_eq!(
        receipt.failed_at, None,
        "source-unavailable is not an inject failure"
    );
    assert_eq!(
        receipt.failure_reason, None,
        "source-unavailable has its own typed boundary"
    );
    assert_event_for_message(
        case.target_workspace(),
        "leader_receiver.receipt_source_unavailable",
        &message_id,
    );
}

#[test]
fn e6_failure_bundle_survives_normal_cleanup_without_override() {
    let executable = std::env::current_exe().expect("current e6 test executable");
    let output = Command::new(executable)
        .args([
            "--exact",
            "e6_real_cli_live_team_unattached_leader_queues_then_attach_replays_once",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_remove("E6_DURABLE_EVIDENCE_DIR")
        .env_remove("TEAM_AGENT_KEEP_TEST_TMP")
        .env_remove("TEAM_AGENT_KEEP_TEST_PROCESSES")
        .env("E6_FORCE_TERMINAL_STATUS", "1")
        .output()
        .expect("run forced child e6 test");
    assert!(
        !output.status.success(),
        "forced terminal child must fail so ordinary unwind cleanup runs; output={:?}",
        output
    );
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let path = transcript
        .lines()
        .find_map(|line| {
            line.split_once("durable evidence=")
                .map(|(_, path)| path.trim())
        })
        .map(PathBuf::from)
        .expect("forced child must print durable bundle path");
    assert!(
        path.is_absolute() && path.exists(),
        "bundle must survive ordinary child cleanup: {} transcript={transcript}",
        path.display()
    );
    let first = std::fs::read_to_string(&path).expect("read surviving bundle");
    let digest = sha256_file(&path);
    let digest_again = sha256_file(&path);
    let second = std::fs::read_to_string(&path).expect("read surviving bundle twice");
    assert_eq!(first, second, "bundle bytes changed after child cleanup");
    assert_eq!(
        digest, digest_again,
        "bundle digest changed after child cleanup"
    );
    assert_eq!(
        digest.len(),
        64,
        "surviving bundle must have a SHA-256 digest"
    );
    let bundle: Value =
        serde_json::from_str(&first).expect("surviving bundle must remain valid JSON");
    validate_e6_bundle(&bundle).expect("surviving E6 bundle schema");
    let mut mutation_notes = Vec::new();
    let mut receipt_removed = bundle.clone();
    receipt_removed["receipt_state"] = Value::Null;
    mutation_notes.push(format!(
        "mutation=receipt-removal red={} restored={}",
        validate_e6_bundle(&receipt_removed).is_err(),
        validate_e6_bundle(&bundle).is_ok()
    ));
    mutation_notes.push("mutation=cleanup-before-read red=true restored=true".to_owned());
    let reviewer = write_reviewer_artifacts("e6", &first, &digest, &mutation_notes);
    assert!(reviewer.0.exists() && reviewer.1.exists() && reviewer.2.exists());
    let directory = path.parent().expect("bundle parent").to_path_buf();
    std::fs::remove_file(&path).expect("exact cleanup of durable bundle");
    assert!(
        std::fs::read_to_string(&path).is_err(),
        "cleanup-before-read mutation must be red"
    );
    assert!(
        serde_json::from_slice::<Value>(&std::fs::read(&reviewer.0).expect("retained E6 body"))
            .is_ok(),
        "retained E6 body must restore readability after cleanup"
    );
    std::fs::remove_dir(&directory).expect("exact cleanup of durable evidence directory");
    eprintln!(
        "E6 durable bundle verified after ordinary cleanup: path={} sha256={}",
        path.display(),
        digest
    );
    assert!(!path.exists(), "exact durable bundle cleanup must complete");
    assert!(
        !directory.exists(),
        "exact durable directory cleanup must complete"
    );
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn unique_token(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{}", std::process::id(), n)
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256", &path.to_string_lossy()])
        .output()
        .expect("shasum bundle");
    assert!(
        output.status.success(),
        "digest probe must succeed for durable bundle; code={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn json_output(output: &Output, label: &str) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout utf8");
    let trimmed = stdout.trim();
    assert!(
        !trimmed.is_empty(),
        "{label}: expected JSON stdout; code={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let json_start = trimmed.find('{').unwrap_or(0);
    serde_json::from_str(&trimmed[json_start..]).unwrap_or_else(|error| {
        panic!(
            "{label}: stdout must contain JSON object: {error}; stdout={stdout:?}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn runtime_state(workspace: &Path) -> Value {
    let path = workspace.join(".team/runtime/state.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read state.json"))
        .expect("state json")
}

#[derive(Debug)]
struct MessageRow {
    message_id: String,
    owner_team_id: Option<String>,
    recipient: Option<String>,
    status: Option<String>,
    delivery_attempts: i64,
    delivered_at: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
struct DeliveryToken {
    unique_token: String,
    injected_at: String,
    consumed_at: Option<String>,
    failed_at: Option<String>,
    failure_reason: Option<String>,
}

fn message_rows(workspace: &Path, token: &str) -> Vec<MessageRow> {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return Vec::new();
    }
    let conn = Connection::open(db).expect("open team.db");
    let mut stmt = conn
        .prepare(
            "select message_id, owner_team_id, recipient, status, delivery_attempts, delivered_at, error \
             from messages where content like ?1 order by created_at",
        )
        .expect("prepare message query");
    stmt.query_map([format!("%{token}%")], |row| {
        Ok(MessageRow {
            message_id: row.get(0)?,
            owner_team_id: row.get(1)?,
            recipient: row.get(2)?,
            status: row.get(3)?,
            delivery_attempts: row.get(4)?,
            delivered_at: row.get(5)?,
            error: row.get(6)?,
        })
    })
    .expect("query messages")
    .map(|row| row.expect("message row"))
    .collect()
}

fn delivery_token(workspace: &Path, message_id: &str) -> Option<DeliveryToken> {
    let db = workspace.join(".team/runtime/team.db");
    let conn = Connection::open(db).expect("open team.db");
    conn.query_row(
        "select unique_token, injected_at, consumed_at, failed_at, failure_reason \
         from delivery_tokens where message_id = ?1",
        [message_id],
        |row| {
            Ok(DeliveryToken {
                unique_token: row.get(0)?,
                injected_at: row.get(1)?,
                consumed_at: row.get(2)?,
                failed_at: row.get(3)?,
                failure_reason: row.get(4)?,
            })
        },
    )
    .optional()
    .expect("query delivery token")
}

fn assert_event_for_message(workspace: &Path, event_name: &str, message_id: &str) {
    let path = workspace.join(".team/logs/events.jsonl");
    let matching = std::fs::read_to_string(&path)
        .expect("read event log")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event.get("event").and_then(Value::as_str) == Some(event_name)
                && event.get("message_id").and_then(Value::as_str) == Some(message_id)
        });
    assert!(
        matching.is_some(),
        "E6 e2e RED: expected durable {event_name} event bound to message_id={message_id}"
    );
}

fn query_message_status(workspace: &Path, message_id: &str) -> Result<Option<String>, String> {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db).map_err(|error| format!("open {}: {error}", db.display()))?;
    conn.query_row(
        "select status from messages where message_id = ?1",
        [message_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| format!("query status for {message_id}: {error}"))
}

fn wait_for_message_status(
    workspace: &Path,
    message_id: &str,
    status: &str,
    team_key: &str,
    tmux_socket: &str,
    pane: &str,
    token: &str,
    durable_evidence_dir: &Path,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_status = None;
    while Instant::now() < deadline {
        match query_message_status(workspace, message_id) {
            Ok(Some(current)) if current == status => return true,
            Ok(Some(current)) if terminal_status(&current) => {
                let evidence = emit_replay_failure_bundle(
                    workspace,
                    message_id,
                    team_key,
                    tmux_socket,
                    pane,
                    "terminal_status_not_expected",
                    Some(&current),
                    None,
                    token,
                    durable_evidence_dir,
                );
                panic!(
                    "E6 replay terminal status {current:?} was not expected; durable evidence={evidence}"
                );
            }
            Ok(Some(current)) => last_status = Some(current),
            Ok(None) => last_status = None,
            Err(error) => {
                let evidence = emit_replay_failure_bundle(
                    workspace,
                    message_id,
                    team_key,
                    tmux_socket,
                    pane,
                    "status_query_error",
                    last_status.as_deref(),
                    Some(&error),
                    token,
                    durable_evidence_dir,
                );
                panic!(
                    "E6 replay status query failed before assertion: {error}; durable evidence={evidence}"
                );
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    let evidence = emit_replay_failure_bundle(
        workspace,
        message_id,
        team_key,
        tmux_socket,
        pane,
        "status_timeout_non_expected",
        last_status.as_deref(),
        None,
        token,
        durable_evidence_dir,
    );
    panic!(
        "E6 replay did not reach expected status {status:?}; last_status={last_status:?}; durable evidence={evidence}"
    );
}

fn terminal_status(status: &str) -> bool {
    matches!(
        status,
        "failed" | "delivered" | "injected_awaiting_receipt" | "rejected" | "cancelled"
    )
}

fn emit_replay_failure_bundle(
    workspace: &Path,
    message_id: &str,
    team_key: &str,
    tmux_socket: &str,
    pane: &str,
    reason: &str,
    observed_status: Option<&str>,
    status_query_error: Option<&str>,
    token: &str,
    durable_evidence_dir: &Path,
) -> String {
    let row = query_message_value(workspace, message_id);
    let receipt = query_delivery_token_value(workspace, message_id);
    let events = query_event_values(workspace, message_id);
    let physical_target = query_physical_target(workspace, team_key);
    let capture = query_capture_value(tmux_socket, pane, token);
    let attempt = row.get("delivery_attempts").cloned().unwrap_or(Value::Null);
    let delivered_at = row.get("delivered_at").cloned().unwrap_or(Value::Null);
    let coordinator = query_coordinator_value(workspace);
    let owned_resources = query_owned_resource_ledger(workspace, tmux_socket, pane);
    let bundle = json!({
        "schema": "team-agent/e6-replay-failure-v2",
        "message_id": message_id,
        "candidate_sha": gate_authoritative_sha(),
        "reason": reason,
        "observed_status": observed_status,
        "status_query_error": status_query_error,
        "failure_classification": classify_replay_failure(reason, status_query_error),
        "row": row,
        "error": row.get("error").cloned().unwrap_or(Value::Null),
        "events": events,
        "physical_target": physical_target,
        "receipt": receipt,
        "capture": capture,
        "attempt": attempt,
        "token_count": capture.get("token_count").cloned().unwrap_or(Value::Null),
        "expected_status": "injected_awaiting_receipt",
        "delivered_at": delivered_at,
        "target": physical_target,
        "receipt_state": receipt,
        "resource_ledger": owned_resources,
        "coordinator": coordinator,
        "owned_resource_ledger": owned_resources,
    });
    write_immutable_bundle(durable_evidence_dir, message_id, &bundle)
        .unwrap_or_else(|error| panic!("E6 replay could not persist failure evidence: {error}"))
}

fn query_message_value(workspace: &Path, message_id: &str) -> Value {
    let db = workspace.join(".team/runtime/team.db");
    let result = (|| -> Result<Value, String> {
        let conn =
            Connection::open(&db).map_err(|error| format!("open {}: {error}", db.display()))?;
        let row = conn
            .query_row(
                "select message_id, owner_team_id, recipient, status, delivery_attempts, delivered_at, error \
                 from messages where message_id = ?1",
                [message_id],
                |row| {
                    Ok(json!({
                        "message_id": row.get::<_, String>(0)?,
                        "owner_team_id": row.get::<_, Option<String>>(1)?,
                        "recipient": row.get::<_, Option<String>>(2)?,
                        "status": row.get::<_, Option<String>>(3)?,
                        "delivery_attempts": row.get::<_, i64>(4)?,
                        "delivered_at": row.get::<_, Option<String>>(5)?,
                        "error": row.get::<_, Option<String>>(6)?,
                    }))
                },
            )
            .optional()
            .map_err(|error| format!("query row for {message_id}: {error}"))?;
        Ok(row.unwrap_or(Value::Null))
    })();
    result.unwrap_or_else(|error| json!({"query_error": error}))
}

fn query_delivery_token_value(workspace: &Path, message_id: &str) -> Value {
    let db = workspace.join(".team/runtime/team.db");
    let result = (|| -> Result<Value, String> {
        let conn =
            Connection::open(&db).map_err(|error| format!("open {}: {error}", db.display()))?;
        let row = conn
            .query_row(
                "select unique_token, injected_at, consumed_at, failed_at, failure_reason \
                 from delivery_tokens where message_id = ?1",
                [message_id],
                |row| {
                    Ok(json!({
                        "unique_token": row.get::<_, String>(0)?,
                        "injected_at": row.get::<_, String>(1)?,
                        "consumed_at": row.get::<_, Option<String>>(2)?,
                        "failed_at": row.get::<_, Option<String>>(3)?,
                        "failure_reason": row.get::<_, Option<String>>(4)?,
                    }))
                },
            )
            .optional()
            .map_err(|error| format!("query receipt for {message_id}: {error}"))?;
        Ok(row.unwrap_or(Value::Null))
    })();
    result.unwrap_or_else(|error| json!({"query_error": error}))
}

fn query_event_values(workspace: &Path, message_id: &str) -> Value {
    let path = workspace.join(".team/logs/events.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({"path": path, "events": [], "read_error": "event log unavailable"});
    };
    let mut events = Vec::new();
    let mut malformed_lines = 0;
    for line in text.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(event) if event.get("message_id").and_then(Value::as_str) == Some(message_id) => {
                events.push(event);
            }
            Ok(_) => {}
            Err(_) => malformed_lines += 1,
        }
    }
    json!({"path": path, "events": events, "malformed_lines": malformed_lines})
}

fn query_physical_target(workspace: &Path, team_key: &str) -> Value {
    let path = workspace.join(".team/runtime/state.json");
    let result = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))
        .and_then(|text| {
            serde_json::from_str::<Value>(&text)
                .map_err(|error| format!("parse {}: {error}", path.display()))
        });
    match result {
        Ok(state) => state
            .pointer(&format!("/teams/{team_key}/leader_receiver"))
            .cloned()
            .unwrap_or_else(|| json!({"missing": "leader_receiver"})),
        Err(error) => json!({"query_error": error}),
    }
}

fn query_capture_value(tmux_socket: &str, pane: &str, token: &str) -> Value {
    match Command::new("tmux")
        .args(["-S", tmux_socket, "capture-pane", "-p", "-t", pane])
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            json!({
                "socket": tmux_socket,
                "pane": pane,
                "exit_code": output.status.code(),
                "stdout": stdout,
                "token_count": stdout.matches(token).count(),
                "stderr": String::from_utf8_lossy(&output.stderr),
            })
        }
        Err(error) => {
            json!({"socket": tmux_socket, "pane": pane, "spawn_error": error.to_string()})
        }
    }
}

fn gate_authoritative_sha() -> String {
    let sha = std::env::var("TEAM_AGENT_GATE_SHA")
        .expect("TEAM_AGENT_GATE_SHA must contain the gate-tested composition SHA");
    assert!(
        sha.len() == 40
            && sha
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
        "TEAM_AGENT_GATE_SHA must be exactly 40 lowercase hex characters: {sha:?}"
    );
    assert_eq!(
        COMPILED_GATE_SHA,
        Some(sha.as_str()),
        "TEAM_AGENT_GATE_SHA changed after test compilation"
    );
    sha
}

fn validate_e6_bundle(bundle: &Value) -> Result<(), String> {
    if bundle.get("schema").and_then(Value::as_str) != Some("team-agent/e6-replay-failure-v2") {
        return Err("schema".into());
    }
    if bundle.get("candidate_sha").and_then(Value::as_str)
        != Some(gate_authoritative_sha().as_str())
    {
        return Err("candidate_sha".into());
    }
    if bundle.get("message_id").and_then(Value::as_str).is_none()
        || bundle.get("attempt").and_then(Value::as_i64) != Some(1)
        || bundle.get("token_count").and_then(Value::as_u64) != Some(1)
        || bundle.get("expected_status").and_then(Value::as_str)
            != Some("injected_awaiting_receipt")
        || !bundle.get("delivered_at").is_some_and(Value::is_null)
        || !bundle.get("target").is_some()
        || !bundle.get("receipt_state").is_some_and(Value::is_object)
        || !bundle.get("resource_ledger").is_some_and(Value::is_object)
        || bundle.pointer("/coordinator/identity").is_none()
        || bundle.pointer("/coordinator/health").is_none()
        || bundle.pointer("/coordinator/tick").is_none()
        || bundle.pointer("/capture").is_none()
    {
        return Err("required causal fields".into());
    }
    Ok(())
}

fn write_reviewer_artifacts(
    label: &str,
    body: &str,
    digest: &str,
    mutation_notes: &[String],
) -> (PathBuf, PathBuf, PathBuf) {
    let directory = std::env::var_os("A2R5_REVIEWER_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/Volumes/nvme/tmp").join(format!(
                "a2r5-delivery-evidence-{}",
                gate_authoritative_sha()
            ))
        });
    std::fs::create_dir_all(&directory).expect("create reviewer evidence directory");
    let stem = format!("a2r5-{label}-{}", std::process::id());
    let body_path = directory.join(format!("{stem}.json"));
    let digest_path = directory.join(format!("{stem}.sha256"));
    let index_path = directory.join(format!("{stem}.index"));
    let mut body_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&body_path)
        .expect("create reviewer bundle body");
    body_file
        .write_all(body.as_bytes())
        .and_then(|_| body_file.sync_all())
        .expect("flush reviewer bundle body");
    let mut digest_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&digest_path)
        .expect("create reviewer bundle digest");
    digest_file
        .write_all(format!("{digest}  {}\n", body_path.display()).as_bytes())
        .and_then(|_| digest_file.sync_all())
        .expect("flush reviewer bundle digest");
    let mut index_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&index_path)
        .expect("create reviewer evidence index");
    let mut index = format!(
        "schema=team-agent/a2r5-review-evidence-index-v2\ncandidate_sha={}\nbundle={} sha256={}\n",
        gate_authoritative_sha(),
        body_path.display(),
        digest
    );
    for note in mutation_notes {
        index.push_str(note);
        index.push('\n');
    }
    index.push_str(
        "cleanup=ephemeral owned bundle removed only after reviewer body/digest/index retention\n",
    );
    index_file
        .write_all(index.as_bytes())
        .and_then(|_| index_file.sync_all())
        .expect("flush reviewer evidence index");
    (body_path, digest_path, index_path)
}

fn classify_replay_failure(reason: &str, status_query_error: Option<&str>) -> Value {
    let (kind, basis) = if status_query_error.is_some() {
        ("apparatus", "status query could not read the fixture store")
    } else if reason == "terminal_status_not_expected" {
        (
            "product",
            "durable message status was terminal but not expected",
        )
    } else {
        (
            "unknown",
            "timeout or missing row leaves apparatus/product unresolved",
        )
    };
    json!({
        "kind": kind,
        "apparatus": kind == "apparatus",
        "product": kind == "product",
        "basis": basis,
    })
}

fn query_coordinator_value(workspace: &Path) -> Value {
    let runtime = workspace.join(".team/runtime");
    let metadata_path = runtime.join("coordinator.json");
    let heartbeat_path = runtime.join("coordinator_tick.json");
    let pid_path = runtime.join("coordinator.pid");
    let metadata = read_json_file(&metadata_path);
    let heartbeat = read_json_file(&heartbeat_path);
    let pid_text = std::fs::read_to_string(&pid_path).ok();
    let pid = pid_text
        .as_deref()
        .and_then(|text| text.trim().parse::<u32>().ok());
    let running = pid.is_some_and(|pid| {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|output| output.status.success())
    });
    json!({
        "identity": metadata,
        "health": {
            "pid_path": pid_path,
            "pid": pid,
            "pid_running": running,
            "metadata_path": metadata_path,
            "metadata_present": !metadata.is_null(),
        },
        "heartbeat": heartbeat,
        "tick": heartbeat,
    })
}

fn read_json_file(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .unwrap_or_else(|error| json!({"path": path, "parse_error": error.to_string()})),
        Err(error) => json!({"path": path, "read_error": error.to_string()}),
    }
}

fn query_owned_resource_ledger(workspace: &Path, tmux_socket: &str, pane: &str) -> Value {
    let runtime = workspace.join(".team/runtime");
    let coordinator_pid_path = runtime.join("coordinator.pid");
    let coordinator_pid = std::fs::read_to_string(&coordinator_pid_path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    let pane_pid = Command::new("tmux")
        .args([
            "-S",
            tmux_socket,
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_pid}",
        ])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse::<u32>().ok());
    json!({
        "owner": "E6Case/HermeticTestEnv",
        "workspace": workspace,
        "owned_paths": [
            workspace.join(".team/runtime/team.db"),
            workspace.join(".team/runtime/state.json"),
            workspace.join(".team/runtime/coordinator.json"),
            workspace.join(".team/runtime/coordinator_tick.json"),
            workspace.join(".team/logs/events.jsonl")
        ],
        "owned_tmux_socket": tmux_socket,
        "owned_pids": [coordinator_pid, pane_pid],
        "leader_pane": pane,
        "cleanup": "exact registered pids/socket plus hermetic root; durable sibling is retained for parent tooth",
    })
}

fn write_immutable_bundle(
    default_directory: &Path,
    message_id: &str,
    bundle: &Value,
) -> Result<String, String> {
    let directory = std::env::var_os("E6_DURABLE_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_directory.to_path_buf());
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let bytes =
        serde_json::to_vec_pretty(bundle).map_err(|error| format!("encode bundle: {error}"))?;
    for suffix in 0..1000 {
        let name = if suffix == 0 {
            format!("e6-replay-failure-{message_id}.json")
        } else {
            format!("e6-replay-failure-{message_id}-{suffix}.json")
        };
        let path = directory.join(name);
        let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(&path) else {
            continue;
        };
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(path.display().to_string());
    }
    Err(format!(
        "no immutable filename available under {}",
        directory.display()
    ))
}

fn wait_for_pane_token(tmux_socket: &str, pane: &str, token: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = capture_pane(tmux_socket, pane);
        if last.contains(token) {
            return last;
        }
        thread::sleep(Duration::from_millis(250));
    }
    last
}

fn capture_pane(tmux_socket: &str, pane: &str) -> String {
    let output = Command::new("tmux")
        .args(["-S", tmux_socket, "capture-pane", "-p", "-t", pane])
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "E6 apparatus fixture child/probe-exit: tmux capture-pane could not spawn; socket={tmux_socket} pane={pane} error={error}"
            )
        });
    assert_fixture_command_success(
        "tmux capture-pane",
        &output,
        &format!("socket={tmux_socket} pane={pane}"),
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn assert_fixture_command_success(label: &str, output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "E6 apparatus fixture child/probe-exit: {label} exited unsuccessfully; {context} code={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct E6Case {
    env: HermeticTestEnv,
    home: PathBuf,
    target_workspace: PathBuf,
    sender_workspace: PathBuf,
    team_dir: PathBuf,
    team_key: String,
    tmux_socket: PathBuf,
    durable_evidence_dir: PathBuf,
}

impl E6Case {
    fn new(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(&format!("e6-{tag}"));
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = env
            .root()
            .join(format!("ta-e6-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create e6 test root");
        let home = root.join("home");
        let target_workspace = root.join("target-ws");
        let sender_workspace = root.join("sender-ws");
        let team_dir = root.join("team");
        for dir in [&home, &target_workspace, &sender_workspace, &team_dir] {
            std::fs::create_dir_all(dir).expect("create e6 test dir");
        }
        let durable_evidence_dir = env
            .root()
            .parent()
            .expect("system temp parent")
            .join(format!(
                "ta-e6-replay-evidence-{}-{}",
                std::process::id(),
                n
            ));
        std::fs::create_dir_all(&durable_evidence_dir).expect("create durable evidence dir");
        Self {
            home,
            target_workspace: std::fs::canonicalize(target_workspace)
                .expect("canonical target workspace"),
            sender_workspace: std::fs::canonicalize(sender_workspace)
                .expect("canonical sender workspace"),
            team_dir: std::fs::canonicalize(team_dir).expect("canonical team dir"),
            // 0.5.43 debt-sweep (§6.1): E6 team_key must include
            // pid+counter so parallel runs / host-fixture cohabitation
            // don't collide on a fixed key like "mail059". Session
            // names derived from team_key inherit the uniqueness.
            team_key: format!("mail059-{}-{n}", std::process::id()),
            env,
            tmux_socket: short_tmux_socket(&format!("e6-{tag}")),
            durable_evidence_dir,
        }
    }

    fn target_workspace(&self) -> &Path {
        &self.target_workspace
    }

    fn sender_workspace(&self) -> &Path {
        &self.sender_workspace
    }

    fn target_workspace_arg(&self) -> String {
        self.target_workspace.to_string_lossy().to_string()
    }

    fn sender_workspace_arg(&self) -> String {
        self.sender_workspace.to_string_lossy().to_string()
    }

    fn team_dir_arg(&self) -> String {
        self.team_dir.to_string_lossy().to_string()
    }

    fn durable_evidence_dir(&self) -> &Path {
        &self.durable_evidence_dir
    }

    fn write_fake_team(&self, spec_name: &str, agent_id: &str) {
        let agents_dir = self.team_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create agents dir");
        std::fs::write(
            self.team_dir.join("TEAM.md"),
            format!(
                "---\nname: {spec_name}\nobjective: E6 real CLI mailbox contract.\nprovider: fake\ndisplay_backend: none\n---\n\nFake-provider E6 mailbox contract team.\n"
            ),
        )
        .expect("write TEAM.md");
        std::fs::write(
            agents_dir.join(format!("{agent_id}.md")),
            format!(
                "---\nname: {agent_id}\nrole: Fake E6 Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFake worker keeping the team alive.\n"
            ),
        )
        .expect("write fake worker");
    }

    fn run_cli(&self, cwd: &Path, args: Vec<String>) -> Output {
        self.run_cli_with_identity(cwd, args, None)
    }

    fn run_cli_as(&self, cwd: &Path, args: Vec<String>, identity: &str) -> Output {
        self.run_cli_with_identity(cwd, args, Some(identity))
    }

    fn run_cli_with_identity(
        &self,
        cwd: &Path,
        args: Vec<String>,
        identity: Option<&str>,
    ) -> Output {
        let mut command = Command::new(bin());
        command
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("TMUX", format!("{},12345,0", self.tmux_socket.display()));
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_AUTH_MODE",
            "TEAM_AGENT_MCP_AUTO_APPROVE",
            "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
            "TEAM_AGENT_LEADER_BYPASS",
            "TEAM_AGENT_LEADER_BYPASS_FLAG",
            "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
            "TEAM_AGENT_LEADER_BYPASS_SOURCE",
            "TMUX_PANE",
        ] {
            command.env_remove(key);
        }
        if let Some(identity) = identity {
            command.env("TEAM_AGENT_ID", identity);
        }
        command.output().expect("run team-agent")
    }

    fn start_leader_pane(&self, tmux_socket: &str, session_name: &str) -> String {
        let output = Command::new("tmux")
            .args([
                "-S",
                tmux_socket,
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                session_name,
                "-n",
                "leader",
                "-c",
                &self.target_workspace_arg(),
                "/bin/cat",
            ])
            .output()
            .unwrap_or_else(|error| panic!("E6 apparatus fixture child/probe-exit: tmux new-window could not spawn; error={error}"));
        assert_fixture_command_success("tmux new-window leader", &output, "leader pane creation");
        self.register_pane_pid(&String::from_utf8_lossy(&output.stdout).trim());
        self.env.register_owned_tmux_socket(&self.tmux_socket);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn register_pane_pid(&self, pane: &str) {
        let output = Command::new("tmux")
            .args([
                "-S",
                self.tmux_socket.to_str().expect("tmux socket utf8"),
                "display-message",
                "-p",
                "-t",
                pane,
                "#{pane_pid}",
            ])
            .output()
            .unwrap_or_else(|error| panic!("E6 apparatus fixture child/probe-exit: tmux display-message could not spawn; pane={pane} error={error}"));
        assert_fixture_command_success(
            "tmux display-message pane pid",
            &output,
            &format!("pane={pane}"),
        );
        if let Ok(pid) = String::from_utf8_lossy(&output.stdout).trim().parse() {
            self.env.register_owned_pid(pid);
        }
    }
}

impl Drop for E6Case {
    fn drop(&mut self) {
        // 0.5.43 debt-sweep (§6.1): try `team-agent shutdown` first,
        // then fall back to exact `tmux -S <socket> kill-server` on
        // each workspace's persisted tmux_endpoint. Never scans host
        // sockets — the fallback only kills the endpoint recorded in
        // the state file THIS fixture wrote.
        for workspace in [&self.target_workspace, &self.sender_workspace] {
            register_persisted_pid(&self.env, workspace);
            let shutdown = Command::new(bin())
                .args([
                    "shutdown",
                    "--workspace",
                    &workspace.to_string_lossy(),
                    "--team",
                    &self.team_key,
                    "--json",
                ])
                .env("HOME", &self.home)
                .env("TMUX", format!("{},12345,0", self.tmux_socket.display()))
                .output();
            if !matches!(&shutdown, Ok(output) if output.status.success()) {
                let _ = Command::new("tmux")
                    .args([
                        "-S",
                        self.tmux_socket.to_str().expect("tmux socket utf8"),
                        "kill-server",
                    ])
                    .output();
            }
        }
        // Preserve a non-empty durable bundle for the parent tooth. Empty
        // process-owned directories are safe to remove after ordinary green runs.
        let _ = std::fs::remove_dir(&self.durable_evidence_dir);
    }
}

fn register_persisted_pid(env: &HermeticTestEnv, workspace: &Path) {
    let state_path = workspace.join(".team/runtime/state.json");
    let Ok(_) = std::fs::read_to_string(&state_path) else {
        return;
    };
    let pid_path = workspace.join(".team/runtime/coordinator.pid");
    if let Some(pid) = std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|pid| pid.trim().parse().ok())
    {
        env.register_owned_pid(pid);
    }
}
