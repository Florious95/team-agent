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

/// Grok MCP 是**目录作用域**：CLI 只读 `<cwd>/.grok/config.toml`
/// （`grok mcp add --scope project`）。同一 cwd 写第二份身份会覆盖
/// `TEAM_AGENT_ID`，两席共用一个 send_message / report_result 身份。
///
/// 硬约束：每个 grok 席必须有自己的 cwd（各自 worktree / 各自目录）。
/// 框架检测到两个 grok 席共用同一 cwd 时必须拒绝启动，不许静默覆盖。
/// 今日 launch/add/fork 的 worker cwd 恒为 workspace（D5），所以一个
/// workspace 最多一个 grok 席。下一步是给另一席单独的 workspace/worktree。
pub(crate) fn ensure_exclusive_grok_cwd(
    spec: &Value,
    workspace: &Path,
) -> Result<(), LifecycleError> {
    let mut by_cwd: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for agent in spec_agent_values(spec) {
        if agent_is_paused(agent) {
            continue;
        }
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .and_then(crate::lifecycle::profile_launch::parse_provider);
        if provider != Some(Provider::Grok) {
            continue;
        }
        // Launch cwd is workspace for every worker today (D5). Grok MCP
        // lives at that directory's `.grok/config.toml`.
        by_cwd
            .entry(workspace.to_path_buf())
            .or_default()
            .push(id.to_string());
    }
    for (cwd, ids) in by_cwd {
        if ids.len() < 2 {
            continue;
        }
        return Err(LifecycleError::RequirementUnmet(format!(
            "error: grok seats cannot share a cwd; grok MCP is directory-scoped to <cwd>/.grok/config.toml and a second seat would overwrite TEAM_AGENT_ID\n\
             cwd: {}\n\
             grok_seats: {}\n\
             action: give each grok seat its own worktree/directory (a separate workspace), then retry; do not start two grok seats in the same workspace",
            cwd.display(),
            ids.join(", "),
        )));
    }
    Ok(())
}

/// 未登录 / 目录未信任时不许起出「能收信、没有手」的 grok 席。
/// 登录态看 `$HOME/.grok/auth.json`；目录信任看 `$HOME/.grok/trusted_folders.toml`
/// （与 grok `--trust` / `/hooks-trust` 同一份；未信任则项目作用域 MCP 不生效）。
/// `GROK_FOLDER_TRUST=0` 时 grok 自己关掉 folder-trust，本检查跟着放行。
pub(crate) fn ensure_grok_login_and_folder_trust(cwd: &Path) -> Result<(), LifecycleError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| {
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
/// `McpConfig.raw` 的 server 名是框架内部的 `team_orchestrator`；写盘时改成
/// grok 侧已实测能 `mcp doctor` 通过的 `team-agent`。
pub(crate) fn apply_grok_mcp_overlay(
    workspace: &Path,
    mcp_config: &crate::provider::McpConfig,
) -> Result<(), LifecycleError> {
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

    let stanza = render_grok_team_agent_stanza(command, &args, &env);
    let dir = workspace.join(".grok");
    std::fs::create_dir_all(&dir)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", dir.display())))?;
    let path = dir.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let body = upsert_toml_table_prefix(&existing, "mcp_servers.team-agent", &stanza);
    let tmp = dir.join("config.toml.tmp");
    std::fs::write(&tmp, body.as_bytes())
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(())
}

fn render_grok_team_agent_stanza(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> String {
    let mut out = String::from("[mcp_servers.team-agent]\n");
    out.push_str(&format!("command = {}\n", toml_quote(command)));
    out.push_str("args = [\n");
    for arg in args {
        out.push_str(&format!("    {},\n", toml_quote(arg)));
    }
    out.push_str("]\n");
    out.push_str("enabled = true\n");
    if !env.is_empty() {
        out.push_str("\n[mcp_servers.team-agent.env]\n");
        for (key, value) in env {
            out.push_str(&format!("{key} = {}\n", toml_quote(value)));
        }
    }
    out
}

fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn upsert_toml_table_prefix(existing: &str, table: &str, stanza: &str) -> String {
    let child_prefix = format!("{table}.");
    let mut out = String::new();
    let mut skip = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let name = &trimmed[1..trimmed.len() - 1];
            skip = name == table || name.starts_with(&child_prefix);
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
