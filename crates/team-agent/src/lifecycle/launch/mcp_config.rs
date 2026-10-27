//! ---
//! purpose: 各 provider 的 MCP 配置落盘与 argv 指向
//! contract:
//!   provides:
//!     - name: resolve_mcp_config
//!       what: 把 MCP 配置里的 workspace、agent_id 与 team_id 占位符替换掉
//!     - name: write_worker_mcp_config_for_provider
//!       what: 写出 per-agent 的 MCP 配置文件，copilot 走字段名翻译
//!     - name: point_native_mcp_config_at_file
//!       what: 把 argv 里的 MCP 配置参数改指到已落盘的文件
//!   depends:
//!     - crate::provider::McpConfig
//!     - std::fs
//! boundary:
//!   - 不读也不写 provider 的凭据文件
//!   - Grok 的目录作用域 `.grok/config.toml` 写者已随 legacy grok worker 退役；grok_build 叶子自带 MCP 注入
//! maturity: wired
//! ---
use std::path::{Path, PathBuf};

use crate::lifecycle::*;
use crate::model::enums::Provider;

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
        crate::provider::adapters::copilot::copilot_translate_mcp_config(&config.raw)
    } else {
        config.raw.clone()
    };
    let body = serde_json::to_string_pretty(&serde_json::json!({"mcpServers": raw}))
        .map_err(|e| LifecycleError::StatePersist(format!("serialize mcp config: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// ---
/// purpose: 把 argv 里 provider 原生的 MCP 配置参数改指到已落盘的文件
/// params:
///   argv: 就地改写；copilot 改 --additional-mcp-config 并加 @ 前缀
/// returns: argv 里没有对应参数时什么都不做
/// ---
pub(crate) fn point_native_mcp_config_at_file(
    argv: &mut [String],
    provider: Provider,
    path: &Path,
) {
    match provider {
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
