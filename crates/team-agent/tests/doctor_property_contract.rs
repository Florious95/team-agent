// real-machine isolation: HOME, TMUX, TEAM_AGENT_WORKSPACE
//! Black-box properties for doctor privacy, process isolation, and output contracts.
//!
//! Deterministic bounded inputs keep failures reproducible. These cases cover provider
//! probes, child processes, generated human output, JSON evidence, and team isolation.

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
fn p1_doctor_never_executes_provider_version_or_auth_poison() {
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
        "doctor must still emit JSON; stdout={} stderr={}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let touched = fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        touched.is_empty(),
        "P1 RED: provider version/auth poison was executed by default doctor: {touched:?}"
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
        let mut child = command.spawn().expect("spawn doctor observer target");
        let mut observed_children = Vec::new();
        loop {
            observed_children.extend(proc_descendants(child.id()));
            if child.try_wait().expect("poll doctor process").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let output = child
            .wait_with_output()
            .expect("collect doctor observer target");
        observed_children.sort_unstable();
        observed_children.dedup();
        assert!(
            observed_children.is_empty(),
            "P2 RED: empty doctor created child process(es) {:?}; stdout={} stderr={}",
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
            "P2 RED: doctor changed an empty workspace; before={before:?} after={after:?}"
        );
        assert!(
            after.is_empty(),
            "P2 RED: empty workspace gained files: {after:?}"
        );
    }
}

#[test]
fn p3_human_doctor_is_bounded_and_control_clean_for_generated_inputs() {
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
        let mut args = vec!["doctor", "--workspace", workspace_arg.as_str()];
        // Empty means no explicit team in this generated-input case, not an
        // explicitly blank scope (which Phase 0 correctly rejects).
        if !team.is_empty() { args.extend(["--team", team.as_str()]); }
        let output = run_with_env(&args, &workspace, &home, None);
        let human = String::from_utf8(output.stdout).expect("human output must be UTF-8");
        args.push("--json");
        let json = run_with_env(&args, &workspace, &home, None);
        let expected: Value = serde_json::from_slice(&json.stdout).expect("doctor JSON evidence");
        // 262f6cdb replaced `details:`/`runtime=` triage with per-link lines;
        // A1/A3 (ec3ef36e) bound every human line to 160 bytes and type the
        // no-runtime L1 failure. JSON keeps the full evidence.
        for (line_no, line) in human.lines().enumerate() {
            assert!(
                line.as_bytes().len() <= 160,
                "P3 RED: case={index} triage line={line_no} is {} bytes: {line:?}",
                line.as_bytes().len()
            );
            assert!(
                !line.chars().any(char::is_control),
                "P3 RED: case={index} leaked control character: {line:?}"
            );
        }
        assert_eq!(json.status.code(), output.status.code(), "P3 text/JSON exit parity");
        assert_eq!(output.status.code(), Some(1), "{human}");
        assert_eq!(expected["schema"], "team-agent.doctor.v1", "{expected}");
        let scope = &expected["links"][0];
        assert_eq!((scope["id"].as_str(), scope["status"].as_str()), (Some("L1"), Some("FAIL")), "{expected}");
        assert!(human.contains("L1 scope [FAIL]"), "{human}");
        if team.is_empty() {
            assert!(scope["message"].as_str().is_some_and(|message| message.contains("has no runtime state")), "{expected}");
            assert!(!human.contains("team_spec_or_runtime_missing"), "{human}");
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
    // 262f6cdb: ok is "all six links PASS"; the unbound leader keeps the
    // return link (L6) from passing, which forces final ok:false.
    let links = report.get("links").and_then(Value::as_array).expect("doctor v1 links");
    assert!(
        links.iter().any(|link| link["id"] == "L6" && link["status"] != "PASS"),
        "P4 fixture must enter the unbound leader path: {report}"
    );
    assert_eq!(
        report.get("ok").and_then(Value::as_bool),
        Some(links.iter().all(|link| link["status"] == "PASS")),
        "P4 RED: ok must equal all links passing: {report}"
    );
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(false), "{report}");
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
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    // 262f6cdb: the v1 shape is schema/ok/team/workspace + six links, with
    // next_action/avoid when not ok. Provider version/auth probing is not part
    // of doctor at all (no probe ever runs; see P1), so no provider inventory
    // or probe status is reported.
    for key in ["schema", "ok", "team", "workspace", "links", "next_action", "avoid"] {
        assert!(
            report.get(key).is_some(),
            "P5 missing top-level key {key}: {report}"
        );
    }
    assert_eq!(report["schema"], "team-agent.doctor.v1", "{report}");
    let links = report["links"].as_array().expect("links array");
    assert_eq!(links.len(), 6, "{report}");
    for link in links {
        for field in ["id", "link", "status", "proof", "message"] {
            assert!(link.get(field).is_some(), "P5 link lost {field}: {link}");
        }
    }
    for retired in ["providers", "probe_status", "version", "auth"] {
        assert_eq!(count_key(&report, retired), 0, "P5 provider probe evidence leaked ({retired}): {report}");
    }
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
    // 262f6cdb retired the secret-scan inventory: doctor neither reports
    // findings nor ever echoes a secret value or excerpt in either renderer.
    assert_eq!(report["schema"], "team-agent.doctor.v1", "{report}");
    for retired in ["secret_scan", "findings", "match_excerpt"] {
        assert_eq!(count_key(&report, retired), 0, "P6 secret inventory leaked ({retired}): {report}");
    }
    assert_eq!(output.status.code(), Some(1));
    let human = run_with_env(&["doctor", "--workspace", workspace_arg.as_str()], &workspace, &home, None);
    for out in [&output, &human] {
        for secret in ["synthetic-positive", "synthetic-export-positive", "synthetic-example", "synthetic-comment"] {
            assert!(!String::from_utf8_lossy(&out.stdout).contains(secret), "P6 leaked {secret}");
            assert!(!String::from_utf8_lossy(&out.stderr).contains(secret), "P6 leaked {secret}");
        }
    }
}

#[test]
fn p7_doctor_is_read_only_and_selected_team_does_not_leak_other_team() {
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
        // 262f6cdb: the selected team is the v1 report's `team`, resolved by L1.
        assert_eq!(
            report.get("team").and_then(Value::as_str),
            Some(team),
            "P7 selected team must be reflected in the report: {report}"
        );
        assert_eq!(report["links"][0]["status"], "PASS", "{report}");
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
        "P7 RED: doctor wrote to the multi-team fixture: before={before:?} after={after:?}"
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
        "ta-doctor-properties-{tag}-{}-{}",
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

fn count_key(value: &Value, key: &str) -> usize {
    match value {
        Value::Object(object) => object
            .iter()
            .map(|(name, child)| usize::from(name == key) + count_key(child, key))
            .sum(),
        Value::Array(items) => items.iter().map(|child| count_key(child, key)).sum(),
        _ => 0,
    }
}
