//! ---
//! purpose: 把 team_orchestrator 身份写进 cursor 实际会读的 mcp.json
//! contract:
//!   provides:
//!     - name: apply_cursor_mcp_overlay
//!       what: 写 <workspace>/.cursor/mcp.json，env 必须带 TEAM_AGENT_ID
//!     - name: cursor_mcp_enable_argv
//!       what: 组 `agent mcp enable team_orchestrator`（不认 --workspace）
//!     - name: enable_cursor_workspace_mcp
//!       what: 在物理工作目录跑 enable；测试隔离下跳过以免写 ~/.cursor
//!     - name: physical_workspace_path
//!       what: pwd -P 等价路径；mcp enable 按 getcwd 分片
//!     - name: refuse_second_cursor_occupant
//!       what: 同一物理 workspace 第二 CursorAgent 拒绝（mcp.json last-writer）
//! boundary:
//!   - 同 workspace 第二 CursorAgent 拒绝；U-07 过前不装「已支持多席」
//!   - 不把身份从 json env 删掉（cursor 不继承父进程 TEAM_AGENT_*）
//!   - 不写 ~/.cursor/mcp.json 全局文件
//!   - 不调 --approve-mcps（未验证能代替 enable）
//! maturity: wired
//! ---

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::LifecycleError;
use crate::model::yaml::Value as YamlValue;
use crate::provider::wire::command_name;
use crate::provider::Provider;

/// Keys that must appear in mcp.json env. Cursor strips parent env down to
/// HOME/PATH/TERM/… — TEAM_AGENT_ID only survives if it is in this table.
const REQUIRED_IDENTITY_KEYS: &[&str] = &[
    "TEAM_AGENT_WORKSPACE",
    "TEAM_AGENT_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
    "TEAM_AGENT_AUTH_MODE",
];

pub fn physical_workspace_path(workspace: &Path) -> PathBuf {
    std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
}

fn status_is_live(status: &str) -> bool {
    !matches!(
        status,
        "stopped" | "stopping" | "removed" | "spawn_failed" | "failed"
    )
}

fn agent_is_cursor(provider: Option<&str>) -> bool {
    matches!(
        provider.and_then(crate::lifecycle::profile_launch::parse_provider),
        Some(Provider::CursorAgent)
    )
}

/// ---
/// purpose: 同一物理 workspace 拒绝第二 CursorAgent
/// params:
///   workspace: 物理工作目录
///   incoming_id: 正要起的席位 id
///   spec: 可选 yaml spec，用来在 state 尚未写入时看见第二席
/// returns: 已有其它 CursorAgent 则 RequirementUnmet，文案含 TEAM_AGENT_ID 与 mcp.json
/// contract:
///   provides:
///     - name: refuse_second_cursor_occupant
///       what: mcp.json last-writer 闸；U-07 过前不装多席
/// boundary:
///   - 不改 grok 独占实现
/// ---
pub fn refuse_second_cursor_occupant(
    workspace: &Path,
    incoming_id: &str,
    spec: Option<&YamlValue>,
) -> Result<(), LifecycleError> {
    let mut others = Vec::new();
    if let Some(spec) = spec {
        if let Some(agents) = spec.get("agents").and_then(YamlValue::as_list) {
            for agent in agents {
                let Some(id) = agent.get("id").and_then(YamlValue::as_str) else {
                    continue;
                };
                if id == incoming_id {
                    continue;
                }
                if agent_is_paused_yaml(agent) {
                    continue;
                }
                if agent_is_cursor(agent.get("provider").and_then(YamlValue::as_str)) {
                    others.push(id.to_string());
                }
            }
        }
    }
    if let Ok(state) = crate::state::persist::load_runtime_state(workspace) {
        if let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) {
            for (id, agent) in agents {
                if id == incoming_id {
                    continue;
                }
                let provider = agent.get("provider").and_then(serde_json::Value::as_str);
                if !agent_is_cursor(provider) {
                    continue;
                }
                let status = agent
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("running");
                if status_is_live(status) {
                    others.push(id.clone());
                }
            }
        }
    }
    others.sort();
    others.dedup();
    if others.is_empty() {
        return Ok(());
    }
    Err(LifecycleError::RequirementUnmet(format!(
        "error: cursor_agent seat already occupies this workspace\n\
         incoming: {incoming_id}\n\
         occupants: {}\n\
         reason: <workspace>/.cursor/mcp.json is directory-scoped; a second seat overwrites TEAM_AGENT_ID (last-writer)\n\
         workspace: {}\n\
         action: do not add another CursorAgent in this workspace until per-seat MCP identity is isolated",
        others.join(", "),
        workspace.display(),
    )))
}

