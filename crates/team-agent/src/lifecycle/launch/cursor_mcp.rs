//! ---
//! purpose: 把 team_orchestrator 身份写进 cursor 实际会读的 mcp.json
//! contract:
//!   provides:
//!     - name: apply_cursor_mcp_overlay
//!       what: 默认写 provider-config/<id>/cursor/.cursor/mcp.json；隔离关闭时写 workspace 文件
//!     - name: apply_cursor_spawn_workspace_pointers
//!       what: 隔离开时 --workspace 指 per-seat 工程根，--add-dir 指真 workspace
//!     - name: cursor_mcp_enable_argv
//!       what: 组 `agent mcp enable team_orchestrator`（不认 --workspace）
//!     - name: enable_cursor_workspace_mcp
//!       what: 在工程根跑 enable；测试隔离下跳过以免写 ~/.cursor
//!     - name: physical_workspace_path
//!       what: pwd -P 等价路径；mcp enable 按 getcwd 分片
//!     - name: refuse_second_cursor_occupant
//!       what: 隔离不可用时拒绝第二席；隔离可用时放行
//! boundary:
//!   - 隔离开启时同 workspace 允许多 CursorAgent；隔离关闭/失败仍 fail-closed
//!   - 不把身份从 json env 删掉（cursor 不继承父进程 TEAM_AGENT_*）
//!   - 不改 HOME，不写 ~/.cursor/mcp.json 全局文件
//!   - 不调 --approve-mcps（未验证能代替 enable）
//! maturity: wired
//! ---

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

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

const CURSOR_ENABLE_STDERR_MAX_BYTES: usize = 512;
const CURSOR_ENABLE_SECRET_INDICATORS: &[&str] = &[
    "authorization",
    "bearer",
    "api_key",
    "cookie",
    "password",
    "token=",
    "sk-",
];

impl LifecycleError {
    pub(crate) fn cursor_mcp_enable_failure(
        argv: &[String],
        physical_cwd: &Path,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        spawn_error: Option<&str>,
    ) -> Self {
        let config_path = physical_cwd.join(".cursor").join("mcp.json");
        let metadata = std::fs::metadata(&config_path).ok();
        let config_exists = metadata.is_some();
        let config_size = metadata
            .as_ref()
            .map(|value| value.len().to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        #[cfg(unix)]
        let config_mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata
                .as_ref()
                .map(|value| format!("0o{:o}", value.permissions().mode()))
                .unwrap_or_else(|| "unavailable".to_string())
        };
        #[cfg(not(unix))]
        let config_mode = "unavailable_non_unix".to_string();
        let config_sha256 = std::fs::read(&config_path)
            .ok()
            .map(|body| format!("{:x}", Sha256::digest(body)))
            .unwrap_or_else(|| "unavailable".to_string());
        let stderr_fact = bounded_safe_cursor_stderr(stderr)
            .map(|safe| format!("stderr_first_safe: {safe}"))
            .unwrap_or_else(|| "stderr_redacted".to_string());
        let (reason, action) = match spawn_error {
            Some(error) => (
                format!("cannot run cursor MCP enable: {error}"),
                format!(
                    "install cursor-agent on PATH (same binary as `{}`) and retry",
                    argv[0]
                ),
            ),
            None => (
                "without enable, cursor keeps MCP as not loaded (needs approval)".to_string(),
                format!(
                    "from that directory run `{} mcp enable team_orchestrator`",
                    argv[0]
                ),
            ),
        };
        LifecycleError::RequirementUnmet(format!(
            "error: `{}` failed (exit {})\n\
             reason: {reason}\n\
             argv: {}\n\
             cwd: {}\n\
             config_path: {}\n\
             config_exists: {config_exists}\n\
             config_mode: {config_mode}\n\
             config_size: {config_size}\n\
             config_sha256: {config_sha256}\n\
             stdout_len: {}\n\
             stderr_len: {}\n\
             {stderr_fact}\n\
             action: {action}",
            argv.join(" "),
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unavailable".to_string()),
            argv.join(" "),
            physical_cwd.display(),
            config_path.display(),
            stdout.len(),
            stderr.len(),
        ))
    }
}

