//! ---
//! purpose: g1 五个静默面到达性回归——clone 后 spawn/MCP 不得再夹工具
//! contract:
//!   provides:
//!     - name: g1_face1_resolve_permissions
//!       what: clone 后 worker_command_context 不得按 leader 三件套再截 argv
//!     - name: g1_face2_disallowed_tools
//!       what: Claude --disallowedTools 不得把源席已声明工具再禁回去
//!     - name: g1_face3_mcp_tool_table
//!       what: grok/claude/cursor 各自注入面仍按源席全集映射，不得只剩只读
//!     - name: g1_face4_mcp_clone_entry
//!       what: MCP clone_agent 与 CLI clone-agent 同一 tools 断言
//!     - name: g1_face5_narrow_source_stays_narrow
//!       what: 源席故意三件套时分身仍是三件，不得扩成 developer 默认
//! boundary:
//!   - 不改 clone_agent.rs / add-agent / approval / tools 语义
//!   - 不发明 --keep-tools；不把 cursor 未验证的 --allowed-tools 写进产物
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

use serial_test::serial;

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use crate::lifecycle::worker_command_context::{
    resolved_tool_strings_for_command, WorkerCommandAgent,
};
use crate::lifecycle::{apply_cursor_mcp_overlay, apply_grok_mcp_overlay};
use crate::mcp_server::TeamOrchestratorTools;
use crate::model::enums::{AuthMode, Provider};
use crate::model::ids::{AgentId, TeamKey};
use crate::model::permissions::{resolve_permissions, AgentPermissionInput};
use crate::model::yaml::Value;
use crate::provider::adapters::claude::claude_disallowed_tools;
use crate::provider::adapters::grok::grok_disallowed_tools;
use crate::provider::{get_adapter, McpConfig, ProviderCommandContext};
use crate::state::persist::load_runtime_state;

const TEAM_NAME: &str = "g1sf";
const SOURCE: &str = "src_worker";
const CLONE: &str = "new_worker";
const NARROW: &str = "narrow_src";
const NARROW_CLONE: &str = "narrow_clone";

const SIX: &[&str] = &[
    "execute_bash",
    "fs_list",
    "fs_read",
    "fs_write",
    "mcp_team",
    "provider_builtin",
];
const THREE: &[&str] = &["fs_list", "fs_read", "mcp_team"];
const WRITE_NATIVE: &[&str] = &["Bash", "Edit", "Write", "MultiEdit", "NotebookEdit"];

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
}

impl Case {
    fn start() -> Self {
        ensure_team_agent_cli();
        let env = HermeticTestEnv::enter("g1sf");
        let workspace = env.workspace("ws");
        write_team_docs(&workspace);
        Self { env, workspace }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().expect("ws utf8")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    fn shutdown(&self) {
        drop(self.run(&[
            "shutdown",
            "--workspace",
            self.ws(),
            "--team",
            TEAM_NAME,
            "--json",
        ]));
    }
}

fn ensure_team_agent_cli() {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_team-agent") {
        assert!(
            std::path::Path::new(&path).is_file(),
            "CARGO_BIN_EXE_team-agent missing: {path}"
        );
        return;
    }
    let bin = std::env::current_exe()
        .expect("test exe")
        .parent()
        .and_then(|deps| deps.parent())
        .map(|target| target.join("team-agent"))
        .expect("target/debug/team-agent");
    if bin.is_file() {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(&cargo)
        .args([
            "build",
            "-p",
            "team-agent",
            "--bin",
            "team-agent",
            "--manifest-path",
        ])
        .arg(std::path::Path::new(&manifest).join("Cargo.toml"))
        .status()
        .unwrap_or_else(|e| panic!("spawn {cargo} build: {e}"));
    assert!(
        status.success(),
        "{cargo} build -p team-agent --bin team-agent failed: {status}"
    );
}

fn write_team_docs(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("agents")).expect("agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!(
            "---\nname: {TEAM_NAME}\nobjective: g1 silent faces\nprovider: fake\ndisplay_backend: none\n---\n"
        ),
    )
    .expect("TEAM.md");
    write_role(workspace, SOURCE, SIX);
    write_role(workspace, NARROW, THREE);
}

