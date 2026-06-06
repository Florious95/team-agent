#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_ws(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-sessions-red-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "stderr={}", stderr(output));
    assert_eq!(stderr(output), "");
}

fn rich_spec(ws: &Path) -> String {
    format!(
        r#"version: 1
team:
  name: "sessions"
  mode: "supervisor_worker"
  objective: "sessions overview"
  workspace: "{ws}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  model: null
  tools:
    - "fs_read"
    - "fs_list"
    - "mcp_team"
  context_policy:
    keep_user_thread: true
    receive_worker_outputs: "structured_only"
    max_worker_result_tokens: 2000
agents:
  - id: "w1"
    role: "Worker One"
    provider: "codex"
    model: "gpt-5.5"
    profile: "prof-a"
    working_directory: "{ws}"
    system_prompt:
      inline: "Work one."
      file: null
    tools:
      - "fs_read"
      - "mcp_team"
    permission_mode: "restricted"
    preferred_for:
      - "implementation"
    avoid_for: []
    output_contract:
      format: "result_envelope_v1"
      required_fields:
        - "task_id"
        - "status"
        - "summary"
        - "artifacts"
  - id: "w2"
    role: "Worker Two"
    provider: "claude_code"
    model: null
    working_directory: "{ws}"
    system_prompt:
      inline: "Work two."
      file: null
    tools:
      - "fs_read"
      - "mcp_team"
    permission_mode: "restricted"
    preferred_for:
      - "review"
    avoid_for: []
    output_contract:
      format: "result_envelope_v1"
      required_fields:
        - "task_id"
        - "status"
        - "summary"
        - "artifacts"
routing:
  default_assignee: "leader"
  rules: []
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  ack_timeout_sec: 60
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
    mirror_files: ".team/messages"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-sessions"
  auto_launch: true
  require_user_approval_before_launch: true
  max_active_agents: 2
  startup_order:
    - "w1"
    - "w2"
context:
  state_file: "team_state.md"
  artifact_dir: ".team/artifacts"
  log_dir: ".team/logs"
  summarization:
    worker_full_logs: "retain_outside_leader_context"
    state_update: "after_each_result"
tasks:
  - id: "old"
    title: "Old"
    type: "implementation"
    assignee: "w1"
    deps: []
    acceptance:
      - "old done"
    status: "done"
    requires_tools:
      - "fs_read"
    files: []
    risk: "low"
  - id: "new"
    title: "New"
    type: "implementation"
    assignee: "w1"
    deps: []
    acceptance:
      - "new running"
    status: "running"
    requires_tools:
      - "fs_read"
    files: []
    risk: "low"
  - id: "other"
    title: "Other"
    type: "review"
    assignee: "w2"
    deps: []
    acceptance:
      - "other pending"
    status: "pending"
    requires_tools:
      - "fs_read"
    files: []
    risk: "low"
"#,
        ws = ws.display()
    )
}

fn seed_rich_workspace(ws: &Path) {
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    std::fs::write(ws.join("team.spec.yaml"), rich_spec(ws)).unwrap();
    std::fs::write(
        ws.join(".team/runtime/state.json"),
        format!(
            r#"{{
  "session_name": "team-sessions",
  "agents": {{
    "w1": {{
      "provider": "codex-STATE",
      "model": "state-model",
      "profile": "state-profile",
      "status": "running",
      "session_id": "sess-1",
      "resume_id": "resume-1",
      "window": "worker-one",
      "pane_id": "%42",
      "rollout_path": "/tmp/rollout.jsonl",
      "captured_at": "2026-06-04T00:00:00Z",
      "captured_via": "fs_watch",
      "attribution_confidence": "high",
      "spawn_cwd": "{ws}",
      "context_usage": {{"used": 12}},
      "handoff_path": "/tmp/handoff.md",
      "display": {{"target": "ghostty"}}
    }}
  }},
  "tasks": [
    {{"id": "old", "assignee": "w1", "status": "done"}},
    {{"id": "new", "assignee": "w1", "status": "running"}},
    {{"id": "other", "assignee": "w2", "status": "pending"}}
  ]
}}"#,
            ws = ws.display()
        ),
    )
    .unwrap();
}