fn bounded_safe_cursor_stderr(stderr: &[u8]) -> Option<String> {
    let stderr = String::from_utf8_lossy(stderr);
    let mut safe = String::new();
    'lines: for line in stderr.lines() {
        let lower = line.to_ascii_lowercase();
        if CURSOR_ENABLE_SECRET_INDICATORS
            .iter()
            .any(|indicator| lower.contains(indicator))
        {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if !safe.is_empty() {
            if safe.len() == CURSOR_ENABLE_STDERR_MAX_BYTES {
                break;
            }
            safe.push('\n');
        }
        for character in line.chars() {
            if safe.len() + character.len_utf8() > CURSOR_ENABLE_STDERR_MAX_BYTES {
                break 'lines;
            }
            safe.push(character);
        }
    }
    (!safe.is_empty()).then_some(safe)
}

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
/// purpose: 同一物理 workspace 的第二 CursorAgent：隔离可用则放行，否则 fail-closed
/// params:
///   workspace: 物理工作目录
///   incoming_id: 正要起的席位 id
///   spec: 可选 yaml spec，用来在 state 尚未写入时看见第二席
/// returns: 隔离关闭且已有其它 CursorAgent 则 RequirementUnmet
/// contract:
///   provides:
///     - name: refuse_second_cursor_occupant
///       what: 共用 mcp.json last-writer 闸；隔离落地后不再挡第二席
/// boundary:
///   - 隔离失败/关闭时不改文案语义（仍点名 mcp.json last-writer）
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
    if cursor_mcp_isolation_enabled() {
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
/// purpose: 写 cursor 会读的 mcp.json；默认进 provider-config 工程根，关闭隔离时才写 workspace 文件
/// params:
///   mcp_config: 已解析的 MCP 配置，须含 team_orchestrator 与 command
/// returns: 写出的文件路径
/// errors: 缺条目或缺 command 时返回 StatePersist；隔离建盘失败返回 RequirementUnmet
/// ---
pub fn apply_cursor_mcp_overlay(
    workspace: &Path,
    mcp_config: &crate::provider::McpConfig,
) -> Result<PathBuf, LifecycleError> {
    let (command, args, env) = parse_orchestrator_env(mcp_config)?;
    let agent_id = env
        .get("TEAM_AGENT_ID")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            LifecycleError::StatePersist(
                "cursor MCP overlay missing resolved TEAM_AGENT_ID in json env \
(cursor does not inherit parent TEAM_AGENT_* into the MCP child)"
                    .to_string(),
            )
        })?;
    let cursor_dir = if cursor_mcp_isolation_enabled() {
        let project = materialize_cursor_mcp_project(workspace, agent_id)?;
        scrub_workspace_orchestrator_mcp(workspace)?;
        project.join(".cursor")
    } else {
        workspace.join(".cursor")
    };
    write_cursor_mcp_json_at(&cursor_dir, command, args, env)
}

fn parse_orchestrator_env(
    mcp_config: &crate::provider::McpConfig,
) -> Result<(String, Vec<String>, BTreeMap<String, String>), LifecycleError> {
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
        })?
        .to_string();
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
    Ok((command, args, env))
}

fn write_cursor_mcp_json_at(
    dir: &Path,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
) -> Result<PathBuf, LifecycleError> {
    std::fs::create_dir_all(dir)
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

fn scrub_workspace_orchestrator_mcp(workspace: &Path) -> Result<(), LifecycleError> {
    let path = workspace.join(".cursor").join("mcp.json");
    if !path.is_file() {
        return Ok(());
    }
    let mut root = read_existing_mcp_json(&path);
    let Some(servers) = root
        .as_object_mut()
        .and_then(|obj| obj.get_mut("mcpServers"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };
    servers.remove("team_orchestrator");
    servers.remove("team-agent");
    let body = serde_json::to_string_pretty(&root)
        .map_err(|e| LifecycleError::StatePersist(format!("serialize cursor mcp.json: {e}")))?;
    std::fs::write(&path, body.as_bytes())
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(())
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
/// purpose: 在工程根下执行 cursor 的 MCP 启用命令（getcwd 分片，不改 HOME）
/// returns: 测试隔离环境或显式跳过标志下直接成功，避免写用户全局配置
/// errors: 命令跑不起来或退出码非零时返回 RequirementUnmet，附带有界脱敏诊断但不读出 json 内容
/// ---
pub fn enable_cursor_workspace_mcp(
    workspace: &Path,
    project_root: Option<&Path>,
) -> Result<(), LifecycleError> {
    if skip_cursor_mcp_enable() {
        return Ok(());
    }
    let physical = match project_root {
        Some(root) => physical_workspace_path(root),
        None => physical_workspace_path(workspace),
    };
    let argv = cursor_mcp_enable_argv();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&physical);
    let output = match cmd.output() {
        Ok(output) => output,
        Err(error) => {
            let error = error.to_string();
            return Err(LifecycleError::cursor_mcp_enable_failure(
                &argv,
                &physical,
                None,
                &[],
                &[],
                Some(&error),
            ));
        }
    };
    if output.status.success() {
        return Ok(());
    }
    Err(LifecycleError::cursor_mcp_enable_failure(
        &argv,
        &physical,
        output.status.code(),
        &output.stdout,
        &output.stderr,
        None,
    ))
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
/// params:
///   argv: 就地改写（可能追加 --add-dir）
///   workspace: 团队工作目录
///   agent_id: 席位 id
/// errors: 工程根路径非法时返回 RequirementUnmet
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
