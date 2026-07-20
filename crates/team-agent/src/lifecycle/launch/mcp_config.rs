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
