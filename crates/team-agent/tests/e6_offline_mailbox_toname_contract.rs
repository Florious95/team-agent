//! E6 RED contract: offline mailbox for `send --to-name <team>/leader`.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E6.
//! - design `.team/artifacts/offline-mailbox-toname-design.md` §§3, 6, 8, 11.
//! - CR red-line spirit from `.team/artifacts/phase-dx-invariant-review.md`:
//!   diagnostics/advisory hints must not become binding authority.
//!
//! Contract: explicit runtime team keys are authoritative. Wrong spec/display
//! names fail as `team_key_not_found`; exact live team + unattached leader queues
//! a non-delivered message row until the owner attaches a leader.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;
use serde_json::{json, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn e6_wrong_spec_name_reports_team_key_not_found_without_mailbox_write() {
    let sender = temp_workspace("e6-sender");
    let target = temp_workspace("e6-target");
    write_runtime_state(
        &target,
        json!({
            "active_team_key": "current",
            "teams": {
                "current": {
                    "status": "alive",
                    "leader_receiver": {
                        "pane_id": "%404",
                        "session_name": "team-current",
                        "window_name": "leader",
                        "tmux_socket": "/tmp/team-agent-e6-no-such-socket"
                    },
                    "agents": {}
                }
            },
            "agents": {}
        }),
    );

    let wrong = format!("{}::twitter-autopub/leader", target.display());
    let output = run(
        &[
            "send",
            "--workspace",
            sender.to_str().expect("sender path"),
            "--to-name",
            &wrong,
            "E6_WRONG_KEY_TOKEN",
            "--sender",
            "third-party",
            "--json",
        ],
        &sender,
    );
    let out = json_stdout(&output);

    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        out.get("reason").and_then(Value::as_str),
        Some("team_key_not_found"),
        "E6 RED: wrong spec/display name `twitter-autopub/leader` must report reason=team_key_not_found, not name_not_live/name_not_resolvable; got {out}"
    );
    assert_eq!(
        out.get("requested_team").and_then(Value::as_str),
        Some("twitter-autopub"),
        "E6 RED: refusal must echo requested_team so users can see spec/display name != runtime key; got {out}"
    );
    assert_array_contains(
        out.get("available_team_keys"),
        "current",
        "E6 RED: team_key_not_found must list available_team_keys including canonical key `current`",
    );
    assert!(
        out.get("suggested_name")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("::current/leader")),
        "E6 RED: team_key_not_found must suggest canonical `::current/leader`; got {out}"
    );
    let text = out.to_string();
    assert!(
        !text.contains("claim-leader") && !text.contains("takeover"),
        "E6 RED: third-party wrong-key refusal must not tell sender to claim/takeover target leader; got {text}"
    );
    assert_eq!(
        message_count(&target, "E6_WRONG_KEY_TOKEN"),
        0,
        "E6 RED: team_key_not_found is fail-closed and must not write a target mailbox row"
    );
}

#[test]
fn e6_resolver_taxonomy_is_split_before_reading_leader_receiver() {
    let named = source("src/cli/named_address.rs");
    let mut missing = Vec::new();
    for required in [
        "TeamKeyNotFound",
        "LeaderNotAttached",
        "WorkspaceNoState",
        "\"team_key_not_found\"",
        "\"leader_not_attached\"",
        "\"workspace_no_state\"",
        "\"requested_team\"",
        "\"available_team_keys\"",
        "\"suggested_name\"",
    ] {
        if !named.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E6 RED: named-address resolver must split wrong key / unattached leader / no-state taxonomy before leader_receiver fallback. Missing markers: {missing:?}"
    );
    assert!(
        top_level_leader_receiver_fallbacks(&named).is_empty(),
        "E6 RED: explicit <team>/leader key miss must not fall through to top-level leader_receiver through or/or_else/unwrap_or_else/else fallback forms; wrong key must be team_key_not_found. Offenders: {:#?}",
        top_level_leader_receiver_fallbacks(&named)
    );
}

#[test]
fn e6_offline_mailbox_uses_non_delivered_status_and_existing_requeue_funnel() {
    let combined = [
        source("src/cli/send.rs"),
        source("src/messaging/leader_receiver.rs"),
        source("src/db/message_store.rs"),
        source("src/messaging/watchers.rs"),
        source("src/cli/status_port.rs"),
        source("src/messaging/delivery.rs"),
    ]
    .join("\n");

    let mut missing = Vec::new();
    for required in [
        "queued_until_leader_attach",
        "leader_mailbox",
        "delivered",
        "owner_team_id",
        "recipient = 'leader'",
        "leader_mailbox.queued_until_attach",
        "requeue_blocked_leader_messages",
        "pending_leader_notifications",
    ] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E6 RED: leader-not-attached --to-name path must enqueue one target team.db row with status=queued_until_leader_attach, delivered=false, channel=leader_mailbox, expose it in status, and requeue the same row via existing requeue_blocked_leader_messages. Missing markers: {missing:?}"
    );
    assert!(
        !claim_for_delivery_statuses().contains("queued_until_leader_attach"),
        "E6 RED guard: queued_until_leader_attach rows must not be claimed by coordinator tick before attach/claim"
    );
}

#[test]
fn e6_mailbox_paths_do_not_spawn_provider_or_worker_processes() {
    let offenders = provider_spawn_offenders(&[
        "src/cli/send.rs",
        "src/cli/named_address.rs",
        "src/messaging/leader_receiver.rs",
        "src/messaging/watchers.rs",
        "src/messaging/results.rs",
    ]);

    assert!(
        offenders.is_empty(),
        "E6 RED guard: offline mailbox is a durable queued row only; send/named_address/leader_receiver/watchers/results must not spawn codex/claude/copilot or worker/provider processes. Offenders: {offenders:#?}"
    );
}

