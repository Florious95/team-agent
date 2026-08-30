//! ---
//! purpose: Pi executable/catalog/adapter 验证、per-seat wrapper 与共享启动计划物化
//! contract:
//!   provides:
//!     - name: materialize_pi_plan
//!       what: 为 leader 与 TeamMate 生成同一来源的 Pi CommandPlan
//!     - name: write_pi_wrapper
//!       what: 原子写入 0600 isolated lazy MCP wrapper
//! boundary:
//!   - 不枚举 Pi session；resume 只重验调用方给出的 exact backing
//!   - 不投递消息、不执行 cleanup 或 doctor 判定
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

use crate::model::enums::ProviderEffort;
use crate::provider::adapters::pi::{build_pi_command_argv, PiCommandRequest, PiSessionSelector};
use crate::provider::{CommandPlan, McpConfig, ProviderError, SessionId};

const PI_VERSION: &str = "0.84.3";
const ADAPTER_NAME: &str = "pi-mcp-adapter";
const ADAPTER_VERSION: &str = "2.30.0";
const ADAPTER_ENTRY: &str = "./index.ts";
const ADAPTER_PACKAGE_SHA256: &str =
    "ce8b8b6154e83e9732c58bd993e7ed69390617616f4f0e7330274d5ee9e2f620";