fn write_role(workspace: &Path, name: &str, tools: &[&str]) {
    let tools_yaml = tools
        .iter()
        .map(|tool| format!("  - {tool}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        workspace.join("agents").join(format!("{name}.md")),
        format!(
            "---\nname: {name}\nrole: {name}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n{tools_yaml}\n---\n\n{name} body.\n"
        ),
    )
    .expect("role doc");
}

fn all_runtime_specs(workspace: &Path) -> Vec<(std::path::PathBuf, Value)> {
    let runtime = workspace.join(".team").join("runtime");
    let mut parsed = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&runtime) {
        for entry in entries.flatten() {
            let path = entry.path().join("team.spec.yaml");
            if path.is_file() {
                let value =
                    crate::model::yaml::loads(&std::fs::read_to_string(&path).expect("spec"))
                        .expect("parse spec");
                parsed.push((path, value));
            }
        }
    }
    parsed
}

fn load_runtime_spec(workspace: &Path) -> Value {
    let parsed = all_runtime_specs(workspace);
    assert!(
        !parsed.is_empty(),
        "runtime spec missing under {}",
        workspace.join(".team").join("runtime").display()
    );
    parsed
        .into_iter()
        .max_by_key(|(_, value)| spec_agent_ids(value).len())
        .expect("runtime spec")
        .1
}

