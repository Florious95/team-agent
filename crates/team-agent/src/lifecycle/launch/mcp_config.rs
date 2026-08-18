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

pub(crate) fn write_worker_mcp_config(
    workspace: &Path,
    agent_id: &str,
    config: &crate::provider::McpConfig,
) -> Result<PathBuf, LifecycleError> {
    write_worker_mcp_config_for_provider(workspace, agent_id, config, None)
}

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