const ADAPTER_INDEX_SHA256: &str =
    "16d260ac25b66346baab6ecef76680324336953bafc7be8cf95b3df5c611b89e";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PiExecutableFileType {
    Wrapper,
    Symlink,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PiAdapterIdentity {
    pub package_name: String,
    pub version: String,
    pub extension_entry: String,
    pub package_json: PathBuf,
    pub index_ts: PathBuf,
    pub package_json_sha256: String,
    pub index_ts_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PiExecutableChain {
    pub path_entry: PathBuf,
    pub path_entry_type: PiExecutableFileType,
    pub launch_executable: PathBuf,
    pub real_binary: PathBuf,
    pub pi_version: String,
    pub catalog_sha256: String,
    pub adapter: PiAdapterIdentity,
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
/// purpose: 为 Pi fresh 启动生成框架预分配 UUID
/// returns: 新 SessionId
/// ---
pub(crate) fn new_pi_session_id() -> SessionId {
    SessionId::new(crate::provider::adapter::next_session_token())
}

/// ---
/// purpose: 解析 pi --list-models 未过滤表格为 provider/model exact ids
/// returns: 保序且无重复的 exact ids
/// errors: 非 UTF-8、header/row 异常、重复或空 catalog 时返回 ProviderError
/// ---
pub(crate) fn parse_pi_list_models_table(bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ProviderError::Command(format!("Pi model catalog is not UTF-8: {error}"))
    })?;
    let mut lines = text.lines();
    let header = lines
        .next()
        .ok_or_else(|| ProviderError::Command("Pi model catalog is empty".to_string()))?;
    let header_columns = header.split_whitespace().collect::<Vec<_>>();
    if header_columns.get(0) != Some(&"provider") || header_columns.get(1) != Some(&"model") {
        return Err(ProviderError::Command(
            "Pi model catalog has an unexpected header".to_string(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 2 || columns[0].is_empty() || columns[1].is_empty() {
            return Err(ProviderError::Command(format!(
                "Pi model catalog row {} is malformed",
                index + 2
            )));
        }
        let exact = format!("{}/{}", columns[0], columns[1]);
        if !seen.insert(exact.clone()) {
            return Err(ProviderError::Command(format!(
                "Pi model catalog contains duplicate exact id {exact:?}"
            )));
        }
        models.push(exact);
    }
    if models.is_empty() {
        return Err(ProviderError::Command(
            "Pi model catalog contains no models".to_string(),
        ));
    }
    Ok(models)
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
/// purpose: 对比选定与启动后客观观测到的 Pi model identity
/// returns: 完全相等且非空时成功
/// errors: 缺失或不匹配时返回 ProviderError
/// ---
pub(crate) fn verify_started_pi_model(selected: &str, observed: &str) -> Result<(), ProviderError> {
    if observed.is_empty() || selected != observed {
        return Err(ProviderError::Command(format!(
            "Pi started model mismatch: selected={selected:?} observed={observed:?}"
        )));
    }
    Ok(())
}

/// ---
/// purpose: 校验首版支持的 pi-mcp-adapter package/version/entry/digests
/// returns: 所有 identity 字段命中冻结 snapshot 时成功
/// errors: 任一字段漂移时返回 ProviderError
/// ---
pub(crate) fn validate_pi_adapter_identity(
    identity: &PiAdapterIdentity,
) -> Result<(), ProviderError> {
    if identity.package_name != ADAPTER_NAME
        || identity.version != ADAPTER_VERSION
        || identity.extension_entry != ADAPTER_ENTRY
        || !identity.package_json.is_absolute()
        || !identity.index_ts.is_absolute()
        || identity.package_json_sha256 != ADAPTER_PACKAGE_SHA256
        || identity.index_ts_sha256 != ADAPTER_INDEX_SHA256
    {
        return Err(ProviderError::Command(
            "Pi MCP adapter identity does not match the supported 2.30.0 snapshot".to_string(),
        ));
    }
    Ok(())
}

/// ---
/// purpose: 校验 Pi launch/real path、版本、catalog 与 adapter identity
/// returns: 首版冻结 executable chain 完整时成功
/// errors: 缺路径或 identity 漂移时返回 ProviderError
/// ---
pub(crate) fn validate_pi_executable_chain(chain: &PiExecutableChain) -> Result<(), ProviderError> {
    if !chain.path_entry.is_absolute()
        || !chain.launch_executable.is_absolute()
        || !chain.real_binary.is_absolute()
        || chain.path_entry_type != PiExecutableFileType::Wrapper
        || chain.launch_executable != chain.path_entry
        || chain.real_binary == chain.launch_executable
        || chain.pi_version != PI_VERSION
        || chain.catalog_sha256.len() != 64
        || !chain
            .catalog_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || chain.catalog_sha256.bytes().all(|byte| byte == b'0')
    {
        return Err(ProviderError::Command(
            "Pi executable chain does not match the supported 0.84.3 snapshot".to_string(),
        ));
    }
    validate_pi_adapter_identity(&chain.adapter)
}

pub(crate) struct PiWrapperRequest<'a> {
    pub destination: &'a Path,
    pub adapter: &'a PiAdapterIdentity,
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
    validate_pi_adapter_identity(request.adapter)?;
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
        "lifecycle".to_string(),
        serde_json::Value::String("lazy".to_string()),
    );
    server.insert("directTools".to_string(), serde_json::Value::Bool(false));
    server.insert(
        "toolPrefix".to_string(),
        serde_json::Value::String("server".to_string()),
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
    let config = serde_json::json!({
        "mcpServers": {
            "team_orchestrator": serde_json::Value::Object(server)
        }
    });
    let import = serde_json::to_string(&request.adapter.index_ts)
        .map_err(|error| ProviderError::Command(format!("serialize Pi adapter path: {error}")))?;
    let config = serde_json::to_string_pretty(&config)
        .map_err(|error| ProviderError::Command(format!("serialize Pi MCP config: {error}")))?;
    Ok(format!(
        "import {{ createMcpAdapter }} from {import};\n\nexport default createMcpAdapter({{ config: {config} }});\n"
    ))
}

/// ---
/// purpose: 静态核对 wrapper 的 exact import、candidate 与 lazy isolation 字段
/// returns: wrapper 保持冻结隔离合同则成功
/// errors: identity 缺失或出现 mcp-config fallback 时返回 ProviderError
/// ---
pub(crate) fn validate_pi_wrapper_source(
    source: &str,
    adapter: &PiAdapterIdentity,
    candidate_executable: &Path,
) -> Result<(), ProviderError> {
    let adapter_path = adapter.index_ts.to_string_lossy();
    let candidate = candidate_executable.to_string_lossy();
    if !source.contains(adapter_path.as_ref())
        || !source.contains(candidate.as_ref())
        || !source.contains("createMcpAdapter")
        || !source.contains("lazy")
        || !source.contains("directTools")
        || !source.contains("false")
        || !source.contains("toolPrefix")
        || !source.contains("server")
        || source.contains("--mcp-config")
    {
        return Err(ProviderError::Command(
            "Pi wrapper source does not preserve the isolated adapter/candidate contract"
                .to_string(),
        ));
    }
    Ok(())
}

/// ---
/// purpose: 在 seat root 原子写入 mode 0600 的 isolated lazy MCP wrapper
/// returns: 最终 wrapper 路径
/// errors: identity、序列化、目录、写盘或 rename 失败时返回 ProviderError
/// ---
pub(crate) fn write_pi_wrapper(request: PiWrapperRequest<'_>) -> Result<PathBuf, ProviderError> {
    let source = render_pi_wrapper(&request)?;
    validate_pi_wrapper_source(&source, request.adapter, request.candidate_executable)?;
    let parent = request.destination.parent().ok_or_else(|| {
        ProviderError::Io("Pi wrapper destination has no parent directory".to_string())
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", parent.display())))?;
    let temp = parent.join(format!(".team-mcp.ts.{}.tmp", std::process::id()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| ProviderError::Io(format!("{}: {error}", temp.display())))?;
        file.write_all(source.as_bytes())
            .map_err(|error| ProviderError::Io(format!("{}: {error}", temp.display())))?;
        file.sync_all()
            .map_err(|error| ProviderError::Io(format!("{}: {error}", temp.display())))?;
        std::fs::rename(&temp, request.destination).map_err(|error| {
            ProviderError::Io(format!(
                "rename {} to {}: {error}",
                temp.display(),
                request.destination.display()
            ))
        })?;
        std::fs::set_permissions(request.destination, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                ProviderError::Io(format!("{}: {error}", request.destination.display()))
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
    pub model: String,
    pub effort: ProviderEffort,
}

/// ---
/// purpose: 解析 Pi leader 唯一允许的 model/thinking 输入
/// returns: 显式 qualified model 与 effort
/// errors: 缺失、重复、未知或 materializer-owned flag 时返回 ProviderError
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
                effort = ProviderEffort::parse(value);
                if effort.is_none() {
                    return Err(ProviderError::Command(format!(
                        "unknown Pi thinking effort {value:?}"
                    )));
                }
            }
            _ => {
                return Err(ProviderError::Command(format!(
                    "Pi leader accepts only one --model and one --thinking; got {flag:?}"
                )));
            }
        }
        index += 2;
    }
    let model = model.ok_or_else(|| {
        ProviderError::Command("Pi leader requires an explicit --model".to_string())
    })?;
    if model.contains('*')
        || !model
            .split_once('/')
            .is_some_and(|(provider, name)| !provider.is_empty() && !name.is_empty())
    {
        return Err(ProviderError::Command(
            "Pi leader --model must be a qualified exact id".to_string(),
        ));
    }
    Ok(PiLeaderArgs {
        model,
        effort: effort.ok_or_else(|| {
            ProviderError::Command("Pi leader requires an explicit --thinking".to_string())
        })?,
    })
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn executable_file_type(path: &Path) -> Result<PiExecutableFileType, ProviderError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", path.display())))?;
    if metadata.file_type().is_symlink() {
        return Ok(PiExecutableFileType::Symlink);
    }
    let bytes = std::fs::read(path)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", path.display())))?;
    if bytes.starts_with(b"#!") {
        Ok(PiExecutableFileType::Wrapper)
    } else {
        Ok(PiExecutableFileType::Binary)
    }
}

fn pi_path_entries() -> Result<Vec<PathBuf>, ProviderError> {
    let path = std::env::var_os("PATH")
        .ok_or_else(|| ProviderError::Command("PATH is not set".to_string()))?;
    let mut seen = BTreeSet::new();
    let entries = std::env::split_paths(&path)
        .map(|directory| directory.join("pi"))
        .filter(|candidate| candidate.is_file())
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Err(ProviderError::Command(
            "Pi executable not found on PATH".to_string(),
        ));
    }
    Ok(entries)
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

fn discover_pi_adapter(launch_executable: &Path) -> Result<PiAdapterIdentity, ProviderError> {
    let listing = command_stdout(launch_executable, &["list", "--no-approve"])?;
    let listing = std::str::from_utf8(&listing).map_err(|error| {
        ProviderError::Command(format!("Pi package list is not UTF-8: {error}"))
    })?;
    let mut lines = listing.lines();
    let mut package_root = None;
    while let Some(line) = lines.next() {
        if line.trim() == "npm:pi-mcp-adapter" {
            package_root = lines
                .next()
                .map(str::trim)
                .filter(|path| Path::new(path).is_absolute())
                .map(PathBuf::from);
            break;
        }
    }
    let package_root = package_root.ok_or_else(|| {
        ProviderError::Command("Pi package list does not resolve npm:pi-mcp-adapter".to_string())
    })?;
    let package_json = package_root.join("package.json");
    let package_bytes = std::fs::read(&package_json)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", package_json.display())))?;
    let package: serde_json::Value = serde_json::from_slice(&package_bytes)
        .map_err(|error| ProviderError::Command(format!("{}: {error}", package_json.display())))?;
    let extension_entry = package
        .pointer("/pi/extensions")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| entries.iter().find_map(serde_json::Value::as_str))
        .unwrap_or("")
        .to_string();
    let index_ts = package_root.join(extension_entry.trim_start_matches("./"));
    let index_bytes = std::fs::read(&index_ts)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", index_ts.display())))?;
    let identity = PiAdapterIdentity {
        package_name: package
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        version: package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        extension_entry,
        package_json,
        index_ts,
        package_json_sha256: sha256(&package_bytes),
        index_ts_sha256: sha256(&index_bytes),
    };
    validate_pi_adapter_identity(&identity)?;
    Ok(identity)
}

/// ---
/// purpose: 从当前 PATH 环境测量 Pi launch/real executable、catalog 与 adapter
/// returns: 已验证的 chain 与同次 live catalog exact ids
/// errors: 缺失、命令失败或任一冻结 identity 漂移时返回 ProviderError
/// ---
pub(crate) fn resolve_pi_executable_chain(
) -> Result<(PiExecutableChain, Vec<String>), ProviderError> {
    let path_entries = pi_path_entries()?;
    let path_entry = path_entries
        .iter()
        .find_map(|candidate| {
            (executable_file_type(candidate).ok()? == PiExecutableFileType::Wrapper)
                .then(|| candidate.clone())
        })
        .ok_or_else(|| {
            ProviderError::Command("Pi PATH has no verified wrapper launch executable".to_string())
        })?;
    let path_entry_type = PiExecutableFileType::Wrapper;
    let version =
        String::from_utf8(command_stdout(&path_entry, &["--version"])?).map_err(|error| {
            ProviderError::Command(format!("Pi version output is not UTF-8: {error}"))
        })?;
    let version = version.trim().to_string();
    if version != PI_VERSION {
        return Err(ProviderError::Command(format!(
            "unsupported Pi version {version:?}; expected {PI_VERSION}"
        )));
    }
    let launch_identity = std::fs::canonicalize(&path_entry)
        .map_err(|error| ProviderError::Io(format!("{}: {error}", path_entry.display())))?;
    let real_binary = path_entries
        .iter()
        .find_map(|candidate| {
            let real = std::fs::canonicalize(candidate).ok()?;
            (real != launch_identity).then_some(real)
        })
        .ok_or_else(|| {
            ProviderError::Command(
                "Pi PATH wrapper has no independently resolved real executable".to_string(),
            )
        })?;
    let real_version =
        String::from_utf8(command_stdout(&real_binary, &["--version"])?).map_err(|error| {
            ProviderError::Command(format!(
                "Pi real binary version output is not UTF-8: {error}"
            ))
        })?;
    if real_version.trim() != PI_VERSION {
        return Err(ProviderError::Command(format!(
            "unsupported Pi real binary version {:?}; expected {PI_VERSION}",
            real_version.trim()
        )));
    }
    let catalog = command_stdout(&path_entry, &["--list-models"])?;
    let models = parse_pi_list_models_table(&catalog)?;
    let adapter = discover_pi_adapter(&path_entry)?;
    let chain = PiExecutableChain {
        path_entry: path_entry.clone(),
        path_entry_type,
        launch_executable: path_entry,
        real_binary,
        pi_version: version,
        catalog_sha256: sha256(&catalog),
        adapter,
    };
    validate_pi_executable_chain(&chain)?;
    Ok((chain, models))
}

pub(crate) struct PiMaterializeRequest<'a> {
    pub workspace: &'a Path,
    pub team_id: &'a str,
    pub agent_id: &'a str,
    pub model: &'a str,
    pub effort: ProviderEffort,
    pub system_prompt: &'a str,
    pub tool_categories: &'a [&'a str],
    pub team_mcp_tools: &'a [&'a str],
    pub mcp_config: &'a McpConfig,
}

