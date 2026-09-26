#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod sim;
#[path = "e2e/framework.rs"]
#[allow(dead_code)]
mod cli_fixture;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::mcp_server::{
    handle_mcp, McpTool, TeamOrchestratorTools, ToolErrorReason,
};
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::{db, messaging};

const RETAINED: [&str; 3] = ["send_message", "report_result", "get_team_status"];
const RETIRED: [&str; 10] = [
    "assign_task",
    "update_state",
    "stop_agent",
    "reset_agent",
    "add_agent",
    "clone_agent",
    "fork_agent",
    "request_human",
    "stuck_list",
    "stuck_cancel",
];

struct TempCase(PathBuf);

impl TempCase {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "team-agent-mcp3-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tools_at(path: &Path, agent: &str, team: &str) -> TeamOrchestratorTools {
    TeamOrchestratorTools::with_identity(
        path,
        Some(AgentId::new(agent)),
        Some(TeamKey::new(team)),
    )
}

fn rpc_tools_list(tools: &TeamOrchestratorTools, id: u64) -> Vec<Value> {
    let response = handle_mcp(
        tools,
        &json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{}}),
    )
    .unwrap()
    .unwrap();
    let frame = serde_json::to_value(response).unwrap();
    assert_eq!(frame["id"], json!(id));
    frame["result"]["tools"].as_array().unwrap().clone()
}

fn contract_names(tools: &[Value]) -> BTreeSet<String> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn m01_tools_list_wire_exposes_exactly_three_tools_for_all_service_scopes() {
    let cases = [
        ("worker_a", "teamA"),
        ("leader", "teamA"),
        ("worker_a", "teamB"),
    ];
    let mut observed = Vec::new();
    for (index, (agent, team)) in cases.into_iter().enumerate() {
        let root = TempCase::new("m01");
        observed.push(rpc_tools_list(&tools_at(root.path(), agent, team), index as u64 + 1));
    }

    let expected = BTreeSet::from(RETAINED.map(str::to_string));
    for (context, tools) in ["worker/teamA", "leader/teamA", "worker/teamB"]
        .into_iter()
        .zip(observed)
    {
        assert_eq!(
            tools.len(),
            RETAINED.len(),
            "M01 {context}: tools/list must not leak the retired management surface"
        );
        assert_eq!(contract_names(&tools), expected, "M01 {context}: names");
        let send = tools.iter().find(|tool| tool["name"] == "send_message").unwrap();
        assert_eq!(send["description"], json!("Send a message to a teammate, the leader, or '*' for all other team members. mailbox=true stores durably without live injection; the default is live delivery."));
        assert_eq!(send["inputSchema"], json!({
            "type":"object", "required":["to","content"], "additionalProperties":false,
            "properties":{
                "to":{"type":"string","description":"Target agent id, 'leader', or '*' for broadcast."},
                "content":{"type":"string","description":"Message body."},
                "mailbox":{"type":"boolean","description":"Set true to store durably without live injection; omit for default live delivery."}
            }
        }));
        let report = tools.iter().find(|tool| tool["name"] == "report_result").unwrap();
        assert_eq!(report["description"], json!("Report task completion with a durable result envelope. Optional presentation routing controls live leader display, not persistence."));
        assert_eq!(report["inputSchema"], json!({
            "type":"object", "required":[], "additionalProperties":false,
            "properties":{
                "envelope":{"type":"object","description":"Optional full result envelope.","additionalProperties":true},
                "summary":{"type":"string","description":"Short result summary."},
                "status":{"type":"string","description":"Result status."},
                "changes":{"type":"array","description":"Changed files or artifacts.","items":{"type":"object","additionalProperties":true}},
                "tests":{"type":"array","description":"Tests or checks performed.","items":{"type":"object","additionalProperties":true}},
                "risks":{"type":"array","description":"Risks or blockers.","items":{"type":"object","additionalProperties":true}},
                "artifacts":{"type":"array","description":"Artifact references.","items":{"type":"object","additionalProperties":true}},
                "next_actions":{"type":"array","description":"Suggested next actions.","items":{"type":"object","additionalProperties":true}},
                "task_id":{"type":"string","description":"Optional task id override."},
                "agent_id":{"type":"string","description":"Optional reporting agent id; must match framework-injected TEAM_AGENT_ID."},
                "presentation":{
                    "type":"object", "description":"Optional durable presentation routing.",
                    "properties":{
                        "sink":{"type":"string","enum":["leader","casefile","silent"]},
                        "class":{"type":"string","enum":["message","progress","stage_result","stage_pass","bounce","blocking","final_review","timeout"]},
                        "case_id":{"type":"string"}
                    },
                    "required":["sink","class"], "additionalProperties":false
                }
            }
        }));
        let status = tools.iter().find(|tool| tool["name"] == "get_team_status").unwrap();
        assert_eq!(status["description"], json!("Return machine-readable team status."));
        assert_eq!(status["inputSchema"], json!({
            "type":"object", "properties":{}, "required":[], "additionalProperties":false
        }));
    }
}

