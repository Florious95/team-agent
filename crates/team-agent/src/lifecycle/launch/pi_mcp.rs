//! ---
//! purpose: Pi leader 的 executable/catalog 验证、MCP 能力注册与 fresh 启动计划物化
//! contract:
//!   provides:
//!     - name: materialize_pi_plan
//!       what: 为 `team-agent pi` leader 生成 fresh Pi CommandPlan
//!     - name: write_pi_wrapper
//!       what: 原子写入 0600 per-seat runtime MCP registration extension
//!     - name: pi_cleanup_plan
//!       what: 移除升级前遗留的 legacy Pi worker 席位时只删 wrapper
//! boundary:
//!   - Pi worker 席位只走 contract `pi_tui` leaf；这里不再 spawn/resume/fork worker
//!   - 不枚举 Pi session；不投递消息、不执行 doctor 判定
//!   - 不读取 credentials 或 provider settings 值
//! maturity: wired
//! ---

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::model::enums::{Provider, ProviderEffort};
use crate::provider::command_helpers::{append_opt_pair, append_pair};
use crate::provider::{CommandPlan, McpConfig, ProviderError};

/// The first executable `pi` on PATH, exactly what the worker's bare `pi`
/// runs: a wrapper, npm symlink or plain binary alike. No inner binary is probed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PiExecutableChain {
    pub path_entry: PathBuf,
    pub pi_version: String,
    pub catalog_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PiSeatPaths {
    pub runtime_root: PathBuf,
    pub wrapper: PathBuf,
    pub sessions: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PiCleanupAction {
    Stop,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PiCleanupPlan {
    pub delete_paths: Vec<PathBuf>,
    pub retain_paths: Vec<PathBuf>,
    pub process_name_scan: Option<String>,
}

/// ---
/// purpose: 给 stop/remove 生成 Pi 席位 exact cleanup 计划
/// returns: stop 全保留；remove 只删 wrapper 并保留 session backing
/// ---
pub(crate) fn pi_cleanup_plan(action: PiCleanupAction, paths: &PiSeatPaths) -> PiCleanupPlan {
    match action {
        PiCleanupAction::Stop => PiCleanupPlan {
            delete_paths: Vec::new(),
            retain_paths: vec![paths.wrapper.clone(), paths.sessions.clone()],
            process_name_scan: None,
        },
        PiCleanupAction::Remove => PiCleanupPlan {
            delete_paths: vec![paths.wrapper.clone()],
            retain_paths: vec![paths.sessions.clone()],
            process_name_scan: None,
        },
    }
}

/// ---
/// purpose: 计算一个 Pi 席位唯一的 Team-owned runtime 路径
/// returns: runtime root、wrapper 与 sessions 路径
/// ---
pub(crate) fn pi_seat_paths(workspace: &Path, team_id: &str, agent_id: &str) -> PiSeatPaths {
    let runtime_root = workspace
        .join(".team")
        .join("runtime")
        .join("pi")
        .join(team_id)
        .join(agent_id);
    PiSeatPaths {
        wrapper: runtime_root.join("team-mcp.ts"),
        sessions: runtime_root.join("sessions"),
        runtime_root,
    }
}

/// ---
/// purpose: 解析 pi --list-models 未过滤表格为 provider/model exact ids
/// returns: 保序且无重复的 exact ids
/// errors: 非 UTF-8、header/row 异常、重复或空 catalog 时返回 ProviderError
/// ---
pub(crate) fn parse_pi_list_models_table(bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
    crate::provider::model_catalog::parse_pi_catalog(bytes)
        .map(|records| records.into_iter().map(|record| record.id).collect())
        .map_err(|error| ProviderError::Command(error.pi_legacy_message()))
}

/// ---
/// purpose: 在动态 catalog 中只接受一个字节精确的 qualified model
/// returns: exact catalog id
/// errors: 非 qualified、wildcard、缺失或重复时返回 ProviderError
/// ---
pub(crate) fn select_exact_pi_model(
    models: &[String],
    requested: &str,
) -> Result<String, ProviderError> {
    if requested.trim() != requested
        || requested.is_empty()
        || requested.contains('*')
        || !requested
            .split_once('/')
            .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
    {
        return Err(ProviderError::Command(format!(
            "Pi model must be a qualified exact catalog id: {requested:?}"
        )));
    }
    let mut matches = models.iter().filter(|model| model.as_str() == requested);
    let selected = matches.next().ok_or_else(|| {
        ProviderError::Command(format!("Pi model is not in the live catalog: {requested}"))
    })?;
    if matches.next().is_some() {
        return Err(ProviderError::Command(format!(
            "Pi model catalog selection is ambiguous: {requested}"
        )));
    }
    Ok(selected.clone())
}

/// ---
/// purpose: 校验 Pi PATH entry 与 live catalog；MCP 能力在实际 Pi context 验证
/// returns: PATH entry 与 catalog receipt 完整时成功
/// errors: 缺路径或 catalog receipt 时返回 ProviderError
/// ---
pub(crate) fn validate_pi_executable_chain(chain: &PiExecutableChain) -> Result<(), ProviderError> {
    if !chain.path_entry.is_absolute()
        || chain.path_entry.file_name().is_none_or(|name| name != "pi")
        || chain.catalog_sha256.len() != 64
        || !chain
            .catalog_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || chain.catalog_sha256.bytes().all(|byte| byte == b'0')
    {
        return Err(ProviderError::Command(
            "Pi executable chain does not provide the required public protocol".to_string(),
        ));
    }
    Ok(())
}

pub(crate) struct PiWrapperRequest<'a> {
    pub destination: &'a Path,
    pub candidate_executable: &'a Path,
    pub mcp_config: &'a McpConfig,
    pub team_id: &'a str,
    pub agent_id: &'a str,
    pub workspace: &'a Path,
    pub include_tools: &'a [&'a str],
}

