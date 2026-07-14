#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use team_agent::cli::{cmd_e2e, cmd_sessions, CmdOutput, E2eArgs, ExitCode, SessionsArgs};

fn tmp_workspace(tag: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let ws = std::env::temp_dir().join(format!(
        "ta-rs-cli-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    ws
}

fn json_output(result: team_agent::cli::CmdResult) -> serde_json::Value {
    match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON output, got {other:?}"),
    }
}

#[test]
fn e2e_nonexistent_provider_is_skipped_not_installed() {
    let ws = tmp_workspace("e2e-missing");
    let result = cmd_e2e(&E2eArgs {
        workspace: ws.clone(),
        providers: vec!["nonexistent".to_string()],
        real: false,
        json: true,
    })
    .expect("e2e result");
    assert_eq!(result.exit, ExitCode::Error);
    let value = json_output(result);
    assert_eq!(value["providers"]["nonexistent"]["ok"], json!(false));
    assert_eq!(value["providers"]["nonexistent"]["skipped"], json!(true));
    assert_eq!(
        value["providers"]["nonexistent"]["reason"],
        json!("nonexistent not installed")
    );
    assert!(value["providers"]["nonexistent"]["version"].is_null());
    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn e2e_fake_writes_spec_and_sends_task_message() {
    let ws = tmp_workspace("e2e-fake");
    let result = cmd_e2e(&E2eArgs {
        workspace: ws.clone(),
        providers: vec!["fake".to_string()],
        real: false,
        json: true,
    })
    .expect("fake e2e result");
    let value = json_output(result);
    assert_eq!(value["providers"]["fake"]["launch"]["ok"], json!(true));
    assert!(
        value["providers"]["fake"]["send"]["message_id"]
            .as_str()
            .is_some(),
        "fake e2e must delegate through send_message and expose message_id"
    );
    assert!(
        ws.join("team.spec.yaml").exists(),
        "fake e2e must write the fake spec into the workspace"
    );
    let _ = std::fs::remove_dir_all(ws);
}

#[test]
fn sessions_json_uses_spec_and_state_rich_rows() {
    let ws = tmp_workspace("sessions-rich");
    std::fs::write(
        ws.join("team.spec.yaml"),
        format!(
            r#"version: 1
team:
  name: "sessions"
  workspace: "{}"
agents:
  - id: "w1"
    provider: "codex"
    model: "gpt-5.5"
    profile: "prof-a"
  - id: "w2"
    provider: "claude_code"
"#,
            ws.to_string_lossy()
        ),
    )
    .unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-sessions",
            "agents": {
                "w1": {
                    "provider": "codex-state",
                    "model": "state-model",
                    "profile": "state-profile",
                    "status": "running",
                    "session_id": "sess-1",
                    "window": "worker-one",
                    "pane_id": "%42",
                    "rollout_path": "/tmp/rollout.jsonl",
                    "captured_at": "2026-06-04T00:00:00Z",
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "spawn_cwd": ws.to_string_lossy().to_string(),
                    "context_usage": {"used": 12},
                    "handoff_path": "/tmp/handoff.md",
                    "display": {"target": "ghostty"}
                },
                "state-only": {
                    "provider": "codex",
                    "status": "running",
                    "session_id": "ignored"
                }
            },
            "tasks": [
                {"id": "old", "assignee": "w1", "status": "done"},
                {"id": "new", "assignee": "w1", "status": "running"},
                {"id": "other", "assignee": "w2", "status": "pending"}
            ]
        }),
    )
    .unwrap();
    let result = cmd_sessions(&SessionsArgs {
        workspace: ws.clone(),
        json: true,
        team: None,
    })
    .expect("sessions result");
    assert_eq!(result.exit, ExitCode::Ok);
    let value = json_output(result);
    let rows = value["sessions"].as_array().expect("sessions rows");
    assert_eq!(rows.len(), 2);
    let row = &rows[0];
    assert_eq!(row["agent_id"], json!("w1"));
    assert_eq!(row["provider"], json!("codex"));
    assert_eq!(row["model"], json!("gpt-5.5"));
    assert_eq!(row["profile"], json!("prof-a"));
    assert_eq!(row["session_id"], json!("sess-1"));
    assert_eq!(row["last_task"], json!("new"));
    assert_eq!(row["display_target"], json!({"target": "ghostty"}));
    assert_eq!(row["terminal_target"]["session"], json!("team-sessions"));
    assert_eq!(row["terminal_target"]["window"], json!("worker-one"));
    assert_eq!(row["terminal_target"]["pane"], json!("%42"));
    let row = &rows[1];
    assert_eq!(row["agent_id"], json!("w2"));
    assert_eq!(row["provider"], json!("claude_code"));
    assert!(row["model"].is_null());
    assert!(row["profile"].is_null());
    assert_eq!(row["status"], json!("unknown"));
    assert_eq!(row["last_task"], json!("other"));
    assert!(row["display_target"].is_null());
    assert_eq!(row["terminal_target"]["window"], json!("w2"));
    let _ = std::fs::remove_dir_all(ws);
}
