//! Car-C successor TARGET-INVARIANT contract (verifier-frozen): the send path
//! persists before any recovery concern, through ONE primitive, at every
//! entry point — and coordinator availability is a delivery blocker, never a
//! pre-persist refusal.
//!
//! Category: target invariant (tests/ admissible). Fixtures are canonical
//! only (five-check clean): real quick-start teams, real tmux, fake provider,
//! real MCP server process against the real workspace; the ONLY fault used is
//! a real `kill` of the coordinator process — explicitly listed by MUST-15 as
//! a canonical trigger. No state/row/transcript synthesis anywhere.
//!
//! Invariants (aligned with runtime-owner's wire proposal, msg_01d41fc37b88):
//!  1. persist-before-recovery: a canonically resolved worker send with the
//!     coordinator DOWN still persists exactly one `msg_*` row, reports
//!     honestly (ok = persisted, delivered = false) and self-heals LOUDLY
//!     (coordinator auto-restart surfaced in the response). The
//!     `queued_coordinator_unavailable` durable-blocker wire (owner proposal
//!     msg_01d41fc37b88) applies only when the ensure itself fails — no
//!     canonical trigger exists for that state, so it is DEFERRED to unit
//!     territory per MUST-15.
//!  2. all-entrypoint parity: positional TO, `--to-name` alias and the MCP
//!     `send_message` tool all land in the same persisted fingerprint
//!     (recipient/sender/team/one-row-per-recipient).
//!  3. pre-persist refusal boundary: unknown recipient / unresolvable name /
//!     unbound leader refuse with ZERO DB side effects — availability is
//!     never mapped to a refusal, and refusals never persist.
//!  4. recovery same-row: once the coordinator is back (lazily ensured by the
//!     next canonical command), the SAME message_id advances out of the
//!     blocker; still exactly one row, no replacement row.
//!  5. fanout independence: each comma-list recipient gets its own row; one
//!     recipient's delivery blocker must not erase or block the other's.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;
#[path = "support/mcp_sim_harness.rs"]
mod mcp_sim_harness;

use std::path::PathBuf;
use std::process::Output;

use serde_json::{json, Value};
use serial_test::serial;

const TEAM: &str = "carc";

struct SendPathCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    socket: PathBuf,
}

