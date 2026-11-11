//! Public CLI regressions for the independently reproduced PR223 R1-R5 findings.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ta-doctor-review-{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self { root, workspace, home }
    }

    fn run(&self, command: &str, workspace: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .arg(command).arg("--workspace").arg(workspace).args(args)
            .current_dir(&self.root)
            .env_clear().env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LANG", "C").env("LC_ALL", "C")
            .output().expect("run diagnostic CLI")
    }

    fn seed_runtime(&self) -> PathBuf {
        let runtime = self.workspace.join(".team/runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::write(runtime.join("state.json"), json!({
            "team_key": "alpha", "active_team_key": "alpha", "status": "alive",
            "session_name": "review-dead-session",
            "tmux_socket": self.workspace.join("never-started.sock"),
            "agents": {"worker": {"status": "missing", "provider": "pi", "window": "worker"}},
            "leader_receiver": {"status": "unbound"}
        }).to_string()).unwrap();
        runtime
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!(
        "invalid report: {error}; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)
    ))
}

fn bounded_triage_with_full_details<'a>(text: &'a str, expected: &Value) -> Vec<&'a str> {
    // 5f0bc003 appends the polling advisory footer; A1 (ec3ef36e) wraps it to
    // <=160-byte lines. It is not a triage line, so it is checked separately.
    let (text, footer) = text.split_once("\n\n[Advisory: do not poll]\n").expect("polling advisory footer");
    for line in footer.lines() {
        assert!(line.len() <= 160 && !line.chars().any(char::is_control), "unbounded advisory: {line}");
    }
    let mut triage = Vec::new();
    let mut details = None;
    for line in text.lines() {
        assert!(!line.chars().any(char::is_control), "control leaked: {line:?}");
        if let Some(json) = line.strip_prefix("details: ") {
            let parsed: Value = serde_json::from_str(json).expect("complete diagnostic JSON");
            assert!(details.replace(parsed).is_none(), "duplicate details line");
        } else {
            assert!(line.len() <= 160, "unbounded triage: {line}");
            triage.push(line);
        }
    }
    let details = details.expect("full diagnostic details missing");
    for key in ["error", "status", "issues", "suggested_repairs"] {
        if expected.get(key).is_some() {
            assert_eq!(details.get(key), expected.get(key), "lost full {key}");
        }
    }
    triage
}

// 262f6cdb (doctor v1): six ordered links, `team`, `workspace`, `ok`,
// `next_action`/`avoid`; the runtime/issues inventory is retired.
fn link_status<'a>(value: &'a Value, id: &str) -> &'a str {
    let links = value["links"].as_array().unwrap_or_else(|| panic!("doctor v1 links: {value}"));
    assert_eq!(links.len(), 6, "{value}");
    links.iter().find(|link| link["id"] == id).and_then(|link| link["status"].as_str()).unwrap_or("")
}

#[test]
fn r1_explicit_missing_team_fails_and_nested_team_resolves_runtime() {
    for command in ["doctor"] {
        let f = Fixture::new(command);
        // A3 (ec3ef36e): an empty workspace is a typed L1 failure (exit 1).
        let empty = f.run(command, &f.workspace, &["--json"]);
        assert_eq!(empty.status.code(), Some(1));
        assert_eq!(link_status(&report(&empty), "L1"), "FAIL");
        let missing = f.run(command, &f.workspace, &["--team", "absent", "--json"]);
        assert_eq!(missing.status.code(), Some(1));
        assert_eq!(link_status(&report(&missing), "L1"), "FAIL");
        assert_eq!(report(&missing)["team"], "absent");
        assert_eq!(report(&missing)["ok"], false);

        let runtime = f.seed_runtime();
        let before = fs::read(runtime.join("state.json")).unwrap();
        let nested = f.workspace.join("team-dir");
        fs::create_dir(&nested).unwrap();
        for input in [&f.workspace, &nested] {
            let out = f.run(command, input, &["--team", "alpha", "--json"]);
            let value = report(&out);
            assert_eq!(out.status.code(), Some(1), "unhealthy runtime: {value}");
            assert_eq!(link_status(&value, "L1"), "PASS", "{value}");
            assert_eq!(value["team"], "alpha");
            assert_eq!(value["workspace"], f.workspace.canonicalize().unwrap().to_string_lossy().as_ref());
        }
        assert_eq!(fs::read(runtime.join("state.json")).unwrap(), before);
    }
}

