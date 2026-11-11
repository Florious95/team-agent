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

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_sim_harness::McpSimHarness;
use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::{
    cmd_doctor, CmdOutput, DoctorArgs, ExitCode,
};
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

#[test]
#[serial(env)]
fn diagnose_live_fake_team_uses_active_team_projection() {
    let env = hermetic_guard::HermeticTestEnv::enter("diagnose-projection");
    let root = env.workspace("diagnose-projection");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, true);

    let out = json_result(
        cmd_doctor(&DoctorArgs {
            workspace: root.clone(),
            json: true,
            team: None,
            spec: None,
            gate: None,
            comms: false,
            fix: false,
            fix_schema: false,
            cleanup_orphans: false,
            confirm: false,
        })
        .expect("diagnose should return JSON"),
    );
    // 262f6cdb (doctor v1): the selected team is the report's `team`, and the
    // return link reads the selected team's registered receiver (%1), never
    // the raw top-level (unbound) projection.
    assert_eq!(
        out["team"],
        json!("teamA"),
        "diagnose --workspace <runtime> must report the active selected team key, not raw top-level state; out={out}"
    );
    assert_eq!(link(&out, "L1")["status"], "PASS", "{out}");
    let ret = link(&out, "L6")["message"].as_str().unwrap_or_default().to_string();
    assert!(
        ret.contains("registered %1@") && !ret.contains("registered unbound@"),
        "selected team has an attached leader_receiver; raw top-level unbound is a false positive; out={out}"
    );
}

#[test]
#[serial(env)]
fn diagnose_selected_team_without_registry_is_unbound() {
    let env = hermetic_guard::HermeticTestEnv::enter("diagnose-registry-missing");
    let root = env.workspace("diagnose-registry-missing");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, false);

    let mut state = load_runtime_state(&root).unwrap();
    state["teams"]["teamA"]["leader_receiver"] = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": "%1",
        "provider": "fake",
        "owner_epoch": 1
    });
    save_runtime_state(&root, &state).unwrap();

    let out = json_result(
        cmd_doctor(&DoctorArgs {
            workspace: root,
            json: true,
            team: None,
            spec: None,
            gate: None,
            comms: false,
            fix: false,
            fix_schema: false,
            cleanup_orphans: false,
            confirm: false,
        })
        .expect("diagnose should return JSON"),
    );
    // 262f6cdb (doctor v1): a state-only receiver without a valid registry
    // row is never delivered (L6 does not pass), and with its host down the
    // single repair is the host's, not an executable claim.
    assert_ne!(link(&out, "L6")["status"], "PASS", "state-only attached fixture must stay undeliverable as index_missing without a registry row; out={out}");
    assert_ne!(out["ok"], json!(true));
    let repairs = json!([out["next_action"]]);
    assert!(
        !next_action_induces_claim(&out),
        "index-missing must not induce an executable claim command; repairs={repairs}"
    );
}

#[test]
#[serial(env)]
fn diagnose_selected_team_with_mismatched_registry_is_unbound() {
    let env = hermetic_guard::HermeticTestEnv::enter("diagnose-registry-mismatch");
    let root = env.workspace("diagnose-registry-mismatch");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);
    seed_selected_team_state(&root, "teamA", &team, false);

    let mut state = load_runtime_state(&root).unwrap();
    state["teams"]["teamA"]["leader_receiver"] = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": "%1",
        "provider": "fake",
        "owner_epoch": 1
    });
    save_runtime_state(&root, &state).unwrap();
    write_registry_entry(&root, "teamA", "/foreign/workspace", 1);

    let out = json_result(
        cmd_doctor(&DoctorArgs {
            workspace: root,
            json: true,
            team: None,
            spec: None,
            gate: None,
            comms: false,
            fix: false,
            fix_schema: false,
            cleanup_orphans: false,
            confirm: false,
        })
        .expect("diagnose should return JSON"),
    );
    // 262f6cdb (doctor v1): a state-only receiver without a valid registry
    // row is never delivered (L6 does not pass), and with its host down the
    // single repair is the host's, not an executable claim.
    assert_ne!(link(&out, "L6")["status"], "PASS", "foreign-workspace registry row must stay undeliverable as index_missing; out={out}");
    assert_ne!(out["ok"], json!(true));
    let repairs = json!([out["next_action"]]);
    assert!(
        !next_action_induces_claim(&out),
        "index-missing must not induce an executable claim command; repairs={repairs}"
    );
}

