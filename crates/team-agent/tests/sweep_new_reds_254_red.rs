//! #254 sweep RED contracts.
//!
//! User-facing contract: a selected runtime team is the only scope for CLI/MCP
//! diagnostics, repair, readiness, add-agent, worker routing, and lifecycle
//! validation. Raw top-level state and per-call worker scope overrides must not
//! reappear as hidden truth sources.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
mod mcp_sim_harness;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_sim_harness::McpSimHarness;
use rusqlite::params;
use serde_json::{json, Value};
use team_agent::cli::{
    cmd_diagnose, cmd_doctor, cmd_init, cmd_preflight, cmd_repair_state, cmd_settle, cmd_validate,
    cmd_wait_ready, CmdOutput, DiagnoseArgs, DoctorArgs, ExitCode, InitArgs, PreflightArgs,
    RepairStateArgs, SettleArgs, ValidateArgs, WaitReadyArgs,
};
use team_agent::message_store::MessageStore;
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

#[test]
fn diagnose_live_fake_team_uses_active_team_projection() {
    let root = tmp_dir("diagnose-projection");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, true);

    let out = json_result(
        cmd_diagnose(&DiagnoseArgs {
            workspace: root.clone(),
            json: true,
            team: None,
        })
        .expect("diagnose should return JSON"),
    );
    let runtime = &out["runtime"];
    assert_eq!(
        runtime["team_key"],
        json!("teamA"),
        "diagnose --workspace <runtime> must report the active selected team key, not raw top-level state; out={out}"
    );
    assert_eq!(
        runtime["session_name"],
        json!("team-teamA"),
        "diagnose must use teams[active].session_name; out={out}"
    );
    assert!(
        runtime["agent_count"].as_u64().unwrap_or(0) >= 1,
        "diagnose must count agents from the selected team projection; out={out}"
    );
    assert!(
        !out["issues"]
            .as_array()
            .is_some_and(|issues| issues.iter().any(|issue| issue.as_str() == Some("leader_not_attached"))),
        "selected team has an attached leader_receiver; raw top-level leader_not_attached is a false positive; out={out}"
    );
}

#[test]
fn repair_state_runtime_workspace_uses_selected_spec_path() {
    let root = tmp_dir("repair-selected-spec");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, true);

    let result = cmd_repair_state(&RepairStateArgs {
        workspace: root.clone(),
        task_id: "task_1".to_string(),
        assignee: Some("worker_a".to_string()),
        status: "done".to_string(),
        summary: Some("patched by repair".to_string()),
        json: true,
        team: None,
    });
    assert!(
        result.is_ok(),
        "repair-state --workspace <runtime> must select teams[active].spec_path and write the selected team_state.md; result={result:?}"
    );
    let out = json_result(result.unwrap());
    assert_eq!(out["ok"], json!(true), "repair-state JSON must be ok=true; out={out}");
    let state = load_runtime_state(&root).expect("state after repair");
    assert_eq!(
        state["teams"]["teamA"]["tasks"][0]["status"],
        json!("done"),
        "repair-state must update the selected team task projection, not raw top-level tasks; state={state}"
    );
    let state_file = out["state_file"].as_str().unwrap_or_default();
    assert!(
        state_file.contains("/teamA/") && Path::new(state_file).exists(),
        "repair-state must write through the selected spec/team dir path; out={out}"
    );
}

#[test]
fn settle_runtime_workspace_collects_selected_team() {
    let root = tmp_dir("settle-selected-team");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, true);
    insert_result(&root, "res_teamA", "task_1", "worker_a", "teamA", "success");

    let out = json_result(
        cmd_settle(&SettleArgs {
        team: None,
            workspace: root.clone(),
            json: true,
        })
        .expect("settle should return JSON"),
    );
    assert_eq!(out["ok"], json!(true), "settle must succeed for a selected runtime team; out={out}");
    let collected = out["collect"]["collected"].as_array().cloned().unwrap_or_default();
    assert!(
        collected.iter().any(|row| row["result_id"] == json!("res_teamA")),
        "settle --workspace <runtime> must collect results scoped to active_team_key; out={out}"
    );
    assert!(
        out["details_log"].as_str().is_some_and(|path| path.contains(".team/logs/settle-")),
        "settle details log belongs under the runtime workspace, while content is selected-team scoped; out={out}"
    );
}

