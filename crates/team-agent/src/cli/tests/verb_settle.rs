use super::*;

const SETTLE_SPEC_TEMPLATE: &str = r#"version: 1
team:
  name: "fake-e2e"
  mode: "supervisor_worker"
  objective: "Exercise settle."
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

fn seed_settle_workspace(ws: &std::path::Path) {
    std::fs::create_dir_all(ws.join(".team").join("logs")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    std::fs::write(
        ws.join("team.spec.yaml"),
        SETTLE_SPEC_TEMPLATE.replace("__WS__", &ws.to_string_lossy()),
    )
    .unwrap();
    crate::state::persist::save_runtime_state(
        ws,
        &json!({
            "spec_path": ws.join("team.spec.yaml").to_string_lossy(),
            "session_name": "team-agent-fake-e2e",
            "leader": {"id": "leader"},
            "agents": {"fake_impl": {"status": "stopped", "provider": "fake"}},
            "tasks": [{
                "id": "task_impl",
                "title": "Fake implementation",
                "type": "implementation",
                "assignee": null,
                "deps": [],
                "acceptance": ["fake result collected"],
                "status": "pending"
            }]
        }),
    )
    .unwrap();
}

// Golden source:
// - cli/parser.py:177-180 registers `settle --workspace . --json`; no timeout or polling args.
// - cli/commands.py:86-87 delegates to `runtime.settle(Path(args.workspace).resolve())`.
// - diagnose/quick_start.py:269-284: `settle` synchronously calls `collect(workspace)`,
//   then `status(workspace, as_json=True)`, writes `settle-<int(time.time())>.json`,
//   and returns `{ok, summary, next_actions, details_log, collect}`.
// - cli/helpers.py:12-16 success `--json` uses pretty sorted JSON; human output uses dict
//   insertion order from the returned dict.
//
// Golden probe:
//   PYTHONPATH=/Users/alauda/Documents/code/team-agent-public/src python3 /tmp/probe_settle_cli.py
//   With a fake `tmux` executable returning exit 1, golden exits 0 and reports
//   summary `collected 0 result(s)`.
#[test]
fn settle_success_json_byte_shape_wraps_collect() {
    let ws = tmp_workspace();
    seed_settle_workspace(&ws);

    let result = cmd_settle(&SettleArgs {
        team: None,
        workspace: ws.clone(),
        json: true,
    })
    .expect("settle should return an ok result for an empty valid workspace");

    assert_eq!(result.exit, ExitCode::Ok);
    let CmdOutput::Json(value) = &result.output else {
        panic!("settle must return a JSON dict");
    };
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["summary"], json!("collected 0 result(s)"));
    assert_eq!(
        value["next_actions"],
        json!(["Review team_state.md and decide whether to continue or shutdown."])
    );
    assert!(
        value["details_log"]
            .as_str()
            .is_some_and(|p| p.contains("/.team/logs/settle-") && p.ends_with(".json")),
        "settle must write a settle-<timestamp>.json details log; got {value}",
    );
    assert_eq!(value["collect"]["ok"], json!(true));
    assert_eq!(value["collect"]["collected"], json!([]));
    assert_eq!(value["collect"]["collected_results"], json!([]));
    assert_eq!(value["collect"]["delivered_messages"], json!([]));
    assert_eq!(value["collect"]["invalid_results"], json!([]));
    assert_eq!(
        value["collect"]["results"],
        json!({
            "total": 0,
            "uncollected": 0,
            "collected": 0,
            "invalid": 0,
            "by_status": {},
        })
    );
    assert_eq!(
        value["collect"]["state_file"],
        json!(ws.join("team_state.md").to_string_lossy().to_string())
    );
    assert_eq!(value["collect"]["coordinator"]["ok"], json!(true));
    assert_eq!(value["collect"]["coordinator"]["status"], json!("started"));
    assert!(
        value["collect"]["coordinator"]["log"]
            .as_str()
            .is_some_and(|p| p.ends_with("/.team/runtime/coordinator.log")),
        "collect.coordinator.log must point at .team/runtime/coordinator.log; got {value}",
    );

    let expected = serde_json::to_string_pretty(&crate::cli::sort_json(value)).unwrap();
    assert_eq!(
        emit(&result.output, true).unwrap(),
        expected,
        "settle --json must emit Python's json.dumps(indent=2, sort_keys=True) byte shape",
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn settle_success_human_output_insertion_order() {
    let ws = tmp_workspace();
    seed_settle_workspace(&ws);

    let result = cmd_settle(&SettleArgs {
        team: None,
        workspace: ws.clone(),
        json: false,
    })
    .expect("settle should return an ok result for an empty valid workspace");

    let human = emit(&result.output, false).unwrap();
    assert!(
        human.starts_with(
            "ok: True\nsummary: collected 0 result(s)\nnext_actions: [\"Review team_state.md and decide whether to continue or shutdown.\"]\ndetails_log: "
        ),
        "non-json settle output must preserve Python returned-dict insertion order; got {human}",
    );
    assert!(
        human.contains("\ncollect: {\"ok\": true, \"collected\": [], \"collected_results\": []"),
        "non-json settle output must render nested collect as compact JSON after details_log; got {human}",
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// Golden source:
// - cli/parser.py:177-180 default workspace is `.`.
// Current Rust dispatch has no `settle` arm; this must be RED until the porter routes it.
#[test]
fn dispatch_routes_settle_default_workspace() {
    let ws = tmp_workspace();
    seed_settle_workspace(&ws);
    let code = run(&["settle".to_string(), "--json".to_string()], &ws);
    assert_eq!(code, ExitCode::Ok, "`settle --json` must route and exit 0 for default workspace");
    let _ = std::fs::remove_dir_all(&ws);
}