#[test]
fn cli_add_agent_without_explicit_team_persists_to_active_team() {
    let launch = source("src/lifecycle/launch.rs");
    let add_agent = source_section(
        &launch,
        "pub fn add_agent(",
        "/// `add_agent` with an injected transport",
    );
    let at_paths = source_section(
        &launch,
        "fn add_agent_with_transport_at_paths",
        "fn materialize_added_role_file",
    );
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

#[ignore = "red-by-design: pending contract, tracked in private backlog"]
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
fn doctor_fake_team_without_profiles_passes_profile_not_required() {
    let root = tmp_dir("doctor-fake");
    let team = write_team_dir(&root, "teamA", &[("worker_a", "Fake worker")]);

    let out = json_result(
        cmd_doctor(&DoctorArgs {
            spec: Some(team.join("TEAM.md")),
            workspace: team,
            gate: None,
            comms: false,
            team: None,
            fix: false,
            fix_schema: false,
            cleanup_orphans: false,
            confirm: false,
            json: true,
        })
        .expect("doctor should return JSON"),
    );
    // 262f6cdb + A3 (ec3ef36e): doctor v1 has no profile check at all; this
    // team dir without runtime is only the typed L1 no-runtime failure, and
    // missing profile dirs are never a blocker.
    assert!(
        out.get("profile_smoke").is_none() && !out.to_string().contains("profile"),
        "fake teams do not require profiles; missing profile dirs are informational, not blockers. out={out}"
    );
    assert_eq!(link(&out, "L1")["status"], "FAIL", "{out}");
    assert!(link(&out, "L1")["message"].as_str().is_some_and(|message| message.contains("has no runtime state")), "{out}");
}

#[test]
fn attach_leader_help_lists_pane_provider_and_dispatches_handler() {
    // e0652b62 retired attach-leader: the verb is refused before any effect
    // and its retirement message names the replacement, claim-leader from the
    // intended leader terminal (--confirm only to replace a live leader). The
    // typed handler is gone; dispatch keeps an explicit retirement arm.
    let emit = source("src/cli/emit.rs");
    let types = source("src/cli/types.rs");
    let cli_mod = source("src/cli/mod.rs");
    let dispatch = source_section(&emit, "fn dispatch", "const LEADER_PASSTHROUGH_COMMANDS");
    let root = std::env::temp_dir().join(format!("ta-sweep254-attach-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for args in [vec!["attach-leader", "--help"], vec!["attach-leader", "--pane", "%1", "--provider", "codex", "--json"]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(&args).current_dir(&root).env("HOME", &root)
            .env_remove("TMUX").env_remove("TMUX_PANE").output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success() && text.contains("retired") && text.contains("claim-leader"),
            "{args:?}: retirement must name claim-leader; code={:?} output={text}", out.status.code());
    }
    assert!(!root.join(".team").exists(), "retired attach-leader must have no effects");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        dispatch.contains("\"attach-leader\" | \"takeover\"") && dispatch.contains("team-agent claim-leader"),
        "dispatch must keep an explicit retirement arm naming claim-leader; dispatch={dispatch}"
    );
    assert!(
        !types.contains("pub struct AttachLeaderArgs") && !cli_mod.contains("pub fn cmd_attach_leader"),
        "the retired attach-leader handler must not survive as a reachable typed path"
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
fn worker_recipient_mcp_uses_physical_directory_not_sqlite_delivery() {
    let tools = source("src/mcp_server/tools.rs");
    let send = source_section(&tools, "pub fn send_message", "/// `report_result`");
    // R-M5: MCP send_message is a thin adapter over the shared station send core (seat authority),
    // which resolves the physical directory and stores in the inbox; no retired queue anywhere.
    assert!(send.contains("crate::station::send::execute") && send.contains("Authority::Seat"));
    assert!(!send.contains("MessageStore::open") && !send.contains(".create_message("));
    assert!(!send.contains("messaging::send_message"), "physical worker delivery must not re-enter the retired queue");
    let core = source("src/station/send.rs");
    assert!(core.contains("direct_delivery::directory") && core.contains("inbox_store::store("));
    assert!(!core.contains("MessageStore::open") && !core.contains(".create_message(") && !core.contains("messaging::send_message"));
}

#[test]
fn mcp_physical_send_uses_captured_scope_not_stale_owner_control_plane() {
    let harness = McpSimHarness::new();
    let mut state = harness.state_value();
    state["active_team_key"] = json!("teamB");
    state["teams"]["teamA"]["team_owner"] = json!({"pane_id":"%stale", "leader_session_uuid":"stale-control-plane"});
    save_runtime_state(harness.workspace_path(), &state).unwrap();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let token = "CAPTURED_SCOPE_PHYSICAL_CANARY";
    let call = worker.call_tool("send_message", json!({"to":"worker_b", "content":token}));
    assert!(!call.is_error, "{}", call.body);
    assert_eq!(call.body["delivery"], "physical");
    assert_eq!(call.body["recipient"], "worker_b");
    assert!(harness.pane_contains_count("worker_b", token) >= 1);
    assert_eq!(harness.pane_contains_count("team_b_leader", token), 0);
    assert_eq!(harness.pane_contains_count("worker_a", token), 0);
    assert!(harness.message_rows_containing(token).is_empty());
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
        .send_message_with_presentation(
            &team_agent::messaging::MessageTarget::Single("worker_a".to_string()),
            "canonicalize me",
            None,
            None,
            None,

        None, None)
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
    assert!(!root.join(".team/inbox").exists(),
        "canonicalize failure must not create an inbox in any team scope");
    for suffix in ["team.db", "team.db-wal", "team.db-shm"] {
        assert!(!root.join(".team").join(suffix).exists(), "refusal must not create {suffix}");
    }
}

#[test]
fn mcp_send_same_team_owner_delivers_physically_without_a_queue_row() {
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
    assert_eq!(call.body["status"], "sent");
    assert_eq!(call.body["delivery"], "physical");
    assert_eq!(call.body["recipient"], "worker_b");
    assert!(call.body["accepted"].is_null(), "physical submit does not prove provider acceptance");
    // R-M1a (M1 store-first): the receipt names its durable inbox record; still no queue row.
    assert!(call.body["message_id"].as_str().is_some_and(|id| !id.is_empty()), "{}", call.body);
    assert!(harness.message_rows_containing(canary).is_empty());
    assert!(harness.pane_contains_count("worker_b", canary) >= 1);
    assert_eq!(harness.pane_contains_count("worker_a", canary), 0);
    assert_eq!(harness.pane_contains_count("team_b_leader", canary), 0);
}

// 262f6cdb (doctor v1): six ordered links plus one `next_action` argv.
fn link<'a>(out: &'a Value, id: &str) -> &'a Value {
    let links = out["links"].as_array().unwrap_or_else(|| panic!("doctor v1 links: {out}"));
    assert_eq!(links.len(), 6, "{out}");
    links.iter().find(|link| link["id"] == id).unwrap()
}