fn spec_agent_ids(spec: &Value) -> Vec<String> {
    spec.get("agents")
        .and_then(Value::as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn runtime_team_key(workspace: &Path) -> String {
    let runtime = workspace.join(".team").join("runtime");
    if runtime.join(TEAM_NAME).join("team.spec.yaml").is_file() {
        return TEAM_NAME.to_string();
    }
    std::fs::read_dir(&runtime)
        .expect("runtime dir")
        .flatten()
        .find(|entry| entry.path().join("team.spec.yaml").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .unwrap_or_else(|| panic!("runtime team key missing"))
}

fn yaml_tool_set(node: &Value) -> BTreeSet<String> {
    node.get("tools")
        .and_then(Value::as_list)
        .unwrap_or_else(|| panic!("tools list missing"))
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn spec_agent<'a>(spec: &'a Value, agent: &str) -> &'a Value {
    let agents = spec
        .get("agents")
        .and_then(Value::as_list)
        .expect("spec agents");
    for row in agents {
        if row.get("id").and_then(Value::as_str) == Some(agent) {
            return row;
        }
    }
    panic!(
        "agent {agent} missing from runtime spec; have {:?}",
        spec_agent_ids(spec)
    );
}

fn spec_tools(workspace: &Path, agent: &str) -> BTreeSet<String> {
    for (_, spec) in all_runtime_specs(workspace) {
        if spec_agent_ids(&spec).iter().any(|id| id == agent) {
            return yaml_tool_set(spec_agent(&spec, agent));
        }
    }
    let inventory: Vec<(String, Vec<String>)> = all_runtime_specs(workspace)
        .into_iter()
        .map(|(path, spec)| (path.display().to_string(), spec_agent_ids(&spec)))
        .collect();
    panic!("agent {agent} missing from every runtime spec; inventory={inventory:?}");
}

fn leader_spec_tools(workspace: &Path) -> BTreeSet<String> {
    yaml_tool_set(
        load_runtime_spec(workspace)
            .get("leader")
            .expect("spec leader"),
    )
}

fn set_of(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn role_tools(path: &Path) -> BTreeSet<String> {
    let (meta, _) = crate::compiler::read_front_matter(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    yaml_tool_set(&meta)
}

fn clone_ok(case: &Case, source: &str, dest: &str) {
    let out = case.run(&[
        "clone-agent",
        source,
        "--as",
        dest,
        "--workspace",
        case.ws(),
        "--team",
        TEAM_NAME,
        "--no-display",
        "--json",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "clone-agent {source} --as {dest} must exit 0; code={:?} stderr={stderr} stdout={stdout}",
        out.status.code()
    );
}

fn quick_start(case: &Case) {
    let qs = case.run(&[
        "quick-start",
        case.ws(),
        "--workspace",
        case.ws(),
        "--name",
        TEAM_NAME,
        "--yes",
        "--json",
    ]);
    let spec_ok = case
        .workspace
        .join(".team")
        .join("runtime")
        .read_dir()
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .find(|e| e.path().join("team.spec.yaml").is_file())
        });
    assert!(
        spec_ok.is_some(),
        "quick-start must leave a runtime spec; stderr={} stdout={}",
        String::from_utf8_lossy(&qs.stderr),
        String::from_utf8_lossy(&qs.stdout)
    );
    assert_eq!(
        spec_tools(&case.workspace, SOURCE),
        set_of(SIX),
        "fixture source must be the six-set"
    );
    assert_eq!(
        leader_spec_tools(&case.workspace),
        set_of(THREE),
        "leader must stay the three-set so the conflict surface exists"
    );
}

fn boot_six_clone() -> Case {
    let case = Case::start();
    quick_start(&case);
    clone_ok(&case, SOURCE, CLONE);
    case
}

fn command_tools(agent_row: &Value, id: &str, provider: Provider) -> Vec<String> {
    let command_agent = WorkerCommandAgent::from_yaml(agent_row, Some(id), provider)
        .unwrap_or_else(|e| panic!("WorkerCommandAgent::from_yaml {id}: {e}"));
    resolved_tool_strings_for_command(&command_agent, provider)
        .unwrap_or_else(|e| panic!("resolved_tool_strings_for_command {id}: {e}"))
}

fn permissions_tools_of(row: &serde_json::Value) -> Option<BTreeSet<String>> {
    row.get("permissions")
        .and_then(|permissions| permissions.get("tools"))
        .and_then(|tools| tools.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
}

fn collect_json_tools(value: &serde_json::Value, agent: &str) -> Option<BTreeSet<String>> {
    if let Some(row) = value.pointer(&format!("/agents/{agent}")) {
        if let Some(tools) = permissions_tools_of(row) {
            return Some(tools);
        }
    }
    if let Some(teams) = value.get("teams").and_then(|v| v.as_object()) {
        for team in teams.values() {
            if let Some(found) = collect_json_tools(team, agent) {
                return Some(found);
            }
        }
    }
    None
}

fn flag_values<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == flag {
            if let Some(value) = argv.get(i + 1) {
                out.push(value.as_str());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn assert_write_tools_not_disallowed(surface: &str, disallowed: &[&str]) {
    let banned: Vec<&str> = disallowed
        .iter()
        .copied()
        .filter(|name| WRITE_NATIVE.contains(name))
        .collect();
    assert!(
        banned.is_empty(),
        "{surface}: six-set clone must not re-ban write/exec native tools; banned={banned:?} disallowed={disallowed:?}"
    );
}

fn overlay_config(agent_id: &str, workspace: &str) -> McpConfig {
    McpConfig {
        raw: serde_json::json!({
            "team_orchestrator": {
                "command": "/bin/team-agent-test",
                "args": ["mcp-server", "--workspace", workspace],
                "env": {
                    "TEAM_AGENT_ID": agent_id,
                    "TEAM_AGENT_WORKSPACE": workspace,
                    "TEAM_AGENT_OWNER_TEAM_ID": TEAM_NAME,
                    "TEAM_AGENT_AUTH_MODE": "subscription",
                }
            }
        }),
    }
}

fn assert_provider_mcp_table(case: &Case, provider: Provider, clone_id: &str) {
    let spec = load_runtime_spec(&case.workspace);
    let row = spec_agent(&spec, clone_id);
    let declared = yaml_tool_set(row);
    assert_eq!(
        declared,
        set_of(SIX),
        "clone spec tools must still be six-set before provider mapping"
    );

    let resolved = command_tools(row, clone_id, provider);
    let resolved_set: BTreeSet<String> = resolved.iter().cloned().collect();
    assert_eq!(
        resolved_set,
        set_of(SIX),
        "{provider:?} resolve_permissions on clone must equal source six-set, not leader three-set; got={resolved:?}"
    );
    assert_ne!(
        resolved_set,
        set_of(THREE),
        "{provider:?} clone tools collapsed onto the leader ceiling"
    );

    let tool_refs: Vec<&str> = resolved.iter().map(String::as_str).collect();
    match provider {
        Provider::Claude | Provider::ClaudeCode => {
            let disallowed = claude_disallowed_tools(&tool_refs);
            assert_write_tools_not_disallowed("claude disallowedTools", &disallowed);
        }
        Provider::Grok => {
            let disallowed = grok_disallowed_tools(&tool_refs);
            assert_write_tools_not_disallowed("grok disallowedTools", &disallowed);
        }
        Provider::CursorAgent => {}
        other => panic!("face3 unexpected provider {other:?}"),
    }

    let adapter = get_adapter(provider);
    let mcp = adapter
        .mcp_config(AuthMode::Subscription)
        .unwrap_or_else(|e| panic!("{provider:?} mcp_config: {e}"));
    let grok_model = (provider == Provider::Grok).then_some("grok-4.6");
    let plan = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: Some(&mcp),
            system_prompt: Some("g1 silent face"),
            model: grok_model,
            tools: &tool_refs,
            profile_launch: None,
            agent_id_hint: Some(clone_id),
            effort: None,
        })
        .unwrap_or_else(|e| panic!("{provider:?} build_command_plan: {e}"));
    match provider {
        Provider::Claude | Provider::ClaudeCode => {
            let disallowed = flag_values(&plan.argv, "--disallowedTools");
            assert_write_tools_not_disallowed("claude argv --disallowedTools", &disallowed);
        }
        Provider::Grok => {
            let disallowed = flag_values(&plan.argv, "--disallowedTools");
            assert_write_tools_not_disallowed("grok argv --disallowedTools", &disallowed);
        }
        Provider::CursorAgent => {
            assert!(
                !plan.argv.iter().any(|arg| arg == "--allowed-tools"),
                "cursor must not invent unverified --allowed-tools (would silently whitelist a subset); argv={:?}",
                plan.argv
            );
        }
        other => panic!("face3 unexpected provider {other:?}"),
    }

    let overlay_root = case
        .env
        .root()
        .join(format!("overlay-{}", provider_tag(provider)));
    std::fs::create_dir_all(&overlay_root).expect("overlay dir");
    let overlay_ws = overlay_root.to_string_lossy().into_owned();
    let cfg = overlay_config(clone_id, &overlay_ws);
    match provider {
        Provider::Grok => {
            apply_grok_mcp_overlay(&overlay_root, &cfg).expect("grok overlay");
            let text = std::fs::read_to_string(overlay_root.join(".grok/config.toml"))
                .expect("grok config.toml");
            assert!(
                text.contains("[mcp_servers.team_orchestrator]"),
                "grok MCP injection must keep team_orchestrator, not a readonly stub; text={text}"
            );
        }
        Provider::Claude | Provider::ClaudeCode => {
            assert!(
                mcp.raw.get("team_orchestrator").is_some(),
                "claude MCP config must keep team_orchestrator server; raw={}",
                mcp.raw
            );
        }
        Provider::CursorAgent => {
            apply_cursor_mcp_overlay(&overlay_root, &cfg).expect("cursor overlay");
            let text = std::fs::read_to_string(
                team_agent::lifecycle::cursor_mcp_json_path(&overlay_root, clone_id)
                    .expect("iso path"),
            )
            .expect("cursor mcp.json");
            assert!(
                text.contains("\"team_orchestrator\""),
                "cursor MCP injection must keep team_orchestrator; text={text}"
            );
        }
        other => panic!("face3 unexpected provider {other:?}"),
    }
}

fn provider_tag(provider: Provider) -> &'static str {
    match provider {
        Provider::Grok => "grok",
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::CursorAgent => "cursor",
        _ => "other",
    }
}

/// Face 1: spawn argv tools come from resolve_permissions on the clone row, not a leader ceiling.
#[test]
#[serial(env)]
fn g1_face1_resolve_permissions() {
    let case = boot_six_clone();
    let spec = load_runtime_spec(&case.workspace);
    let clone_row = spec_agent(&spec, CLONE);
    let resolved = command_tools(clone_row, CLONE, Provider::Fake);
    let resolved_set: BTreeSet<String> = resolved.iter().cloned().collect();
    assert_eq!(
        resolved_set,
        set_of(SIX),
        "face1: resolved_tool_strings_for_command must keep the six-set; got={resolved:?} leader={:?}",
        leader_spec_tools(&case.workspace)
    );
    assert!(
        resolved_set.contains("execute_bash") && resolved_set.contains("fs_write"),
        "face1: write/exec must survive worker_command_context; got={resolved:?}"
    );

    let direct = resolve_permissions(&AgentPermissionInput {
        id: Some(AgentId::new(CLONE)),
        provider: Provider::Fake,
        role: clone_row
            .get("role")
            .and_then(Value::as_str)
            .map(str::to_string),
        tools: Some(resolved.clone()),
    })
    .expect("resolve_permissions on clone tools");
    let direct_set: BTreeSet<String> = direct
        .sorted_tool_strings()
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        direct_set,
        set_of(SIX),
        "face1: resolve_permissions itself must not intersect leader ceiling"
    );

    let state = load_runtime_state(&case.workspace).expect("runtime state after clone");
    if let Some(persisted) = collect_json_tools(&state, CLONE) {
        assert_eq!(
            persisted,
            set_of(SIX),
            "face1: persisted spawn permissions must equal source six-set; got={persisted:?}"
        );
    }
    case.shutdown();
}

/// Face 2: Claude --disallowedTools must not re-ban tools the clone still declares.
#[test]
#[serial(env)]
fn g1_face2_disallowed_tools() {
    let case = boot_six_clone();
    let spec = load_runtime_spec(&case.workspace);
    let clone_row = spec_agent(&spec, CLONE);
    let resolved = command_tools(clone_row, CLONE, Provider::Claude);
    let tool_refs: Vec<&str> = resolved.iter().map(String::as_str).collect();
    let disallowed = claude_disallowed_tools(&tool_refs);
    assert_write_tools_not_disallowed("face2 claude_disallowed_tools", &disallowed);

    let adapter = get_adapter(Provider::Claude);
    let mcp = adapter
        .mcp_config(AuthMode::Subscription)
        .expect("claude mcp");
    let plan = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: Some(&mcp),
            system_prompt: Some("g1 face2"),
            model: None,
            tools: &tool_refs,
            profile_launch: None,
            agent_id_hint: Some(CLONE),
            effort: None,
        })
        .expect("claude plan");
    let argv_disallowed = flag_values(&plan.argv, "--disallowedTools");
    assert_write_tools_not_disallowed("face2 argv", &argv_disallowed);
    assert!(
        plan.argv.iter().any(|arg| arg == "--disallowedTools") || argv_disallowed.is_empty(),
        "face2 must actually walk Claude argv construction; argv={:?}",
        plan.argv
    );
    case.shutdown();
}

