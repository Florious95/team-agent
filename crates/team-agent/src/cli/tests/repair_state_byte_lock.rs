use super::*;

const REPAIR_SPEC_TEMPLATE: &str = r#"version: 1
team:
  name: "repair-team"
  mode: "supervisor_worker"
  objective: "Exercise repair-state."
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
  session_name: "team-agent-repair"
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

fn seed_repair_workspace(ws: &std::path::Path) {
    std::fs::create_dir_all(ws.join(".team").join("logs")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    let spec_path = ws.join("team.spec.yaml");
    std::fs::write(
        &spec_path,
        REPAIR_SPEC_TEMPLATE.replace("__WS__", &ws.to_string_lossy()),
    )
    .unwrap();
    crate::state::persist::save_runtime_state(
        ws,
        &json!({
            "spec_path": spec_path.to_string_lossy(),
            "session_name": "team-agent-repair",
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

fn repair_args(
    ws: &std::path::Path,
    status: &str,
    summary: Option<&str>,
    json: bool,
) -> RepairStateArgs {
    RepairStateArgs {
        workspace: ws.to_path_buf(),
        task_id: "task_impl".to_string(),
        assignee: Some("fake_impl".to_string()),
        status: status.to_string(),
        summary: summary.map(str::to_string),
        json,
        team: None,
    }
}

fn team_repair_state(ws: &std::path::Path, team: &str) -> serde_json::Value {
    json!({
        "active_team_key": team,
        "status": "alive",
        "spec_path": ws.join(".team").join("runtime").join(team).join("team.spec.yaml").to_string_lossy(),
        "session_name": format!("team-agent-{team}"),
        "leader": {"id": "leader"},
        "agents": {"fake_impl": {"status": "stopped", "provider": "fake"}},
        "tasks": [{
            "id": "task_impl",
            "title": format!("{team} implementation"),
            "type": "implementation",
            "assignee": null,
            "deps": [],
            "acceptance": ["fake result collected"],
            "status": "pending"
        }]
    })
}

fn seed_two_team_repair_workspace(ws: &std::path::Path) {
    std::fs::create_dir_all(ws.join(".team").join("logs")).unwrap();
    for team in ["alpha", "beta"] {
        let spec_dir = ws.join(".team").join("runtime").join(team);
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            spec_dir.join("team.spec.yaml"),
            REPAIR_SPEC_TEMPLATE.replace("__WS__", &ws.to_string_lossy()),
        )
        .unwrap();
    }
    crate::state::persist::save_runtime_state(
        ws,
        &json!({
            "active_team_key": "alpha",
            "teams": {
                "alpha": team_repair_state(ws, "alpha"),
                "beta": team_repair_state(ws, "beta"),
            }
        }),
    )
    .unwrap();
}

// Golden source:
// - cli/parser.py:303-310 registers `repair-state --workspace --task --assignee --status --summary --json`.
// - cli/commands.py:196-203 delegates all five args to `runtime.repair_state`.
// - diagnose/quick_start.py:285-324:
//   * loads state + spec, validates assignee against spec agents plus leader id,
//   * validates status against task_graph.py TASK_STATUSES,
//   * before/after are three-field projections `{assignee,status,last_result_summary}`,
//   * summary writes `last_result_summary` (not `summary`),
//   * writes EventLog event `repair_state.task` with task_id/before/after,
//   * returns `{ok,task_id,before,after,state_file}` in that insertion order.
// - task_graph.py:5-14 legal statuses are exactly:
//   blocked,cancelled,done,failed,needs_retry,pending,ready,running.
//
// Golden probe:
//   PYTHONPATH=/Users/alauda/Documents/code/team-agent-public/src python3 /tmp/probe_repair_state_cli.py
#[test]
fn repair_state_success_json_human_and_event_byte_shape() {
    let ws = tmp_workspace();
    seed_repair_workspace(&ws);

    let json_result = cmd_repair_state(&repair_args(&ws, "done", Some("patched"), true))
        .expect("repair-state success should return ok");
    assert_eq!(json_result.exit, ExitCode::Ok);
    assert_eq!(
        emit(&json_result.output, true).unwrap(),
        format!(
            "{{\n  \"after\": {{\n    \"assignee\": \"fake_impl\",\n    \"last_result_summary\": \"patched\",\n    \"status\": \"done\"\n  }},\n  \"before\": {{\n    \"assignee\": null,\n    \"last_result_summary\": null,\n    \"status\": \"pending\"\n  }},\n  \"ok\": true,\n  \"state_file\": \"{}\",\n  \"task_id\": \"task_impl\"\n}}",
            ws.join("team_state.md").to_string_lossy()
        ),
        "repair-state --json must match Python pretty sorted JSON byte shape",
    );

    let event_line = std::fs::read_to_string(ws.join(".team/logs/events.jsonl"))
        .expect("repair-state must write events.jsonl")
        .lines()
        .last()
        .expect("repair-state must append an event")
        .to_string();
    assert!(
        event_line.starts_with("{\"after\": {\"assignee\": \"fake_impl\", \"last_result_summary\": \"patched\", \"status\": \"done\"}, \"before\": {\"assignee\": null, \"last_result_summary\": null, \"status\": \"pending\"}, \"event\": \"repair_state.task\", \"task_id\": \"task_impl\", \"ts\": \""),
        "repair_state.task event must be Python sort_keys JSON with after/before/event/task_id/ts order; got {event_line}",
    );
    assert!(event_line.ends_with("\"}"), "event line must end with timestamp string; got {event_line}");

    let ws_human = tmp_workspace();
    seed_repair_workspace(&ws_human);
    let human_result = cmd_repair_state(&repair_args(&ws_human, "done", Some("patched"), false))
        .expect("repair-state human success should return ok");
    assert_eq!(
        emit(&human_result.output, false).unwrap(),
        format!(
            "ok: True\ntask_id: task_impl\nbefore: {{\"assignee\": null, \"status\": \"pending\", \"last_result_summary\": null}}\nafter: {{\"assignee\": \"fake_impl\", \"status\": \"done\", \"last_result_summary\": \"patched\"}}\nstate_file: {}",
            ws_human.join("team_state.md").to_string_lossy()
        ),
        "repair-state human output must preserve Python returned-dict and nested-dict insertion order",
    );

    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(
        state["tasks"][0]["last_result_summary"],
        json!("patched"),
        "golden writes summary into last_result_summary, not summary",
    );
    assert!(
        state["tasks"][0].get("summary").is_none(),
        "golden does not create a task.summary field during repair-state",
    );
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&ws_human);
}

#[test]
fn repair_state_rejects_status_outside_task_statuses() {
    let ws = tmp_workspace();
    seed_repair_workspace(&ws);
    let legal_statuses = vec![
        "blocked",
        "cancelled",
        "done",
        "failed",
        "needs_retry",
        "pending",
        "ready",
        "running",
    ];
    assert_eq!(
        legal_statuses,
        vec![
            "blocked",
            "cancelled",
            "done",
            "failed",
            "needs_retry",
            "pending",
            "ready",
            "running"
        ],
        "golden TASK_STATUSES from task_graph.py:5 must stay locked",
    );

    let err = cmd_repair_state(&repair_args(&ws, "assigned", None, true))
        .expect_err("status outside TASK_STATUSES must error");
    assert_eq!(err.to_string(), "unknown task status for repair: assigned");
    let payload = err.to_payload(&ws.join(".team/logs/cli-error-123.log"), "repair-state");
    assert_eq!(
        serde_json::to_string(&payload).unwrap(),
        format!(
            "{{\"ok\":false,\"error\":\"unknown task status for repair: assigned\",\"action\":\"run `team-agent doctor` or inspect the log path shown here\",\"log\":\"{}\"}}",
            ws.join(".team/logs/cli-error-123.log").to_string_lossy()
        ),
        "repair-state --json error envelope must match Python compact key order and text",
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn repair_state_explicit_team_updates_selected_team_not_active_team() {
    let ws = tmp_workspace();
    seed_two_team_repair_workspace(&ws);
    let mut args = repair_args(&ws, "done", Some("patched beta"), true);
    args.team = Some("beta".to_string());

    let cmd = cmd_repair_state(&args).expect("repair-state --team beta should update beta");
    assert_eq!(cmd.exit, ExitCode::Ok);
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(
        state["teams"]["alpha"]["tasks"][0]["status"],
        json!("pending"),
        "repair-state --team beta must not mutate active/default alpha"
    );
    assert_eq!(
        state["teams"]["beta"]["tasks"][0]["status"],
        json!("done"),
        "repair-state must consume args.team and update beta"
    );
    assert_eq!(
        state["teams"]["beta"]["tasks"][0]["last_result_summary"],
        json!("patched beta")
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// CONTRACT (real-machine repair_state product FAIL): rt-host-d ran `repair-state --task rp1 --status done`
// in a quick-start workspace whose spec lives in a teamdir (state.spec_path -> <ws>/teamdir/team.spec.yaml),
// NOT at <ws>/team.spec.yaml. Rust cmd_repair_state calls load_team_spec(workspace) which HARDCODES
// workspace.join("team.spec.yaml") (adapters.rs:982) -> read_to_string fails 'No such file or directory'.
// golden (quick_start.py:295) resolves spec_path = state.get("spec_path", workspace/"team.spec.yaml") and
// SUCCEEDS. NB: golden ALSO writes team_state.md (write_team_state -> state.py mkdir+write) and returns
// state_file=team_state.md, so the fix is NOT to drop write_team_state (that diverges from golden and breaks
// repair_state_success_json_human_and_event_byte_shape) — it is to resolve the spec from state.spec_path
// (Rust already has load_team_spec_optional(workspace, state) that does exactly this).
#[test]
fn contract_repair_state_resolves_spec_from_state_spec_path_teamdir_layout() {
    let ws = tmp_workspace();
    std::fs::create_dir_all(ws.join(".team").join("logs")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    let teamdir = ws.join("teamdir");
    std::fs::create_dir_all(&teamdir).unwrap();
    let spec_path = teamdir.join("team.spec.yaml");
    std::fs::write(
        &spec_path,
        REPAIR_SPEC_TEMPLATE.replace("__WS__", &ws.to_string_lossy()),
    )
    .unwrap();
    // NOTE: NO <ws>/team.spec.yaml (the hardcoded path) and NO pre-existing team_state.md.
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "spec_path": spec_path.to_string_lossy(),
            "session_name": "team-agent-repair",
            "leader": {"id": "leader"},
            "agents": {"fake_impl": {"status": "running", "provider": "fake"}},
            "tasks": [{
                "id": "task_impl", "title": "x", "type": "implementation",
                "assignee": null, "deps": [], "acceptance": ["a"], "status": "pending"
            }]
        }),
    )
    .unwrap();

    let cmd = cmd_repair_state(&repair_args(&ws, "done", Some("patched"), true)).expect(
        "CONTRACT: repair-state --status done must resolve the spec from state.spec_path (golden \
         quick_start.py:295), not the hardcoded <ws>/team.spec.yaml (adapters.rs:982) which is absent in \
         the teamdir layout -> currently io 'No such file or directory'",
    );
    assert_eq!(cmd.exit, ExitCode::Ok);
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(
        state["tasks"][0]["status"],
        json!("done"),
        "the task must persist status=done after repair"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