#[test]
fn r1_selected_team_status_is_independent_of_the_top_level_sibling() {
    let f = Fixture::new("selected-team-status");
    let runtime = f.workspace.join(".team/runtime");
    fs::create_dir_all(&runtime).unwrap();
    // The selected team's verdict depends only on that team, never on the
    // top-level sibling (262f6cdb v1: compare verdicts across sibling states).
    let mut verdicts: std::collections::BTreeMap<&str, (Option<i32>, Value, Vec<String>)> = Default::default();
    for top_status in ["stopped", "alive"] {
        for selected_status in ["alive", "stopped"] {
            let state = json!({
                "team_key": "alpha", "active_team_key": "alpha",
                "status": top_status, "session_name": "review-alpha", "agents": {},
                "teams": {
                    "alpha": {"team_key": "alpha", "status": top_status, "agents": {}},
                    "beta": {
                        "team_key": "beta", "status": selected_status,
                        "session_name": "review-disconnected-beta",
                        "tmux_socket": f.workspace.join("never-started-beta.sock"),
                        "agents": {"beta-worker": {"provider": "pi", "status": "missing", "window": "beta-worker"}},
                        "leader_receiver": {"status": "unbound"}
                    }
                }
            }).to_string();
            fs::write(runtime.join("state.json"), &state).unwrap();
            for command in ["doctor"] {
                let out = f.run(command, &f.workspace, &["--team", "beta", "--json"]);
                let value = report(&out);
                assert_eq!(value["team"], "beta", "{value}");
                if selected_status == "alive" {
                    assert_eq!(out.status.code(), Some(1), "{value}");
                    assert_eq!(link_status(&value, "L1"), "PASS", "{value}");
                }
                let statuses = ["L1", "L2", "L3", "L4", "L5", "L6"].iter()
                    .map(|id| link_status(&value, id).to_string()).collect::<Vec<_>>();
                let verdict = (out.status.code(), value["ok"].clone(), statuses);
                if let Some(previous) = verdicts.insert(selected_status, verdict.clone()) {
                    assert_eq!(previous, verdict, "selected beta verdict changed with top-level {top_status}: {value}");
                }
                assert_eq!(fs::read_to_string(runtime.join("state.json")).unwrap(), state);
            }
        }
    }
}

#[test]
fn r2_existing_bad_retired_database_is_ignored_without_state_and_never_rewritten() {
    // R-M4: the SQLite message store is retired; doctor never opens it, so a corrupt legacy file
    // is neither a schema failure nor ever rewritten, and doctor creates no state.
    for command in ["doctor"] {
        let f = Fixture::new("bad-db");
        let runtime = f.workspace.join(".team/runtime");
        fs::create_dir_all(&runtime).unwrap();
        let db = runtime.join("team.db");
        let original = b"not-a-sqlite-database\n";
        fs::write(&db, original).unwrap();
        let out = f.run(command, &f.workspace, &["--json"]);
        let value = report(&out);
        let text = value.to_string().to_lowercase();
        assert!(!text.contains("coordinator_schema_incompatible"), "{value}");
        assert!(!text.contains("sqlite") && !text.contains("not a database"), "{value}");
        assert_eq!(fs::read(db).unwrap(), original);
        assert!(!runtime.join("state.json").exists());
    }
}


#[test]
fn r3_each_finding_keeps_its_target_in_json_repairs_and_human_lines() {
    let f = Fixture::new("findings");
    // Synthetic assignments only; never inspect real user credentials.
    fs::write(f.workspace.join("first.md"),
        "OPENAI_API_KEY=synthetic-test-only\nANTHROPIC_API_KEY=synthetic-test-only\n").unwrap();
    fs::write(f.workspace.join("second.md"), "OPENAI_API_KEY=synthetic-test-only\n").unwrap();
    // 262f6cdb retired the secret-scan findings: neither renderer reports a
    // secret finding or ever echoes a secret value; human lines stay bounded.
    for command in ["doctor"] {
        let value = report(&f.run(command, &f.workspace, &["--json"]));
        assert_eq!(value["schema"], "team-agent.doctor.v1", "{value}");
        let rendered = value.to_string();
        assert!(!rendered.contains("secret_scan_finding") && !rendered.contains("synthetic-test-only"), "{value}");
        let human = f.run(command, &f.workspace, &[]);
        let text = String::from_utf8(human.stdout).unwrap();
        assert!(!text.contains("synthetic-test-only") && !text.contains("secret_scan_finding"), "{text}");
        for line in text.lines() {
            assert!(line.len() <= 160 && !line.chars().any(char::is_control), "unbounded triage: {line}");
        }
    }
}

#[test]
fn r4_comms_human_has_only_summary_issue_and_repair_lines() {
    let f = Fixture::new("comms");
    for command in ["doctor"] {
        let value = report(&f.run(command, &f.workspace, &["--comms", "--json"]));
        let output = f.run(command, &f.workspace, &["--comms"]);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with("doctor:"), "{text}");
        let triage = bounded_triage_with_full_details(&text, &value);
        assert_eq!(triage.len(), 1 + value["issues"].as_array().unwrap().len()
            + value["suggested_repairs"].as_array().unwrap().len(), "{text}");
    }
}
