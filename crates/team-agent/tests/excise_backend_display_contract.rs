//! Independent RED contract for Issue #231: remove display-backend control
//! while keeping legacy input harmless and the tmux transport intact.
//!
//! These tests deliberately exercise the public CLI and inspect only owned
//! workspace/tmux resources.  They do not import the display implementation.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![allow(non_snake_case)]
#![cfg(unix)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use hermetic_guard::HermeticTestEnv;
use serde_json::{json, Value};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TEAM: &str = "excise-display";
const SOURCE: &str = "worker";

#[test]
#[serial]
fn R1_no_display_flag_rejected() {
    let env = HermeticTestEnv::enter("excise-r1");
    let workspace = env.workspace("workspace");
    let workspace_arg = path_string(&workspace);
    let before = relative_entries(&workspace);

    // `--no-display` was the old escape hatch.  The new CLI must reject it
    // before lifecycle dispatch, so no runtime state, process, or log is made.
    let output = env.run_cli(
        &workspace,
        &[
            "doctor",
            "--workspace",
            &workspace_arg,
            "--no-display",
            "--json",
        ],
    );
    let text = output_text(&output).to_ascii_lowercase();
    assert_eq!(
        output.status.code(),
        Some(1),
        "--no-display must be a usage rejection: {text}"
    );
    assert!(
        text.contains("no-display")
            && ["unknown", "unsupported", "unexpected", "usage", "remove"]
                .iter()
                .any(|needle| text.contains(needle)),
        "rejection must identify the removed --no-display option: {text}"
    );
    assert_eq!(
        relative_entries(&workspace),
        before,
        "a rejected option must not write runtime state, logs, or spawn resources"
    );
}

#[test]
#[serial]
fn R2_team_md_display_backend_is_ignored_and_tmux_stays_silent() {
    // The old front-matter key is compatibility input, not a new error.  Use
    // several old values, including non-scalars, because migration readers
    // must ignore the key before backend selection.
    for (index, value) in [
        "ghostty",
        "terminal",
        "null",
        "42",
        "{kind: ghostty}",
    ]
    .into_iter()
    .enumerate()
    {
        let env = HermeticTestEnv::enter(&format!("excise-r2-{index}"));
        let workspace = env.workspace("workspace");
        write_fake_team(&workspace, Some(value));
        let workspace_arg = path_string(&workspace);
        let output = env.run_cli(
            &workspace,
            &[
                "quick-start",
                &workspace_arg,
                "--workspace",
                &workspace_arg,
                "--team-id",
                TEAM,
                "--yes",
                "--backend",
                "tmux",
                "--json",
            ],
        );
        let body = json_stdout(&output, "R2 quick-start");
        assert_eq!(
            body["worker_readiness"]["all_workers_spawned"],
            json!(true),
            "legacy display_backend={value} must still reach the tmux worker launch: body={body} stderr={}",
            stderr(&output)
        );

        let state_path = workspace.join(".team/runtime/state.json");
        assert!(state_path.is_file(), "R2 must persist runtime state");
        let state_text = std::fs::read_to_string(&state_path).expect("read R2 state");
        assert!(
            state_text.contains("tmux_socket") || state_text.contains("tmux_endpoint"),
            "R2 state must expose the owned tmux transport: {state_text}"
        );
        if value == "ghostty" || value == "terminal" {
            assert!(
                !state_text.contains(value) && !stdout(&output).contains(value),
                "legacy value {value} must be ignored, not projected as an active renderer"
            );
        }
        if let Some(socket) = body.get("tmux_socket").and_then(Value::as_str) {
            let _ = Command::new("tmux")
                .args(["-S", socket, "kill-server"])
                .output();
        }
    }
}