fn old_tool_args(name: &str, shape: usize) -> Value {
    match (name, shape) {
        ("assign_task", 0) => json!({"task":{"id":"retired-task","assignee":"worker_a","status":"pending"},"message":"legacy"}),
        ("update_state", 0) => json!({"note":"legacy note"}),
        ("stop_agent", 0) => json!({"agent_id":"worker_a"}),
        ("reset_agent", 0) => json!({"agent_id":"worker_a","discard_session":true}),
        ("add_agent", 0) => json!({"new_agent_id":"legacy-added","role_file_path":"agents/legacy.md"}),
        ("clone_agent" | "fork_agent", 0) => json!({"source_agent_id":"worker_a","as_agent_id":"legacy-copy"}),
        ("request_human", 0) => json!({"question":"legacy question","task_id":"task_mcp","agent_id":"worker_a"}),
        ("stuck_list", 0) => json!({}),
        ("stuck_cancel", 0) => json!({"agent_id":"worker_a","alert_type":"stuck"}),
        (_, 1) => json!({}),
        (_, 2) => json!({"agent_id":42,"task":[],"discard_session":"yes"}),
        (_, _) => json!({"agent_id":"forged","team_id":"teamB","owner_team_id":"teamB","envelope":{"agent_id":"forged"}}),
    }
}

fn business_counts(path: &Path) -> (i64, i64) {
    let store = MessageStore::open(path).unwrap();
    let conn = db::schema::open_db(store.db_path()).unwrap();
    let messages = conn.query_row("select count(*) from messages", [], |row| row.get(0)).unwrap();
    let results = conn.query_row("select count(*) from results", [], |row| row.get(0)).unwrap();
    (messages, results)
}

fn assert_unknown_tool_frame(frame: &Value, expected_id: u64) -> bool {
    if frame["id"] != json!(expected_id) || frame["result"]["isError"] != json!(true) {
        return false;
    }
    let text = frame["result"]["content"][0]["text"].as_str().unwrap_or_default();
    let Ok(body) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    body["ok"] == json!(false)
        && body["reason"] == json!("unknown_tool")
        && body["error_code"] == json!("unknown_tool")
        && body["exc_type"] == json!("UnknownTool")
        && body["message"] == body["error"]
}

#[test]
fn m02_all_retired_wire_and_legacy_dispatch_names_fail_closed_without_business_effects() {
    let mut violations = Vec::new();
    let mut id = 1_u64;
    for name in RETIRED {
        for shape in 0..4 {
            let root = TempCase::new("m02");
            team_agent::state::persist::save_runtime_state(root.path(), &json!({
                "active_team_key":"teamA", "session_name":"team-teamA",
                "agents":{"worker_a":{"status":"running","provider":"fake"}},
                "tasks":[{"id":"task_mcp","assignee":"worker_a","status":"pending"}]
            })).unwrap();
            let tools = tools_at(root.path(), "worker_a", "teamA");
            let state_before = team_agent::state::persist::load_runtime_state(root.path()).unwrap();
            let counts_before = business_counts(root.path());
            let args = old_tool_args(name, shape);
            let frame = serde_json::to_value(
                handle_mcp(
                    &tools,
                    &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}),
                )
                .unwrap()
                .unwrap(),
            )
            .unwrap();
            if !assert_unknown_tool_frame(&frame, id) {
                violations.push(format!("tools/call {name} variant {shape} was not the typed unknown_tool envelope: {frame}"));
            }
            let legacy = team_agent::mcp_server::dispatch(
                &tools,
                &json!({"tool":name,"arguments":old_tool_args(name, 0)}),
            );
            if !matches!(legacy, Err(ref error) if error.reason == ToolErrorReason::UnknownTool) {
                violations.push(format!("legacy dispatch still routes {name}: {legacy:?}"));
            }
            let state_after = team_agent::state::persist::load_runtime_state(root.path()).unwrap();
            let counts_after = business_counts(root.path());
            if state_after != state_before || counts_after != counts_before {
                violations.push(format!("{name} variant {shape} had business side effects: state_changed={} messages/results={counts_before:?}->{counts_after:?}", state_after != state_before));
            }
            id += 1;
        }
    }
    assert!(violations.is_empty(), "M02 retired names must be fail-closed before any handler: {}", violations.join("\n"));
}

fn include_tools_from_wrapper(path: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("M03 expected a materialized Pi team_orchestrator wrapper at {}: {error}", path.display())
    });
    let marker = "\"includeTools\"";
    let start = source.find(marker).expect("Pi MCP wrapper contains includeTools");
    let array_start = source[start..].find('[').unwrap() + start;
    let mut depth = 0_i32;
    let mut array_end = None;
    for (offset, byte) in source.as_bytes()[array_start..].iter().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    array_end = Some(array_start + offset + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = array_end.expect("includeTools JSON array closes");
    serde_json::from_str::<Vec<String>>(&source[array_start..end]).unwrap()
}