fn team_server(
    config: &McpConfig,
) -> Result<serde_json::Map<String, serde_json::Value>, ProviderError> {
    let server = config
        .raw
        .get("team_orchestrator")
        .or_else(|| config.raw.pointer("/mcpServers/team_orchestrator"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .ok_or_else(|| {
            ProviderError::Command("resolved MCP config is missing team_orchestrator".to_string())
        })?;
    Ok(server)
}

fn candidate_from_mcp_config(config: &McpConfig) -> Result<PathBuf, ProviderError> {
    let server = team_server(config)?;
    let command = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            ProviderError::Command("resolved team_orchestrator MCP command is missing".to_string())
        })?;
    if !command.is_absolute() {
        return Err(ProviderError::Command(
            "resolved team_orchestrator MCP command must be absolute".to_string(),
        ));
    }
    Ok(command)
}

fn render_pi_wrapper(request: &PiWrapperRequest<'_>) -> Result<String, ProviderError> {
    let allowed = request.include_tools.iter().copied().collect::<BTreeSet<_>>();
    if allowed.is_empty()
        || allowed.len() != request.include_tools.len()
        || allowed
            .iter()
            .any(|tool| !["send_message", "report_result", "get_team_status"].contains(tool))
    {
        return Err(ProviderError::Command(
            "Pi MCP tools must be a non-empty subset of the three Team tools".to_string(),
        ));
    }
    let configured_candidate = candidate_from_mcp_config(request.mcp_config)?;
    let expected = std::fs::canonicalize(request.candidate_executable)
        .unwrap_or_else(|_| request.candidate_executable.to_path_buf());
    let configured = std::fs::canonicalize(&configured_candidate).unwrap_or(configured_candidate);
    if configured != expected {
        return Err(ProviderError::Command(format!(
            "Pi wrapper candidate mismatch: expected {} got {}",
            expected.display(),
            configured.display()
        )));
    }
    let mut server = team_server(request.mcp_config)?;
    server.insert(
        "command".to_string(),
        serde_json::Value::String(expected.to_string_lossy().into_owned()),
    );
    server.insert(
        "includeTools".to_string(),
        serde_json::Value::Array(
            request
                .include_tools
                .iter()
                .map(|tool| serde_json::Value::String((*tool).to_string()))
                .collect(),
        ),
    );
    let env = server
        .entry("env".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| ProviderError::Command("Pi MCP server env must be an object".to_string()))?;
    env.insert(
        "TEAM_AGENT_WORKSPACE".to_string(),
        serde_json::Value::String(request.workspace.to_string_lossy().into_owned()),
    );
    env.insert(
        "TEAM_AGENT_ID".to_string(),
        serde_json::Value::String(request.agent_id.to_string()),
    );
    env.insert(
        "TEAM_AGENT_AGENT_ID".to_string(),
        serde_json::Value::String(request.agent_id.to_string()),
    );
    env.insert(
        "TEAM_AGENT_OWNER_TEAM_ID".to_string(),
        serde_json::Value::String(request.team_id.to_string()),
    );
    let definition = serde_json::to_string(&serde_json::Value::Object(server))
        .map_err(|error| ProviderError::Command(format!("serialize Pi MCP server: {error}")))?;
    let receipt_root = request
        .destination
        .parent()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            ProviderError::Command("Pi MCP wrapper requires an absolute seat directory".to_string())
        })?;
    let receipt_root = serde_json::to_string(&receipt_root.to_string_lossy())
        .map_err(|error| ProviderError::Command(format!("serialize Pi MCP seat directory: {error}")))?;
    Ok(format!(
        "const teamMcpDefinition = {definition};\nconst teamMcpReceiptRoot = {receipt_root};\n\n{}",
        include_str!("pi_mcp_extension.js")
    ))
}