#[test]
fn wait_ready_fake_live_worker_with_mcp_config_and_window_is_ready() {
    let root = tmp_dir("wait-ready-selected");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, true);
    let mcp_config = root.join(".team").join("runtime").join("worker_a.mcp.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    let mut state = load_runtime_state(&root).unwrap();
    state["teams"]["teamA"]["agents"]["worker_a"]["mcp_config"] =
        json!(mcp_config.to_string_lossy().to_string());
    state["teams"]["teamA"]["agents"]["worker_a"]["task_prompt_delivered"] = json!(false);
    save_runtime_state(&root, &state).unwrap();

    let out = json_result(
        cmd_wait_ready(&WaitReadyArgs {
            workspace: root.clone(),
            timeout: 0.01,
            json: true,
            team: None,
        })
        .expect("wait-ready should return JSON"),
    );
    assert_eq!(
        out["ok"],
        json!(true),
        "wait-ready must treat selected-team fake running worker + MCP config as startup ready; task prompt delivery is a separate signal. out={out}"
    );
    assert_eq!(out["readiness"]["process_started"], json!(true), "out={out}");
    assert_eq!(out["readiness"]["mcp_ready"], json!(true), "out={out}");
}

#[test]
fn cli_add_agent_without_explicit_team_persists_to_active_team() {
    let launch = source("src/lifecycle/launch.rs");
    let add_agent = source_section(&launch, "pub fn add_agent(", "/// `add_agent` with an injected transport");
    let at_paths = source_section(&launch, "fn add_agent_with_transport_at_paths", "fn materialize_added_role_file");
    assert!(
        add_agent.contains("resolve_active_team")
            && (add_agent.contains("Some(selected.team_key.as_str())")
                || add_agent.contains("Some(&selected.team_key")
                || add_agent.contains("selected.team_key.as_str()")),
        "CLI add-agent without explicit --team must pass the resolved active selected.team_key into the internal lifecycle path, not the raw user `team` Option. source={add_agent}"
    );
    assert!(
        !at_paths.contains("if team.is_some()")
            && at_paths.contains("save_launched_team_state_for_key")
            && at_paths.contains("start_agent_at_paths"),
        "add-agent internals must always use the resolved canonical team key for upsert and start-agent, so teams[active].agents is the source start/send reads. source={at_paths}"
    );
}