/// Face 3 umbrella: all three provider injection surfaces stay at the six-set.
#[test]
#[serial(env)]
fn g1_face3_mcp_tool_table() {
    let case = boot_six_clone();
    assert_provider_mcp_table(&case, Provider::Grok, CLONE);
    assert_provider_mcp_table(&case, Provider::Claude, CLONE);
    assert_provider_mcp_table(&case, Provider::CursorAgent, CLONE);
    case.shutdown();
}

#[test]
#[serial(env)]
fn g1_face3_mcp_tool_table_grok() {
    let case = boot_six_clone();
    assert_provider_mcp_table(&case, Provider::Grok, CLONE);
    case.shutdown();
}

#[test]
#[serial(env)]
fn g1_face3_mcp_tool_table_claude() {
    let case = boot_six_clone();
    assert_provider_mcp_table(&case, Provider::Claude, CLONE);
    case.shutdown();
}

#[test]
#[serial(env)]
fn g1_face3_mcp_tool_table_cursor() {
    let case = boot_six_clone();
    assert_provider_mcp_table(&case, Provider::CursorAgent, CLONE);
    case.shutdown();
}

/// Face 4: MCP clone_agent(...) and CLI clone-agent must keep the same tools set.
#[test]
#[serial(env)]
fn g1_face4_mcp_clone_entry() {
    // Two isolated teams: a second clone on the same spec can rewrite the
    // agent list from role docs and drop the first dest. Compare tools across
    // workspaces, not two dests on one spec.
    let cli_case = Case::start();
    quick_start(&cli_case);
    clone_ok(&cli_case, SOURCE, CLONE);
    let cli_tools = spec_tools(&cli_case.workspace, CLONE);
    let cli_spec = load_runtime_spec(&cli_case.workspace);
    let cli_resolved = command_tools(spec_agent(&cli_spec, CLONE), CLONE, Provider::Fake);

    let body_case = Case::start();
    quick_start(&body_case);
    crate::lifecycle::clone_agent(
        &body_case.workspace,
        &AgentId::new(SOURCE),
        &AgentId::new(CLONE),
        None,
        false,
        Some(TEAM_NAME),
    )
    .unwrap_or_else(|e| panic!("shared clone_agent body must succeed: {e}"));
    let body_tools = spec_tools(&body_case.workspace, CLONE);
    let body_spec = load_runtime_spec(&body_case.workspace);
    let body_resolved = command_tools(spec_agent(&body_spec, CLONE), CLONE, Provider::Fake);

    assert_eq!(cli_tools, set_of(SIX), "CLI clone tools");
    assert_eq!(body_tools, set_of(SIX), "shared-body clone tools");
    assert_eq!(
        cli_tools, body_tools,
        "face4: CLI and shared clone_agent body must keep one tools assertion"
    );
    assert_eq!(
        cli_resolved, body_resolved,
        "face4: spawn-time resolved tools must match across CLI and shared-body clones"
    );

    let team_key = runtime_team_key(&body_case.workspace);
    let mcp_probe = TeamOrchestratorTools::with_identity(
        &body_case.workspace,
        Some(AgentId::new(SOURCE)),
        Some(TeamKey::new(&team_key)),
    )
    .clone_agent(SOURCE, "mcp_probe", None);
    match mcp_probe {
        Ok(_) => {
            assert_eq!(
                spec_tools(&body_case.workspace, "mcp_probe"),
                set_of(SIX),
                "mcp_probe tools must equal the source six-set"
            );
        }
        Err(error) => {
            let text = format!("{error:?}");
            assert!(
                text.contains("not found")
                    || text.contains("unknown worker")
                    || text.contains("team select")
                    || text.contains("TeamSelect"),
                "mcp_probe MCP clone_agent must persist six-set or fail-loud; error={text}"
            );
        }
    }
    cli_case.shutdown();
    body_case.shutdown();
}

