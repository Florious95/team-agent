//! Car-1 (msg-protocol) M1–M3 TARGET-INVARIANT contract (category: target
//! invariant — the only kind admitted into `tests/`; the M0 status-lock
//! sibling stays archived in `.team/artifacts/` per the status-lock-vs-target
//! criterion). Successor to the archived M0 freeze contract
//! (`contract-m0-freeze-verifier.rs`, sha `1f529958…66d2`).
//!
//! Every assertion here states what MUST hold once the corresponding
//! milestone lands; on the 22d7906 baseline they are RED anchors:
//!
//!  M1 — identity is not caller-supplied:
//!   1a. a `--sender <other>` override must be rejected outright (unknown
//!       flag / refusal) — and under no outcome may a forged sender identity
//!       reach the message store.
//!   1b. unknown flags fail closed: an unrecognized flag must fail the
//!       command with zero delivery side effects, never be silently ignored.
//!
//!  M2 — one create-message funnel behind every public address form:
//!   2a. positional / --targets fanout / --to-name (bare and qualified) all
//!       enter through the same persisted create-message fingerprint: a
//!       store-backed `msg_*` id per recipient, row visible in the DB.
//!   2b. `--pane` has no direct-inject bypass: it is either refused (post
//!       sunset) or persists a store row first — and its acceptance carries a
//!       sunset/deprecation notice.
//!
//!  M3 — delivery semantics collapse to persist-and-return:
//!   3a. a bare `send` (no delivery flags at all) returns a persisted `msg_*`
//!       correlation id — persistence IS the return contract.
//!   3b. legacy delivery-semantic flags (--watch-result / --requires-ack /
//!       --no-ack / --timeout / --confirm-human / --message-id) are either
//!       rejected or accepted WITH an explicit sunset/deprecation notice;
//!       silent unchanged acceptance is a failure.
//!
//! The report-misattribution drift anchor (M0 test C2) is deliberately NOT
//! here — it is reserved for the M4–M5 slice per leader ruling.
//!
//! Harness: real binary + real tmux + fake provider (zero tokens), canonical
//! commands only.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;
use serial_test::serial;

const TEAM: &str = "mtteam";

struct TargetCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    socket: PathBuf,
}