/// ---
/// purpose: 静态核对 wrapper 使用原生/公共 MCP 能力模板与指定 candidate
/// returns: 模板完整且 candidate、seat 目录正确时成功
/// errors: 模板、candidate 或 seat 目录不匹配时返回 ProviderError
/// ---
pub(crate) fn validate_pi_wrapper_source(
    source: &str,
    candidate_executable: &Path,
) -> Result<(), ProviderError> {
    let invalid = || {
        ProviderError::Command(
            "Pi wrapper source is not the owned MCP capability template".to_string(),
        )
    };
    let prefix = source
        .strip_suffix(include_str!("pi_mcp_extension.js"))
        .ok_or_else(invalid)?;
    let (definition, root) = prefix
        .strip_prefix("const teamMcpDefinition = ")
        .and_then(|value| value.split_once(";\nconst teamMcpReceiptRoot = "))
        .ok_or_else(invalid)?;
    let definition: serde_json::Value = serde_json::from_str(definition).map_err(|_| invalid())?;
    let command = definition
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(invalid)?;
    let root: String = serde_json::from_str(root.strip_suffix(";\n\n").ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    let expected = std::fs::canonicalize(candidate_executable)
        .unwrap_or_else(|_| candidate_executable.to_path_buf());
    if Path::new(command) != expected || !Path::new(&root).is_absolute() {
        return Err(invalid());
    }
    Ok(())
}

/// ---
/// purpose: 在 seat root 原子写入 mode 0600 的 runtime MCP registration extension
/// returns: 最终 wrapper 路径
/// errors: identity、序列化、目录、写盘或 rename 失败时返回 ProviderError
/// ---
pub(crate) fn write_pi_wrapper(request: PiWrapperRequest<'_>) -> Result<PathBuf, ProviderError> {
    write_pi_wrapper_with_publish(request, |from, to| std::fs::rename(from, to))
}

pub(crate) fn write_pi_wrapper_with_publish(
    request: PiWrapperRequest<'_>,
    publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<PathBuf, ProviderError> {
    let source = render_pi_wrapper(&request)?;
    validate_pi_wrapper_source(&source, request.candidate_executable)?;
    let parent = request.destination.parent().ok_or_else(|| {
        ProviderError::Io("Pi wrapper destination has no parent directory".to_string())
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        ProviderError::Io(format!(
            "Pi wrapper atomic write stage=create_parent path={}: {error}",
            parent.display()
        ))
    })?;
    let temp = parent.join(format!(".team-mcp.ts.{}.tmp", std::process::id()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| {
                ProviderError::Io(format!(
                    "Pi wrapper atomic write stage=open_temp path={}: {error}",
                    temp.display()
                ))
            })?;
        file.write_all(source.as_bytes()).map_err(|error| {
            ProviderError::Io(format!(
                "Pi wrapper atomic write stage=write_temp path={}: {error}",
                temp.display()
            ))
        })?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                ProviderError::Io(format!(
                    "Pi wrapper atomic write stage=chmod_temp_fd path={}: {error}",
                    temp.display()
                ))
            })?;
        file.sync_all().map_err(|error| {
            ProviderError::Io(format!(
                "Pi wrapper atomic write stage=fsync_temp path={}: {error}",
                temp.display()
            ))
        })?;
        publish(&temp, request.destination).map_err(|error| {
            ProviderError::Io(format!(
                "Pi wrapper atomic write stage=rename_publish from={} to={}: {error}",
                temp.display(),
                request.destination.display()
            ))
        })?;
        Ok(request.destination.to_path_buf())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    write_result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PiLeaderArgs {
    pub model: Option<String>,
    pub effort: Option<ProviderEffort>,
}

/// ---
/// purpose: 解析 Pi leader 唯一允许的 model/thinking 输入
/// returns: 仅保留调用方显式给出的 qualified model 与 effort
/// errors: 重复、未知或 materializer-owned flag 时返回 ProviderError
/// ---
pub(crate) fn parse_pi_leader_args(args: &[String]) -> Result<PiLeaderArgs, ProviderError> {
    let args = args.strip_prefix(&["--".to_string()]).unwrap_or(args);
    let mut model = None;
    let mut effort = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args.get(index + 1).ok_or_else(|| {
            ProviderError::Command(format!("Pi leader argument {flag} requires a value"))
        })?;
        match flag {
            "--model" if model.is_none() => model = Some(value.clone()),
            "--thinking" if effort.is_none() => {
                let parsed = ProviderEffort::parse(value).ok_or_else(|| {
                    ProviderError::Command(format!("unknown Pi thinking effort {value:?}"))
                })?;
                effort = parsed
                    .resolve_for_provider(Provider::Pi)
                    .map_err(|reason| ProviderError::Command(reason.to_string()))?;
            }
            _ => {
                return Err(ProviderError::Command(format!(
                    "Pi leader accepts only one --model and one --thinking; got {flag:?}"
                )));
            }
        }
        index += 2;
    }
    if model.as_ref().is_some_and(|model| {
        model.contains('*')
            || !model
                .split_once('/')
                .is_some_and(|(provider, name)| !provider.is_empty() && !name.is_empty())
    }) {
        return Err(ProviderError::Command(
            "Pi leader --model must be a qualified exact id".to_string(),
        ));
    }
    Ok(PiLeaderArgs { model, effort })
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The shell's own PATH choice: the first executable `pi`, kept as found.
fn pi_path_entry() -> Result<PathBuf, ProviderError> {
    let path = std::env::var_os("PATH")
        .ok_or_else(|| ProviderError::Command("PATH is not set".to_string()))?;
    team_agent_contract::host::command::PublicCommand::resolve("pi", &path)
        .map(|command| command.entry)
        .map_err(|_| ProviderError::Command("Pi executable not found on PATH".to_string()))
}

fn command_stdout(executable: &Path, args: &[&str]) -> Result<Vec<u8>, ProviderError> {
    let output = Command::new(executable)
        .args(args)
        .output()
        .map_err(|error| {
            ProviderError::Io(format!(
                "{} {}: {error}",
                executable.display(),
                args.join(" ")
            ))
        })?;
    if !output.status.success() {
        return Err(ProviderError::Command(format!(
            "{} {} exited {}",
            executable.display(),
            args.join(" "),
            output.status
        )));
    }
    Ok(output.stdout)
}

/// ---
/// purpose: 从当前 PATH 环境测量 Pi launch/real executable 与 live catalog，不检查 Pi packages
/// returns: 已验证的 chain 与同次 live catalog exact ids
/// errors: 缺失、命令失败或任一公共 protocol capability 缺失时返回 ProviderError
/// ---
pub(crate) fn resolve_pi_executable_chain(
) -> Result<(PiExecutableChain, Vec<String>), ProviderError> {
    // Probe through the PATH entry itself so a user wrapper sees every probe.
    let path_entry = pi_path_entry()?;
    let version =
        String::from_utf8(command_stdout(&path_entry, &["--version"])?).map_err(|error| {
            ProviderError::Command(format!("Pi version output is not UTF-8: {error}"))
        })?;
    let catalog = command_stdout(&path_entry, &["--list-models"])?;
    let models = parse_pi_list_models_table(&catalog)?;
    let chain = PiExecutableChain {
        path_entry,
        pi_version: version.trim().to_string(),
        catalog_sha256: sha256(&catalog),
    };
    validate_pi_executable_chain(&chain)?;
    Ok((chain, models))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PiSessionScope {
    Isolated,
    NativeDefault,
}

/// Zero-argument managed Pi is the interactive leader entry. Only that route
/// may use Pi's cwd-keyed native session root; every explicit or worker route
/// remains Team Agent session-isolated.
pub(crate) fn pi_leader_session_scope(
    provider_args: &[String],
    external_path: bool,
    allow_nested_attach: bool,
) -> PiSessionScope {
    if provider_args.is_empty() && !external_path && !allow_nested_attach {
        PiSessionScope::NativeDefault
    } else {
        PiSessionScope::Isolated
    }
}

pub(crate) struct PiMaterializeRequest<'a> {
    pub workspace: &'a Path,
    pub team_id: &'a str,
    pub agent_id: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<ProviderEffort>,
    pub system_prompt: &'a str,
    pub team_mcp_tools: &'a [&'a str],
    pub mcp_config: &'a McpConfig,
    pub session_scope: PiSessionScope,
}

/// ---
/// purpose: 物化 Pi leader 的 wrapper 和 fresh CommandPlan
/// returns: fresh Pi CommandPlan 不传 --session-id；an isolated leader carries its session root,
///          while the interactive leader preserves Pi's native default root
/// errors: resolver、exact model、wrapper 或 argv 构造失败时返回 ProviderError
/// ---
pub(crate) fn materialize_pi_plan(
    request: PiMaterializeRequest<'_>,
) -> Result<CommandPlan, ProviderError> {
    let (_chain, catalog) = resolve_pi_executable_chain()?;
    let model = request
        .model
        .map(|model| select_exact_pi_model(&catalog, model))
        .transpose()?;
    let paths = pi_seat_paths(request.workspace, request.team_id, request.agent_id);
    let session_dir = match request.session_scope {
        PiSessionScope::Isolated => Some(paths.sessions.as_path()),
        PiSessionScope::NativeDefault => None,
    };
    if let Some(session_dir) = session_dir {
        std::fs::create_dir_all(session_dir)
            .map_err(|error| ProviderError::Io(format!("{}: {error}", session_dir.display())))?;
    }
    let candidate = candidate_from_mcp_config(request.mcp_config)?;
    let wrapper = write_pi_wrapper(PiWrapperRequest {
        destination: &paths.wrapper,
        candidate_executable: &candidate,
        mcp_config: request.mcp_config,
        team_id: request.team_id,
        agent_id: request.agent_id,
        workspace: request.workspace,
        include_tools: request.team_mcp_tools,
    })?;
    let argv = build_pi_command_argv(PiCommandRequest {
        extension: &wrapper,
        model: model.as_deref(),
        effort: request.effort,
        system_prompt: request.system_prompt,
        session_dir,
        agent_id: request.agent_id,
    })?;
    Ok(CommandPlan {
        argv,
        expected_session_id: None,
        provider_projects_root: session_dir.map(Path::to_path_buf),
        managed_mcp_config: false,
    })
}

pub(crate) struct PiCommandRequest<'a> {
    pub extension: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Option<ProviderEffort>,
    pub system_prompt: &'a str,
    /// Team-owned session root for an isolated leader; `None` preserves Pi's native default.
    pub session_dir: Option<&'a Path>,
    pub agent_id: &'a str,
}