fn write_fake_pi(root: &Path) -> (PathBuf, String) {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let bin = root.join("pi-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let package = root.join("pi-adapter");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), r#"{"name":"pi-mcp-adapter","version":"2.30.0","pi":{"extensions":["./index.ts"]}}"#).unwrap();
    std::fs::write(package.join("index.ts"), "export default function(pi: any) {}\n").unwrap();
    let real_pi = bin.join("real-pi");
    std::fs::write(
        &real_pi,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 0.84.4 ;;\n  --list-models) printf 'provider model\\nteam-agent qwen3.8-27b\\n' ;;\n  list) printf 'npm:pi-mcp-adapter\\n{}\\n' ;;\n  *) exec cat ;;\nesac\n",
            package.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&real_pi).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&real_pi, permissions).unwrap();
    symlink(&real_pi, bin.join("pi")).unwrap();
    let mut path = bin.to_string_lossy().into_owned();
    if let Some(old) = std::env::var_os("PATH") {
        path.push(':');
        path.push_str(&old.to_string_lossy());
    }
    (bin, path)
}

struct OwnedTmuxBackend(TmuxBackend);
impl Drop for OwnedTmuxBackend {
    fn drop(&mut self) { let _ = self.0.kill_server(); }
}

struct PathGuard(Option<std::ffi::OsString>);
impl PathGuard {
    fn install(value: &str) -> Self {
        let old = std::env::var_os("PATH");
        std::env::set_var("PATH", value);
        Self(old)
    }
}
impl Drop for PathGuard {
    fn drop(&mut self) {
        if let Some(old) = &self.0 { std::env::set_var("PATH", old); }
        else { std::env::remove_var("PATH"); }
    }
}

fn assert_pi_wrapper_identity(path: &Path, agent_id: &str, team_id: &str) {
    let source = std::fs::read_to_string(path).unwrap();
    assert!(source.contains("\"TEAM_AGENT_ID\""), "wrapper must retain framework-owned agent identity");
    assert!(source.contains(&format!("\"TEAM_AGENT_ID\": \"{agent_id}\"")), "wrong wrapper agent identity: {}", path.display());
    assert!(source.contains(&format!("\"TEAM_AGENT_OWNER_TEAM_ID\": \"{team_id}\"")), "wrong wrapper owner team: {}", path.display());
    assert!(source.contains("\"command\": \"/"), "candidate MCP command must stay absolutely pinned: {}", path.display());
    assert!(source.contains("name: \"team_orchestrator\""), "runtime wrapper lost the team_orchestrator server entry");
}

