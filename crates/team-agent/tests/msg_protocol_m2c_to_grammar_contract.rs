//! Car-1 (msg-protocol) M2c TARGET-INVARIANT contract: the positional `TO`
//! grammar is the ONE canonical address surface, covering every logical form
//! the sunset aliases used to carry (r4 constitutional finding: M2 TO grammar
//! not yet converged; this is the target anchor for the fix).
//!
//! Category: target invariant (admitted into `tests/`); RED at the M0-M3
//! chain tail `6668141` where positional TO still accepts only bare short
//! ids, GREEN when the owner's M2 grammar-convergence fix lands.
//!
//! Invariants:
//!  1. Positional `TO` accepts the full logical grammar —
//!     short `w1` (with `--team`), qualified `team/agent`, fully-qualified
//!     `workspace::team/agent`, and comma-list fanout `w1,w2` — and every
//!     accepted form enters the same persisted create-message funnel
//!     (store-backed `msg_*` id, one row per recipient, correct recipient
//!     column).
//!  2. The leader logical form `team/leader` resolves through the same
//!     grammar: with no leader bound it is refused with the canonical
//!     `leader_not_attached` reason and zero side effects (same shape the
//!     alias path reports), never a parse error.
//!  3. Grammar consistency with the alias it replaces: a form accepted via
//!     `--to-name` must be accepted as positional TO with the same DB
//!     fingerprint (alias minus warning == positional).
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

const TEAM: &str = "togram";

struct GrammarCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    socket: PathBuf,
}

impl GrammarCase {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            format!("---\nname: {TEAM}\nobjective: positional TO grammar.\nprovider: fake\n---\n"),
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

    /// Positional send: `send <to> <message> [--team]` — the canonical form.
    fn send_positional(
        &self,
        to: &str,
        team: Option<&str>,
        probe: &str,
    ) -> (Option<Value>, String) {
        let mut args = vec!["send", to, probe, "--workspace", self.workspace_str()];
        if let Some(team) = team {
            args.extend_from_slice(&["--team", team]);
        }
        args.push("--json");
        let output = self.run_cli(&args);
        let parsed = serde_json::from_slice::<Value>(&output.stdout).ok();
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (parsed, combined)
    }

    fn db_rows(&self, needle: &str) -> Vec<(String, String)> {
        let db = self.workspace.join(".team").join("runtime").join("team.db");
        if !db.exists() {
            return Vec::new();
        }
        let connection = rusqlite::Connection::open(&db).expect("open team.db");
        let mut statement = connection
            .prepare(
                "SELECT message_id, recipient FROM messages WHERE content LIKE ?1 ORDER BY rowid",
            )
            .expect("prepare message query");
        statement
            .query_map([format!("%{needle}%")], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
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

fn assert_persisted(
    label: &str,
    parsed: &Option<Value>,
    combined: &str,
    rows: &[(String, String)],
    expected_recipients: &[&str],
) {
    let value = parsed.as_ref().unwrap_or_else(|| {
        panic!("M2c ({label}): canonical positional TO must emit JSON; got: {combined}")
    });
    assert_eq!(
        value.get("ok").and_then(Value::as_bool),
        Some(true),
        "M2c ({label}): the canonical positional TO grammar must accept this logical form; \
         got: {combined}"
    );
    let message_id = value
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("<absent>");
    assert!(
        message_id.starts_with("msg_"),
        "M2c ({label}): positional TO must return a store-backed msg_* id; got {message_id}"
    );
    let mut recipients: Vec<&str> = rows
        .iter()
        .map(|(_, recipient)| recipient.as_str())
        .collect();
    recipients.sort_unstable();
    let mut expected = expected_recipients.to_vec();
    expected.sort_unstable();
    assert_eq!(
        recipients, expected,
        "M2c ({label}): one persisted row per recipient with the resolved recipient column; \
         rows={rows:?}"
    );
}

/// Invariant 1 — the four accept-forms of the positional TO grammar all enter
/// the persisted create-message funnel.
#[test]
#[serial(env)]
fn positional_to_accepts_full_logical_grammar_into_one_funnel() {
    let case = GrammarCase::start("m2c-grammar");
    let workspace_qualified = format!("{}::{}/w1", case.workspace_str(), TEAM);
    let probes: Vec<(&str, String, Option<&str>, Vec<&str>)> = vec![
        ("short", "w1".to_string(), Some(TEAM), vec!["w1"]),
        ("qualified", format!("{TEAM}/w1"), None, vec!["w1"]),
        ("workspace-qualified", workspace_qualified, None, vec!["w1"]),
        (
            "comma-fanout",
            "w1,w2".to_string(),
            Some(TEAM),
            vec!["w1", "w2"],
        ),
    ];
    for (label, to, team, expected) in probes {
        let probe = format!("m2c grammar probe {label}");
        let (parsed, combined) = case.send_positional(to.as_str(), team, probe.as_str());
        let rows = case.db_rows(probe.as_str());
        assert_persisted(label, &parsed, &combined, &rows, &expected);
    }
    case.shutdown();
}

/// Invariant 2 — the leader logical form flows through the same grammar and
/// refuses canonically (not a parse error) when no leader is bound.
#[test]
#[serial(env)]
fn positional_to_leader_form_refuses_canonically_when_unbound() {
    let case = GrammarCase::start("m2c-leader");
    let to = format!("{TEAM}/leader");
    let (parsed, combined) = case.send_positional(to.as_str(), None, "m2c leader probe");
    let value = parsed.as_ref().unwrap_or_else(|| {
        panic!("M2c (leader): the leader form must parse and emit JSON; got: {combined}")
    });
    assert_eq!(
        value.get("ok").and_then(Value::as_bool),
        Some(false),
        "M2c (leader): unbound leader must refuse: {combined}"
    );
    assert_eq!(
        value.get("reason").and_then(Value::as_str),
        Some("leader_not_attached"),
        "M2c (leader): the refusal must be the canonical leader_not_attached shape (grammar \
         resolved, binding absent) — never an unknown-target/parse error; got: {combined}"
    );
    assert!(
        case.db_rows("m2c leader probe").is_empty(),
        "M2c (leader): a refused leader send must have zero side effects"
    );
    case.shutdown();
}

/// Invariant 3 — alias/positional parity: what `--to-name` accepts, the
/// positional grammar accepts with the same persisted fingerprint.
#[test]
#[serial(env)]
fn positional_to_matches_to_name_alias_fingerprint() {
    let case = GrammarCase::start("m2c-parity");
    let qualified = format!("{TEAM}/w1");

    let (alias_parsed, alias_combined) = {
        let output = case.run_cli(&[
            "send",
            "--to-name",
            qualified.as_str(),
            "m2c parity alias probe",
            "--workspace",
            case.workspace_str(),
            "--json",
        ]);
        (
            serde_json::from_slice::<Value>(&output.stdout).ok(),
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    };
    let alias_rows = case.db_rows("m2c parity alias probe");
    assert_persisted(
        "alias-baseline",
        &alias_parsed,
        &alias_combined,
        &alias_rows,
        &["w1"],
    );

    let (positional_parsed, positional_combined) =
        case.send_positional(qualified.as_str(), None, "m2c parity positional probe");
    let positional_rows = case.db_rows("m2c parity positional probe");
    assert_persisted(
        "positional-parity",
        &positional_parsed,
        &positional_combined,
        &positional_rows,
        &["w1"],
    );
    case.shutdown();
}
