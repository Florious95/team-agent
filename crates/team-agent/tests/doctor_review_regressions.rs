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

fn issue_id(issue: &Value) -> Option<&str> {
    issue.as_str().or_else(|| issue.get("id").and_then(Value::as_str))
}

#[test]
fn r1_explicit_missing_team_fails_and_nested_team_resolves_runtime() {
    for command in ["doctor"] {
        let f = Fixture::new(command);
        assert!(f.run(command, &f.workspace, &["--json"]).status.success());
        let missing = f.run(command, &f.workspace, &["--team", "absent", "--json"]);
        assert_eq!(missing.status.code(), Some(1));
        assert_eq!(report(&missing)["runtime"]["status"], "unresolved");
        assert_eq!(report(&missing)["ok"], false);

        let runtime = f.seed_runtime();
        let before = fs::read(runtime.join("state.json")).unwrap();
        let nested = f.workspace.join("team-dir");
        fs::create_dir(&nested).unwrap();
        for input in [&f.workspace, &nested] {
            let out = f.run(command, input, &["--team", "alpha", "--json"]);
            let value = report(&out);
            assert_eq!(out.status.code(), Some(1), "unhealthy runtime: {value}");
            assert_eq!(value["runtime"]["status"], "present");
            assert_eq!(value["runtime"]["team_key"], "alpha");
            assert_eq!(value["runtime"]["run_workspace"], f.workspace.to_string_lossy().as_ref());
        }
        assert_eq!(fs::read(runtime.join("state.json")).unwrap(), before);
    }
}

#[test]
fn r1_selected_team_status_is_independent_of_the_top_level_sibling() {
    let f = Fixture::new("selected-team-status");
    let runtime = f.workspace.join(".team/runtime");
    fs::create_dir_all(&runtime).unwrap();
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
                let alive = selected_status == "alive";
                assert_eq!(out.status.code(), Some(if alive { 1 } else { 0 }), "{value}");
                assert_eq!(value["ok"], !alive, "{value}");
                assert_eq!(value["runtime"]["status"], if alive { "present" } else { "not_present" }, "{value}");
                assert_eq!(value["runtime"]["team_key"], "beta", "{value}");
                assert_eq!(fs::read_to_string(runtime.join("state.json")).unwrap(), state);
            }
        }
    }
}

#[test]
fn r2_existing_bad_database_is_checked_without_state_and_never_rewritten() {
    for command in ["doctor"] {
        let f = Fixture::new("bad-db");
        let runtime = f.workspace.join(".team/runtime");
        fs::create_dir_all(&runtime).unwrap();
        let db = runtime.join("team.db");
        let original = b"not-a-sqlite-database\n";
        fs::write(&db, original).unwrap();
        let out = f.run(command, &f.workspace, &["--json"]);
        let value = report(&out);
        assert_eq!(out.status.code(), Some(1), "{value}");
        assert_eq!(value["ok"], false);
        assert_eq!(value["coordinator"]["schema_ok"], false);
        assert!(value["issues"].as_array().unwrap().iter()
            .any(|issue| issue_id(issue) == Some("coordinator_schema_incompatible")));
        assert_eq!(fs::read(db).unwrap(), original);
        assert!(!runtime.join("state.json").exists());
    }
}

#[test]
fn r2_valid_database_without_state_does_not_require_a_coordinator() {
    let f = Fixture::new("valid-db");
    drop(team_agent::message_store::MessageStore::open(&f.workspace).unwrap());
    let db = f.workspace.join(".team/runtime/team.db");
    let before = fs::read(&db).unwrap();
    let out = f.run("doctor", &f.workspace, &["--json"]);
    let value = report(&out);
    assert!(out.status.success(), "{value}");
    assert_eq!(value["runtime"]["status"], "not_present");
    assert_eq!(value["coordinator"]["schema_ok"], true);
    assert_eq!(fs::read(db).unwrap(), before);
    assert!(!f.workspace.join(".team/runtime/state.json").exists());
}

#[test]
fn r3_each_finding_keeps_its_target_in_json_repairs_and_human_lines() {
    let f = Fixture::new("findings");
    // Synthetic assignments only; never inspect real user credentials.
    fs::write(f.workspace.join("first.md"),
        "OPENAI_API_KEY=synthetic-test-only\nANTHROPIC_API_KEY=synthetic-test-only\n").unwrap();
    fs::write(f.workspace.join("second.md"), "OPENAI_API_KEY=synthetic-test-only\n").unwrap();
    for command in ["doctor"] {
        let value = report(&f.run(command, &f.workspace, &["--json"]));
        let findings = value["issues"].as_array().unwrap().iter()
            .filter(|issue| issue_id(issue) == Some("secret_scan_finding")).collect::<Vec<_>>();
        let repairs = value["suggested_repairs"].as_array().unwrap().iter()
            .filter(|repair| repair["issue"] == "secret_scan_finding").collect::<Vec<_>>();
        assert_eq!(findings.len(), 3, "{value}");
        assert_eq!(repairs.len(), 3, "{value}");
        for finding in findings {
            assert!(repairs.iter().any(|repair| repair["path"] == finding["path"]
                && repair["line"] == finding["line"] && repair["rule"] == finding["rule"]));
        }
        let human = f.run(command, &f.workspace, &[]);
        let text = String::from_utf8(human.stdout).unwrap();
        for target in ["first.md:1", "first.md:2", "second.md:1"] {
            assert!(text.lines().any(|line| line.starts_with("issue:") && line.contains(target)), "{text}");
            assert!(text.lines().any(|line| line.starts_with("repair:") && line.contains(target)), "{text}");
        }
        assert!(!text.contains("synthetic-test-only"));
        assert!(text.lines().all(|line| line.len() <= 160));
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
        assert_eq!(text.lines().count(), 1 + value["issues"].as_array().unwrap().len()
            + value["suggested_repairs"].as_array().unwrap().len(), "{text}");
        assert!(text.lines().all(|line| line.len() <= 160));
    }
}

#[test]
#[cfg(unix)]
fn r5_coordinator_issue_and_detail_share_one_observation_during_pid_rotation() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    for command in ["doctor"] {
        let f = Fixture::new("pid-rotation");
        let runtime = f.seed_runtime();
        let fifo = runtime.join("coordinator.pid");
        let replacement = runtime.join("replacement.pid");
        fs::write(&replacement, "2147483647\n").unwrap();
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let writer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(25);
            loop {
                if let Ok(mut stream) = fs::OpenOptions::new().write(true)
                    .custom_flags(libc::O_NONBLOCK).open(&fifo)
                {
                    stream.write_all(b"not-a-pid\n").unwrap();
                    // Replacement precedes first-reader EOF: a second health
                    // read would deterministically observe stale, not invalid_pid.
                    fs::rename(&replacement, &fifo).unwrap();
                    break;
                }
                assert!(Instant::now() < deadline, "no coordinator snapshot reader");
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let value = report(&f.run(command, &f.workspace, &["--json"]));
        writer.join().unwrap();
        let issue = value["issues"].as_array().unwrap().iter()
            .find(|issue| issue_id(issue) == Some("coordinator_unavailable")).unwrap();
        assert_eq!(issue["status"], "invalid_pid", "{value}");
        assert_eq!(value["coordinator"]["status"], issue["status"], "{value}");
        assert_eq!(value["coordinator"]["pid"], issue["pid"]);
        assert_eq!(value["coordinator"]["schema_ok"], issue["schema_ok"]);
    }
}