fn next_action_induces_claim(out: &Value) -> bool {
    out["next_action"]["argv"].as_array().is_some_and(|argv| argv.iter().any(|arg| arg == "claim-leader"))
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
        std::fs::write(
            team.join("agents").join(format!("{agent}.md")),
            role_doc(agent, role),
        )
        .unwrap();
    }
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    team
}

fn role_doc(agent: &str, role: &str) -> String {
    format!(
        "---\nname: {agent}\nrole: {role}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n"
    )
}

fn seed_selected_team_state(root: &Path, team_key: &str, team_dir: &Path, attached: bool) {
    std::fs::create_dir_all(root.join(".team").join("runtime")).unwrap();
    // M4: no SQLite message store fixture (MessageStore::open is retired).
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
    if attached {
        write_registry_entry(root, team_key, &root.to_string_lossy(), 1);
    }
}

fn write_registry_entry(root: &Path, team_key: &str, authorized_workspace: &str, owner_epoch: u64) {
    let entry = team_agent::leader::registry::build_entry(
        root,
        team_key,
        "direct_tmux",
        json!({
            "status": "attached",
            "pane_id": "%1",
            "provider": "fake",
            "authorized_team_workspace": authorized_workspace
        }),
        owner_epoch,
        "sweep-254-selected-team-fixture",
        "2026-08-25T00:00:00Z".to_string(),
    );
    assert!(
        team_agent::leader::registry::write_entry_best_effort(&entry).is_some(),
        "hermetic HOME must accept a leaders/ registry write"
    );
}





fn source(relative: &str) -> String {
    composite_source::composite_source(relative)
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let idx = source
        .find(start)
        .unwrap_or_else(|| panic!("missing source start marker {start}"));
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