/// ---
/// purpose: 从已验证 semantic request 构造仅追加 Team Agent MCP/提示/session root 的 fresh Pi argv
/// returns: exact ordered argv，argv[0] 固定为裸 `pi`；Pi 自己分配 session id
/// errors: 必需字段为空时返回 ProviderError
/// ---
pub(crate) fn build_pi_command_argv(
    request: PiCommandRequest<'_>,
) -> Result<Vec<String>, ProviderError> {
    if request.extension.as_os_str().is_empty()
        || request
            .session_dir
            .is_some_and(|session_dir| session_dir.as_os_str().is_empty())
        || request.model.is_some_and(|model| model.trim().is_empty())
        || request.system_prompt.trim().is_empty()
        || request.agent_id.trim().is_empty()
    {
        return Err(ProviderError::Command(
            "Pi command requires extension, prompt, and agent id".to_string(),
        ));
    }
    // The command stays PATH-resolved at spawn time, so a user wrapper runs.
    let mut argv = vec![
        "pi".to_string(),
        "-e".to_string(),
        request.extension.to_string_lossy().into_owned(),
    ];
    append_opt_pair(&mut argv, "--model", request.model);
    if let Some(effort) = request.effort {
        argv.push("--thinking".to_string());
        argv.push(effort.as_str().to_string());
    }
    append_pair(&mut argv, "--append-system-prompt", request.system_prompt);
    if let Some(session_dir) = request.session_dir {
        append_pair(&mut argv, "--session-dir", session_dir.to_string_lossy().into_owned());
    }
    // Passing `--session-id` makes Pi warn and then create: Pi names a fresh session.
    append_pair(&mut argv, "--name", request.agent_id);
    Ok(argv)
}