impl TargetCase {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            format!(
                "---\nname: {TEAM}\nobjective: m1-m3 target invariants.\nprovider: fake\n---\n"
            ),
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
            "--yes",
            "--no-display",
            "--json",
        ]);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "quick-start must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
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

    /// Raw send invocation: returns (parsed-JSON-if-any, combined text, exit ok).
    fn send_raw(&self, args: &[&str]) -> (Option<Value>, String, bool) {
        let mut full = vec!["send"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--workspace", self.workspace_str(), "--json"]);
        let output = self.run_cli(&full);
        let parsed = serde_json::from_slice::<Value>(&output.stdout).ok();
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (parsed, combined, output.status.success())
    }

    fn db_rows(&self, needle: &str) -> Vec<(String, String, String)> {
        let db = self.workspace.join(".team").join("runtime").join("team.db");
        if !db.exists() {
            return Vec::new();
        }
        let connection = rusqlite::Connection::open(&db).expect("open team.db");
        let mut statement = connection
            .prepare(
                "SELECT message_id, sender, recipient FROM messages WHERE content LIKE ?1 ORDER BY rowid",
            )
            .expect("prepare message query");
        statement
            .query_map([format!("%{needle}%")], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("query messages")
            .filter_map(Result::ok)
            .collect()
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

fn accepted(parsed: &Option<Value>, exit_ok: bool) -> bool {
    exit_ok
        && parsed
            .as_ref()
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            == Some(true)
}

fn sunset_notice(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("sunset") || lower.contains("deprecat")
}

/// M1a — a caller-supplied sender identity must be rejected, and a forged
/// sender must never reach the store.
#[test]
#[serial(env)]
fn m1a_sender_override_is_rejected_and_never_persisted() {
    let case = TargetCase::start("mt-m1a");
    let (parsed, text, exit_ok) = case.send_raw(&[
        "w1",
        "m1a forged sender probe",
        "--team",
        TEAM,
        "--sender",
        "w2",
        "--no-wait",
    ]);
    let rows = case.db_rows("m1a forged sender probe");
    let forged_persisted = rows.iter().any(|(_, sender, _)| sender == "w2");
    assert!(
        !accepted(&parsed, exit_ok),
        "M1a: `--sender` is a caller-supplied identity claim and must be rejected \
         (MUST-11 positive-source identity); got acceptance: {text}"
    );
    assert!(
        !forged_persisted,
        "M1a: a forged sender must never reach the message store; rows={rows:?}"
    );
    case.shutdown();
}

/// M1b — unknown flags fail closed with zero delivery side effects.
#[test]
#[serial(env)]
fn m1b_unknown_flag_fails_closed_with_zero_side_effects() {
    let case = TargetCase::start("mt-m1b");
    let (parsed, text, exit_ok) = case.send_raw(&[
        "w1",
        "m1b unknown flag probe",
        "--team",
        TEAM,
        "--definitely-not-a-real-flag",
        "--no-wait",
    ]);
    let rows = case.db_rows("m1b unknown flag probe");
    assert!(
        !accepted(&parsed, exit_ok),
        "M1b: an unrecognized flag must fail the command, never be silently ignored; got: {text}"
    );
    assert!(
        rows.is_empty(),
        "M1b: a failed parse must have zero delivery side effects; rows={rows:?}"
    );
    case.shutdown();
}

/// M2a — every public logical address form enters the same persisted
/// create-message funnel: store-backed `msg_*` id per recipient.
#[test]
#[serial(env)]
fn m2a_all_logical_address_forms_share_the_persisted_create_message_funnel() {
    let case = TargetCase::start("mt-m2a");
    let probes: Vec<(&str, Vec<&str>, usize)> = vec![
        (
            "positional",
            vec!["w1", "m2a probe positional", "--team", TEAM, "--no-wait"],
            1,
        ),
        (
            "targets-fanout",
            vec![
                "--targets",
                "w1,w2",
                "m2a probe fanout",
                "--team",
                TEAM,
                "--no-wait",
            ],
            2,
        ),
        (
            "to-name-bare",
            vec![
                "--to-name",
                "w1",
                "m2a probe named bare",
                "--team",
                TEAM,
                "--no-wait",
            ],
            1,
        ),
        (
            "to-name-qualified",
            vec![
                "--to-name",
                "mtteam/w1",
                "m2a probe named qualified",
                "--no-wait",
            ],
            1,
        ),
    ];
    for (label, args, expected_rows) in probes {
        let needle = args
            .iter()
            .find(|arg| arg.starts_with("m2a probe"))
            .expect("probe content");
        let (parsed, text, exit_ok) = case.send_raw(&args);
        assert!(
            accepted(&parsed, exit_ok),
            "M2a ({label}): the canonical logical form must be accepted; got: {text}"
        );
        let value = parsed.expect("accepted implies JSON");
        let message_id = value
            .get("message_id")
            .and_then(Value::as_str)
            .unwrap_or("<absent>");
        assert!(
            message_id.starts_with("msg_"),
            "M2a ({label}): every logical address form must return a store-backed msg_* \
             correlation id (no named_send_*/synthetic ids); got {message_id}: {value}"
        );
        let rows = case.db_rows(needle);
        assert_eq!(
            rows.len(),
            expected_rows,
            "M2a ({label}): the message must be persisted before delivery (one row per \
             recipient); rows={rows:?}"
        );
    }
    case.shutdown();
}

/// M2b — `--pane` has no direct-inject bypass: refused, or persisted-first
/// with an explicit sunset notice.
#[test]
#[serial(env)]
fn m2b_pane_form_has_no_direct_inject_bypass() {
    let case = TargetCase::start("mt-m2b");
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
    let pane = String::from_utf8_lossy(&pane_list.stdout)
        .lines()
        .find_map(|line| {
            let mut cols = line.split("__TA_FIELD__");
            (cols.next()? == "w1").then(|| cols.next().map(ToString::to_string))?
        })
        .expect("w1 pane present");

    let (parsed, text, exit_ok) =
        case.send_raw(&["--pane", pane.as_str(), "m2b pane probe", "--no-wait"]);
    if accepted(&parsed, exit_ok) {
        let value = parsed.expect("accepted implies JSON");
        let message_id = value
            .get("message_id")
            .and_then(Value::as_str)
            .unwrap_or("<absent>");
        assert!(
            message_id.starts_with("msg_"),
            "M2b: an accepted --pane send must persist a store row first (msg_* id), not \
             direct-inject (pane_send_*); got {message_id}: {value}"
        );
        assert_eq!(
            case.db_rows("m2b pane probe").len(),
            1,
            "M2b: accepted --pane send must be store-backed"
        );
        assert!(
            sunset_notice(&text),
            "M2b: --pane acceptance must carry a sunset/deprecation notice (M2 公告); got: {text}"
        );
    } else {
        assert!(
            case.db_rows("m2b pane probe").is_empty(),
            "M2b: a refused --pane send must have zero side effects"
        );
    }
    case.shutdown();
}

/// M3a — bare send: persistence IS the return contract.
#[test]
#[serial(env)]
fn m3a_bare_send_returns_persisted_correlation_id() {
    let case = TargetCase::start("mt-m3a");
    let (parsed, text, exit_ok) = case.send_raw(&["w1", "m3a bare send probe", "--team", TEAM]);
    assert!(
        accepted(&parsed, exit_ok),
        "M3a: a bare send (no delivery flags) must be accepted; got: {text}"
    );
    let value = parsed.expect("accepted implies JSON");
    let message_id = value
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("<absent>");
    assert!(
        message_id.starts_with("msg_"),
        "M3a: the bare send must return a stable store-backed correlation id; got {message_id}"
    );
    assert_eq!(
        case.db_rows("m3a bare send probe").len(),
        1,
        "M3a: persisted row is the return contract"
    );
    case.shutdown();
}

/// M3b — legacy delivery-semantic flags: rejected, or accepted only WITH an
/// explicit sunset/deprecation notice. Silent unchanged acceptance fails.
#[test]
#[serial(env)]
fn m3b_delivery_semantic_flags_are_rejected_or_sunset_noticed() {
    let case = TargetCase::start("mt-m3b");
    let flag_probes: Vec<(&str, Vec<&str>)> = vec![
        (
            "--watch-result",
            vec![
                "w1",
                "m3b probe watch",
                "--team",
                TEAM,
                "--watch-result",
                "--timeout",
                "1",
            ],
        ),
        (
            "--requires-ack",
            vec![
                "w1",
                "m3b probe ack",
                "--team",
                TEAM,
                "--requires-ack",
                "--no-wait",
            ],
        ),
        (
            "--no-ack",
            vec![
                "w1",
                "m3b probe noack",
                "--team",
                TEAM,
                "--no-ack",
                "--no-wait",
            ],
        ),
        (
            "--confirm-human",
            vec![
                "w1",
                "m3b probe human",
                "--team",
                TEAM,
                "--confirm-human",
                "--no-wait",
            ],
        ),
        (
            "--message-id",
            vec![
                "w1",
                "m3b probe msgid",
                "--team",
                TEAM,
                "--message-id",
                "msg_custom_0001",
                "--no-wait",
            ],
        ),
    ];
    let mut silently_accepted = Vec::new();
    for (flag, args) in flag_probes {
        let (parsed, text, exit_ok) = case.send_raw(&args);
        if accepted(&parsed, exit_ok) && !sunset_notice(&text) {
            silently_accepted.push(flag.to_string());
        }
    }
    assert!(
        silently_accepted.is_empty(),
        "M3b: delivery-semantic flags must be rejected or carry a sunset/deprecation notice; \
         silently accepted unchanged: {silently_accepted:?}"
    );
    case.shutdown();
}
