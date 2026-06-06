use super::*;

const VALIDATE_SPEC_TEMPLATE: &str = r#"version: 1
team:
  name: "fake-e2e"
  mode: "supervisor_worker"
  objective: "Exercise validate."
  workspace: "__WS__"
leader:
  id: "leader"
  role: "leader"
  provider: "fake"
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
  - id: "fake_impl"
    role: "implementation_engineer"
    provider: "fake"
    model: null
    working_directory: "__WS__"
    system_prompt:
      inline: "Handle fake implementation tasks."
      file: null
    tools:
      - "fs_read"
      - "fs_write"
      - "fs_list"
      - "execute_bash"
      - "git_diff"
      - "mcp_team"
      - "provider_builtin"
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
routing:
  default_assignee: "leader"
  rules:
    - id: "implementation-to-fake"
      match:
        type:
          - "implementation"
      assign_to: "fake_impl"
      priority: 10
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  ack_timeout_sec: 2
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
    mirror_files: ".team/messages"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-agent-fake-e2e"
  auto_launch: true
  require_user_approval_before_launch: false
  max_active_agents: 1
  startup_order:
    - "fake_impl"
context:
  state_file: "team_state.md"
  artifact_dir: ".team/artifacts"
  log_dir: ".team/logs"
  summarization:
    worker_full_logs: "retain_outside_leader_context"
    state_update: "after_each_result"
tasks:
  - id: "task_impl"
    title: "Fake implementation"
    type: "implementation"
    assignee: null
    deps: []
    acceptance:
      - "fake result collected"
    status: "pending"
"#;

fn write_validate_spec(ws: &std::path::Path, file_name: &str, provider: &str) -> std::path::PathBuf {
    let spec = VALIDATE_SPEC_TEMPLATE
        .replace("__WS__", &ws.to_string_lossy())
        .replacen("provider: \"fake\"", &format!("provider: \"{provider}\""), 1);
    let path = ws.join(file_name);
    std::fs::write(&path, spec).unwrap();
    path
}

// Golden source:
// - cli/parser.py:120-123 registers `validate [spec=team.spec.yaml] --json`.
// - cli/commands.py:38-39 delegates to `runtime.validate_file(Path(args.spec).resolve())`.
// - launch/bootstrap.py:30-43: file specs return `{ok: True, workspace: str(workspace), team: name}`.
// - cli/helpers.py:12-16 JSON emits `json.dumps(..., indent=2, ensure_ascii=False, sort_keys=True)`.
//
// Golden probe:
//   PYTHONPATH=/Users/alauda/Documents/code/team-agent-public/src python3 /tmp/probe_validate_cli.py
//   valid file --json rc=0 stdout:
//   {
//     "ok": true,
//     "team": "fake-e2e",
//     "workspace": "<ws>"
//   }
#[test]
fn validate_valid_spec_file_json_byte_shape() {
    let ws = tmp_workspace();
    let spec_path = write_validate_spec(&ws, "team.spec.yaml", "fake");

    let result = cmd_validate(&ValidateArgs {
        spec: spec_path.clone(),
        json: true,
    })
    .expect("valid spec must return an ok CmdResult");

    assert_eq!(result.exit, ExitCode::Ok);
    assert_eq!(
        emit(&result.output, true).unwrap(),
        format!(
            "{{\n  \"ok\": true,\n  \"team\": \"fake-e2e\",\n  \"workspace\": \"{}\"\n}}",
            ws.to_string_lossy()
        ),
        "validate --json must match Python sort_keys+indent bytes for a valid file spec",
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// Golden source:
// - spec.py:85-91 accumulates validation messages and raises
//   `team.spec.yaml validation failed:\n- ...`.
// - spec.py semantic provider check emits `/leader/provider: unknown provider 'bogus'`.
// - cli/helpers.py:137-155 error payload insertion order is `ok,error,action,log`.
// - cli/helpers.py:128-131 prints compact JSON, not pretty/sorted JSON, for `--json` errors.
#[test]
fn validate_invalid_spec_json_error_envelope_byte_shape() {
    let ws = tmp_workspace();
    let spec_path = write_validate_spec(&ws, "bad.spec.yaml", "bogus");

    let err = cmd_validate(&ValidateArgs {
        spec: spec_path,
        json: true,
    })
    .expect_err("invalid spec must surface a CLI/runtime validation error");

    assert_eq!(
        err.to_string(),
        "team.spec.yaml validation failed:\n- /leader/provider: unknown provider 'bogus'",
    );
    let payload = err.to_payload(&ws.join(".team/logs/cli-error-123.log"), "validate");
    assert_eq!(
        serde_json::to_string(&payload).unwrap(),
        format!(
            "{{\"ok\":false,\"error\":\"team.spec.yaml validation failed:\\n- /leader/provider: unknown provider 'bogus'\",\"action\":\"run `team-agent doctor` or inspect the log path shown here\",\"log\":\"{}\"}}",
            ws.join(".team/logs/cli-error-123.log").to_string_lossy()
        ),
        "validate --json errors must match Python compact error envelope key order and text",
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// Golden source:
// - cli/parser.py:120-123 default spec argument is `team.spec.yaml`.
// Current Rust dispatch has no `validate` arm; this must be RED until the porter routes it.
#[test]
fn dispatch_routes_validate_default_spec() {
    let ws = tmp_workspace();
    let _spec_path = write_validate_spec(&ws, "team.spec.yaml", "fake");
    let code = run(&["validate".to_string(), "--json".to_string()], &ws);
    assert_eq!(code, ExitCode::Ok, "`validate --json` must route and exit 0 for default team.spec.yaml");
    let _ = std::fs::remove_dir_all(&ws);
}