#[test]
fn mcp_add_agent_is_not_placeholder() {
    let mcp_tools = source("src/mcp_server/tools.rs");
    let mcp_mod = source("src/mcp_server/mod.rs");
    let add_section = source_section(&mcp_tools, "pub fn add_agent", "/// `fork_agent`");
    assert!(
        !add_section.contains("lifecycle_placeholder::add_agent"),
        "MCP add_agent must call the real lifecycle add-agent path under spawn-time owner scope, not the placeholder. source={add_section}"
    );
    assert!(
        !mcp_mod.contains(r#"{"ok": true, "status": "added""#),
        "placeholder add_agent returning ok/status without state/spec/spawn side effects must be removed. source excerpt={}",
        source_section(&mcp_mod, "pub mod lifecycle_placeholder", "/// `runtime.fork_agent")
    );
}

#[test]
fn quick_start_grandchild_depth_limit_refuses_before_state_or_spawn() {
    let launch = source("src/lifecycle/launch.rs");
    let quick_start_section = source_section(
        &launch,
        "pub fn quick_start_with_transport_in_workspace",
        "/// BUG-7 helper",
    );
    assert!(
        quick_start_section.contains("team_depth")
            && quick_start_section.contains("parent_team_key")
            && quick_start_section.contains("> 2")
            && quick_start_section.find("team_depth").unwrap_or(usize::MAX)
                < quick_start_section.find("write_spec_atomic(&spec_path").unwrap_or(0),
        "quick-start must enforce nesting depth before writing spec/state or spawning; current quick-start path lacks the F11.4 depth guard. source={quick_start_section}"
    );
}

#[test]
fn doctor_bogus_workspace_exits_nonzero_json_ok_false() {
    let bogus = tmp_dir("doctor-bogus").join("missing-workspace");
    let out = cmd_doctor(&DoctorArgs {
        spec: None,
        workspace: bogus.clone(),
        gate: None,
        comms: false,
        team: None,
        fix: false,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
        json: true,
    })
    .expect("doctor returns a JSON result for bogus workspace");
    assert_eq!(
        out.exit,
        ExitCode::Error,
        "doctor --json on a bogus workspace must be nonzero; result={out:?}"
    );
    let body = json_result(out);
    assert_eq!(
        body["ok"],
        json!(false),
        "doctor must not fabricate ok=true for a workspace with no spec/runtime context; body={body}"
    );
}

#[test]
fn preflight_fake_team_without_profiles_passes_profile_not_required() {
    let root = tmp_dir("preflight-fake");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);

    let out = json_result(
        cmd_preflight(&PreflightArgs {
            team: team.clone(),
            json: true,
        })
        .expect("preflight should return JSON"),
    );
    assert_eq!(
        out["ok"],
        json!(true),
        "fake teams do not require profiles; missing profile dirs are informational, not blockers. out={out}"
    );
    assert!(
        !out["blockers"]
            .as_array()
            .is_some_and(|blockers| blockers.iter().any(|b| b["name"] == json!("profile_dir"))),
        "profile_dir must not be a blocker for a fake team with no profiles; out={out}"
    );
}

#[test]
fn peek_help_and_parser_accept_head_search() {
    let emit = source("src/cli/emit.rs");
    let types = source("src/cli/types.rs");
    let adapters = source("src/cli/adapters.rs");
    let help = source_section(&emit, "Some(\"peek\")", "Some(\"coordinator\")");
    let parsed = source_section(&emit, "struct ParsedArgs", "fn parse_args");
    let parser = source_section(&emit, "fn parse_args", "fn next_arg");
    let peek_args = source_section(&emit, "fn peek_args", "fn run_coordinator");
    let peek_type = source_section(&types, "pub struct PeekArgs", "/// `e2e`");
    let handler = source_section(&adapters, "pub fn cmd_peek", "fn peek_unavailable");
    let failures = [
        (!help.contains("--head") || !help.contains("--search"), "help must list --head and --search"),
        (!parsed.contains("head: Option<usize>") || !parsed.contains("search: Option<String>"), "ParsedArgs must carry head/search"),
        (!parser.contains("\"--head\"") || !parser.contains("\"--search\""), "parse_args must accept --head/--search"),
        (!peek_args.contains("head:") || !peek_args.contains("search:"), "peek_args must transfer head/search"),
        (!peek_type.contains("pub head: Option<usize>") || !peek_type.contains("pub search: Option<String>"), "PeekArgs must expose head/search"),
        (!handler.contains("CaptureRange::Head") || !handler.contains("search"), "cmd_peek must dispatch head/search behavior"),
    ]
    .into_iter()
    .filter_map(|(bad, msg)| bad.then_some(msg))
    .collect::<Vec<_>>();
    assert!(failures.is_empty(), "peek CLI parity is incomplete: {}\nhelp={help}\nPeekArgs={peek_type}\nhandler={handler}", failures.join("; "));
}

