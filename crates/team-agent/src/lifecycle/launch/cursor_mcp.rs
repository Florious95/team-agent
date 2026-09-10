//! ---
//! purpose: 把 team_orchestrator 身份写进 cursor 实际会读的 mcp.json
//! contract:
//!   provides:
//!     - name: apply_cursor_mcp_overlay
//!       what: 写 <workspace>/.cursor/mcp.json，env 必须带 TEAM_AGENT_ID
//!     - name: cursor_mcp_enable_argv
//!       what: 组 `agent mcp enable team_orchestrator`（不认 --workspace）
//!     - name: enable_cursor_workspace_mcp
//!       what: 在工程根（可选）或物理工作目录跑 enable；测试隔离下跳过以免写 ~/.cursor
//!     - name: apply_cursor_spawn_workspace_pointers
//!       what: 隔离开时 --workspace 指 per-seat 工程根，--add-dir 指真 workspace
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

use super::cursor_mcp_iso::{
    cursor_mcp_isolation_enabled, cursor_mcp_project_dir, materialize_cursor_mcp_project,
};

/// Keys that must appear in mcp.json env. Cursor strips parent env down to
/// HOME/PATH/TERM/… — TEAM_AGENT_ID only survives if it is in this table.
const REQUIRED_IDENTITY_KEYS: &[&str] = &[
    "TEAM_AGENT_WORKSPACE",
    "TEAM_AGENT_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
    "TEAM_AGENT_AUTH_MODE",
];

/// ---
/// purpose: 给出 workspace 的物理路径
/// returns: 能 canonicalize 就用它，否则原样返回
/// ---
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

/// ---
/// purpose: 写 workspace 下 cursor 会读的 mcp.json，env 里必须带席位身份键
/// params:
///   mcp_config: 已解析的 MCP 配置，须含 team_orchestrator 与 command
/// returns: 写出的文件路径
/// errors: 缺条目或缺 command 时返回 StatePersist，写盘失败也返回 StatePersist
/// ---
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

/// ---
/// purpose: 组出启用 team_orchestrator 的 cursor 命令行
/// returns: 不带 workspace 参数的 argv，该命令按进程工作目录分片
/// ---
pub fn cursor_mcp_enable_argv() -> Vec<String> {
    vec![
        command_name(Provider::CursorAgent).to_string(),
        "mcp".to_string(),
        "enable".to_string(),
        "team_orchestrator".to_string(),
    ]
}

/// ---
/// purpose: 在物理工作目录下执行 cursor 的 MCP 启用命令
/// returns: 测试隔离环境或显式跳过标志下直接成功，避免写用户全局配置
/// errors: 命令跑不起来或退出码非零时返回 RequirementUnmet，错误里只记输出长度不记内容
/// ---
pub fn cursor_mcp_enable_working_dir(workspace: &Path, project_root: Option<&Path>) -> PathBuf {
    match project_root {
        Some(root) => physical_workspace_path(root),
        None => physical_workspace_path(workspace),
    }
}

pub fn prepare_cursor_seat_mcp(
    workspace: &Path,
    agent_id: &str,
    mcp_config: &crate::provider::McpConfig,
) -> Result<Option<PathBuf>, LifecycleError> {
    if cursor_mcp_isolation_enabled() {
        let project = materialize_cursor_mcp_project(workspace, agent_id)?;
        apply_cursor_mcp_overlay(&project, mcp_config)?;
        Ok(Some(project))
    } else {
        apply_cursor_mcp_overlay(workspace, mcp_config)?;
        Ok(None)
    }
}

pub fn enable_cursor_workspace_mcp(
    workspace: &Path,
    project_root: Option<&Path>,
) -> Result<(), LifecycleError> {
    enable_cursor_workspace_mcp_with_profile(workspace, project_root, None)
}

/// Run Cursor's MCP enable command with the same profile-local environment
/// policy as the worker spawn. In particular, subscription direct mode must
/// not reintroduce proxy URL keys during this preparatory command.
pub fn enable_cursor_workspace_mcp_with_profile(
    workspace: &Path,
    project_root: Option<&Path>,
    profile_launch: Option<&crate::provider::ProviderProfileLaunch>,
) -> Result<(), LifecycleError> {
    let physical = cursor_mcp_enable_working_dir(workspace, project_root);
    if skip_cursor_mcp_enable() {
        return Ok(());
    }
    let argv = cursor_mcp_enable_argv();
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&physical);
    if let Some(profile_launch) = profile_launch {
        for key in &profile_launch.env_unset {
            command.env_remove(key);
        }
        for (key, value) in &profile_launch.env_overlay {
            command.env(key, value);
        }
    }
    let output = command.output().map_err(|e| {
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

/// ---
/// purpose: 把 argv 里 workspace 参数的值换成物理路径
/// params:
///   argv: 就地改写；没有该参数时什么都不做
/// ---
pub fn apply_cursor_workspace_physical_path(argv: &mut [String], workspace: &Path) {
    let physical = physical_workspace_path(workspace);
    let Some(index) = argv.iter().position(|arg| arg == "--workspace") else {
        return;
    };
    if let Some(value) = argv.get_mut(index.saturating_add(1)) {
        *value = physical.to_string_lossy().into_owned();
    }
}

/// ---
/// purpose: 隔离开时把 cursor --workspace 指到 per-seat 工程根，并用 --add-dir 挂上真 workspace
/// ---
pub fn apply_cursor_spawn_workspace_pointers(
    argv: &mut Vec<String>,
    workspace: &Path,
    agent_id: &str,
) -> Result<(), LifecycleError> {
    if cursor_mcp_isolation_enabled() {
        let project = physical_workspace_path(&cursor_mcp_project_dir(workspace, agent_id)?);
        apply_cursor_workspace_physical_path(argv, &project);
        let team = physical_workspace_path(workspace);
        let already = argv
            .windows(2)
            .any(|pair| pair[0] == "--add-dir" && Path::new(&pair[1]) == team.as_path());
        if !already {
            argv.push("--add-dir".to_string());
            argv.push(team.to_string_lossy().into_owned());
        }
    } else {
        apply_cursor_workspace_physical_path(argv, workspace);
    }
    Ok(())
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
