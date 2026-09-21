//! P1-P18 black-box RED contracts for the doctor/diagnose unification taskbook.
//!
//! This file deliberately talks to the public CLI only.  Every fixture is disposable and
//! every assertion is phrased as a user-visible contract, rather than an implementation
//! detail.  It is kept in the tester worktree so the development track cannot use it as an
//! implementation guide before the red-to-green handoff.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

struct Fixture {
    workspace: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ta-doctor-unification-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir_all(&workspace).expect("create fixture workspace");
        fs::create_dir_all(&home).expect("create fixture home");
        Self { workspace, home }
    }

    fn clone_as(&self, tag: &str) -> Self {
        let clone = Self::new(tag);
        copy_tree(&self.workspace, &clone.workspace);
        clone
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(root) = self.workspace.parent() {
            let _ = fs::remove_dir_all(root);
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copied fixture");
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("fixture entry");
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &target);
        } else {
            fs::copy(source, target).expect("copy fixture file");
        }
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn run(name: &str, args: &[&str], fixture: &Fixture) -> Output {
    run_with_path(name, args, fixture, None)
}

fn run_with_path(
    name: &str,
    args: &[&str],
    fixture: &Fixture,
    path_override: Option<&Path>,
) -> Output {
    let mut command = Command::new(bin());
    command
        .arg(name)
        .args(args)
        .current_dir(&fixture.workspace)
        .env("HOME", &fixture.home)
        .env("TMPDIR", std::env::temp_dir())
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TEAM_AGENT_WORKSPACE")
        .env_remove("TEAM_AGENT_ACTIVE_TEAM");
    if let Some(path) = path_override {
        command.env("PATH", path);
    }
    command.output().expect("run team-agent command")
}

