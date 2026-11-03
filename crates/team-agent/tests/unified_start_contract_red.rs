//! RED — unified start contract, END STATE, black-box binary.
//!
//! `team-agent start` is the only seat verb a human or an LLM leader needs. The
//! retired seat verbs are either gone or trivial aliases of `start`; they never
//! keep a second engine, a second grammar or second guidance. All cases are
//! `--dry-run` in an isolated HOME/workspace, so nothing is launched.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Case {
    root: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ta-unified-start-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("ws")).unwrap();
        Self { root }
    }
    fn workspace(&self) -> PathBuf {
        self.root.join("ws")
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TEAM_AGENT_LEADER_PROVIDER", "claude")
            .output()
            .expect("run team-agent")
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be JSON ({error}); status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// `team-agent start` as a verb, not the prefix of `team-agent start-agent`.
fn names_start(text: &str) -> bool {
    text.match_indices("team-agent start")
        .any(|(index, needle)| !text[index + needle.len()..].starts_with('-'))
}

fn empty(path: &Path) -> bool {
    std::fs::read_dir(path).unwrap().next().is_none()
}

const CREATE: &[&str] = &["newbie", "--provider", "claude", "--bypass", "false", "--dry-run", "--json"];

#[test]
fn start_dry_run_declares_the_created_seat() {
    let case = Case::new("create");
    let mut args = vec!["start"];
    args.extend_from_slice(CREATE);
    let output = case.run(&args);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let plan = json(&output);
    assert!(empty(&case.workspace()), "dry-run must not create files");
    let step = &plan["steps"][0];
    assert_eq!(step["agent"], "newbie", "{plan}");
    assert_eq!(step["op"], "created", "{plan}");
    assert_eq!(step["declaration"]["bypass"], false, "{plan}");
    assert!(step["declaration"]["model"].is_null(), "{plan}");
    assert!(step["defaulted"].is_array(), "defaulted must always be reported: {plan}");
    // Zero ceremony: omitted decisions are filled and reported, never hidden.
    let bare = json(&case.run(&["start", "solo", "--dry-run", "--json"]));
    let defaulted = bare["steps"][0]["defaulted"].as_array().cloned().unwrap_or_default();
    for field in ["provider", "bypass"] {
        assert!(defaulted.iter().any(|value| value == field), "{field} must be reported: {bare}");
    }
}

/// A retired seat verb is either a trivial alias of `start` (identical plan) or
/// absent with a refusal that names `team-agent start` — never its own engine.
fn assert_alias_or_retired(case: &Case, legacy: &[&str]) {
    let mut canonical = vec!["start"];
    canonical.extend_from_slice(CREATE);
    let expected = json(&case.run(&canonical));
    let output = case.run(legacy);
    let value = json(&output);
    if output.status.success() {
        assert_eq!(value["action"], "start", "{legacy:?} must run start: {value}");
        assert_eq!(value["steps"], expected["steps"], "{legacy:?} must plan exactly like start");
    } else {
        let guidance = format!("{} {}", value["error"], value["action"]);
        assert!(names_start(&guidance), "{legacy:?} refusal must name team-agent start: {value}");
        for retired in ["add-agent", "start-agent", "agent add"] {
            assert!(!value["action"].as_str().unwrap_or_default().contains(retired),
                "{legacy:?} must not send users back to {retired}: {value}");
        }
    }
}

#[test]
fn add_agent_is_start_or_gone() {
    let case = Case::new("add-agent");
    let mut args = vec!["add-agent"];
    args.extend_from_slice(CREATE);
    assert_alias_or_retired(&case, &args);
}

#[test]
fn start_agent_is_start_or_gone() {
    let case = Case::new("start-agent");
    let mut args = vec!["start-agent"];
    args.extend_from_slice(CREATE);
    assert_alias_or_retired(&case, &args);
}

#[test]
fn agent_add_is_start_or_gone() {
    let case = Case::new("agent-add");
    let mut args = vec!["agent", "add"];
    args.extend_from_slice(CREATE);
    assert_alias_or_retired(&case, &args);
}

#[test]
fn help_teaches_one_seat_verb_and_in_place_migration() {
    let case = Case::new("help");
    let help = String::from_utf8_lossy(&case.run(&["--help"]).stdout).into_owned();
    assert!(help.lines().any(|line| line.trim_start().starts_with("start ")), "{help}");
    for retired in ["add-agent", "start-agent", "quick-start"] {
        assert!(!help.lines().any(|line| line.trim_start().starts_with(&format!("{retired} "))),
            "--help must not advertise {retired}:\n{help}");
    }
    let start = String::from_utf8_lossy(&case.run(&["start", "--help"]).stdout).into_owned();
    assert!(start.contains("--session preserve|auto|fresh"), "{start}");
    assert!(start.contains("--provider") && start.contains("--session fresh"), "{start}");
}