impl SendPathCase {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            format!("---\nname: {TEAM}\nobjective: car-c persist-first contract.\nprovider: fake\n---\n"),
        )
        .expect("write TEAM.md");
        for worker in ["w1", "w2"] {
            std::fs::write(
                workspace.join("agents").join(format!("{worker}.md")),
                format!(
                    "---\nname: {worker}\nrole: {worker}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{worker}.\n"
                ),
            )
            .expect("write worker role doc");
        }
        let mut case = Self {
            env,
            workspace,
            socket: PathBuf::new(),
        };
        let output = case.run_cli(&[
            "quick-start",
            "--workspace",
            case.workspace_str(),
            "--team-id",
            TEAM,
            "--yes",
            "--no-display",
            "--json",
        ]);
        let value = json_stdout(&output, "quick-start");
        assert!(
            value
                .get("worker_readiness")
                .and_then(|node| node.get("all_workers_spawned"))
                .and_then(Value::as_bool)
                == Some(true),
            "fixture: quick-start must spawn workers; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        );
        let state_raw = std::fs::read_to_string(
            case.workspace
                .join(".team")
                .join("runtime")
                .join("state.json"),
        )
        .expect("read state.json");
        let state: Value = serde_json::from_str(&state_raw).expect("parse state.json");
        case.socket = PathBuf::from(
            state
                .get("tmux_socket")
                .and_then(Value::as_str)
                .expect("state tmux_socket"),
        );
        case
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    fn send_json(&self, args: &[&str]) -> Value {
        let mut full = vec!["send"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--workspace", self.workspace_str(), "--json"]);
        json_stdout(&self.run_cli(&full), "send")
    }

    /// Canonical fault (MUST-15-listed trigger): really kill the coordinator
    /// process and wait for it to exit.
    fn kill_coordinator(&self) {
        let pid_path = self
            .workspace
            .join(".team")
            .join("runtime")
            .join("coordinator.pid");
        let pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|raw| raw.trim().parse::<i32>().ok())
            .expect("coordinator.pid present after quick-start");
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|probe| probe.status.success())
                .unwrap_or(false);
            if !alive {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("coordinator pid {pid} did not exit within 5s of SIGTERM");
    }

    fn db_rows(&self, needle: &str) -> Vec<DbRow> {
        let db = self.workspace.join(".team").join("runtime").join("team.db");
        if !db.exists() {
            return Vec::new();
        }
        let connection = rusqlite::Connection::open(&db).expect("open team.db");
        let mut statement = connection
            .prepare(
                "SELECT message_id, sender, recipient, owner_team_id, status FROM messages \
                 WHERE content LIKE ?1 ORDER BY rowid",
            )
            .expect("prepare message query");
        statement
            .query_map([format!("%{needle}%")], |row| {
                Ok(DbRow {
                    message_id: row.get(0)?,
                    sender: row.get(1)?,
                    recipient: row.get(2)?,
                    owner_team_id: row.get(3)?,
                    status: row.get(4)?,
                })
            })
            .expect("query messages")
            .filter_map(Result::ok)
            .collect()
    }

    fn wait_status(&self, message_id: &str, wanted: &[&str], seconds: u64) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        loop {
            let db = self.workspace.join(".team").join("runtime").join("team.db");
            let status = rusqlite::Connection::open(&db)
                .ok()
                .and_then(|connection| {
                    connection
                        .query_row(
                            "SELECT status FROM messages WHERE message_id = ?1",
                            [message_id],
                            |row| row.get::<_, String>(0),
                        )
                        .ok()
                })
                .unwrap_or_else(|| "<norow>".to_string());
            if wanted.contains(&status.as_str()) || std::time::Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    fn shutdown(&self) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace_str(),
            "--yes",
            "--json",
        ]);
        let _ = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "kill-server",
            ])
            .output();
    }
}

#[derive(Debug)]
struct DbRow {
    message_id: String,
    sender: String,
    recipient: String,
    owner_team_id: Option<String>,
    status: Option<String>,
}

fn json_stdout(output: &Output, context: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context}: expected JSON stdout; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Invariant 1 — persist-before-recovery with the coordinator really dead:
/// the send must persist exactly one row, report honestly (never delivered at
/// return), and the self-heal must be loud (auto-restart evidenced in the
/// response). NOTE: the canonical path self-heals (loud ensure respawns the
/// coordinator — the praised 0.5.22 behavior), so the
/// `queued_coordinator_unavailable` durable-blocker wire (owner proposal)
/// applies only when the ensure itself FAILS — a state with no canonical
/// trigger on this platform; that branch is DEFERRED per MUST-15 and belongs
/// to unit territory (src/**/tests), where an ensure-failure can be
/// synthesized legally.
#[test]
#[serial(env)]
fn c1_resolved_send_with_dead_coordinator_persists_one_row_and_self_heals_loudly() {
    let case = SendPathCase::start("carc-c1");
    case.kill_coordinator();

    let value = case.send_json(&["w1", "carc c1 probe", "--team", TEAM]);
    let rows = case.db_rows("carc c1 probe");

    assert_eq!(
        value.get("ok"),
        Some(&json!(true)),
        "C1: persistence success must be reported ok=true even when the coordinator was down: {value}"
    );
    assert!(
        str_field(&value, "message_id").is_some_and(|id| id.starts_with("msg_")),
        "C1: a store-backed msg_* id must be returned (no synthetic no-row outcome): {value}"
    );
    assert_eq!(
        value.get("delivered"),
        Some(&json!(false)),
        "C1: ok=true means persisted, never delivered at return time: {value}"
    );
    assert_eq!(
        value.get("coordinator_auto_restarted"),
        Some(&json!(true)),
        "C1: recovering from a dead coordinator must be LOUD (auto-restart surfaced in the \
         response), never silent: {value}"
    );
    assert_eq!(
        rows.len(),
        1,
        "C1: exactly one persisted row; rows={rows:?}"
    );
    case.shutdown();
}