#[test]
fn e6_mailbox_does_not_create_parallel_inbox_or_message_store() {
    let offenders = parallel_store_offenders(&[
        "src/cli/send.rs",
        "src/cli/named_address.rs",
        "src/messaging/leader_receiver.rs",
        "src/messaging/watchers.rs",
        "src/messaging/results.rs",
    ]);

    assert!(
        offenders.is_empty(),
        "E6 RED guard: offline mailbox must reuse target team.db messages rows, not leader-inbox.log, .team/messages, File::create/OpenOptions/write_all, or another message store. Offenders: {offenders:#?}"
    );
}

#[test]
fn e6_third_party_copy_never_suggests_target_takeover() {
    let send = source("src/cli/send.rs");
    let named = source("src/cli/named_address.rs");
    let third_party_markers_present = send.contains("third-party")
        || named.contains("third-party")
        || named.contains("third_party");

    assert!(
        third_party_markers_present,
        "E6 RED: implementation must distinguish third-party sender copy from owner status copy so external send output never suggests claim-leader/takeover"
    );
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run team-agent")
}

fn json_stdout(output: &Output) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout utf8");
    assert!(
        !stdout.trim().is_empty(),
        "E6 RED setup: expected JSON stdout; status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!(
            "E6 RED setup: stdout must be JSON, parse error={error}; stdout={stdout:?}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_array_contains(value: Option<&Value>, expected: &str, red_reason: &str) {
    let values = value
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{red_reason}; missing array"));
    assert!(
        values.iter().any(|item| item.as_str() == Some(expected)),
        "{red_reason}; got {values:?}"
    );
}

fn temp_workspace(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ta-059-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp workspace");
    std::fs::canonicalize(path).expect("canonical temp workspace")
}

fn write_runtime_state(workspace: &Path, state: Value) {
    let runtime = workspace.join(".team/runtime");
    std::fs::create_dir_all(&runtime).expect("create runtime dir");
    std::fs::write(
        runtime.join("state.json"),
        serde_json::to_string_pretty(&state).expect("state json"),
    )
    .expect("write state");
}

fn message_count(workspace: &Path, token: &str) -> i64 {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return 0;
    }
    let conn = Connection::open(db).expect("open team.db");
    conn.query_row(
        "select count(*) from messages where content like ?1",
        [format!("%{token}%")],
        |row| row.get(0),
    )
    .expect("count messages")
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap_or_default()
}

fn top_level_leader_receiver_fallbacks(text: &str) -> Vec<(usize, String)> {
    // CR verdict R2 (`.team/artifacts/059-impl-cr-verdict.md §4`): documented
    // legacy single-team compat reads are allowed as long as they mark the
    // exact line with `ALLOWED-LEGACY-SINGLE-TEAM`. The marker distinguishes
    // an intentional, time-boxed backwards-compat bridge from an
    // authority-consuming fallback (which the guard still forbids). Delete
    // the marker + the fallback branch when B1 canonical state layout lands
    // (`.team/artifacts/next-version-staged-plan.md §5 Phase-Foundation-1`).
    let lines = text.lines().collect::<Vec<_>>();
    let mut offenders = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if !line.contains("state.get(\"leader_receiver\")") {
            continue;
        }
        if line.contains("ALLOWED-LEGACY-SINGLE-TEAM") {
            continue;
        }
        let start = idx.saturating_sub(6);
        let end = usize::min(lines.len(), idx + 7);
        let window = lines[start..end].join("\n");
        // The line-above/-below marker is also acceptable so callers can
        // place the marker on the branch-guard line rather than cluttering
        // the read expression itself.
        let has_nearby_marker = window.contains("ALLOWED-LEGACY-SINGLE-TEAM");
        let fallback_shape = window.contains("team_entry(state, team)")
            && (window.contains(".or_else")
                || window.contains(".or(")
                || window.contains("unwrap_or")
                || window.contains("} else {"));
        if fallback_shape && !has_nearby_marker {
            offenders.push((idx + 1, line.trim().to_string()));
        }
    }
    offenders
}

fn provider_spawn_offenders(files: &[&str]) -> Vec<(String, usize, String)> {
    let mut offenders = Vec::new();
    for rel in files {
        let text = source(rel);
        for (idx, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            if line.contains("Command::new")
                && (lower.contains("codex") || lower.contains("claude") || lower.contains("copilot"))
            {
                offenders.push(((*rel).to_string(), idx + 1, line.trim().to_string()));
            }
        }
    }
    offenders
}

fn parallel_store_offenders(files: &[&str]) -> Vec<(String, usize, String)> {
    let mut offenders = Vec::new();
    for rel in files {
        let text = source(rel);
        for (idx, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            let writes_file_store = lower.contains("leader-inbox.log")
                || lower.contains(".team/messages")
                || line.contains("File::create")
                || line.contains("OpenOptions")
                || line.contains(".write_all(");
            if writes_file_store {
                offenders.push(((*rel).to_string(), idx + 1, line.trim().to_string()));
            }
        }
    }
    offenders
}

fn claim_for_delivery_statuses() -> String {
    let message_store = source("src/db/message_store.rs");
    let start = message_store
        .find("pub fn claim_for_delivery")
        .unwrap_or_default();
    let slice = &message_store[start..];
    let end = slice.find("    /// Read inbox rows").unwrap_or(slice.len());
    slice[..end].to_string()
}
