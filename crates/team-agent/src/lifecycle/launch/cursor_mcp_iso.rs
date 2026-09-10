//! ---
//! purpose: cursor 的目录作用域 mcp.json 改成 per-seat provider-config 工程根（对齐 grok overlay / claude CLAUDE_CONFIG_DIR）
//! contract:
//!   provides:
//!     - name: materialize_cursor_mcp_project
//!       what: 在 provider-config/<id>/cursor 建 .cursor/，失败不写共用 mcp.json
//!     - name: cursor_mcp_isolation_enabled
//!       what: 默认开；TEAM_AGENT_CURSOR_MCP_ISOLATION=0 时关（只给破坏齿/回归旧闸）
//!     - name: cursor_mcp_project_dir
//!       what: 该席位作为 cursor --workspace 的工程根（未建盘）
//! boundary:
//!   - 隔离失败 = RequirementUnmet，⛔ 不降级写 <workspace>/.cursor/mcp.json
//!   - 不改 HOME（不是 copilot COPILOT_HOME / 会话库分叉）
//!   - 不削弱 refuse_second：隔离关闭时仍拒第二席
//! maturity: wired
//! ---

use std::path::{Path, PathBuf};

use crate::lifecycle::LifecycleError;

const ISOLATION_ENV: &str = "TEAM_AGENT_CURSOR_MCP_ISOLATION";

/// ---
/// purpose: 默认开启 per-seat provider-config 工程根；显式 0/false/off 才关
/// ---
pub fn cursor_mcp_isolation_enabled() -> bool {
    match std::env::var(ISOLATION_ENV) {
        Ok(value) => {
            let trimmed = value.trim();
            !matches!(
                trimmed,
                "0" | "false" | "FALSE" | "off" | "OFF" | "no" | "NO"
            )
        }
        Err(_) => true,
    }
}

/// ---
/// purpose: 算出该席位 cursor --workspace 工程根，不建盘
/// ---
pub fn cursor_mcp_project_dir(workspace: &Path, agent_id: &str) -> Result<PathBuf, LifecycleError> {
    validate_agent_id(agent_id)?;
    Ok(workspace
        .join(".team")
        .join("runtime")
        .join("provider-config")
        .join(agent_id)
        .join("cursor"))
}

/// ---
/// purpose: 该席位 cursor 实际会读的项目 mcp.json
/// ---
pub fn cursor_mcp_json_path(workspace: &Path, agent_id: &str) -> Result<PathBuf, LifecycleError> {
    Ok(cursor_mcp_project_dir(workspace, agent_id)?
        .join(".cursor")
        .join("mcp.json"))
}

/// ---
/// purpose: 建 per-seat 工程根，并把团队 rules 链进该根（不链 mcp.json）
/// returns: 工程根
/// errors: agent_id 非法或建盘失败 → RequirementUnmet（不写共用 mcp.json）
/// ---
pub fn materialize_cursor_mcp_project(
    workspace: &Path,
    agent_id: &str,
) -> Result<PathBuf, LifecycleError> {
    let project = cursor_mcp_project_dir(workspace, agent_id)?;
    let cursor_dir = project.join(".cursor");
    std::fs::create_dir_all(&cursor_dir).map_err(|e| {
        LifecycleError::RequirementUnmet(format!(
            "error: cannot materialize cursor per-seat MCP project\n\
             reason: {e}\n\
             path: {}\n\
             action: fix workspace permissions; do not fall back to <workspace>/.cursor/mcp.json",
            cursor_dir.display()
        ))
    })?;
    link_workspace_rules(workspace, &cursor_dir)?;
    Ok(project)
}

fn validate_agent_id(agent_id: &str) -> Result<(), LifecycleError> {
    if agent_id.is_empty()
        || agent_id.contains('/')
        || agent_id.contains('\\')
        || agent_id.contains("..")
        || !agent_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(LifecycleError::RequirementUnmet(format!(
            "error: cursor per-seat MCP project cannot use agent id {agent_id:?}\n\
             reason: id must be a single path segment [A-Za-z0-9._-]+\n\
             action: rename the seat; do not share <workspace>/.cursor/mcp.json"
        )));
    }
    Ok(())
}

fn link_workspace_rules(workspace: &Path, dest_cursor: &Path) -> Result<(), LifecycleError> {
    let source = workspace.join(".cursor").join("rules");
    if !source.exists() {
        return Ok(());
    }
    let dest = dest_cursor.join("rules");
    if dest.exists() {
        return Ok(());
    }
    std::os::unix::fs::symlink(&source, &dest).map_err(|e| {
        LifecycleError::RequirementUnmet(format!(
            "error: cannot link workspace cursor rules into per-seat project\n\
             reason: {e}\n\
             action: do not fall back to <workspace>/.cursor/mcp.json"
        ))
    })
}