/// Invariant 2 — all-entrypoint parity: positional, --to-name alias, and the
/// real MCP send_message tool share one persisted fingerprint.
#[test]
#[serial(env)]
fn c2_positional_alias_and_mcp_share_one_persisted_fingerprint() {
    let case = SendPathCase::start("carc-c2");

    let positional = case.send_json(&["w2", "carc c2 positional", "--team", TEAM]);
    assert_eq!(positional.get("ok"), Some(&json!(true)), "{positional}");
    let alias = case.send_json(&["--to-name", "w2", "carc c2 alias", "--team", TEAM]);
    assert_eq!(alias.get("ok"), Some(&json!(true)), "{alias}");

    // Real MCP server process against the real quick-started workspace —
    // worker identity w1, owner scope = the real runtime team key.
    let mut worker = mcp_sim_harness::spawn_mcp_client(&case.workspace, "w1", TEAM);
    let mcp = worker.call_tool(
        "send_message",
        json!({"to": "w2", "content": "carc c2 mcp"}),
    );
    assert!(
        mcp.body.get("message_id").and_then(Value::as_str).is_some()
            || mcp.body.get("status").and_then(Value::as_str) == Some("accepted"),
        "C2: MCP send must enter the persisted primitive; body={} raw={}",
        mcp.body,
        mcp.raw
    );

    for (label, needle, sender) in [
        ("positional", "carc c2 positional", "leader"),
        ("alias", "carc c2 alias", "leader"),
        ("mcp", "carc c2 mcp", "w1"),
    ] {
        let rows = case.db_rows(needle);
        assert_eq!(
            rows.len(),
            1,
            "C2 ({label}): exactly one row; rows={rows:?}"
        );
        let row = &rows[0];
        assert!(
            row.message_id.starts_with("msg_"),
            "C2 ({label}): store-backed id; rows={rows:?}"
        );
        assert_eq!(row.recipient, "w2", "C2 ({label}): resolved recipient");
        assert_eq!(row.sender, sender, "C2 ({label}): trusted sender identity");
        assert_eq!(
            row.owner_team_id.as_deref(),
            Some(TEAM),
            "C2 ({label}): canonical team scope"
        );
    }
    case.shutdown();
}

/// Invariant 3 — pre-persist refusals have zero DB side effects, and refusal
/// is reserved for resolution/identity errors, never availability.
#[test]
#[serial(env)]
fn c3_pre_persist_refusals_leave_zero_db_side_effects() {
    let case = SendPathCase::start("carc-c3");
    let leader_form = format!("{TEAM}/leader");
    let probes: Vec<(&str, Vec<&str>)> = vec![
        (
            "unknown-recipient",
            vec!["nosuchworker", "carc c3 unknown", "--team", TEAM],
        ),
        (
            "unresolvable-name",
            vec!["--to-name", "nosuchteam/agent", "carc c3 name"],
        ),
        (
            "unbound-leader",
            vec![leader_form.as_str(), "carc c3 leader", "--team", TEAM],
        ),
    ];
    for (label, args) in &probes {
        let mut full = vec!["send"];
        full.extend(args.iter().copied());
        full.extend_from_slice(&["--workspace", case.workspace_str(), "--json"]);
        let output = case.run_cli(&full);
        let value: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        let accepted = output.status.success()
            && value
                .as_ref()
                .and_then(|v| v.get("ok"))
                .and_then(Value::as_bool)
                == Some(true);
        assert!(
            !accepted,
            "C3 ({label}): resolution/identity failures must refuse; got acceptance: {value:?}"
        );
    }
    for needle in ["carc c3 unknown", "carc c3 name", "carc c3 leader"] {
        assert!(
            case.db_rows(needle).is_empty(),
            "C3: refusals must leave zero DB side effects; needle={needle}"
        );
    }
    case.shutdown();
}