/// ---
/// purpose: 共享物化 Pi leader 与 TeamMate 的 wrapper、UUID 和 CommandPlan
/// returns: fresh Pi CommandPlan，含 expected UUID 与 seat session root
/// errors: resolver、exact model、wrapper 或 argv 构造失败时返回 ProviderError
/// ---
pub(crate) fn materialize_pi_plan(
    request: PiMaterializeRequest<'_>,
) -> Result<CommandPlan, ProviderError> {
    materialize_pi_plan_with_session(request, None)
}

/// ---
/// purpose: 用上游已验证的 captured id/path 物化 Pi exact-path resume 计划
/// returns: 与 fresh 共用 resolver/wrapper/argv builder，且不再生成 session id
/// errors: backing 缺失或越出席位 root 时拒绝；header id/cwd 由 session scanner 在调用前验证
/// ---
pub(crate) fn materialize_pi_resume_plan(
    request: PiMaterializeRequest<'_>,
    session_id: &SessionId,
    session_path: &Path,
    spawn_cwd: &Path,
) -> Result<CommandPlan, ProviderError> {
    materialize_pi_plan_with_session(request, Some((session_id, session_path, spawn_cwd)))
}

fn materialize_pi_plan_with_session(
    request: PiMaterializeRequest<'_>,
    resume: Option<(&SessionId, &Path, &Path)>,
) -> Result<CommandPlan, ProviderError> {
    let (chain, catalog) = resolve_pi_executable_chain()?;
    let model = select_exact_pi_model(&catalog, request.model)?;
    let paths = pi_seat_paths(request.workspace, request.team_id, request.agent_id);
    let resume = if let Some((session_id, session_path, spawn_cwd)) = resume {
        let session_root = std::fs::canonicalize(&paths.sessions).map_err(|error| {
            ProviderError::ResumeUnavailable(format!(
                "Pi session root is unavailable {}: {error}",
                paths.sessions.display()
            ))
        })?;
        let exact_path = std::fs::canonicalize(session_path).map_err(|error| {
            ProviderError::ResumeUnavailable(format!(
                "Pi exact session backing is unavailable {}: {error}",
                session_path.display()
            ))
        })?;
        if !exact_path.starts_with(&session_root) {
            return Err(ProviderError::ResumeUnavailable(format!(
                "Pi exact session backing is outside the recorded seat root: {}",
                exact_path.display()
            )));
        }
        let _ = spawn_cwd;
        Some((session_id, exact_path))
    } else {
        std::fs::create_dir_all(&paths.sessions)
            .map_err(|error| ProviderError::Io(format!("{}: {error}", paths.sessions.display())))?;
        None
    };
    let candidate = candidate_from_mcp_config(request.mcp_config)?;
    let wrapper = write_pi_wrapper(PiWrapperRequest {
        destination: &paths.wrapper,
        adapter: &chain.adapter,
        candidate_executable: &candidate,
        mcp_config: request.mcp_config,
        team_id: request.team_id,
        agent_id: request.agent_id,
        workspace: request.workspace,
        include_tools: request.team_mcp_tools,
    })?;
    let (argv, expected_session_id) = if let Some((_session_id, exact_path)) = resume {
        let argv = build_pi_command_argv(PiCommandRequest {
            executable: &chain.launch_executable,
            extension: &wrapper,
            model: &model,
            effort: request.effort,
            system_prompt: request.system_prompt,
            tool_categories: request.tool_categories,
            session_dir: &paths.sessions,
            session: PiSessionSelector::Resume { path: &exact_path },
            agent_id: request.agent_id,
        })?;
        (argv, None)
    } else {
        let session_id = new_pi_session_id();
        let argv = build_pi_command_argv(PiCommandRequest {
            executable: &chain.launch_executable,
            extension: &wrapper,
            model: &model,
            effort: request.effort,
            system_prompt: request.system_prompt,
            tool_categories: request.tool_categories,
            session_dir: &paths.sessions,
            session: PiSessionSelector::Fresh {
                session_id: session_id.as_str(),
            },
            agent_id: request.agent_id,
        })?;
        (argv, Some(session_id))
    };
    Ok(CommandPlan {
        argv,
        expected_session_id,
        provider_projects_root: Some(paths.sessions),
        managed_mcp_config: false,
    })
}