#[test]
fn peek_head_and_search_have_public_handler_contracts() {
    let transport = source("src/transport.rs");
    let adapters = source("src/cli/adapters.rs");
    assert!(
        transport.contains("Head(u32)") || transport.contains("Head(usize)"),
        "transport CaptureRange must expose a head capture variant so peek --head is not faked by tail/full; transport={}",
        source_section(&transport, "pub enum CaptureRange", "}")
    );
    let handler = source_section(&adapters, "pub fn cmd_peek", "fn peek_unavailable");
    assert!(
        (handler.contains("\"matches\"") || handler.contains("\"matching_lines\""))
            && handler.contains("search")
            && handler.contains("allow_raw_screen"),
        "peek --search --json should return matching lines/metadata via a search-specific branch; handler={handler}"
    );
}

#[test]
fn attach_leader_help_lists_pane_provider_and_dispatches_handler() {
    let emit = source("src/cli/emit.rs");
    let types = source("src/cli/types.rs");
    let cli_mod = source("src/cli/mod.rs");
    let help = source_section(&emit, "Some(\"attach-leader\")", "Some(\"identity\")");
    let dispatch = source_section(&emit, "fn dispatch", "const DISPATCH_COMMANDS");
    let parser = source_section(&emit, "fn parse_args", "fn next_arg");
    assert!(
        help.contains("--pane") && help.contains("--provider"),
        "attach-leader help must list explicit --pane and --provider; help={help}"
    );
    assert!(
        !emit.contains("SPEC_ONLY_HELP_COMMANDS: &[&str] = &[\"start\", \"purge-agent\", \"attach-leader\"]")
            && dispatch.contains("\"attach-leader\"")
            && dispatch.contains("cmd_attach_leader"),
        "attach-leader must be a real dispatch arm, not help-only; dispatch={dispatch}"
    );
    assert!(
        parser.contains("\"--pane\"") && parser.contains("\"--provider\""),
        "attach-leader parser must accept --pane and --provider; parser={parser}"
    );
    assert!(
        types.contains("pub struct AttachLeaderArgs") && cli_mod.contains("pub fn cmd_attach_leader"),
        "attach-leader must have typed args and a public handler that writes leader_receiver; types/cli missing"
    );
}

#[test]
fn init_workspace_output_is_usable_by_validate_or_explicitly_refuses_runtime_workspace() {
    let root = tmp_dir("init-usable");
    let init = cmd_init(&InitArgs {
        workspace: root.clone(),
        force: false,
        json: true,
    })
    .expect("init returns JSON");
    let body = json_result(init);
    if body["ok"] == json!(false) {
        let reason = body["reason"].as_str().unwrap_or_default();
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            reason.contains("runtime_workspace") || error.contains("runtime workspace"),
            "if init refuses this workspace shape it must say so explicitly; body={body}"
        );
        return;
    }

    let validate = cmd_validate(&ValidateArgs {
        spec: root.clone(),
        json: true,
    });
    assert!(
        validate.is_ok(),
        "after init --workspace WS succeeds, validate WS must be directly usable by the user; init_body={body} validate={validate:?}"
    );
    let validate_body = json_result(validate.unwrap());
    assert_eq!(
        validate_body["ok"],
        json!(true),
        "init output must be compatible with validate/quick flow, not only an internal .team/current spec path; init_body={body} validate={validate_body}"
    );
}

#[test]
fn mcp_send_message_schema_has_no_team_or_scope_override_for_worker() {
    let send = team_agent::mcp_server::tools_contract()
        .into_iter()
        .find(|tool| tool["name"] == json!("send_message"))
        .expect("send_message tool schema");
    let props = &send["inputSchema"]["properties"];
    assert!(
        props.get("team").is_none() && props.get("scope").is_none(),
        "worker MCP send_message schema must not expose per-call team/scope overrides; scope is spawn-time TEAM_AGENT_OWNER_TEAM_ID. schema={send}"
    );
}