#[test]
#[serial]
fn R3_no_display_subsystem_import() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lifecycle = crate_root.join("src/lifecycle");
    assert!(
        !lifecycle.join("display.rs").exists(),
        "Issue #231 must physically remove src/lifecycle/display.rs"
    );

    // Keep this list to the old display subsystem's API/ownership vocabulary,
    // rather than rejecting unrelated English prose containing "display".
    let forbidden = [
        "display_backend",
        "DisplayBackend",
        "resolve_display_backend",
        "probe_display_capabilities",
        "open_worker_displays",
        "close_team_display_backends",
        "rebuild_adaptive_display_after_rebind",
        "WorkerDisplay",
        "DisplayProbe",
        "AdaptiveBlockReason",
        "display_identifiers",
        "close_adaptive_windows",
        "open_adaptive_worker_displays",
    ];
    let mut findings = Vec::new();
    for path in rust_files(&crate_root.join("src")) {
        let text = std::fs::read_to_string(&path).expect("read Rust source");
        for needle in forbidden {
            if text.contains(needle) {
                findings.push(format!("{} contains {needle}", path.display()));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "display subsystem symbols must not remain in product source:\n{}",
        findings.join("\n")
    );
}

#[test]
#[serial]
fn R4_cleanup_matrix_mock() {
    let env = HermeticTestEnv::enter("excise-r4");
    let workspace = env.workspace("workspace");
    write_fake_team(&workspace, None);
    let workspace_arg = path_string(&workspace);
    let launch = env.run_cli(
        &workspace,
        &[
            "quick-start",
            &workspace_arg,
            "--workspace",
            &workspace_arg,
            "--team-id",
            TEAM,
            "--yes",
            "--backend",
            "tmux",
            "--json",
        ],
    );
    let launch_body = json_stdout(&launch, "R4 fixture quick-start");
    assert!(
        launch_body["worker_readiness"]["all_workers_spawned"] == json!(true),
        "R4 fixture must create a real owned tmux session: {launch_body}"
    );
    let state_path = workspace.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).expect("read state"))
        .expect("parse quick-start state");
    let socket = PathBuf::from(
        state["tmux_socket"]
            .as_str()
            .expect("state tmux_socket"),
    );
    let session = state["session_name"].as_str().expect("state session_name").to_string();
    let agent = state["agents"]
        .get_mut(SOURCE)
        .expect("worker row")
        .as_object_mut()
        .expect("worker object");
    agent.insert(
        "display".to_string(),
        json!({
            "backend": "adaptive",
            "status": "opened",
            "window": "aux",
            "workspace_window": "aux",
            "target": format!("{session}:aux")
        }),
    );
    if let Some(team_agents) = state["teams"][TEAM]["agents"].as_object_mut() {
        if let Some(team_agent) = team_agents.get_mut(SOURCE).and_then(Value::as_object_mut) {
            team_agent.insert(
                "display".to_string(),
                json!({"backend":"adaptive","status":"opened","window":"aux","workspace_window":"aux","target":format!("{session}:aux")}),
            );
        }
    }
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state).expect("serialize old state"))
        .expect("write old state");
    let created = Command::new("tmux")
        .args(["-S", path_string(&socket).as_str(), "new-window", "-d", "-t", &session, "-n", "aux", "-c"])
        .arg(&workspace)
        .arg("sleep 300")
        .status()
        .expect("create auxiliary tmux window");
    assert!(created.success(), "create auxiliary window: {created}");
    // Foreign session is a scoped-cleanup control: only the state-owned
    // auxiliary window may be removed.
    let foreign_session = "foreign-excise-r4";
    let created = Command::new("tmux")
        .args(["-S", path_string(&socket).as_str(), "-f", "/dev/null", "new-session", "-d", "-s", foreign_session, "-n", "aux", "-c"])
        .arg(&workspace)
        .arg("sleep 300")
        .status()
        .expect("create foreign tmux session");
    assert!(created.success(), "create foreign session: {created}");

    let workspace_arg = path_string(&workspace);
    let output = env.run_cli(
        &workspace,
        &[
            "shutdown",
            "--workspace",
            &workspace_arg,
            "--team",
            TEAM,
            "--keep-logs",
            "--json",
        ],
    );
    let body = json_stdout(&output, "R4 scoped shutdown");
    assert_eq!(
        output.status.code(),
        Some(0),
        "scoped shutdown must tolerate legacy owned auxiliary window: body={body} stderr={}",
        stderr(&output)
    );
    assert!(!tmux_has_session(&socket, &session), "owned team session must be gone");
    assert!(
        tmux_has_session(&socket, foreign_session),
        "scoped cleanup must not kill a foreign tmux session"
    );
    let _ = Command::new("tmux")
        .args(["-S", path_string(&socket).as_str(), "kill-server"])
        .output();
}

#[test]
#[serial]
fn P_backend_transport_parser_stays_usable() {
    let tmux = team_agent::cli::emit::__test_quick_start_args(
        &vec!["--backend".to_string(), "tmux".to_string(), "--yes".to_string()],
        &std::env::temp_dir(),
    )
    .expect("--backend tmux must parse");
    assert_eq!(tmux.backend.as_deref(), Some("tmux"));

    let conpty = team_agent::cli::emit::__test_quick_start_args(
        &vec!["--backend".to_string(), "conpty".to_string(), "--yes".to_string()],
        &std::env::temp_dir(),
    )
    .expect("--backend conpty must parse");
    assert_eq!(conpty.backend.as_deref(), Some("conpty"));
}

#[test]
#[serial]
fn P_doctor_empty_workspace_stays_read_only() {
    let env = HermeticTestEnv::enter("excise-p-doctor");
    let workspace = env.workspace("empty");
    let workspace_arg = path_string(&workspace);
    let output = env.run_cli(
        &workspace,
        &["doctor", "--workspace", &workspace_arg, "--json"],
    );
    let body = json_stdout(&output, "P doctor");
    assert_eq!(output.status.code(), Some(0), "doctor must pass: {body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["runtime"]["status"], json!("not_present"));
    assert!(relative_entries(&workspace).is_empty());
}

fn write_fake_team(workspace: &Path, display_backend: Option<&str>) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents");
    let legacy = display_backend
        .map(|value| format!("display_backend: {value}\n"))
        .unwrap_or_default();
    std::fs::write(
        workspace.join("TEAM.md"),
        format!(
            "---\nname: {TEAM}\nobjective: backend display compatibility\nprovider: fake\n{legacy}---\n\nTeam.\n"
        ),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: fake worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFake worker.\n"
        ),
    )
    .expect("write agent role");
}

fn tmux_has_session(socket: &Path, session: &str) -> bool {
    Command::new("tmux")
        .args(["-S", path_string(socket).as_str(), "has-session", "-t", session])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(path) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files
}

fn relative_entries(root: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&path) else { continue };
        for entry in read_dir.flatten() {
            let child = entry.path();
            entries.push(
                child
                    .strip_prefix(root)
                    .expect("relative path")
                    .to_string_lossy()
                    .into_owned(),
            );
            if child.is_dir() {
                stack.push(child);
            }
        }
    }
    entries.sort();
    entries
}

fn json_stdout(output: &Output, label: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} must emit JSON; parse_error={error} stdout={} stderr={}",
            stdout(output),
            stderr(output)
        )
    })
}

fn output_text(output: &Output) -> String {
    format!("{}\n{}", stdout(output), stderr(output))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