fn run_root(args: &[&str], fixture: &Fixture) -> Output {
    let mut command = Command::new(bin());
    command
        .args(args)
        .current_dir(&fixture.workspace)
        .env("HOME", &fixture.home)
        .env("TMPDIR", std::env::temp_dir())
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .output()
        .expect("run team-agent root command")
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON stdout; error={error}; stdout={}; stderr={}",
            text(&output.stdout),
            text(&output.stderr)
        )
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn issue_values(value: &Value) -> Vec<Value> {
    value
        .get("issues")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn issue_keys(value: &Value) -> BTreeSet<String> {
    issue_values(value)
        .into_iter()
        .map(|issue| issue.to_string())
        .collect()
}

fn runtime_status(value: &Value) -> &str {
    value
        .pointer("/runtime/status")
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
}

fn save_state(fixture: &Fixture, state: Value) {
    team_agent::state::persist::save_runtime_state(&fixture.workspace, &state)
        .expect("save runtime fixture");
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, current: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(current).expect("read snapshot directory") {
            let entry = entry.expect("snapshot entry");
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("snapshot relative path")
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.push((format!("{relative}/"), Vec::new()));
                visit(root, &path, out);
            } else {
                out.push((relative, fs::read(path).expect("read snapshot file")));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

fn count_key(value: &Value, key: &str) -> usize {
    match value {
        Value::Object(object) => object
            .iter()
            .map(|(name, child)| (if name == key { 1 } else { 0 }) + count_key(child, key))
            .sum(),
        Value::Array(items) => items.iter().map(|child| count_key(child, key)).sum(),
        _ => 0,
    }
}

fn contains_issue_fragment(value: &Value, fragment: &str) -> bool {
    issue_values(value)
        .iter()
        .any(|issue| issue.to_string().contains(fragment))
}

#[test]
fn p1_doctor_and_diagnose_are_byte_and_exit_equivalent() {
    let cases: &[&[&str]] = &[
        &[],
        &["--json"],
        &["--team", "alpha", "--json"],
        &["--comms", "--json"],
        &["--gate", "unknown", "--json"],
        &["--fix"],
        &["--fix-schema", "--json"],
    ];
    for (index, args) in cases.iter().enumerate() {
        let left = Fixture::new(&format!("p1-doctor-{index}"));
        let right = left.clone_as(&format!("p1-diagnose-{index}"));
        let doctor = run("doctor", args, &left);
        let diagnose = run("diagnose", args, &right);
        assert_eq!(
            doctor.status.code(),
            diagnose.status.code(),
            "P1 exit mismatch for args={args:?}; doctor={} diagnose={}",
            text(&doctor.stdout),
            text(&diagnose.stdout)
        );
        assert_eq!(
            doctor.stdout, diagnose.stdout,
            "P1 stdout mismatch for args={args:?}"
        );
        assert_eq!(
            doctor.stderr, diagnose.stderr,
            "P1 stderr mismatch for args={args:?}"
        );
    }
}

#[test]
fn p2_doctor_is_the_only_discoverable_entry_and_alias_forwards_all_flags() {
    let fixture = Fixture::new("p2-help");
    let default_help = run_root(&["--help"], &fixture);
    let help_text = text(&default_help.stdout);
    assert!(
        help_text.contains("doctor"),
        "P2 default help must recommend doctor"
    );
    assert!(
        !help_text
            .lines()
            .any(|line| line.trim_start().starts_with("diagnose")),
        "P2 default help must not expose diagnose as a peer command: {help_text}"
    );

    let doctor_help = run("doctor", &["--help"], &fixture);
    let diagnose_help = run("diagnose", &["--help"], &fixture);
    assert_eq!(doctor_help.status.code(), Some(0));
    assert_eq!(diagnose_help.status.code(), Some(0));
    assert_eq!(doctor_help.stdout, diagnose_help.stdout);
    assert_eq!(doctor_help.stderr, diagnose_help.stderr);

    let doctor = run("doctor", &["--fix", "--json"], &fixture);
    let diagnose = run("diagnose", &["--fix", "--json"], &fixture);
    assert_eq!(doctor.status.code(), diagnose.status.code());
    assert_eq!(doctor.stdout, diagnose.stdout);
    assert_eq!(doctor.stderr, diagnose.stderr);
}

#[test]
fn p3_empty_and_unstarted_workspaces_are_not_present_not_missing_sessions() {
    for (index, with_team_dir) in [false, true].into_iter().enumerate() {
        let fixture = Fixture::new(&format!("p3-{index}"));
        if with_team_dir {
            fs::create_dir_all(fixture.workspace.join(".team")).expect("create empty team dir");
        }
        for name in ["doctor", "diagnose"] {
            let output = run(name, &["--json"], &fixture);
            let value = report(&output);
            assert_eq!(
                runtime_status(&value),
                "not_present",
                "P3 {name} must report no runtime for an unstarted workspace: {value}"
            );
            assert!(
                !contains_issue_fragment(&value, "session_missing")
                    && !contains_issue_fragment(&value, "missing_session"),
                "P3 {name} fabricated a missing-session issue for no runtime: {value}"
            );
        }
    }
}

#[test]
fn p4_ok_issues_and_exit_code_are_one_truth_in_both_renderers() {
    let fixture = Fixture::new("p4-truth");
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {},
            "leader_receiver": {"status": "unbound"}
        }),
    );
    for name in ["doctor", "diagnose"] {
        let json_output = run(name, &["--json"], &fixture);
        let value = report(&json_output);
        let issues = issue_values(&value);
        let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
        assert!(
            !issues.is_empty(),
            "P4 fixture must contain an unbound issue: {value}"
        );
        assert_eq!(
            ok,
            issues.is_empty(),
            "P4 final ok diverged from issues: {value}"
        );
        assert_eq!(json_output.status.code(), Some(if ok { 0 } else { 1 }));

        let human_output = run(name, &[], &fixture);
        assert_eq!(
            human_output.status.code(),
            json_output.status.code(),
            "P4 human/JSON exit mismatch for {name}; human={}",
            text(&human_output.stdout)
        );
    }
}

#[test]
fn p5_environment_facts_survive_adding_runtime_facts() {
    let base = Fixture::new("p5-base");
    fs::write(
        base.workspace.join("credential-fixture.txt"),
        "OPENAI_API_KEY=synthetic-p5-secret\n",
    )
    .expect("write secret fixture");
    let extended = base.clone_as("p5-runtime");
    save_state(
        &extended,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {"worker": {"status": "missing"}},
            "leader_receiver": {"status": "unbound"}
        }),
    );
    let before = report(&run("doctor", &["--json"], &base));
    let after = report(&run("doctor", &["--json"], &extended));
    let before_issues = issue_keys(&before);
    let after_issues = issue_keys(&after);
    assert!(
        !before_issues.is_empty(),
        "P5 base must expose its environment failure: {before}"
    );
    assert!(
        before_issues.is_subset(&after_issues),
        "P5 adding runtime facts deleted an environment issue: before={before} after={after}"
    );
    assert_eq!(
        before.pointer("/secret_scan/findings"),
        after.pointer("/secret_scan/findings"),
        "P5 secret/environment evidence disappeared after runtime selection"
    );
}