#[test]
fn worker_mcp_send_ignores_or_refuses_scope_arg_and_cannot_self_route_to_sibling_team() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SCOPE_WORKSPACE_MUST_NOT_CROSS_TEAM";
    let call = worker_a.call_tool(
        "send_message",
        json!({
            "to": "worker_x",
            "content": canary,
            "scope": "workspace"
        }),
    );
    let rows = harness.message_rows_containing(canary);
    let events = harness.events_text();
    assert!(
        call.is_error || call.body["ok"] == json!(false) || call.body["status"] == json!("refused"),
        "worker MCP scope=workspace must be refused or ignored under own-team scope, not accepted as cross-team routing; body={} raw={}",
        call.body,
        call.raw
    );
    assert!(
        rows.is_empty(),
        "worker(env=teamA) must not create a message to sibling-team worker_x through scope=workspace; rows={rows:?}"
    );
    assert!(
        events.contains("mcp.scope_refused") || events.contains("mcp.send_message_refused"),
        "scope override/cross-team attempt must emit an audit event; events={events}"
    );
}

#[test]
fn worker_recipient_mcp_uses_messaging_send_message_not_direct_store_write() {
    let tools = source("src/mcp_server/tools.rs");
    let worker_path = source_section(&tools, "if is_worker_recipient(to)", "let opts = SendOptions");
    assert!(
        !worker_path.contains("MessageStore::open")
            && !worker_path.contains(".create_message(")
            && worker_path.contains("messaging::send_message"),
        "MCP worker-recipient path must delegate to messaging::send_message so it inherits target validation, owner gates, and #235 rebind behavior; source={worker_path}"
    );
}

#[test]
fn mcp_send_inherits_target_validation_and_owner_gate() {
    let tools = source("src/mcp_server/tools.rs");
    let send = source_section(&tools, "pub fn send_message", "/// `report_result`");
    assert!(
        send.contains("messaging::send_message")
            && !send.contains("MessageStore::open")
            && !send.contains(".create_message("),
        "all MCP send targets, including worker recipients, must enter messaging::send_message instead of bypassing target_not_in_team/rebind_required/owner-gate logic; source={send}"
    );
}

#[test]
fn mcp_send_canonicalize_owner_team_id_no_fallback() {
    let root = tmp_dir("mcp-canonical-owner");
    save_runtime_state(
        &root,
        &json!({
            "active_team_key": "teamA",
            "teams": {
                "teamA": {"agents": {"worker_a": {"status": "running"}}}
            }
        }),
    )
    .unwrap();
    let tools = team_agent::mcp_server::TeamOrchestratorTools::with_identity(
        &root,
        Some(AgentId::new("worker_a")),
        Some(team_agent::model::ids::TeamKey::new("legacy-name")),
    );
    let err = tools
        .send_message(
            &team_agent::messaging::MessageTarget::Single("worker_a".to_string()),
            "canonicalize me",
            None,
            None,
            None,
            None,
        )
        .expect_err("unknown owner team ids must be refused, not fallback to active/sibling team");
    assert_eq!(
        err.reason,
        team_agent::mcp_server::ToolErrorReason::McpScopeRefused,
        "canonicalize failure should be the #232 typed mcp.scope_refused refusal, not a silent fallback; err={err:?}"
    );
    let envelope = err.to_envelope();
    assert_eq!(
        envelope.get("reason"),
        Some(&json!("mcp.scope_refused")),
        "canonicalize failure refusal envelope must expose mcp.scope_refused; envelope={envelope}"
    );
    let rows = MessageStore::open(&root)
        .ok()
        .map(|store| count_messages(store.db_path()))
        .unwrap_or(0);
    assert_eq!(rows, 0, "canonicalize failure must not create a message row");
}

