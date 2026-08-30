//! ---
//! purpose: 各 provider 的 MCP 配置落盘与 argv 指向，含 grok 目录作用域配置的清洗与前置检查
//! contract:
//!   provides:
//!     - name: resolve_mcp_config
//!       what: 把 MCP 配置里的 workspace、agent_id 与 team_id 占位符替换掉
//!     - name: write_worker_mcp_config_for_provider
//!       what: 写出 per-agent 的 MCP 配置文件，copilot 走字段名翻译
//!     - name: apply_grok_mcp_overlay
//!       what: 写 workspace 下 grok 的项目作用域配置，是该文件的唯一写者
//!     - name: reconcile_grok_toml_per_seat_keys
//!       what: 起 grok 席前清掉共享 toml 里遗留的 per-seat 键并留审计事件
//!     - name: ensure_grok_login_and_folder_trust
//!       what: grok 未登录或目录未信任时拒绝起席
//!     - name: point_native_mcp_config_at_file
//!       what: 把 argv 里的 MCP 配置参数改指到已落盘的文件
//!   depends:
//!     - crate::provider::McpConfig
//!     - crate::model::permissions
//!     - crate::event_log::EventLog
//!     - crate::lifecycle::profile_launch
//!     - std::fs
//! boundary:
//!   - 不读也不写 provider 的凭据文件，只判断登录态文件是否存在且非空
//!   - 审计事件只落键名，不落键值
//!   - 清洗共享 toml 时不删未知键，未知不等于可删
//!   - 清洗后校验失败一律拒绝起席，不带着脏配置继续
//! maturity: wired
//! ---
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

/// ---
/// purpose: 把 MCP 配置里的占位符替换成本次 workspace、席位与团队
/// returns: 替换后的配置
/// ---
pub(crate) fn resolve_mcp_config(
    config: crate::provider::McpConfig,
    workspace: &Path,
    agent_id: &str,
    team_id: &str,
) -> crate::provider::McpConfig {
    crate::provider::McpConfig {
        raw: resolve_mcp_placeholders(config.raw, workspace, agent_id, team_id),
    }
}