fn agent_is_paused_yaml(agent: &YamlValue) -> bool {
    matches!(agent.get("paused"), Some(YamlValue::Bool(true)))
}

pub fn apply_cursor_mcp_overlay(
    workspace: &Path,
    mcp_config: &crate::provider::McpConfig,
) -> Result<PathBuf, LifecycleError> {
    let server = mcp_config
        .raw
        .get("team_orchestrator")
        .or_else(|| mcp_config.raw.get("team-agent"))
        .ok_or_else(|| {
            LifecycleError::StatePersist(
                "cursor MCP overlay requires team_orchestrator in resolved mcp config".to_string(),
            )
        })?;
    let command = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            LifecycleError::StatePersist("cursor MCP overlay missing command".to_string())
        })?;
    let args = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let env = server
        .get("env")
        .and_then(serde_json::Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    for key in REQUIRED_IDENTITY_KEYS {
        match env.get(*key).map(String::as_str) {
            Some(value) if !value.trim().is_empty() && !value.contains('{') => {}
            _ => {
                return Err(LifecycleError::StatePersist(format!(
                    "cursor MCP overlay missing resolved {key} in json env \
(cursor does not inherit parent TEAM_AGENT_* into the MCP child)"
                )));
            }
        }
    }

    let dir = workspace.join(".cursor");
    std::fs::create_dir_all(&dir)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", dir.display())))?;
    let path = dir.join("mcp.json");
    let mut root = read_existing_mcp_json(&path);
    let servers = root
        .as_object_mut()
        .ok_or_else(|| {
            LifecycleError::StatePersist(format!(
                "{}: cursor mcp.json root must be an object",
                path.display()
            ))
        })?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist(format!("{}: mcpServers must be an object", path.display()))
    })?;
    // 0.5.67 overlay 曾误用 team-agent 作 inbound key。cursor 按 server 名
    // 给工具加命名空间，旧名会变成另一套工具。写新表时摘掉旧名。
    servers.remove("team-agent");
    let mut env_json = serde_json::Map::new();
    for (key, value) in &env {
        env_json.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    servers.insert(
        "team_orchestrator".to_string(),
        serde_json::json!({
            "command": command,
            "args": args,
            "env": env_json,
        }),
    );
    let body = serde_json::to_string_pretty(&root)
        .map_err(|e| LifecycleError::StatePersist(format!("serialize cursor mcp.json: {e}")))?;
    let tmp = dir.join("mcp.json.tmp");
    std::fs::write(&tmp, body.as_bytes())
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(path)
}

pub fn cursor_mcp_enable_argv() -> Vec<String> {
    vec![
        command_name(Provider::CursorAgent).to_string(),
        "mcp".to_string(),
        "enable".to_string(),
        "team_orchestrator".to_string(),
    ]
}

pub fn enable_cursor_workspace_mcp(workspace: &Path) -> Result<(), LifecycleError> {
    if skip_cursor_mcp_enable() {
        return Ok(());
    }
    let physical = physical_workspace_path(workspace);
    let argv = cursor_mcp_enable_argv();
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&physical)
        .output()
        .map_err(|e| {
            LifecycleError::RequirementUnmet(format!(
                "error: cannot run `{} mcp enable team_orchestrator`\n\
                 reason: {e}\n\
                 action: install cursor-agent on PATH (same binary as `agent`) and retry",
                argv[0]
            ))
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(LifecycleError::RequirementUnmet(format!(
        "error: `{} mcp enable team_orchestrator` failed (exit {:?})\n\
         reason: without enable, cursor keeps MCP as not loaded (needs approval)\n\
         cwd: {}\n\
         stdout_len: {}\n\
         stderr_len: {}\n\
         action: from that directory run `{} mcp enable team_orchestrator`",
        argv[0],
        output.status.code(),
        physical.display(),
        stdout.len(),
        stderr.len(),
        argv[0]
    )))
}

pub fn apply_cursor_workspace_physical_path(argv: &mut [String], workspace: &Path) {
    let physical = physical_workspace_path(workspace);
    let Some(index) = argv.iter().position(|arg| arg == "--workspace") else {
        return;
    };
    if let Some(value) = argv.get_mut(index.saturating_add(1)) {
        *value = physical.to_string_lossy().into_owned();
    }
}

fn skip_cursor_mcp_enable() -> bool {
    cfg!(test)
        || std::env::var_os("TEAM_AGENT_TEST_TMP").is_some()
        || matches!(
            std::env::var("TEAM_AGENT_SKIP_CURSOR_MCP_ENABLE").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        )
}

fn read_existing_mcp_json(path: &Path) -> serde_json::Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return serde_json::json!({"mcpServers": {}});
    };
    serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"mcpServers": {}}))
}