#[test]
fn p6_dead_runtime_is_not_absent_and_keeps_failure_visible() {
    let absent = Fixture::new("p6-absent");
    let absent_report = report(&run("doctor", &["--json"], &absent));
    assert_eq!(runtime_status(&absent_report), "not_present");

    let dead = Fixture::new("p6-dead");
    save_state(
        &dead,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {"worker": {"status": "missing"}},
            "leader_receiver": {"status": "attached", "pane_id": "%gone"}
        }),
    );
    let dead_report = report(&run("doctor", &["--json"], &dead));
    assert_ne!(
        runtime_status(&dead_report),
        "not_present",
        "P6 persisted-but-dead runtime was collapsed into absent: {dead_report}"
    );
    assert!(!issue_values(&dead_report).is_empty());
    assert!(
        text(&serde_json::to_vec(&dead_report).unwrap()).contains("missing")
            || text(&serde_json::to_vec(&dead_report).unwrap()).contains("dead"),
        "P6 dead runtime report lost concrete failure evidence: {dead_report}"
    );
}

#[test]
fn p7_issue_deduplication_does_not_collapse_distinct_failures() {
    let fixture = Fixture::new("p7-dedup");
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {
                "worker-a": {"status": "missing"},
                "worker-b": {"status": "missing"}
            },
            "leader_receiver": {"status": "unbound"}
        }),
    );
    let value = report(&run("doctor", &["--json"], &fixture));
    let values = issue_values(&value);
    let unique = issue_keys(&value);
    assert_eq!(
        values.len(),
        unique.len(),
        "P7 duplicate issue entries leaked: {value}"
    );
    assert!(
        values.len() >= 2,
        "P7 distinct failures were collapsed: {value}"
    );
    let rendered = value.to_string();
    assert!(rendered.contains("worker-a") && rendered.contains("worker-b"));
}

#[test]
fn p8_runtime_report_contains_one_coordinator_observation() {
    let fixture = Fixture::new("p8-coordinator");
    fs::create_dir_all(fixture.workspace.join(".team/runtime")).expect("create runtime directory");
    fs::write(
        fixture.workspace.join(".team/runtime/coordinator.pid"),
        "4000000\n",
    )
    .expect("write stale coordinator pid");
    fs::write(
        fixture.workspace.join(".team/runtime/coordinator.json"),
        json!({
            "pid": 4000000,
            "protocol_version": 2,
            "message_store_schema_version": 5,
            "binary_path": path_arg(Path::new(bin())),
            "binary_version": env!("CARGO_PKG_VERSION"),
            "source": "test",
            "updated_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("write coordinator metadata");
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {},
            "leader_receiver": {"status": "attached", "pane_id": "%gone"}
        }),
    );
    let value = report(&run("doctor", &["--json"], &fixture));
    assert_eq!(
        count_key(&value, "coordinator"),
        1,
        "P8 report must expose one shared coordinator observation, not duplicate probes: {value}"
    );
}

#[test]
fn p9_default_diagnosis_is_read_only_for_both_spellings() {
    for name in ["doctor", "diagnose"] {
        let fixture = Fixture::new(&format!("p9-{name}"));
        save_state(
            &fixture,
            json!({
                "team_key": "alpha",
                "active_team_key": "alpha",
                "session_name": "alpha-session",
                "agents": {"worker": {"status": "missing"}},
                "leader_receiver": {"status": "unbound"}
            }),
        );
        let before = snapshot(&fixture.workspace);
        let _ = run(name, &["--json"], &fixture);
        let after = snapshot(&fixture.workspace);
        assert_eq!(before, after, "P9 {name} mutated the selected workspace");
    }
}

