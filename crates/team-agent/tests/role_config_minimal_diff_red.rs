#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "e2e/framework.rs"]
mod framework;

use framework::{quick_start_workers_available, run_ta, TestWorkspace};
use rusqlite::Connection;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use team_agent::model::yaml::Value as YamlValue;

fn quick_start(ws: &TestWorkspace, team_id: &str) {
    let root = ws.path().to_str().expect("workspace path is UTF-8");
    let result = run_ta(
        ws,
        &[
            "quick-start",
            root,
            "--workspace",
            root,
            "--team-id",
            team_id,
            "--yes",
            "--no-display",
            "--json",
        ],
    );
    assert!(
        quick_start_workers_available(&result),
        "quick-start failed: exit={} stdout={} stderr={}",
        result.exit_code,
        result.stdout,
        result.stderr
    );
    let state = ws.read_state();
    if let Some(socket) = state.get("tmux_socket").and_then(JsonValue::as_str) {
        ws.register_owned_tmux_socket(Path::new(socket));
    }
}

fn add_agent(
    ws: &TestWorkspace,
    agent_id: &str,
    role_file: &Path,
    extra_args: &[&str],
) -> framework::TaResult {
    let mut args = vec![
        "add-agent",
        agent_id,
        "--role-file",
        role_file.to_str().expect("role path is UTF-8"),
    ];
    args.extend_from_slice(extra_args);
    args.extend_from_slice(&[
        "--workspace",
        ws.path().to_str().expect("workspace path is UTF-8"),
        "--no-display",
        "--json",
    ]);
    run_ta(ws, &args)
}

fn assert_silent_add(result: &framework::TaResult, agent_id: &str) {
    assert!(
        result.is_success(),
        "add-agent {agent_id} must succeed: exit={} stdout={} stderr={}",
        result.exit_code,
        result.stdout,
        result.stderr
    );
    let json = result.json();
    assert_eq!(json["ok"], true, "add-agent must report success: {json}");
    assert_eq!(json["agent_id"], agent_id, "agent registration: {json}");
    for field in ["errors", "warnings", "deprecations", "rejections"] {
        if let Some(value) = json.get(field) {
            assert!(
                value.as_array().is_some_and(Vec::is_empty),
                "add-agent must report no {field}: {value}"
            );
        }
    }
    let output = format!("{}\n{}", result.stdout, result.stderr).to_lowercase();
    for forbidden in ["deprecated", "deprecation", "rejected", "warning:", "error:"] {
        assert!(
            !output.contains(forbidden),
            "legacy-compatible add must not emit {forbidden:?}: {}",
            result.stdout
        );
    }
}

fn runtime_spec(ws: &TestWorkspace) -> YamlValue {
    let state = ws.read_state();
    let team_key = state
        .get("active_team_key")
        .and_then(JsonValue::as_str)
        .expect("runtime state records active team key");
    let path = team_agent::model::paths::runtime_spec_path(ws.path(), team_key);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read runtime spec {}: {error}", path.display()));
    team_agent::model::yaml::loads(&raw).expect("runtime spec parses")
}

fn spec_agent<'a>(spec: &'a YamlValue, agent_id: &str) -> &'a YamlValue {
    spec.get("agents")
        .and_then(YamlValue::as_list)
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent.get("id").and_then(YamlValue::as_str) == Some(agent_id))
        })
        .unwrap_or_else(|| panic!("compiled spec lacks {agent_id}: {spec:?}"))
}

fn string_field<'a>(value: &'a YamlValue, field: &str) -> &'a str {
    value
        .get(field)
        .and_then(YamlValue::as_str)
        .unwrap_or_else(|| panic!("compiled field {field} must be string: {value:?}"))
}

fn assert_running(ws: &TestWorkspace, agent_id: &str) {
    let state = ws.read_state();
    let agent = state
        .pointer(&format!("/agents/{agent_id}"))
        .unwrap_or_else(|| panic!("runtime does not register {agent_id}: {state}"));
    assert_eq!(agent["status"], "running", "agent must be started: {agent}");
}

fn assert_safe_launch(ws: &TestWorkspace, agent_id: &str) {
    let state = ws.read_state();
    let agent = state
        .pointer(&format!("/agents/{agent_id}"))
        .unwrap_or_else(|| panic!("runtime does not register {agent_id}: {state}"));
    assert_eq!(
        agent["as_launched_dangerously_skip_permissions"], false,
        "legacy or plain-text fields must not bypass permissions: {agent}"
    );
}

fn correlated_reply_exists(db: &Path, message_id: &str) -> bool {
    let Ok(conn) = Connection::open(db) else {
        return false;
    };
    let Ok(mut stmt) = conn.prepare("select task_id, envelope from results order by created_at desc") else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) else {
        return false;
    };
    let found = rows.filter_map(Result::ok).any(|(task_id, raw)| {
        task_id == message_id
            && serde_json::from_str::<JsonValue>(&raw)
                .ok()
                .and_then(|envelope| envelope["summary"].as_str().map(str::to_string))
                .is_some_and(|summary| summary.contains(message_id))
    });
    found
}

#[test]
fn f1_legacy_fields_are_silently_tolerated_by_public_add_and_worker_replies() {
    let team_id = "rolecfgf1";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["seed"]);
    let original = b"---\nprovider: fake\ndangerously_skip_permissions: false\ntools: ['fs_read', 'fs_write']\npermission_mode: trusted\nlabel: legacy\ndangerous_auto_approve: true\n---\nHandle the assigned message and return a concise result.\n";
    let role = ws.path().join("roles/legacy.md");
    fs::create_dir_all(role.parent().expect("roles parent")).expect("create roles dir");
    fs::write(&role, original).expect("write historical-format role file");

    quick_start(&ws, team_id);
    let add = add_agent(&ws, "legacy", &role, &[]);
    assert_silent_add(&add, "legacy");
    assert_eq!(fs::read(&role).expect("legacy role remains readable"), original);
    assert_running(&ws, "legacy");

    let spec = runtime_spec(&ws);
    let legacy = spec_agent(&spec, "legacy");
    assert_eq!(string_field(legacy, "provider"), "fake");
    assert_eq!(
        legacy
            .get("system_prompt")
            .and_then(|value| value.get("inline"))
            .and_then(YamlValue::as_str),
        Some("Handle the assigned message and return a concise result."),
        "the role body must stay the compiled prompt: {legacy:?}"
    );

    let canary = format!("legacy-config-natural-reply-{}", std::process::id());
    let send = run_ta(
        &ws,
        &[
            "send",
            "legacy",
            &canary,
            "--workspace",
            ws.path().to_str().expect("workspace path is UTF-8"),
            "--json",
        ],
    );
    assert!(send.is_success(), "send failed: {} {}", send.stdout, send.stderr);
    let message_id = send
        .json()
        .get("message_id")
        .and_then(JsonValue::as_str)
        .expect("send returns message id")
        .to_string();
    let db = ws.path().join(".team/runtime/team.db");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !correlated_reply_exists(&db, &message_id) {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        correlated_reply_exists(&db, &message_id),
        "started worker must naturally reply to message {message_id}"
    );
    assert!(matches!(
        legacy.get("dangerously_skip_permissions"),
        Some(YamlValue::Bool(false))
    ), "dangerous_auto_approve must not elevate permissions: {legacy:?}");
    assert_safe_launch(&ws, "legacy");
}