#[test]
fn sessions_empty_workspace_json_and_human_are_byte_locked() {
    let ws = tmp_ws("empty");
    let json = run(&["sessions", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert_success(&json);
    assert_eq!(
        stdout(&json),
        format!(
            "{{\n  \"ok\": true,\n  \"sessions\": [],\n  \"workspace\": \"{}\"\n}}\n",
            ws.display()
        )
    );

    let human = run(&["sessions", "--workspace", ws.to_str().unwrap()], &ws);
    assert_success(&human);
    assert_eq!(
        stdout(&human),
        format!("ok: True\nsessions: []\nworkspace: {}\n", ws.display())
    );
}

#[test]
fn sessions_state_only_without_spec_stays_empty_like_python() {
    let ws = tmp_ws("state-only");
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    std::fs::write(
        ws.join(".team/runtime/state.json"),
        r#"{"session_name":"team-stateonly","agents":{"w1":{"provider":"codex","status":"running","session_id":"sess-1","window":"w1","pane_id":"%42"}},"tasks":[{"id":"new","assignee":"w1","status":"running"}]}"#,
    )
    .unwrap();

    let output = run(&["sessions", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "{{\n  \"ok\": true,\n  \"sessions\": [],\n  \"workspace\": \"{}\"\n}}\n",
            ws.display()
        ),
        "golden sessions only iterates spec.agents; state-only agents are ignored"
    );
}

#[test]
fn sessions_resolves_spec_path_from_runtime_state_like_quick_start() {
    let ws = tmp_ws("state-spec-path");
    let team_dir = ws.join(".team/current");
    let spec_path = team_dir.join("team.spec.yaml");
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    std::fs::create_dir_all(&team_dir).unwrap();
    std::fs::write(&spec_path, rich_spec(&ws)).unwrap();
    assert!(
        !ws.join("team.spec.yaml").exists(),
        "quick-start layout stores the spec in the team dir, not at workspace root"
    );
    std::fs::write(
        ws.join(".team/runtime/state.json"),
        format!(
            r#"{{
  "spec_path": "{spec_path}",
  "session_name": "team-sessions",
  "agents": {{
    "w1": {{
      "status": "running",
      "session_id": "sess-1",
      "window": "worker-one",
      "pane_id": "%42"
    }}
  }},
  "tasks": [
    {{"id": "new", "assignee": "w1", "status": "running"}},
    {{"id": "other", "assignee": "w2", "status": "pending"}}
  ]
}}"#,
            spec_path = spec_path.display()
        ),
    )
    .unwrap();

    let output = run(&["sessions", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            r#"{{
  "ok": true,
  "sessions": [
    {{
      "agent_id": "w1",
      "attribution_confidence": null,
      "captured_at": null,
      "captured_via": null,
      "context_usage": null,
      "display_target": null,
      "handoff_path": null,
      "last_task": "new",
      "model": "gpt-5.5",
      "profile": "prof-a",
      "provider": "codex",
      "resume_id": null,
      "rollout_path": null,
      "session_id": "sess-1",
      "spawn_cwd": null,
      "status": "running",
      "terminal_target": {{
        "pane": "%42",
        "session": "team-sessions",
        "window": "worker-one"
      }}
    }},
    {{
      "agent_id": "w2",
      "attribution_confidence": null,
      "captured_at": null,
      "captured_via": null,
      "context_usage": null,
      "display_target": null,
      "handoff_path": null,
      "last_task": "other",
      "model": null,
      "profile": null,
      "provider": "claude_code",
      "resume_id": null,
      "rollout_path": null,
      "session_id": null,
      "spawn_cwd": null,
      "status": "unknown",
      "terminal_target": {{
        "pane": null,
        "session": "team-sessions",
        "window": "w2"
      }}
    }}
  ],
  "workspace": "{}"
}}
"#,
            ws.display()
        ),
        "golden inventory.py resolves state.spec_path before falling back to workspace/team.spec.yaml"
    );
}

#[test]
fn sessions_resolves_team_dir_from_runtime_state_like_real_quick_start() {
    let ws = tmp_ws("state-team-dir");
    let team_dir = ws.join(".team/current");
    let spec_path = team_dir.join("team.spec.yaml");
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    std::fs::create_dir_all(&team_dir).unwrap();
    std::fs::write(&spec_path, rich_spec(&ws)).unwrap();
    assert!(
        !ws.join("team.spec.yaml").exists(),
        "real quick-start layout does not write workspace/team.spec.yaml"
    );
    std::fs::write(
        ws.join(".team/runtime/state.json"),
        format!(
            r#"{{
  "team_dir": "{team_dir}",
  "session_name": "team-sessions",
  "agents": {{
    "w1": {{
      "status": "running",
      "session_id": "sess-1",
      "window": "worker-one",
      "pane_id": "%42"
    }}
  }},
  "tasks": [
    {{"id": "new", "assignee": "w1", "status": "running"}},
    {{"id": "other", "assignee": "w2", "status": "pending"}}
  ]
}}"#,
            team_dir = team_dir.display()
        ),
    )
    .unwrap();

    let output = run(&["sessions", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            r#"{{
  "ok": true,
  "sessions": [
    {{
      "agent_id": "w1",
      "attribution_confidence": null,
      "captured_at": null,
      "captured_via": null,
      "context_usage": null,
      "display_target": null,
      "handoff_path": null,
      "last_task": "new",
      "model": "gpt-5.5",
      "profile": "prof-a",
      "provider": "codex",
      "resume_id": null,
      "rollout_path": null,
      "session_id": "sess-1",
      "spawn_cwd": null,
      "status": "running",
      "terminal_target": {{
        "pane": "%42",
        "session": "team-sessions",
        "window": "worker-one"
      }}
    }},
    {{
      "agent_id": "w2",
      "attribution_confidence": null,
      "captured_at": null,
      "captured_via": null,
      "context_usage": null,
      "display_target": null,
      "handoff_path": null,
      "last_task": "other",
      "model": null,
      "profile": null,
      "provider": "claude_code",
      "resume_id": null,
      "rollout_path": null,
      "session_id": null,
      "spawn_cwd": null,
      "status": "unknown",
      "terminal_target": {{
        "pane": null,
        "session": "team-sessions",
        "window": "w2"
      }}
    }}
  ],
  "workspace": "{}"
}}
"#,
            ws.display()
        ),
        "real quick-start state has team_dir without spec_path; sessions must resolve team_dir/team.spec.yaml"
    );
}