/// Invariant 4 — recovery advances the SAME row; no replacement rows.
#[test]
#[serial(env)]
fn c4_coordinator_recovery_advances_same_row_without_replacement() {
    let case = SendPathCase::start("carc-c4");
    case.kill_coordinator();

    let value = case.send_json(&["w1", "carc c4 probe", "--team", TEAM]);
    let message_id = str_field(&value, "message_id")
        .unwrap_or_else(|| panic!("C4 fixture: blocked send must persist an id: {value}"))
        .to_string();

    // Canonical recovery: the next command lazily ensures the coordinator.
    let status_output = case.run_cli(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
    ]);
    assert!(
        !status_output.stdout.is_empty(),
        "C4 fixture: status must run to re-ensure the coordinator"
    );

    let final_status = case.wait_status(
        &message_id,
        &["delivered", "submitted_pending_acceptance"],
        20,
    );
    assert!(
        final_status == "delivered" || final_status == "submitted_pending_acceptance",
        "C4: after coordinator recovery the SAME message_id must advance out of the blocker; \
         final={final_status}"
    );
    let rows = case.db_rows("carc c4 probe");
    assert_eq!(
        rows.len(),
        1,
        "C4: recovery must reuse the original row, never create a replacement; rows={rows:?}"
    );
    assert_eq!(rows[0].message_id, message_id, "C4: same id end to end");
    case.shutdown();
}

/// Invariant 5 — fanout rows are independent: one recipient's blocker leaves
/// the other recipient's row alone.
#[test]
#[serial(env)]
fn c5_fanout_rows_are_independent_under_partial_blockers() {
    let case = SendPathCase::start("carc-c5");
    // Canonical partial fault: kill w1's pane (w2 stays live, session intact).
    let pane_list = std::process::Command::new("tmux")
        .args([
            "-S",
            case.socket.to_str().expect("socket utf8"),
            "list-panes",
            "-a",
            "-F",
            "#{window_name}__TA_FIELD__#{pane_id}",
        ])
        .output()
        .expect("tmux list-panes");
    let w1_pane = String::from_utf8_lossy(&pane_list.stdout)
        .lines()
        .find_map(|line| {
            let mut cols = line.split("__TA_FIELD__");
            (cols.next()? == "w1").then(|| cols.next().map(ToString::to_string))?
        })
        .expect("w1 pane present");
    let _ = std::process::Command::new("tmux")
        .args([
            "-S",
            case.socket.to_str().expect("socket utf8"),
            "kill-pane",
            "-t",
            w1_pane.as_str(),
        ])
        .output();

    let value = case.send_json(&["w1,w2", "carc c5 fanout", "--team", TEAM]);
    assert_eq!(
        value.get("ok"),
        Some(&json!(true)),
        "C5: fanout with one blocked recipient must still persist both intents: {value}"
    );
    let rows = case.db_rows("carc c5 fanout");
    assert_eq!(rows.len(), 2, "C5: one row per recipient; rows={rows:?}");
    let w2_row = rows
        .iter()
        .find(|row| row.recipient == "w2")
        .expect("w2 row present");
    let w2_status = case.wait_status(&w2_row.message_id, &["delivered"], 15);
    assert_eq!(
        w2_status, "delivered",
        "C5: the live recipient must deliver despite the sibling blocker"
    );
    let w1_row = rows
        .iter()
        .find(|row| row.recipient == "w1")
        .expect("w1 row present");
    let w1_status = case.wait_status(&w1_row.message_id, &["queued_pane_missing"], 15);
    assert_eq!(
        w1_status, "queued_pane_missing",
        "C5: the blocked recipient parks as its own durable blocker, not erased"
    );
    case.shutdown();
}