#[cfg(unix)]
#[test]
fn p10_default_empty_check_does_not_execute_unrelated_commands() {
    let poison = Fixture::new("p10-poison");
    let marker = poison.workspace.join("executed.log");
    let commands = [
        "claude", "codex", "gemini", "grok", "agent", "tmux", "git", "sysctl", "curl",
    ];
    for command in commands {
        let script = poison.workspace.join(command);
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}' >> '{}'\nexit 0\n",
                script.display(),
                marker.display()
            ),
        )
        .expect("write poison command");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script)
            .expect("poison metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make poison executable");
    }
    let output = run_with_path("doctor", &["--json"], &poison, Some(&poison.workspace));
    let touched = fs::read_to_string(marker).unwrap_or_default();
    assert!(
        touched.is_empty(),
        "P10 default empty diagnosis executed an unrelated command: {touched:?}; output={}",
        text(&output.stdout)
    );
}

#[test]
fn p11_dead_coordinator_short_circuits_without_repeated_failure_entries() {
    let fixture = Fixture::new("p11-short-circuit");
    fs::create_dir_all(fixture.workspace.join(".team/runtime")).expect("create runtime directory");
    fs::write(
        fixture.workspace.join(".team/runtime/coordinator.pid"),
        "4000001\n",
    )
    .expect("write dead pid");
    fs::write(
        fixture.workspace.join(".team/runtime/coordinator.json"),
        json!({"pid": 4000001, "protocol_version": 2}).to_string(),
    )
    .expect("write dead coordinator metadata");
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {"worker": {"status": "missing"}},
            "leader_receiver": {"status": "attached", "pane_id": "%gone"}
        }),
    );
    let value = report(&run("doctor", &["--json"], &fixture));
    let coordinator_issues = issue_values(&value)
        .into_iter()
        .filter(|issue| issue.to_string().contains("coordinator"))
        .count();
    assert_eq!(
        coordinator_issues, 1,
        "P11 dead coordinator was probed repeatedly: {value}"
    );
    assert_eq!(value.get("ok").and_then(Value::as_bool), Some(false));
}

#[test]
fn p12_human_output_is_bounded_utf8_control_free_triage() {
    let fixture = Fixture::new("p12-triage");
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {
                "agent-🙂-" : {"status": "missing", "detail": "中".repeat(120)}
            },
            "leader_receiver": {"status": "unbound"}
        }),
    );
    for name in ["doctor", "diagnose"] {
        let output = run(name, &[], &fixture);
        let human = text(&output.stdout);
        assert!(
            human
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("doctor:")),
            "P12 {name} human output must use doctor triage prefix: {human}"
        );
        for (line_no, line) in human.lines().enumerate() {
            assert!(
                line.as_bytes().len() <= 160,
                "P12 {name} line {line_no} exceeds 160 bytes: {}",
                line.as_bytes().len()
            );
            assert!(!line.chars().any(char::is_control));
        }
        assert!(!human.contains("issues: [") && !human.contains("runtime: {"));
    }
}

#[test]
fn p13_json_keeps_full_evidence_but_redacts_secret_values() {
    let fixture = Fixture::new("p13-redaction");
    let secret = "synthetic-p13-secret-value";
    fs::write(
        fixture.workspace.join("credential-fixture.rs"),
        format!("let example = \"OPENAI_API_KEY={secret}\";\nOPENAI_API_KEY={secret}\n"),
    )
    .expect("write redaction fixture");
    let value = report(&run("doctor", &["--json"], &fixture));
    for key in [
        "event_log",
        "issues",
        "ok",
        "providers",
        "runtime",
        "suggested_repairs",
    ] {
        assert!(
            value.get(key).is_some(),
            "P13 missing stable JSON field {key}: {value}"
        );
    }
    let findings = value
        .pointer("/secret_scan/findings")
        .and_then(Value::as_array)
        .expect("P13 secret findings array");
    assert!(
        !findings.is_empty(),
        "P13 fixture did not produce a secret finding"
    );
    assert!(!value.to_string().contains(secret));
    for finding in findings {
        assert!(finding.get("rule").and_then(Value::as_str).is_some());
        assert!(finding.get("path").and_then(Value::as_str).is_some());
        assert!(finding.get("line").is_some());
        for forbidden in ["value", "secret", "match", "match_excerpt"] {
            assert!(
                finding.get(forbidden).is_none(),
                "P13 leaked {forbidden}: {finding}"
            );
        }
    }
}