/// ---
/// purpose: 递归替换 JSON 里字符串中的三个占位符
/// returns: 同结构的新值，非字符串标量原样返回
/// ---
pub(super) fn resolve_mcp_placeholders(
    value: serde_json::Value,
    workspace: &Path,
    agent_id: &str,
    team_id: &str,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(
            s.replace("{workspace}", &workspace.to_string_lossy())
                .replace("{agent_id}", agent_id)
                .replace("{team_id}", team_id),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| resolve_mcp_placeholders(item, workspace, agent_id, team_id))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        key,
                        resolve_mcp_placeholders(value, workspace, agent_id, team_id),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

/// ---
/// purpose: 写出 per-agent 的 MCP 配置文件，不做 provider 特化
/// returns: 写出的文件路径
/// errors: 建目录、序列化或写文件失败时返回 StatePersist
/// contract_id: lifecycle.mcp_config.write_worker_config
/// ---
pub(crate) fn write_worker_mcp_config(
    workspace: &Path,
    agent_id: &str,
    config: &crate::provider::McpConfig,
) -> Result<PathBuf, LifecycleError> {
    write_worker_mcp_config_for_provider(workspace, agent_id, config, None)
}

/// ---
/// purpose: 写出 per-agent 的 MCP 配置文件，copilot 先把 type 字段翻成 transport
/// params:
///   provider: 为 copilot 时做字段名翻译，其余原样写
/// returns: 写出的文件路径
/// errors: 建目录、序列化或写文件失败时返回 StatePersist
/// contract_id: lifecycle.mcp_config.write_worker_config
/// ---
/// C-3-4 cr verdict v2 — Copilot 的 mcp config schema 字段名是 `transport`
/// (实测 cmd-mcp-add 原文取值 stdio|http|sse),不是 canonical 的 `type`。当
/// provider==Copilot 时写出文件前先做 type→transport 翻译;其它 provider 不动。
/// 文件路径同 canonical `<ws>/.team/runtime/mcp/<agent_id>.json`,因为 launch
/// 路径会用 `--additional-mcp-config @<file>` 直指它。
pub(crate) fn write_worker_mcp_config_for_provider(
    workspace: &Path,
    agent_id: &str,
    config: &crate::provider::McpConfig,
    provider: Option<Provider>,
) -> Result<PathBuf, LifecycleError> {
    let path = workspace
        .join(".team/runtime/mcp")
        .join(format!("{agent_id}.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let raw = if matches!(provider, Some(Provider::Copilot)) {
        copilot_translate_mcp_servers(&config.raw)
    } else {
        config.raw.clone()
    };
    let body = serde_json::to_string_pretty(&serde_json::json!({"mcpServers": raw}))
        .map_err(|e| LifecycleError::StatePersist(format!("serialize mcp config: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// Grok MCP 是**目录作用域**（`mcp_injection: dir_scoped`）：CLI 只读
/// `<cwd>/.grok/config.toml`。同目录后起席位会覆盖这份文件，先起席位的
/// 身份被回溯性改写且不报错。本版 worker cwd 恒为 workspace（D5），
/// exclusive 检查也不读 per-agent cwd ⇒ 一个 workspace 只支持一个 grok 席。
/// 这是 grok 的 provider 能力边界，不是框架通则（claude/codex 走 argv
/// `--mcp-config`，同目录多席没有这个问题）。
struct GrokOccupant {
    id: String,
    spawned_at: Option<String>,
    status: String,
}

fn agent_is_grok(agent: &Value) -> bool {
    agent
        .get("provider")
        .and_then(Value::as_str)
        .and_then(crate::lifecycle::profile_launch::parse_provider)
        == Some(Provider::Grok)
}

fn status_is_live(status: &str) -> bool {
    !matches!(
        status,
        "stopped" | "stopping" | "removed" | "spawn_failed" | "failed"
    )
}

fn occupant_from_state_row(id: &str, agent: &serde_json::Value) -> Option<GrokOccupant> {
    let provider = agent
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::lifecycle::profile_launch::parse_provider);
    if provider != Some(Provider::Grok) {
        return None;
    }
    let status = agent
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("running");
    if !status_is_live(status) {
        return None;
    }
    Some(GrokOccupant {
        id: id.to_string(),
        spawned_at: agent
            .get("spawned_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        status: status.to_string(),
    })
}

fn live_grok_occupants_from_state(workspace: &Path) -> Result<Vec<GrokOccupant>, LifecycleError> {
    let path = crate::state::persist::runtime_state_path(workspace);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let state = crate::state::persist::load_runtime_state(workspace).map_err(|e| {
        LifecycleError::StatePersist(format!(
            "cannot count live grok seats (state unreadable): {e}"
        ))
    })?;
    let mut out = Vec::new();
    if let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) {
        for (id, agent) in agents {
            if let Some(row) = occupant_from_state_row(id, agent) {
                out.push(row);
            }
        }
    }
    if let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) {
        for team in teams.values() {
            let Some(agents) = team.get("agents").and_then(serde_json::Value::as_object) else {
                continue;
            };
            for (id, agent) in agents {
                if out.iter().any(|o| o.id == *id) {
                    continue;
                }
                if let Some(row) = occupant_from_state_row(id, agent) {
                    out.push(row);
                }
            }
        }
    }
    Ok(out)
}

fn spec_grok_ids(spec: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for agent in spec_agent_values(spec) {
        if agent_is_paused(agent) || !agent_is_grok(agent) {
            continue;
        }
        if let Some(id) = agent.get("id").and_then(Value::as_str) {
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// ---
/// purpose: 清掉 grok 共享 toml 里遗留的 per-seat 键，并写一条只含键名的审计事件
/// returns: 文件不存在或本就没有 per-seat 键时直接成功
/// errors: 文件读不出、清洗后仍残留或审计事件写不出时返回 RequirementUnmet
/// ---
/// Reconcile `<cwd>/.grok/config.toml` before any grok start: drop leftover
/// framework per-seat keys (`is_per_seat_env_key`), leave user/shared keys,
/// emit an audit event with names only. Hung only from
/// [`apply_grok_mcp_overlay`] — the unique writer, so launch/restart/resume
/// all pass through. Clean-failure keeps the previous refuse shape.
pub(crate) fn reconcile_grok_toml_per_seat_keys(workspace: &Path) -> Result<(), LifecycleError> {
    let path = workspace.join(".grok").join("config.toml");
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        LifecycleError::RequirementUnmet(format!(
            "error: cannot judge grok shared slot (unreadable {})\n\
             reason: {error}\n\
             action: fix permissions on that file before adding another grok seat",
            path.display()
        ))
    })?;
    let keys = super::per_seat_keys_in_toml(&text);
    if keys.is_empty() {
        return Ok(());
    }
    let (cleaned, removed) = super::strip_per_seat_keys_from_toml(&text);
    let dir = workspace.join(".grok");
    if let Err(_error) = write_grok_config_toml(&dir, &cleaned) {
        return Err(refuse_dirty_grok_toml(workspace, &keys));
    }
    let after =
        std::fs::read_to_string(&path).map_err(|_| refuse_dirty_grok_toml(workspace, &keys))?;
    if !super::per_seat_keys_in_toml(&after).is_empty() {
        return Err(refuse_dirty_grok_toml(workspace, &keys));
    }
    crate::event_log::EventLog::new(workspace)
        .write(
            crate::lifecycle::types::event_names::GROK_TOML_PER_SEAT_KEYS_CLEARED,
            serde_json::json!({
                "path": path.display().to_string(),
                "keys": removed,
            }),
        )
        .map_err(|error| {
            LifecycleError::RequirementUnmet(format!(
                "error: cannot audit grok shared-slot cleanup ({})\n\
                 reason: {error}\n\
                 action: fix permissions on .team/logs then retry",
                path.display()
            ))
        })?;
    Ok(())
}

fn refuse_dirty_grok_toml(workspace: &Path, keys: &[(String, String)]) -> LifecycleError {
    let named = keys
        .iter()
        .map(|(key, _value)| key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    LifecycleError::RequirementUnmet(format!(
        "error: grok shared slot still carries per-seat keys ({named})\n\
         reason: .grok/config.toml is directory-scoped; per-seat keys would be inherited by every grok seat\n\
         workspace: {}\n\
         action: remove per-seat keys from the toml (identity belongs on pane env)",
        workspace.display()
    ))
}

fn write_grok_config_toml(dir: &Path, body: &str) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("config.toml");
    let tmp = dir.join("config.toml.tmp");
    std::fs::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// ---
/// purpose: 构造同一 workspace 已被别的 grok 席占用的拒绝错误
/// params:
///   seats: 已占用该目录的席位名，写进错误正文
/// returns: 带 error/reason/workspace/grok_seats 的 RequirementUnmet
/// ---
pub(crate) fn grok_shared_cwd_error(cwd: &Path, seats: &[String]) -> LifecycleError {
    LifecycleError::RequirementUnmet(format!(
        "error: grok seat already occupies this workspace\n\
         reason: grok MCP is directory-scoped (<cwd>/.grok/config.toml); a second seat overwrites TEAM_AGENT_ID\n\
         workspace: {}\n\
         grok_seats: {}",
        cwd.display(),
        seats.join(", "),
    ))
}

fn grok_occupied_cwd_error(
    cwd: &Path,
    incoming: &str,
    occupants: &[GrokOccupant],
) -> LifecycleError {
    let holder = occupants
        .iter()
        .find(|o| o.id != incoming)
        .or_else(|| occupants.first());
    let holder_id = holder.map(|o| o.id.as_str()).unwrap_or("unknown");
    let started = holder
        .and_then(|o| o.spawned_at.as_deref())
        .unwrap_or("unknown");
    let names = occupants
        .iter()
        .map(|o| {
            if let Some(at) = &o.spawned_at {
                format!("{} (status={}, started {})", o.id, o.status, at)
            } else {
                format!("{} (status={})", o.id, o.status)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    LifecycleError::RequirementUnmet(format!(
        "error: grok seat {holder_id} already occupies this workspace (started {started})\n\
         incoming: {incoming}\n\
         reason: grok MCP is directory-scoped (<cwd>/.grok/config.toml); a second seat overwrites TEAM_AGENT_ID and the first seat's first turn would inherit the wrong identity\n\
         workspace: {}\n\
         grok_seats: {names}",
        cwd.display(),
    ))
}

/// ---
/// purpose: 起 grok 席前确认已登录且该目录已被信任
/// params:
///   cwd: 席位的工作目录，用于判断目录信任
/// returns: 通过返回空值；关闭 folder-trust 的环境变量下跳过信任检查
/// errors: HOME 未设、登录态文件缺失或为空、目录未被信任时返回 RequirementUnmet
/// ---
/// 未登录 / 目录未信任时不许起出「能收信、没有手」的 grok 席。
/// 登录态看 `$HOME/.grok/auth.json`；目录信任看 `$HOME/.grok/trusted_folders.toml`
/// （与 grok `--trust` / `/hooks-trust` 同一份；未信任则项目作用域 MCP 不生效）。
/// `GROK_FOLDER_TRUST=0` 时 grok 自己关掉 folder-trust，本检查跟着放行。
pub(crate) fn ensure_grok_login_and_folder_trust(cwd: &Path) -> Result<(), LifecycleError> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        LifecycleError::RequirementUnmet(
            "error: HOME is unset; cannot verify grok login or folder trust\n\
                 action: export HOME and retry"
                .to_string(),
        )
    })?;
    let grok_home = home.join(".grok");
    if !grok_auth_present(&grok_home) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "error: grok is not logged in (missing or empty {})\n\
             action: run `grok login` then retry add-agent/launch",
            grok_home.join("auth.json").display()
        )));
    }
    if folder_trust_required() && !grok_folder_is_trusted(&grok_home, cwd) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "error: grok folder is not trusted; project-scope MCP at {}/.grok/config.toml will not load\n\
             cwd: {}\n\
             action: from that directory run `grok --trust` (or `/hooks-trust` in a grok session), then retry",
            cwd.display(),
            cwd.display()
        )));
    }
    Ok(())
}