fn write_pi_runtime_spec(root: &Path, team_id: &str) -> PathBuf {
    let spec = root.join(".team").join("runtime").join(team_id).join("team.spec.yaml");
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(root.join("TEAM.md"), format!(r#"---
name: {team_id}
objective: M03 Pi MCP tool registration.
provider: pi
---

M03 team.
"#)).unwrap();
    std::fs::write(root.join("agents/worker_a.md"), r#"---
name: worker_a
role: Worker
provider: pi
model: team-agent/qwen3.8-27b
auth_mode: subscription
dangerously_skip_permissions: true
tools:
  - mcp_team
---

M03 worker.
"#).unwrap();
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    let text = format!(r#"version: 1
team:
  name: "{team_id}"
  mode: "supervisor_worker"
  objective: "M03 Pi MCP tool registration"
  workspace: "{}"
leader:
  id: "leader"
  role: "Leader"
  provider: "pi"
  model: "team-agent/qwen3.8-27b"
  effort: "max"
  tools: ["mcp_team"]
agents:
  - id: "worker_a"
    role: "Worker"
    provider: "pi"
    model: "team-agent/qwen3.8-27b"
    effort: "max"
    auth_mode: "subscription"
    dangerously_skip_permissions: true
    working_directory: "{}"
    system_prompt:
      inline: "A test worker."
      file: null
    tools: ["mcp_team"]
    permission_mode: "restricted"
routing:
  default_assignee: "worker_a"
  rules: []
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-{team_id}"
  auto_launch: true
  startup_order: ["worker_a"]
  dangerously_skip_permissions: false
tasks: []
"#, root.display(), root.display());
    std::fs::write(&spec, &text).unwrap();
    std::fs::write(root.join("team.spec.yaml"), text).unwrap();
    spec
}

#[test]
#[serial_test::serial(env)]
fn m03_initial_resume_and_pi_leader_materialization_all_consume_three_tools() {
    let case = TempCase::new("m03");
    let (_bin, path) = write_fake_pi(case.path());
    let _path_guard = PathGuard::install(&path);
    let spec = write_pi_runtime_spec(case.path(), "teamA");
    let backend = OwnedTmuxBackend(TmuxBackend::for_workspace(case.path()));
    team_agent::lifecycle::launch_with_transport_in_workspace(
        case.path(), &spec, false, false, true, &backend.0,
    ).expect("initial Pi worker launch must materialize its MCP registration");
    let worker_path = case.path().join(".team/runtime/pi/teamA/worker_a/team-mcp.ts");
    let initial = include_tools_from_wrapper(&worker_path);
    assert_pi_wrapper_identity(&worker_path, "worker_a", "teamA");
    let _ = team_agent::lifecycle::restart::restart(case.path(), true, Some("teamA"))
        .expect("restart/resume Pi worker must rematerialize its MCP registration");
    let resumed = include_tools_from_wrapper(&worker_path);
    assert_pi_wrapper_identity(&worker_path, "worker_a", "teamA");

    // Execute the real leader dispatcher; this writes the per-leader Pi wrapper before
    // starting the provider and exercises the same start plan consumed by the CLI.
    let _ = team_agent::cli::emit::run(&["pi".to_string()], case.path());
    let leader_path = case.path().join(".team/runtime/pi/teamA/leader/team-mcp.ts");
    let leader = include_tools_from_wrapper(&leader_path);
    assert_pi_wrapper_identity(&leader_path, "leader", "teamA");
    let expected = BTreeSet::from(RETAINED.map(str::to_string));
    for (consumer, tools) in [("initial worker", initial), ("restart/resume worker", resumed), ("Pi leader", leader)] {
        assert_eq!(tools.len(), 3, "M03 {consumer} includeTools: {tools:?}");
        assert_eq!(tools.into_iter().collect::<BTreeSet<_>>(), expected, "M03 {consumer} exact tools");
    }
}

#[test]
fn m04_send_message_single_leader_broadcast_and_mailbox_close_the_team_scope_loop() {
    let harness = sim::McpSimHarness::new();
    let mut worker = sim::spawn_mcp_client_without_catalog_check(harness.workspace_path(), "worker_b", "teamA");
    for (target, marker) in [
        ("worker_a", "M04_PEER"),
        ("leader", "M04_LEADER"),
        ("*", "M04_BROADCAST"),
    ] {
        let call = worker.call_tool("send_message", json!({"to":target,"content":marker}));
        assert!(!call.is_error, "send {target}: {}", call.body);
        if target == "worker_a" {
            assert_eq!(call.body["status"], json!("accepted"));
            assert_eq!(call.body["delivery_pending"], json!(true));
            let message_id = call.body["message_id"].as_str().expect("accepted async peer send returns real message id");
            assert_eq!(call.body["poll_via"], json!(format!("team-agent inbox {message_id}")));
        }
    }
    let mailbox = worker.call_tool("send_message", json!({"to":"worker_a","content":"M04_MAILBOX","mailbox":true}));
    assert!(!mailbox.is_error, "mailbox send: {}", mailbox.body);
    harness.drive_delivery_twice();

    for marker in ["M04_PEER", "M04_LEADER", "M04_BROADCAST", "M04_MAILBOX"] {
        let rows = harness.message_rows_containing(marker);
        assert!(!rows.is_empty(), "durable row(s) for {marker}");
        assert!(rows.iter().all(|row| row.owner_team_id.as_deref() == Some("teamA")), "wrong owner scope for {marker}: {rows:?}");
    }
    assert_eq!(harness.message_rows_containing("M04_PEER").len(), 1, "single peer send must create one target row");
    assert_eq!(harness.message_rows_containing("M04_LEADER")[0].recipient, "leader");
    let broadcast = harness.message_rows_containing("M04_BROADCAST");
    let broadcast_recipients = broadcast.iter().map(|row| row.recipient.as_str()).collect::<BTreeSet<_>>();
    assert_eq!(broadcast_recipients, BTreeSet::from(["leader", "worker_a", "worker_c"]));
    assert!(broadcast.iter().all(|row| row.recipient != "worker_x"), "broadcast escaped teamA: {broadcast:?}");
    assert!(harness.pane_text("leader").contains("M04_LEADER"), "direct leader message was not delivered");
    assert!(!harness.pane_text("worker_a").contains("M04_MAILBOX"), "mailbox=true must remain stored-only");
    let mailbox_row = &harness.message_rows_containing("M04_MAILBOX")[0];
    assert_ne!(mailbox_row.status, "delivered", "mailbox message must not be injected: {mailbox_row:?}");
}

fn send_cli_input(harness: &sim::McpSimHarness, worker_id: &str, marker: &str) -> sim::MessageRow {
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["send", worker_id, marker, "--workspace"])
        .arg(harness.workspace_path())
        .args(["--team", "teamA", "--json"])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run existing CLI send to create attributable worker input");
    assert!(output.status.success(), "CLI send failed: stdout={} stderr={}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let response: Value = serde_json::from_slice(&output.stdout).expect("CLI send returns JSON");
    assert_eq!(response["ok"], json!(true), "CLI send must persist/deliver input: {response}");
    harness.drive_delivery_twice();
    let rows = harness.message_rows_containing(marker);
    assert_eq!(rows.len(), 1, "one CLI input row for {marker}: {rows:?}");
    assert_eq!(rows[0].owner_team_id.as_deref(), Some("teamA"));
    assert_eq!(rows[0].recipient, worker_id);
    rows[0].clone()
}

#[test]
fn m05_direct_input_and_full_envelope_reports_persist_and_reach_the_leader_without_assignment() {
    let first = sim::McpSimHarness::new();
    let inbound = send_cli_input(&first, "worker_a", "M05_MINIMAL_INPUT");
    let mut worker = sim::spawn_mcp_client_without_catalog_check(first.workspace_path(), "worker_a", "teamA");
    let minimal = worker.call_tool("report_result", json!({"task_id":"task_mcp","summary":"M05_MINIMAL_RESULT"}));
    assert!(!minimal.is_error, "minimal report_result: {}", minimal.body);
    let id = minimal.body["result_id"].as_str().expect("minimal report returns durable result id");
    let row = first.result_row(id).expect("minimal report persists result without assign_task");
    assert_eq!(row.owner_team_id.as_deref(), Some("teamA"));
    assert_eq!(row.agent_id, "worker_a");
    assert_eq!(row.task_id, inbound.message_id, "minimal result must be tied to the current direct-message input");
    assert!(row.envelope.contains("M05_MINIMAL_RESULT"));
    assert!(first.pane_text("leader").contains("M05_MINIMAL_RESULT"), "result delivery must reach attached leader");

    let second = sim::McpSimHarness::new();
    let _envelope_input = send_cli_input(&second, "worker_a", "M05_ENVELOPE_INPUT");
    let mut worker = sim::spawn_mcp_client_without_catalog_check(second.workspace_path(), "worker_a", "teamA");
    let envelope = json!({
        "schema_version":"result_envelope_v1", "task_id":"task_mcp", "agent_id":"worker_a",
        "status":"success", "summary":"M05_FULL_ENVELOPE", "changes":[{"path":"src/lib.rs","kind":"modified"}],
        "tests":[{"command":"cargo test --locked","status":"passed"}], "risks":[], "artifacts":[{"path":"artifact.json"}],
        "next_actions":[{"action":"review","owner":"leader"}],
        "presentation":{"sink":"leader","class":"stage_result","case_id":"m05"}
    });
    let full = worker.call_tool("report_result", json!({"envelope":envelope}));
    assert!(!full.is_error, "full-envelope report_result: {}", full.body);
    let id = full.body["result_id"].as_str().expect("full envelope returns result id");
    let row = second.result_row(id).expect("full envelope report persists");
    let stored: Value = serde_json::from_str(&row.envelope).unwrap();
    assert_eq!(stored["summary"], json!("M05_FULL_ENVELOPE"));
    assert_eq!(stored["tests"][0]["status"], json!("passed"));
    assert_eq!(stored["presentation"]["case_id"], json!("m05"));
    assert_eq!(row.owner_team_id.as_deref(), Some("teamA"));
    assert_eq!(row.agent_id, "worker_a");
    assert_eq!(row.task_id, "task_mcp");
    assert!(second.pane_text("leader").contains("M05_FULL_ENVELOPE"));
}

#[test]
fn m06_get_team_status_is_read_only_and_anchored_to_captured_owner_team() {
    let harness = sim::McpSimHarness::new();
    let mut switched = harness.state_value();
    switched["active_team_key"] = json!("teamB");
    team_agent::state::persist::save_runtime_state(harness.workspace_path(), &switched).unwrap();
    let before_call = harness.state_value();
    let counts_before = business_counts(harness.workspace_path());
    let mut worker = sim::spawn_mcp_client_without_catalog_check(harness.workspace_path(), "worker_a", "teamA");
    let status = worker.call_tool("get_team_status", json!({}));
    assert!(!status.is_error && status.body["ok"] == json!(true), "get_team_status: {}", status.body);
    assert_eq!(status.body["teams"].as_object().unwrap().keys().cloned().collect::<BTreeSet<_>>(), BTreeSet::from(["teamA".to_string()]));
    assert!(status.body.to_string().contains("worker_b"), "teamA roster missing from status: {}", status.body);
    assert!(!status.body.to_string().contains("worker_x"), "teamB roster leaked through status: {}", status.body);
    assert_eq!(harness.state_value(), before_call, "status query must not mutate runtime state");
    assert_eq!(business_counts(harness.workspace_path()), counts_before, "status query must not add tasks/messages/results");
}

#[test]
fn m07_identity_scope_and_foreign_task_guards_run_before_business_effects() {
    let harness = sim::McpSimHarness::new();
    let mut state = harness.state_value();
    state["teams"]["teamB"]["tasks"] = json!([{"id":"teamB_task","assignee":"worker_x","status":"pending"}]);
    team_agent::state::persist::save_runtime_state(harness.workspace_path(), &state).unwrap();
    let mut worker = sim::spawn_mcp_client_without_catalog_check(harness.workspace_path(), "worker_a", "teamA");

    let spoof = worker.call_tool("send_message", json!({"to":"worker_c","content":"M07_SPOOF","agent_id":"forged"}));
    assert!(spoof.is_error, "spoofed sender identity must be rejected: {}", spoof.body);
    assert_ne!(spoof.body["reason"], json!("unknown_tool"));
    let widen = worker.call_tool("send_message", json!({"to":"worker_c","content":"M07_SCOPE","team_id":"teamB"}));
    assert!(widen.is_error, "worker cannot widen the captured team scope: {}", widen.body);
    assert_ne!(widen.body["reason"], json!("unknown_tool"));
    let cross_peer = worker.call_tool("send_message", json!({"to":"worker_x","content":"M07_CROSS_TEAM"}));
    assert!(cross_peer.is_error, "cross-team peer must be refused: {}", cross_peer.body);
    assert_ne!(cross_peer.body["reason"], json!("unknown_tool"));
    let forged_report = worker.call_tool("report_result", json!({"agent_id":"forged","task_id":"task_mcp","summary":"M07_FORGED_REPORT"}));
    assert!(forged_report.is_error, "forged report identity must be rejected: {}", forged_report.body);
    assert_ne!(forged_report.body["reason"], json!("unknown_tool"));
    let forged_envelope = worker.call_tool("report_result", json!({"envelope":{"schema_version":"result_envelope_v1","task_id":"task_mcp","agent_id":"forged","status":"success","summary":"M07_FORGED_ENVELOPE"}}));
    assert!(forged_envelope.is_error, "forged envelope identity must be rejected: {}", forged_envelope.body);
    assert_ne!(forged_envelope.body["reason"], json!("unknown_tool"));
    let report_scope = worker.call_tool("report_result", json!({"envelope":{"schema_version":"result_envelope_v1","task_id":"task_mcp","agent_id":"worker_a","owner_team_id":"teamB","status":"success","summary":"M07_REPORT_SCOPE"}}));
    assert!(report_scope.is_error, "report envelope cannot expand owner-team scope: {}", report_scope.body);
    assert_ne!(report_scope.body["reason"], json!("unknown_tool"));
    let status_scope = worker.call_tool("get_team_status", json!({"team_id":"teamB"}));
    assert!(status_scope.is_error, "status cannot widen scope from captured owner team: {}", status_scope.body);
    assert_ne!(status_scope.body["reason"], json!("unknown_tool"));
    let foreign_task = worker.call_tool("report_result", json!({"agent_id":"worker_a","task_id":"teamB_task","summary":"M07_FOREIGN_TASK"}));
    assert_ne!(foreign_task.body["reason"], json!("unknown_tool"), "retained report_result must reach task-ownership validation");
    for marker in ["M07_SPOOF", "M07_SCOPE", "M07_CROSS_TEAM", "M07_FORGED_REPORT", "M07_FORGED_ENVELOPE", "M07_REPORT_SCOPE"] {
        let rows = harness.message_rows_containing(marker);
        assert!(rows.is_empty(), "identity/scope rejection must precede durable side effects for {marker}: {rows:?}");
    }
    let final_state = harness.state_value();
    assert_eq!(final_state["teams"]["teamB"]["tasks"][0]["status"], json!("pending"), "cross-team report must not complete another team's task");

    let no_identity = TempCase::new("m07-no-identity");
    let tools = TeamOrchestratorTools::with_identity(no_identity.path(), None, Some(TeamKey::new("teamA")));
    let response = handle_mcp(&tools, &json!({"jsonrpc":"2.0","id":77,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"leader","content":"M07_NO_ID"}}})).unwrap().unwrap();
    let frame = serde_json::to_value(response).unwrap();
    assert_eq!(frame["result"]["isError"], json!(true));
    let detail: Value = serde_json::from_str(frame["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_ne!(detail["reason"], json!("unknown_tool"));
}

#[test]
fn m08_native_cli_lifecycle_commands_still_dispatch_and_preserve_source_and_scope() {
    use cli_fixture::{quick_start_fake, run_ta, state_agent, state_has_agent, TestWorkspace};

    let team = "mcp3-cli";
    let workspace = TestWorkspace::new(team).with_fake_spec(&["a", "b"]);
    let launched = quick_start_fake(&workspace, team);
    assert!(cli_fixture::quick_start_workers_available(&launched), "fake CLI workers did not start: {}", launched.stdout);
    let ws = workspace.path().to_str().unwrap();

    let stop = run_ta(&workspace, &["stop-agent", "a", "--workspace", ws, "--json"]);
    assert!(stop.is_success(), "stop-agent must remain a real CLI action: {} {}", stop.stdout, stop.stderr);
    assert!(state_agent(&workspace.read_state(), "a")["status"] != json!("running"));
    assert_eq!(state_agent(&workspace.read_state(), "b")["status"], json!("running"), "stop must stay scoped to its target");

    let start = run_ta(&workspace, &["start-agent", "a", "--workspace", ws, "--allow-fresh", "--no-display", "--json"]);
    assert!(start.is_success() && start.json()["ok"] == json!(true), "start-agent dispatch/lifecycle regression: {} {}", start.stdout, start.stderr);
    assert_eq!(state_agent(&workspace.read_state(), "a")["status"], json!("running"));

    workspace.mutate_agent_everywhere("a", |agent| {
        agent.insert("session_id".to_string(), json!("mcp3-reset-session"));
        agent.insert("rollout_path".to_string(), json!("/missing/mcp3-reset.jsonl"));
        agent.insert("captured_at".to_string(), json!("2026-01-01T00:00:00Z"));
        agent.insert("captured_via".to_string(), json!("mcp3-fixture"));
        agent.insert("attribution_confidence".to_string(), json!("high"));
    });
    let reset = run_ta(&workspace, &["reset-agent", "a", "--workspace", ws, "--discard-session", "--no-display", "--json"]);
    assert!(reset.is_success() && reset.json()["status"] == json!("reset"), "reset-agent core contract: {} {}", reset.stdout, reset.stderr);
    assert_eq!(state_agent(&workspace.read_state(), "a")["status"], json!("running"));

    let role = workspace.path().join("roles").join("added.md");
    std::fs::create_dir_all(role.parent().unwrap()).unwrap();
    std::fs::write(&role, "---\nname: added\nrole: Added fake worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools: [mcp_team]\n---\nAdded.\n").unwrap();
    let add = run_ta(&workspace, &["add-agent", "added", "--role-file", role.to_str().unwrap(), "--workspace", ws, "--no-display", "--json"]);
    assert!(add.is_success() && state_has_agent(&workspace.read_state(), "added"), "add-agent must still add a scoped runtime seat: {} {}", add.stdout, add.stderr);

    let source_before = state_agent(&workspace.read_state(), "a").clone();
    for command in ["clone-agent", "fork-agent"] {
        let target = format!("{command}-target");
        let attempt = run_ta(&workspace, &[command, "a", "--as", &target, "--workspace", ws, "--no-display", "--json"]);
        assert!(!attempt.stdout.contains("unknown subcommand") && !attempt.stderr.contains("unknown subcommand"), "{command} was removed from native CLI: {} {}", attempt.stdout, attempt.stderr);
        let state = workspace.read_state();
        assert_eq!(state_agent(&state, "a")["pane_id"], source_before["pane_id"], "{command} refusal/success must not replace source session");
        if !state_has_agent(&state, &target) {
            assert!(!attempt.json().get("ok").and_then(Value::as_bool).unwrap_or(true), "failed {command} must not report success without target state: {}", attempt.stdout);
        }
    }
    assert!(state_agent(&workspace.read_state(), "b")["status"] == json!("running"), "other seat must remain intact");
    let _ = run_ta(&workspace, &["shutdown", "--workspace", ws, "--keep-logs", "--json"]);
}

#[test]
fn m09_due_scheduler_idle_suppression_and_retained_rpc_stream_remain_live() {
    let case = TempCase::new("m09-scheduler");
    let store = MessageStore::open(case.path()).unwrap();
    let conn = db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into scheduled_events(owner_team_id,due_at,target,kind,payload_json,status,created_at) values(null,'2000-01-01T00:00:00+00:00','leader','health_ping','{}','pending','2000-01-01T00:00:00+00:00')",
        [],
    ).unwrap();
    let backend = TmuxBackend::for_workspace(case.path());
    let fired = messaging::fire_due_scheduled_events(case.path(), &store, &backend, &EventLog::new(case.path())).unwrap();
    assert_eq!(fired.len(), 1, "one due shared-engine event must be consumed");
    let row: (String, String) = conn.query_row("select status,result_json from scheduled_events", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(row.0, "done");
    assert!(row.1.contains("logged"));

    let idle = TempCase::new("m09-idle");
    let store = MessageStore::open(idle.path()).unwrap();
    let state = json!({"active_team_key":"idleTeam","session_name":"team-idleTeam","agents":{"worker_a":{"status":"running"}},"tasks":[]});
    team_agent::state::persist::save_runtime_state(idle.path(), &state).unwrap();
    store.create_message(Some("idle-task"), "leader", "worker_a", "idle obligation", None, false, Some("idleTeam")).unwrap();
    let conn = db::schema::open_db(store.db_path()).unwrap();
    conn.execute("insert into agent_health(owner_team_id,agent_id,status,updated_at) values('idleTeam','worker_a','IDLE','now')", []).unwrap();
    let event_log = EventLog::new(idle.path());
    let first = messaging::detect_idle_fallbacks(idle.path(), &state, &store, &event_log).unwrap();
    assert_eq!(first.len(), 1, "eligible idle fallback should fire once");
    let saved = team_agent::state::persist::load_runtime_state(idle.path()).unwrap();
    let second = messaging::detect_idle_fallbacks(idle.path(), &saved, &store, &event_log).unwrap();
    assert!(second.is_empty(), "matching suppression snapshot must prevent repeated idle fallback");

    let rpc_root = TempCase::new("m09-rpc");
    let tools = tools_at(rpc_root.path(), "worker_a", "teamA");
    let initialized = handle_mcp(&tools, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}})).unwrap().unwrap();
    assert_eq!(initialized.result.unwrap()["serverInfo"]["name"], json!("team_orchestrator"));
    assert!(handle_mcp(&tools, &json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})).unwrap().is_none());
    let stream = sim::McpSimHarness::new();
    let mut client = sim::spawn_mcp_client_without_catalog_check(stream.workspace_path(), "worker_a", "teamA");
    assert!(!client.call_tool("get_team_status", json!({})).is_error);
    let send = client.call_tool("send_message", json!({"to":"leader","content":"M09_STREAM_CONTINUES"}));
    assert!(!send.is_error && send.body["ok"] == json!(true), "retained tool call after status must share a healthy stdio stream: {}", send.body);
    stream.drive_delivery_twice();
    assert!(stream.pane_text("leader").contains("M09_STREAM_CONTINUES"));
}