#[test]
fn mcp_send_same_team_owner_still_creates_team_scoped_message() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SAME_TEAM_OWNER_MUST_SEND";

    let call = worker_a.call_tool(
        "send_message",
        json!({
            "to": "worker_b",
            "content": canary
        }),
    );
    assert!(
        !call.is_error,
        "same-team worker MCP send must not be refused while owner-scope guards are enforced; body={} raw={}",
        call.body,
        call.raw
    );
    assert!(
        call.body["message_id"].as_str().is_some_and(|id| !id.is_empty())
            || call.body["status"] == json!("accepted")
            || call.body["status"] == json!("delivered"),
        "same-team worker MCP send must return an accepted/delivered message shape; body={} raw={}",
        call.body,
        call.raw
    );
    let rows = harness.message_rows_containing(canary);
    assert!(
        rows.iter().any(|row| {
            row.owner_team_id.as_deref() == Some("teamA") && row.recipient == "worker_b"
        }),
        "same-team worker MCP send must create a message row scoped to teamA for worker_b; rows={rows:?}"
    );
}

fn json_result(result: team_agent::cli::CmdResult) -> Value {
    match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON output, got {other:?}"),
    }
}

fn write_team_dir(root: &Path, name: &str, agents: &[(&str, &str)]) -> PathBuf {
    let team = root.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: #254 selected team contract\nprovider: fake\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    for (agent, role) in agents {
        std::fs::write(team.join("agents").join(format!("{agent}.md")), role_doc(agent, role)).unwrap();
    }
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(team.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
    team
}

fn role_doc(agent: &str, role: &str) -> String {
    format!(
        "---\nname: {agent}\nrole: {role}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
    )
}

fn seed_selected_team_state(root: &Path, team_key: &str, team_dir: &Path, attached: bool) {
    std::fs::create_dir_all(root.join(".team").join("runtime")).unwrap();
    let _ = MessageStore::open(root).unwrap();
    let receiver = if attached {
        json!({
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%1",
            "provider": "fake",
            "owner_epoch": 1
        })
    } else {
        Value::Null
    };
    let team_state = json!({
        "status": "alive",
        "session_name": format!("team-{team_key}"),
        "team_dir": team_dir.to_string_lossy().to_string(),
        "spec_path": team_dir.join("team.spec.yaml").to_string_lossy().to_string(),
        "leader_receiver": receiver.clone(),
        "team_owner": receiver,
        "agents": {
            "worker_a": {
                "agent_id": "worker_a",
                "owner_team_id": team_key,
                "provider": "fake",
                "status": "running",
                "window": "worker_a",
                "pane_id": "%2",
                "mcp_ready": true
            }
        },
        "tasks": [
            {"id": "task_1", "assignee": "worker_a", "status": "pending", "summary": "todo"}
        ]
    });
    let mut teams = serde_json::Map::new();
    teams.insert(team_key.to_string(), team_state);
    let state = json!({
        "active_team_key": team_key,
        "session_name": Value::Null,
        "leader_receiver": Value::Null,
        "team_owner": Value::Null,
        "agents": {},
        "tasks": [],
        "teams": Value::Object(teams),
    });
    save_runtime_state(root, &state).unwrap();
}

fn insert_result(root: &Path, result_id: &str, task_id: &str, agent_id: &str, owner_team_id: &str, status: &str) {
    let store = MessageStore::open(root).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let envelope = json!({
        "schema_version": "result_envelope_v1",
        "task_id": task_id,
        "agent_id": agent_id,
        "status": status,
        "summary": "done",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    });
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
        params![
            result_id,
            owner_team_id,
            task_id,
            agent_id,
            envelope.to_string(),
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .unwrap();
}

fn count_messages(db_path: &Path) -> i64 {
    let conn = team_agent::db::schema::open_db(db_path).unwrap();
    conn.query_row("select count(*) from messages", [], |row| row.get(0)).unwrap()
}

fn source(relative: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap()
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let idx = source.find(start).unwrap_or_else(|| panic!("missing source start marker {start}"));
    let rest = &source[idx..];
    let end_idx = rest.find(end).unwrap_or(rest.len());
    &rest[..end_idx]
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-sweep-254-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