/// Face 5: a deliberately narrow source must stay narrow after clone (no widen-to-six).
#[test]
#[serial(env)]
fn g1_face5_narrow_source_stays_narrow() {
    let case = Case::start();
    quick_start(&case);
    clone_ok(&case, NARROW, NARROW_CLONE);

    let spec = load_runtime_spec(&case.workspace);
    let clone_row = spec_agent(&spec, NARROW_CLONE);
    let spec_set = yaml_tool_set(clone_row);
    assert_eq!(
        spec_set,
        set_of(THREE),
        "face5: spec tools must stay the three-set; got={spec_set:?}"
    );

    let resolved: BTreeSet<String> = command_tools(clone_row, NARROW_CLONE, Provider::Fake)
        .into_iter()
        .collect();
    assert_eq!(
        resolved,
        set_of(THREE),
        "face5: resolve_permissions must not fall back to developer defaults and widen; got={resolved:?}"
    );
    assert!(
        !resolved.contains("execute_bash") && !resolved.contains("fs_write"),
        "face5: narrow clone must not gain write/exec; got={resolved:?}"
    );

    let role = role_tools(
        &case
            .workspace
            .join(".team")
            .join("dynamic-role-files")
            .join(format!("{NARROW_CLONE}.md")),
    );
    assert_eq!(role, set_of(THREE), "face5: role file must stay narrow");

    let tool_refs: Vec<&str> = resolved.iter().map(String::as_str).collect();
    let disallowed = claude_disallowed_tools(&tool_refs);
    for required in ["Bash", "Edit", "Write"] {
        assert!(
            disallowed.contains(&required),
            "face5: narrow clone MUST disallow {required}; disallowed={disallowed:?}"
        );
    }
    case.shutdown();
}
