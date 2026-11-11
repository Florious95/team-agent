//! Black-box contracts for the canonical doctor health report.
//!
//! Disposable fixtures exercise public runtime, issue, rendering, and read-only behavior.
//! Command-removal contracts belong to the dedicated command-removal suite.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use serde_json::{json, Value};
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
        let root =
            std::env::temp_dir().join(format!("ta-doctor-health-{tag}-{}-{n}", std::process::id()));
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
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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

// 262f6cdb (doctor v1): the report is `schema`, `ok`, `team`, `workspace`,
// six ordered `links` (L1 scope .. L6 return, each PASS/WARN/FAIL/BLOCKED with
// a message), plus `next_action`/`avoid` when not ok. The retired inventory
// (runtime/issues/secret_scan/coordinator) is not resurrected.
fn links(value: &Value) -> Vec<Value> {
    let links = value["links"].as_array().cloned().unwrap_or_default();
    let ids: Vec<_> = links.iter().map(|link| link["id"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(ids, ["L1", "L2", "L3", "L4", "L5", "L6"], "doctor v1 must report six ordered links: {value}");
    assert_eq!(value["schema"], "team-agent.doctor.v1", "{value}");
    links
}

fn link(value: &Value, id: &str) -> Value {
    links(value).into_iter().find(|link| link["id"] == id).unwrap()
}

fn link_status(value: &Value, id: &str) -> String {
    link(value, id)["status"].as_str().unwrap_or("").to_string()
}

fn assert_v1_truth(value: &Value, output: &Output) {
    let all_pass = links(value).iter().all(|link| link["status"] == "PASS");
    assert_eq!(value["ok"], json!(all_pass), "doctor ok must equal all six links passing: {value}");
    assert_eq!(output.status.code(), Some(if all_pass { 0 } else { 1 }), "{value}");
    if !all_pass {
        assert!(value.get("next_action").is_some() && value["avoid"].is_string(), "{value}");
    }
}

/// A test-owned, bounded tmux host (<=120s via TmuxRoot's watchdog) whose
/// absolute socket a fixture records, so L2 is reachable and L3 is evaluated.
struct OwnedHost {
    root: hermetic_guard::proc_guard::TmuxRoot,
    socket: PathBuf,
}

impl OwnedHost {
    fn new() -> Self {
        let root = hermetic_guard::proc_guard::TmuxRoot::new();
        let socket = root.path().join("h.sock");
        let output = Command::new("tmux")
            .args(["-S"]).arg(&socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "doctor-host", "sleep 100"])
            .output()
            .expect("start owned tmux host");
        assert!(output.status.success(), "{}", text(&output.stderr));
        Self { root, socket }
    }
}

impl Drop for OwnedHost {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-S"]).arg(&self.socket).arg("kill-server").output();
        self.root.close();
    }
}

