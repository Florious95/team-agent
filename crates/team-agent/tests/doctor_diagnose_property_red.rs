// real-machine isolation: HOME, TMUX, TEAM_AGENT_WORKSPACE
//! Property-oriented black-box RED contracts for the doctor/diagnose refactor taskbook.
//!
//! These tests intentionally exercise only the public CLI.  Inputs are deterministic,
//! bounded generators so a failing case is reproducible from the test name and fixture.
//! The baseline is expected to fail: provider probes are still visible, human output is
//! a compact JSON dump, and doctor does not close its final `ok` value after appending an
//! unbound registry issue.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
fn p1_diagnose_never_executes_provider_version_or_auth_poison() {
    let workspace = tmp_dir("p1-empty");
    let home = tmp_dir("p1-home");
    let poison_dir = tmp_dir("p1-poison");
    let marker = poison_dir.join("provider-executed.log");
    for command in ["claude", "codex", "gemini", "grok", "agent"] {
        write_poison_command(&poison_dir.join(command), &marker);
    }

    let workspace_arg = path(&workspace);
    let output = run_with_env(
        &["doctor", "--workspace", workspace_arg.as_str(), "--json"],
        &workspace,
        &home,
        Some(&poison_dir),
    );
    assert!(
        output.stdout.starts_with(b"{") || output.stdout.starts_with(b"["),
        "diagnose must still emit JSON; stdout={} stderr={}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let touched = fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        touched.is_empty(),
        "P1 RED: provider version/auth poison was executed by default diagnose: {touched:?}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn p2_empty_workspace_has_zero_child_processes_and_zero_disk_writes() {
    for explicit_team in [None, Some("current")] {
        let workspace = tmp_dir("p2-empty");
        let home = tmp_dir("p2-home");
        let poison_dir = tmp_dir("p2-poison");
        let marker = poison_dir.join("unexpected-command.log");
        for command in [
            "claude", "codex", "gemini", "grok", "agent", "tmux", "sysctl",
        ] {
            write_delayed_poison_command(&poison_dir.join(command), &marker);
        }
        let before = snapshot(&workspace);
        let workspace_arg = path(&workspace);
        let mut command = Command::new(bin());
        command
            .args(["doctor", "--workspace", workspace_arg.as_str(), "--json"])
            .current_dir(&workspace)
            .env("HOME", &home)
            .env("TMPDIR", std::env::temp_dir())
            .env("PATH", &poison_dir)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env_remove("TEAM_AGENT_ID")
            .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
            .env_remove("TEAM_AGENT_TEAM_ID")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(team) = explicit_team {
            command.args(["--team", team]);
        }
        let mut child = command.spawn().expect("spawn diagnose observer target");
        let mut observed_children = Vec::new();
        loop {
            observed_children.extend(proc_descendants(child.id()));
            if child.try_wait().expect("poll diagnose process").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let output = child
            .wait_with_output()
            .expect("collect diagnose observer target");
        observed_children.sort_unstable();
        observed_children.dedup();
        assert!(
            observed_children.is_empty(),
            "P2 RED: empty diagnose created child process(es) {:?}; stdout={} stderr={}",
            observed_children,
            text(&output.stdout),
            text(&output.stderr)
        );
        assert!(
            fs::read_to_string(&marker).unwrap_or_default().is_empty(),
            "P2 RED: a known external command was executed"
        );
        let after = snapshot(&workspace);
        assert_eq!(
            before, after,
            "P2 RED: diagnose changed an empty workspace; before={before:?} after={after:?}"
        );
        assert!(
            after.is_empty(),
            "P2 RED: empty workspace gained files: {after:?}"
        );
    }
}

#[test]
fn p3_human_diagnose_is_bounded_and_control_clean_for_generated_inputs() {
    let generated = [
        String::new(),
        "a".repeat(17),
        "中".repeat(47),
        "🙂".repeat(35),
        "x".repeat(200),
        "a\t\u{1b}[31m".repeat(20),
        "mixed-路径-".to_string() + &"z".repeat(171),
    ];
    for (index, team) in generated.iter().enumerate() {
        let workspace = tmp_dir(&format!("p3-long-{index}"));
        let home = tmp_dir(&format!("p3-home-{index}"));
        let workspace_arg = path(&workspace);
        let output = run_with_env(
            &[
                "doctor",
                "--workspace",
                workspace_arg.as_str(),
                "--team",
                team.as_str(),
            ],
            &workspace,
            &home,
            None,
        );
        let human = String::from_utf8(output.stdout).expect("human output must be UTF-8");
        for (line_no, line) in human.lines().enumerate() {
            assert!(
                line.as_bytes().len() <= 160,
                "P3 RED: case={index} line={line_no} is {} bytes: {line:?}",
                line.as_bytes().len()
            );
            assert!(
                !line.chars().any(char::is_control),
                "P3 RED: case={index} leaked control character: {line:?}"
            );
        }
        if team.is_empty() {
            assert!(output.status.success(), "{human}");
            assert!(human.contains("runtime=not_present"), "{human}");
            assert!(!human.contains("team_spec_or_runtime_missing"), "{human}");
        } else {
            assert_eq!(output.status.code(), Some(1), "{human}");
            assert!(human.contains("runtime_selection_failed"), "{human}");
            assert!(human.contains("repair: action=unable to select runtime team:"), "{human}");
        }
        assert!(
            !human.contains("providers: {")
                && !human.contains("runtime: {")
                && !human.contains("issues: ["),
            "P3 RED: human output contains a nested JSON dump: {human}"
        );
    }
}

#[test]
fn p4_doctor_issues_are_monotone_and_text_json_exit_codes_match() {
    let workspace = tmp_dir("p4-unbound");
    let home = tmp_dir("p4-home");
    let state = serde_json::json!({
        "team_key": "alpha",
        "active_team_key": "alpha",
        "session_name": "alpha-session",
        "agents": {},
        "tasks": [],
        "leader_receiver": {"status": "unbound"}
    });
    team_agent::state::persist::save_runtime_state(&workspace, &state).unwrap();
    let mut coordinator = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("start fixture coordinator process");
    let runtime = workspace.join(".team").join("runtime");
    fs::write(
        runtime.join("coordinator.pid"),
        coordinator.id().to_string(),
    )
    .unwrap();
    fs::write(
        runtime.join("coordinator.json"),
        serde_json::json!({
            "pid": coordinator.id(),
            "protocol_version": 2,
            "message_store_schema_version": 5,
            "binary_path": path(Path::new(bin())),
            "binary_version": env!("CARGO_PKG_VERSION"),
            "source": "start",
            "updated_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let workspace_arg = path(&workspace);
    let json_output = run_with_env(
        &["doctor", "--workspace", workspace_arg.as_str(), "--json"],
        &workspace,
        &home,
        None,
    );
    let report: Value = serde_json::from_slice(&json_output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor JSON parse failed: {error}; stdout={} stderr={}",
            text(&json_output.stdout),
            text(&json_output.stderr)
        )
    });
    let issues = report
        .get("issues")
        .and_then(Value::as_array)
        .expect("doctor report issues array");
    assert!(
        !issues.is_empty(),
        "P4 fixture must enter the unbound registry issue path: {report}"
    );
    assert_eq!(
        report.get("ok").and_then(Value::as_bool),
        Some(false),
        "P4 RED: non-empty doctor issues must force final ok:false: {report}"
    );
    assert_eq!(
        json_output.status.code(),
        Some(1),
        "P4 RED: doctor JSON with issues must exit 1; report={report}"
    );

    let text_output = run_with_env(
        &["doctor", "--workspace", workspace_arg.as_str()],
        &workspace,
        &home,
        None,
    );
    assert_eq!(
        text_output.status.code(),
        Some(1),
        "P4 RED: human and JSON doctor exits must agree; human={} stderr={}",
        text(&text_output.stdout),
        text(&text_output.stderr)
    );
    let _ = coordinator.kill();
    let _ = coordinator.wait();
}

#[test]
fn p5_json_shape_keeps_provider_keys_unknown_and_probe_not_run() {
    let workspace = tmp_dir("p5-empty");
    let home = tmp_dir("p5-home");
    let workspace_arg = path(&workspace);
    let output = run_with_env(
        &["doctor", "--workspace", workspace_arg.as_str(), "--json"],
        &workspace,
        &home,
        None,
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("diagnose JSON");
    for key in [
        "event_log",
        "issues",
        "ok",
        "providers",
        "runtime",
        "suggested_repairs",
    ] {
        assert!(
            report.get(key).is_some(),
            "P5 missing top-level key {key}: {report}"
        );
    }
    let providers = report
        .get("providers")
        .and_then(Value::as_object)
        .expect("providers object");
    for key in [
        "claude",
        "claude_code",
        "codex",
        "gemini_cli",
        "grok",
        "cursor_agent",
    ] {
        let provider = providers.get(key).expect("provider key retained");
        assert_eq!(
            provider.get("version").and_then(Value::as_str),
            Some("unknown")
        );
        assert_eq!(
            provider.get("auth").and_then(Value::as_str),
            Some("unknown")
        );
        assert_eq!(
            provider.get("probe_status").and_then(Value::as_str),
            Some("not_run"),
            "P5 provider probe status invariant failed for {key}: {provider}"
        );
    }
    let fake = providers.get("fake").expect("fake provider key retained");
    assert_eq!(fake.get("version").and_then(Value::as_str), Some("fake"));
    assert_eq!(
        fake.get("probe_status").and_then(Value::as_str),
        Some("not_run")
    );
}

#[test]
fn p6_secret_scan_ignores_examples_but_catches_real_line_start_assignments() {
    let workspace = tmp_dir("p6-secret-fixture");
    let home = tmp_dir("p6-home");
    fs::write(
        workspace.join("redaction.rs"),
        concat!(
            "let example = \"OPENAI_API_KEY=synthetic-example\";\n",
            "// OPENAI_API_KEY=synthetic-comment\n",
            "# ANTHROPIC_API_KEY=synthetic-comment\n",
            "OPENAI_API_KEY=synthetic-positive\n",
            "export ANTHROPIC_API_KEY=synthetic-export-positive\n",
        ),
    )
    .unwrap();
    let workspace_arg = path(&workspace);
    let output = run_with_env(
        &["doctor", "--workspace", workspace_arg.as_str(), "--json"],
        &workspace,
        &home,
        None,
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    let findings = report
        .pointer("/secret_scan/findings")
        .and_then(Value::as_array)
        .expect("secret findings array");
    assert_eq!(
        findings.len(),
        2,
        "P6 RED: comments/string examples must not be findings, while two real assignments remain: {findings:?}"
    );
    for finding in findings {
        assert_eq!(
            finding.get("path").and_then(Value::as_str),
            Some("redaction.rs")
        );
        assert_eq!(
            finding.get("rule").and_then(Value::as_str),
            Some("api_key_assignment")
        );
        assert!(finding.get("match_excerpt").is_none());
    }
    assert_eq!(findings[0]["line"], 4);
    assert_eq!(findings[1]["line"], 5);
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-positive"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-export-positive"));
}

#[test]
fn p7_diagnose_is_read_only_and_selected_team_does_not_leak_other_team() {
    let workspace = tmp_dir("p7-two-teams");
    let home = tmp_dir("p7-home");
    let state = serde_json::json!({
        "active_team_key": "alpha",
        "team_key": "alpha",
        "session_name": "alpha-session",
        "agents": {},
        "tasks": [],
        "teams": {
            "alpha": {
                "status": "alive",
                "team_key": "alpha",
                "session_name": "alpha-session",
                "agents": {},
                "tasks": []
            },
            "beta": {
                "status": "alive",
                "team_key": "beta",
                "session_name": "beta-session",
                "agents": {"beta-only-agent": {"status": "missing"}},
                "tasks": []
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&workspace, &state).unwrap();
    let before = snapshot(&workspace);

    for team in ["alpha", "beta"] {
        let workspace_arg = path(&workspace);
        let output = run_with_env(
            &[
                "doctor",
                "--workspace",
                workspace_arg.as_str(),
                "--team",
                team,
                "--json",
            ],
            &workspace,
            &home,
            None,
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "P7 {team} JSON parse failed: {error}; stdout={} stderr={}",
                text(&output.stdout),
                text(&output.stderr)
            )
        });
        assert_eq!(
            report.pointer("/runtime/team_key").and_then(Value::as_str),
            Some(team),
            "P7 selected team must be reflected in runtime summary: {report}"
        );
        if team == "alpha" {
            assert!(
                !text(&output.stdout).contains("beta-only-agent"),
                "P7 RED: unselected beta incident leaked into alpha report: {}",
                text(&output.stdout)
            );
        }
    }
    let after = snapshot(&workspace);
    assert_eq!(
        before, after,
        "P7 RED: diagnose wrote to the multi-team fixture: before={before:?} after={after:?}"
    );
}

fn run_with_env(args: &[&str], cwd: &Path, home: &Path, path_prefix: Option<&Path>) -> Output {
    let path_value = match path_prefix {
        Some(prefix) => {
            let mut value = OsString::from(prefix.as_os_str());
            if let Some(original) = std::env::var_os("PATH") {
                value.push(":");
                value.push(original);
            }
            value
        }
        None => std::env::var_os("PATH").unwrap_or_default(),
    };
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("TMPDIR", std::env::temp_dir())
        .env("PATH", path_value)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run team-agent command")
}

fn write_poison_command(path: &Path, marker: &Path) {
    write_poison_script(path, marker, false);
}

fn write_delayed_poison_command(path: &Path, marker: &Path) {
    write_poison_script(path, marker, true);
}

fn write_poison_script(path: &Path, marker: &Path, delayed: bool) {
    let delay = if delayed { "/bin/sleep 1" } else { ":" };
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$0\" \"$*\" >> '{}'\n{}\nexit 0\n",
            marker.display(),
            delay
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

#[cfg(target_os = "linux")]
fn proc_descendants(root: u32) -> Vec<u32> {
    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    let mut descendants = Vec::new();
    while let Some(pid) = pending.pop() {
        let children_path = format!("/proc/{pid}/task/{pid}/children");
        let Ok(children) = fs::read_to_string(children_path) else {
            continue;
        };
        for child in children
            .split_whitespace()
            .filter_map(|raw| raw.parse::<u32>().ok())
        {
            if seen.insert(child) {
                descendants.push(child);
                pending.push(child);
            }
        }
    }
    descendants
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            if kind.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                visit(root, &path, out);
            } else if kind.is_file() {
                out.insert(rel, fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn tmp_dir(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-doctor-diagnose-properties-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