fn grok_auth_present(grok_home: &Path) -> bool {
    let path = grok_home.join("auth.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value.as_object().is_some_and(|map| !map.is_empty())
}

fn folder_trust_required() -> bool {
    !matches!(
        std::env::var("GROK_FOLDER_TRUST").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

fn grok_folder_is_trusted(grok_home: &Path, cwd: &Path) -> bool {
    let text = std::fs::read_to_string(grok_home.join("trusted_folders.toml")).unwrap_or_default();
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    grok_trusted_folders(&text).into_iter().any(|trusted| {
        let trusted = std::fs::canonicalize(&trusted).unwrap_or(trusted);
        cwd == trusted || cwd.starts_with(&trusted)
    })
}

fn grok_trusted_folders(text: &str) -> Vec<PathBuf> {
    let mut current = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[folders.\"") {
            current = rest.strip_suffix("\"]").map(str::to_string);
            continue;
        }
        if trimmed.starts_with('[') {
            current = None;
            continue;
        }
        if trimmed == "trusted = true" {
            if let Some(path) = current.take() {
                out.push(PathBuf::from(path));
            }
        }
    }
    out
}

/// ---
/// purpose: 写 workspace 下 grok 的项目作用域 MCP 配置，服务名固定为 team_orchestrator
/// params:
///   mcp_config: 已解析的 MCP 配置，须含 team_orchestrator 条目与 command
/// returns: 成功返回空值；写前先清 per-seat 键，已有表里的未知环境键会被保留
/// errors: 缺条目或缺 command 时返回 StatePersist，清洗失败透传 RequirementUnmet，写文件失败返回 StatePersist
/// ---
/// Grok CLI 没有 `--mcp-config`。同 `apply_cursor_agent_rules_overlay`：launch
/// 路径写一份 provider 实际会读的文件。Grok 只认项目作用域
/// `<cwd>/.grok/config.toml`（`grok mcp add --scope project` 的产物）。
///
/// Unique writer of `<cwd>/.grok/config.toml`. Reconcile leftover per-seat
/// keys here so restart/resume cannot skip the upgrade migration.
///
/// `McpConfig.raw` 与写出的 grok 表名都必须是 `team_orchestrator`，与
/// `worker_command_context` 契约（grok: `team_orchestrator__send_message`）对齐。
/// grok 按 server 名给工具加命名空间，写成 `team-agent` 会变成 `team-agent__*`。
pub fn apply_grok_mcp_overlay(
    workspace: &Path,
    mcp_config: &crate::provider::McpConfig,
) -> Result<(), LifecycleError> {
    // adapter.rs 只产出 team_orchestrator。team-agent 是 0.5.67 overlay
    // 误用的旧 inbound key，读侧留一版以免夹具还带旧名；0.5.68 若无引用再删。
    let server = mcp_config
        .raw
        .get("team_orchestrator")
        .or_else(|| mcp_config.raw.get("team-agent"))
        .ok_or_else(|| {
            LifecycleError::StatePersist(
                "grok MCP overlay requires team_orchestrator in resolved mcp config".to_string(),
            )
        })?;
    let command = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            LifecycleError::StatePersist("grok MCP overlay missing command".to_string())
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

    // Per-seat keys live on the pane env. Unknown keys on the existing
    // table stay: a full stanza rewrite must not treat "not in our list"
    // as permission to delete.
    reconcile_grok_toml_per_seat_keys(workspace)?;
    let dir = workspace.join(".grok");
    let path = dir.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let preserved = super::non_per_seat_env_in_tables(
        &existing,
        &["mcp_servers.team_orchestrator", "mcp_servers.team-agent"],
    );
    let mut env = env;
    env.retain(|key, value| !super::is_per_seat_env_key(key) && !value.trim().is_empty());
    for (key, value) in preserved {
        env.entry(key).or_insert(value);
    }

    let stanza = render_grok_team_agent_stanza(command, &args, &env);
    // 同时摘掉新表和 0.5.67 误写的 [mcp_servers.team-agent]，否则改名后两套
    // team MCP 并存，grok 会列出两份指向同一进程的工具。
    let body = upsert_toml_table_prefixes(
        &existing,
        &["mcp_servers.team_orchestrator", "mcp_servers.team-agent"],
        &stanza,
    );
    write_grok_config_toml(&dir, &body)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(())
}

fn render_grok_team_agent_stanza(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> String {
    let mut out = String::from("[mcp_servers.team_orchestrator]\n");
    out.push_str(&format!("command = {}\n", toml_quote(command)));
    out.push_str("args = [\n");
    for arg in args {
        out.push_str(&format!("    {},\n", toml_quote(arg)));
    }
    out.push_str("]\n");
    out.push_str("enabled = true\n");
    if !env.is_empty() {
        out.push_str("\n[mcp_servers.team_orchestrator.env]\n");
        for (key, value) in env {
            out.push_str(&format!("{key} = {}\n", toml_quote(value)));
        }
    }
    out
}

fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn upsert_toml_table_prefixes(existing: &str, tables: &[&str], stanza: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            skip = tables
                .iter()
                .any(|table| name == *table || name.starts_with(&format!("{table}.")));
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        stanza.to_string()
    } else {
        format!("{trimmed}\n\n{stanza}")
    }
}

/// ---
/// purpose: 把 MCP 配置里每个 server 的 type 字段名换成 transport
/// returns: 同结构的新值，其余字段全部保留；非对象原样返回
/// ---
/// C-3-4 cr verdict v2 — McpConfig.raw 是 `{name: {type, command, args, env}}` 形;
/// copilot mcp add schema 取 `transport` 替 `type`(stdio|http|sse 同值)。仅
/// 字段名变换,其余字段全保留。
pub(super) fn copilot_translate_mcp_servers(raw: &serde_json::Value) -> serde_json::Value {
    let Some(servers) = raw.as_object() else {
        return raw.clone();
    };
    let mut translated = serde_json::Map::new();
    for (name, server) in servers {
        let Some(obj) = server.as_object() else {
            translated.insert(name.clone(), server.clone());
            continue;
        };
        let mut out = serde_json::Map::new();
        for (key, value) in obj {
            if key == "type" {
                out.insert("transport".to_string(), value.clone());
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
        translated.insert(name.clone(), serde_json::Value::Object(out));
    }
    serde_json::Value::Object(translated)
}

/// ---
/// purpose: 把 argv 里 provider 原生的 MCP 配置参数改指到已落盘的文件
/// params:
///   argv: 就地改写；claude 系改 --mcp-config 的值，copilot 改 --additional-mcp-config 并加 @ 前缀
/// returns: argv 里没有对应参数时什么都不做
/// ---
pub(crate) fn point_native_mcp_config_at_file(
    argv: &mut [String],
    provider: Provider,
    path: &Path,
) {
    match provider {
        Provider::Claude | Provider::ClaudeCode => {
            let Some(index) = argv.iter().position(|arg| arg == "--mcp-config") else {
                return;
            };
            if let Some(value) = argv.get_mut(index.saturating_add(1)) {
                *value = path.to_string_lossy().to_string();
            }
        }
        // §C1 note: copilot `--additional-mcp-config` 接受 `@file`,直接指向既有
        // `.team/runtime/mcp/<agent>.json`(launch 路径 write_worker_mcp_config 已写)。
        // 既避免 inline JSON 包 mcpServers wrapper 的语义错位,也更利于 ps 验法。
        Provider::Copilot => {
            let Some(index) = argv.iter().position(|arg| arg == "--additional-mcp-config") else {
                return;
            };
            if let Some(value) = argv.get_mut(index.saturating_add(1)) {
                *value = format!("@{}", path.to_string_lossy());
            }
        }
        _ => {}
    }
}

/// ---
/// purpose: 解析该席位的权限并序列化成 JSON
/// returns: 含 agent_id、provider、排序后的工具串、逐工具的执行强度与是否有仅提示项
/// errors: 权限解析失败时返回 ModelError
/// ---
pub(super) fn permissions_json(
    agent: &Value,
    id: &str,
    provider: Provider,
) -> Result<serde_json::Value, crate::model::ModelError> {
    let tools = agent.get("tools").and_then(Value::as_list).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let resolved = permissions::resolve_permissions(&AgentPermissionInput {
        id: Some(AgentId::new(id)),
        provider,
        role: agent
            .get("role")
            .and_then(Value::as_str)
            .map(str::to_string),
        tools,
    })?;
    let mut out = serde_json::Map::new();
    out.insert("agent_id".to_string(), serde_json::json!(id));
    out.insert("provider".to_string(), serde_json::json!(provider));
    out.insert(
        "tools".to_string(),
        serde_json::json!(resolved.sorted_tool_strings()),
    );
    out.insert(
        "resolved_tools".to_string(),
        serde_json::Value::Array(
            resolved
                .resolved_tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "tool": tool.tool,
                        "enforcement": tool.enforcement,
                    })
                })
                .collect(),
        ),
    );
    out.insert(
        "has_prompt_only".to_string(),
        serde_json::json!(resolved.has_prompt_only),
    );
    Ok(serde_json::Value::Object(out))
}