#[test]
fn sessions_rich_workspace_json_and_human_are_byte_locked() {
    let ws = tmp_ws("rich");
    seed_rich_workspace(&ws);

    let json = run(&["sessions", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    assert_success(&json);
    assert_eq!(stdout(&json), expected_rich_json(&ws));

    let human = run(&["sessions", "--workspace", ws.to_str().unwrap()], &ws);
    assert_success(&human);
    assert_eq!(stdout(&human), expected_rich_human(&ws));
}

fn expected_rich_json(ws: &Path) -> String {
    format!(
        r#"{{
  "ok": true,
  "sessions": [
    {{
      "agent_id": "w1",
      "attribution_confidence": "high",
      "captured_at": "2026-06-04T00:00:00Z",
      "captured_via": "fs_watch",
      "context_usage": {{
        "used": 12
      }},
      "display_target": {{
        "target": "ghostty"
      }},
      "handoff_path": "/tmp/handoff.md",
      "last_task": "new",
      "model": "gpt-5.5",
      "profile": "prof-a",
      "provider": "codex",
      "resume_id": "resume-1",
      "rollout_path": "/tmp/rollout.jsonl",
      "session_id": "sess-1",
      "spawn_cwd": "{ws}",
      "status": "running",
      "terminal_target": {{
        "pane": "%42",
        "session": "team-sessions",
        "window": "worker-one"
      }}
    }},
    {{
      "agent_id": "w2",
      "attribution_confidence": null,
      "captured_at": null,
      "captured_via": null,
      "context_usage": null,
      "display_target": null,
      "handoff_path": null,
      "last_task": "other",
      "model": null,
      "profile": null,
      "provider": "claude_code",
      "resume_id": null,
      "rollout_path": null,
      "session_id": null,
      "spawn_cwd": null,
      "status": "unknown",
      "terminal_target": {{
        "pane": null,
        "session": "team-sessions",
        "window": "w2"
      }}
    }}
  ],
  "workspace": "{ws}"
}}
"#,
        ws = ws.display()
    )
}

fn expected_rich_human(ws: &Path) -> String {
    format!(
        "ok: True\nsessions: [{{\"agent_id\": \"w1\", \"provider\": \"codex\", \"model\": \"gpt-5.5\", \"profile\": \"prof-a\", \"session_id\": \"sess-1\", \"resume_id\": \"resume-1\", \"rollout_path\": \"/tmp/rollout.jsonl\", \"captured_at\": \"2026-06-04T00:00:00Z\", \"captured_via\": \"fs_watch\", \"attribution_confidence\": \"high\", \"spawn_cwd\": \"{ws}\", \"context_usage\": {{\"used\": 12}}, \"status\": \"running\", \"last_task\": \"new\", \"handoff_path\": \"/tmp/handoff.md\", \"display_target\": {{\"target\": \"ghostty\"}}, \"terminal_target\": {{\"session\": \"team-sessions\", \"window\": \"worker-one\", \"pane\": \"%42\"}}}}, {{\"agent_id\": \"w2\", \"provider\": \"claude_code\", \"model\": null, \"profile\": null, \"session_id\": null, \"resume_id\": null, \"rollout_path\": null, \"captured_at\": null, \"captured_via\": null, \"attribution_confidence\": null, \"spawn_cwd\": null, \"context_usage\": null, \"status\": \"unknown\", \"last_task\": \"other\", \"handoff_path\": null, \"display_target\": null, \"terminal_target\": {{\"session\": \"team-sessions\", \"window\": \"w2\", \"pane\": null}}}}]\nworkspace: {ws}\n",
        ws = ws.display()
    )
}