#[test]
fn p14_explicit_team_selection_is_isolated_and_missing_selection_fails() {
    let fixture = Fixture::new("p14-teams");
    save_state(
        &fixture,
        json!({
            "active_team_key": "alpha",
            "team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {},
            "teams": {
                "alpha": {"status": "alive", "team_key": "alpha", "agents": {"alpha-only": {"status": "running"}}},
                "beta": {"status": "alive", "team_key": "beta", "agents": {"beta-only": {"status": "missing"}}}
            }
        }),
    );
    let alpha = report(&run("doctor", &["--team", "alpha", "--json"], &fixture));
    let beta = report(&run("doctor", &["--team", "beta", "--json"], &fixture));
    assert_eq!(
        alpha.pointer("/runtime/team_key").and_then(Value::as_str),
        Some("alpha")
    );
    assert_eq!(
        beta.pointer("/runtime/team_key").and_then(Value::as_str),
        Some("beta")
    );
    assert!(!alpha.to_string().contains("beta-only"));
    assert!(!beta.to_string().contains("alpha-only"));
    let missing = run("doctor", &["--team", "does-not-exist", "--json"], &fixture);
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn p15_explicit_gate_and_repair_modes_keep_safe_rejections_and_evidence() {
    let cases: &[&[&str]] = &[
        &["--fix", "--json"],
        &["--gate", "not-a-gate", "--json"],
        &["--comms", "--json"],
    ];
    for args in cases {
        let left = Fixture::new("p15-doctor");
        let right = left.clone_as("p15-diagnose");
        let doctor = run("doctor", args, &left);
        let diagnose = run("diagnose", args, &right);
        assert_eq!(
            doctor.status.code(),
            diagnose.status.code(),
            "P15 rc mismatch: {args:?}"
        );
        assert_eq!(
            doctor.stdout, diagnose.stdout,
            "P15 stdout mismatch: {args:?}"
        );
        assert_eq!(
            doctor.stderr, diagnose.stderr,
            "P15 stderr mismatch: {args:?}"
        );
    }
    let fixture = Fixture::new("p15-comms-shape");
    let comms = report(&run("doctor", &["--comms", "--json"], &fixture));
    for key in ["boundary", "scope", "checks", "status"] {
        assert!(
            comms.get(key).is_some(),
            "P15 comms evidence lost field {key}: {comms}"
        );
    }
}

#[test]
fn p16_base_failures_are_not_overwritten_by_schema_or_receiver_failures() {
    let fixture = Fixture::new("p16-combined");
    fs::create_dir_all(fixture.workspace.join(".team/runtime")).expect("create runtime directory");
    fs::write(fixture.workspace.join(".team/team.db"), b"not sqlite").expect("write bad schema");
    fs::write(
        fixture.workspace.join(".team/runtime/coordinator.pid"),
        "4000002\n",
    )
    .expect("write stale pid");
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "agents": {},
            "leader_receiver": {"status": "unbound"}
        }),
    );
    let output = run("doctor", &["--json"], &fixture);
    let value = report(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        contains_issue_fragment(&value, "coordinator") || contains_issue_fragment(&value, "schema")
    );
    assert!(
        contains_issue_fragment(&value, "leader")
            || contains_issue_fragment(&value, "receiver")
            || contains_issue_fragment(&value, "unbound"),
        "P16 receiver/leader failure was overwritten: {value}"
    );
}

#[test]
fn p17_normal_shutdown_is_not_reported_as_missing_session() {
    let fixture = Fixture::new("p17-stopped");
    save_state(
        &fixture,
        json!({
            "status": "stopped",
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "archived-alpha",
            "agents": {},
            "leader_receiver": {"status": "stopped"},
            "teams": {"alpha": {"status": "stopped", "team_key": "alpha", "agents": {}}}
        }),
    );
    let value = report(&run("doctor", &["--json"], &fixture));
    assert_eq!(runtime_status(&value), "not_present");
    assert!(!contains_issue_fragment(&value, "session_missing"));
    assert!(!contains_issue_fragment(&value, "missing_session"));
}

#[test]
fn p18_unrelated_cli_help_and_status_entry_points_remain_available() {
    let fixture = Fixture::new("p18-other-commands");
    for command in ["preflight", "wait-ready", "status"] {
        let output = run(command, &["--help"], &fixture);
        assert_eq!(
            output.status.code(),
            Some(0),
            "P18 unrelated command {command} help regressed: {}",
            text(&output.stderr)
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore = "P18 ConPTY fixture is NOT-RUN in the Mac/Linux red-test lane"]
fn p18_windows_conpty_fixture_is_reserved_for_platform_lane() {}