#[test]
fn m10_retired_mcp_facades_helpers_intents_and_agent_ops_are_physically_absent() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let read = |relative: &str| std::fs::read_to_string(src.join(relative)).unwrap_or_default();
    let forbidden = [
        ("mcp_server/wire.rs", "McpTool::AssignTask"),
        ("mcp_server/wire.rs", "McpTool::UpdateState"),
        ("mcp_server/wire.rs", "McpTool::StopAgent"),
        ("mcp_server/wire.rs", "McpTool::ResetAgent"),
        ("mcp_server/wire.rs", "McpTool::AddAgent"),
        ("mcp_server/wire.rs", "McpTool::CloneAgent"),
        ("mcp_server/wire.rs", "McpTool::ForkAgent"),
        ("mcp_server/wire.rs", "McpTool::RequestHuman"),
        ("mcp_server/wire.rs", "McpTool::StuckList"),
        ("mcp_server/wire.rs", "McpTool::StuckCancel"),
        ("mcp_server/wire.rs", "task_property("),
        ("mcp_server/helpers.rs", "merge_tasks_by_id"),
        ("mcp_server/tools.rs", "pub fn assign_task("),
        ("mcp_server/tools.rs", "pub fn update_state("),
        ("mcp_server/tools.rs", "pub fn stop_agent("),
        ("mcp_server/tools.rs", "pub fn reset_agent("),
        ("mcp_server/tools.rs", "pub fn add_agent("),
        ("mcp_server/tools.rs", "pub fn clone_agent("),
        ("mcp_server/tools.rs", "pub fn fork_agent("),
        ("mcp_server/tools.rs", "pub fn request_human("),
        ("mcp_server/tools.rs", "pub fn stuck_list("),
        ("mcp_server/tools.rs", "pub fn stuck_cancel("),
        ("mcp_server/lifecycle_tools/state_status.rs", "pub(crate) fn update_state("),
        ("messaging/scheduler.rs", "pub fn stuck_list("),
        ("messaging/scheduler.rs", "pub fn stuck_cancel("),
        ("messaging/scheduler.rs", "struct SuppressionRecord"),
        ("messaging/mod.rs", "pub use scheduler::{detect_stuck_agents, fire_due_scheduled_events, stuck_cancel, stuck_list}"),
        ("messaging/types.rs", "enum AlertType"),
        ("state/repository.rs", "McpAssignTask"),
        ("state/repository.rs", "McpUpdateStateNote"),
        ("state/repository.rs", "McpLifecycleAgentOps"),
        ("state/repository.rs", "SchedulerSuppression"),
    ];
    let mut violations = Vec::new();
    for (file, symbol) in forbidden {
        let source = read(file);
        if source.contains(symbol) {
            violations.push(format!("{file} still contains {symbol}"));
        }
    }
    let types = read("mcp_server/types.rs");
    let enum_start = types.find("pub enum McpTool {").expect("McpTool enum remains for three retained names");
    let enum_end = types[enum_start..].find('}').map(|offset| enum_start + offset).unwrap();
    let variants = &types[enum_start..enum_end];
    for variant in ["AssignTask", "UpdateState", "StopAgent", "ResetAgent", "AddAgent", "CloneAgent", "ForkAgent", "RequestHuman", "StuckList", "StuckCancel"] {
        if variants.contains(variant) { violations.push(format!("McpTool enum still declares {variant}")); }
    }
    assert!(!src.join("mcp_server/lifecycle_tools/agent_ops.rs").exists(), "M10 exclusive agent_ops.rs must be deleted");
    let module = read("mcp_server/lifecycle_tools/mod.rs");
    if module.contains("mod agent_ops") || module.contains("pub(crate) use agent_ops") || module.contains("pub(crate) use state_status::{get_team_status, update_state}") {
        violations.push("lifecycle_tools/mod.rs still declares/re-exports excised MCP handlers".to_string());
    }
    assert!(violations.is_empty(), "M10 physical excision audit failed: {}", violations.join("; "));
}

#[test]
fn retained_wire_tool_enum_has_only_the_public_three_tool_contract() {
    // Independent enum-to-wire check prevents a hidden parser alias from keeping a
    // retired tool reachable after tools/list is reduced.
    let parsed = RETAINED.iter().filter(|name| McpTool::parse(name).is_some()).count();
    assert_eq!(parsed, RETAINED.len());
    for name in RETIRED {
        assert!(McpTool::parse(name).is_none(), "retired enum parser alias remains: {name}");
    }
}