#[test]
fn p3_empty_and_unstarted_workspaces_are_not_present_not_missing_sessions() {
    for (index, with_team_dir) in [false, true].into_iter().enumerate() {
        let fixture = Fixture::new(&format!("p3-{index}"));
        if with_team_dir {
            fs::create_dir_all(fixture.workspace.join(".team")).expect("create empty team dir");
        }
        let output = run("doctor", &["--json"], &fixture);
        let value = report(&output);
        // 262f6cdb + A3 (ec3ef36e): no runtime is a typed L1 scope failure
        // with a creation hint; every later link is BLOCKED, never a
        // fabricated missing session.
        assert_v1_truth(&value, &output);
        assert_eq!(link_status(&value, "L1"), "FAIL", "{value}");
        assert!(link(&value, "L1")["message"].as_str().unwrap().contains("has no runtime state"),
            "P3 doctor must report no runtime for an unstarted workspace: {value}");
        assert!(links(&value)[1..].iter().all(|link| link["status"] == "BLOCKED"), "{value}");
        assert!(value["next_action"]["context"].as_str().is_some_and(|hint| hint.contains("team-agent start AGENT --team")),
            "P3 doctor must hint creation for no runtime: {value}");
        assert!(
            !value.to_string().contains("session_missing") && !value.to_string().contains("missing_session"),
            "P3 doctor fabricated a missing-session issue for no runtime: {value}"
        );
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
    let json_output = run("doctor", &["--json"], &fixture);
    let value = report(&json_output);
    // 262f6cdb: ok is exactly "all six links PASS"; the unbound leader keeps
    // L6 from passing, so this fixture is not ok.
    assert_v1_truth(&value, &json_output);
    assert_ne!(link_status(&value, "L6"), "PASS", "P4 fixture must contain an unbound failure: {value}");
    assert_eq!(value["ok"], false, "{value}");

    let human_output = run("doctor", &[], &fixture);
    assert_eq!(
        human_output.status.code(),
        json_output.status.code(),
        "P4 human/JSON exit mismatch for doctor; human={}",
        text(&human_output.stdout)
    );
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
    // 262f6cdb: doctor v1's environment facts are the invoked binary
    // reference (and a differing PATH entry); adding runtime facts must not
    // drop or change them, and no secret value is ever echoed.
    assert_eq!(before["ok"], false, "P5 base must expose its failure: {before}");
    assert_eq!(after["ok"], false, "{after}");
    assert!(before["binary_reference"].is_string(), "{before}");
    for key in ["binary_reference", "path_entry"] {
        assert_eq!(before.get(key), after.get(key),
            "P5 adding runtime facts changed environment fact {key}: before={before} after={after}");
    }
    for value in [&before, &after] {
        assert!(!value.to_string().contains("synthetic-p5-secret"), "P5 secret echoed: {value}");
    }
}

#[test]
fn p6_dead_runtime_is_not_absent_and_keeps_failure_visible() {
    let absent = Fixture::new("p6-absent");
    let absent_report = report(&run("doctor", &["--json"], &absent));
    // 262f6cdb + A3 (ec3ef36e): absent runtime is a typed L1 failure.
    assert_eq!(link_status(&absent_report, "L1"), "FAIL", "{absent_report}");
    assert!(link(&absent_report, "L1")["message"].as_str().unwrap().contains("has no runtime state"), "{absent_report}");

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
    // A persisted-but-dead runtime resolves its scope (L1 PASS) and keeps a
    // concrete later failure visible; it is never collapsed into absent.
    assert_eq!(
        link_status(&dead_report, "L1"),
        "PASS",
        "P6 persisted-but-dead runtime was collapsed into absent: {dead_report}"
    );
    assert_eq!(dead_report["ok"], false, "{dead_report}");
    assert!(
        links(&dead_report).iter().any(|link| link["status"] == "FAIL"
            && link["message"].as_str().is_some_and(|message| message.contains("unreachable") || message.contains("missing"))),
        "P6 dead runtime report lost concrete failure evidence: {dead_report}"
    );
}

#[test]
fn p7_issue_deduplication_does_not_collapse_distinct_failures() {
    let fixture = Fixture::new("p7-dedup");
    // 262f6cdb: seat faults are evaluated per running seat on a reachable
    // host (L3). Two distinct dead seats must both be reported, once each.
    let host = OwnedHost::new();
    let socket = path_arg(&host.socket);
    save_state(
        &fixture,
        json!({
            "team_key": "alpha",
            "active_team_key": "alpha",
            "session_name": "alpha-session",
            "tmux_socket": socket,
            "agents": {
                "worker-a": {"status": "running", "pane_id": "%998", "tmux_socket": socket},
                "worker-b": {"status": "running", "pane_id": "%999", "tmux_socket": socket}
            },
            "leader_receiver": {"status": "unbound"}
        }),
    );
    let value = report(&run("doctor", &["--json"], &fixture));
    let seats = link(&value, "L3");
    let message = seats["message"].as_str().unwrap_or_default();
    assert_eq!(seats["status"], "FAIL", "P7 distinct failures were collapsed: {value}");
    for worker in ["worker-a", "worker-b"] {
        assert_eq!(message.matches(&format!("{worker}:")).count(), 1,
            "P7 {worker} fault must appear exactly once: {value}");
    }
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
    let output = run("doctor", &["--json"], &fixture);
    let value = report(&output);
    // 262f6cdb: the coordinator is retired; the report holds exactly one
    // observation per link (six, ordered) and no coordinator probe at all.
    assert_v1_truth(&value, &output);
    assert_eq!(
        count_key(&value, "coordinator"),
        0,
        "P8 report must not probe the retired coordinator: {value}"
    );
    assert!(!value.to_string().contains("4000000"), "P8 stale coordinator pid was probed: {value}");
}

#[test]
fn p9_default_doctor_is_read_only() {
    let fixture = Fixture::new("p9-doctor");
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
    let _ = run("doctor", &["--json"], &fixture);
    let after = snapshot(&fixture.workspace);
    assert_eq!(before, after, "P9 doctor mutated the selected workspace");
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
    let output = run("doctor", &["--json"], &fixture);
    let value = report(&output);
    // R-M3/262f6cdb: the retired coordinator is never probed; an unreachable
    // host short-circuits dependent links to BLOCKED instead of repeating the
    // same failure in every link.
    assert_v1_truth(&value, &output);
    assert!(
        links(&value).iter().all(|link| !link["message"].as_str().unwrap_or("").contains("coordinator")),
        "P11 dead coordinator was probed: {value}"
    );
    assert_eq!(link_status(&value, "L2"), "FAIL", "{value}");
    assert_eq!(link_status(&value, "L3"), "BLOCKED", "P11 host failure repeated in L3: {value}");
    assert_eq!(link_status(&value, "L5"), "BLOCKED", "P11 host failure repeated in L5: {value}");
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
    let output = run("doctor", &[], &fixture);
    let human = text(&output.stdout);
    let json_output = run("doctor", &["--json"], &fixture);
    let expected = report(&json_output);
    // 262f6cdb replaced the `doctor:` prefix and the embedded `details:` JSON
    // line with one triage line per link; A1 (ec3ef36e) bounds every human
    // line, including the wrapped polling advisory, to 160 bytes while JSON
    // keeps the full single-string advisory.
    assert_eq!(output.status.code(), json_output.status.code(), "{human}");
    for link in links(&expected) {
        if link["status"] == "BLOCKED" {
            continue;
        }
        let head = format!("{} {} [{}]", link["id"].as_str().unwrap(), link["link"].as_str().unwrap(), link["status"].as_str().unwrap());
        assert!(human.lines().any(|line| line.contains(&head)), "P12 human output lost {head}: {human}");
    }
    assert!(human.contains("[Advisory: do not poll]"), "{human}");
    assert!(expected["polling_advisory"].as_str().is_some_and(|advisory| !advisory.contains('\n')), "{expected}");
    for (line_no, line) in human.lines().enumerate() {
        assert!(
            line.as_bytes().len() <= 160,
            "P12 doctor triage line {line_no} exceeds 160 bytes: {}",
            line.as_bytes().len()
        );
        assert!(!line.chars().any(char::is_control));
        assert!(serde_json::from_str::<Value>(line).map_or(true, |value| !value.is_object()),
            "P12 human output must not embed a JSON details line: {line}");
    }
    assert!(!human.contains("issues: [") && !human.contains("runtime: {"));
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
    let output = run("doctor", &["--json"], &fixture);
    let value = report(&output);
    // 262f6cdb retired the secret-scan inventory: the stable v1 evidence is
    // schema/ok/team/workspace/links (+ next_action/avoid when not ok). No
    // secret value, match or excerpt may appear anywhere in either renderer.
    assert_v1_truth(&value, &output);
    for key in ["schema", "ok", "team", "workspace", "links", "next_action", "avoid"] {
        assert!(
            value.get(key).is_some(),
            "P13 missing stable JSON field {key}: {value}"
        );
    }
    assert!(!value.to_string().contains(secret), "P13 leaked the secret value: {value}");
    for forbidden in ["value", "secret", "match", "match_excerpt"] {
        assert_eq!(count_key(&value, forbidden), 0, "P13 leaked {forbidden}: {value}");
    }
    let human = run("doctor", &[], &fixture);
    assert!(!text(&human.stdout).contains(secret) && !text(&human.stderr).contains(secret));
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
    // 262f6cdb: the selected team is the report's `team`, resolved in L1.
    assert_eq!(alpha["team"], "alpha", "{alpha}");
    assert_eq!(beta["team"], "beta", "{beta}");
    assert_eq!(link_status(&alpha, "L1"), "PASS", "{alpha}");
    assert_eq!(link_status(&beta, "L1"), "PASS", "{beta}");
    assert!(!alpha.to_string().contains("beta-only"));
    assert!(!beta.to_string().contains("alpha-only"));
    let missing = run("doctor", &["--team", "does-not-exist", "--json"], &fixture);
    assert_eq!(missing.status.code(), Some(1));
    let missing = report(&missing);
    assert_eq!(missing["team"], "does-not-exist", "{missing}");
    assert_eq!(link_status(&missing, "L1"), "FAIL", "{missing}");
    assert!(links(&missing)[1..].iter().all(|link| link["status"] == "BLOCKED"), "{missing}");
}

#[test]
fn p15_explicit_gate_and_repair_modes_keep_safe_rejections_and_evidence() {
    for args in [
        &["--fix", "--json"][..],
        &["--gate", "not-a-gate", "--json"][..],
    ] {
        let fixture = Fixture::new("p15-doctor");
        let doctor = run("doctor", args, &fixture);
        assert_eq!(doctor.status.code(), Some(1), "{args:?}");
        assert_eq!(report(&doctor)["ok"], false, "{args:?}");
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
    // R-M3/R-M4: a stale legacy pid and a corrupt retired store are no longer doctor failures
    // (the store is never opened and must stay byte-unchanged); the receiver failure must still
    // surface on its own.
    assert_eq!(
        fs::read(fixture.workspace.join(".team/team.db")).expect("legacy store kept"),
        b"not sqlite"
    );
    // 262f6cdb: the return link (L6) reports the unbound leader on its own.
    let ret = link(&value, "L6");
    assert!(
        ret["status"] == "FAIL"
            && ret["message"].as_str().is_some_and(|message| message.contains("leader") || message.contains("unbound")),
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
    let output = run("doctor", &["--json"], &fixture);
    let value = report(&output);
    // 262f6cdb: a normally stopped team has no running seat, so no seat can
    // be reported missing, and no link fabricates a missing session.
    assert_v1_truth(&value, &output);
    assert_ne!(link_status(&value, "L3"), "FAIL", "P17 stopped team reported as a seat failure: {value}");
    assert!(!value.to_string().contains("session_missing"), "{value}");
    assert!(!value.to_string().contains("missing_session"), "{value}");
}

#[test]
fn p18_unrelated_cli_help_and_status_entry_points_remain_available() {
    let fixture = Fixture::new("p18-other-commands");
    for (command, exit) in [("status", 0), ("wait", 2), ("coordinator", 2)] {
        let output = run(command, &["--help"], &fixture);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "P18 unrelated command {command} help regressed: {}",
            text(&output.stderr)
        );
    }
}
