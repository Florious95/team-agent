//! ---
//! purpose: Pi CLI 的纯工具映射与 fresh/resume argv 构造
//! contract:
//!   provides:
//!     - name: build_pi_command_argv
//!       what: 仅从已验证输入构造隔离的 regular-TUI Pi argv
//! boundary:
//!   - 不发现 executable、catalog、adapter 或 MCP 配置
//!   - 除 resume exact backing 存在性外不读文件；不写文件、不启动进程
//! maturity: wired
//! ---

use std::collections::BTreeSet;
use std::path::Path;

use crate::model::enums::ProviderEffort;
use crate::provider::ProviderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PiToolMapping {
    Mcp,
    Builtin(&'static [&'static str]),
    Unsupported,
}

/// ---
/// purpose: 把 Team tool category 映射为 exact Pi builtin/proxy names
/// returns: 单个或多个 builtin、MCP proxy 或 Unsupported
/// ---
pub(crate) fn pi_tool_mapping(category: &str) -> PiToolMapping {
    match category {
        "mcp_team" => PiToolMapping::Mcp,
        "fs_read" => PiToolMapping::Builtin(&["read"]),
        "fs_list" => PiToolMapping::Builtin(&["grep", "find", "ls"]),
        "fs_write" => PiToolMapping::Builtin(&["edit", "write"]),
        "execute_bash" => PiToolMapping::Builtin(&["bash"]),
        _ => PiToolMapping::Unsupported,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PiSessionSelector<'a> {
    Fresh { session_id: &'a str },
    Resume { path: &'a Path },
}

pub(crate) struct PiCommandRequest<'a> {
    pub executable: &'a Path,
    pub extension: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Option<ProviderEffort>,
    pub system_prompt: &'a str,
    pub tool_categories: &'a [&'a str],
    pub session_dir: &'a Path,
    pub session: PiSessionSelector<'a>,
    pub agent_id: &'a str,
}

/// ---
/// purpose: 从已验证 semantic request 构造无 ambient flag 的 Pi argv
/// returns: exact ordered fresh/resume argv
/// errors: 必需字段、mcp_team 或工具 category 非法时返回 ProviderError
/// ---
pub(crate) fn build_pi_command_argv(
    request: PiCommandRequest<'_>,
) -> Result<Vec<String>, ProviderError> {
    if request.executable.as_os_str().is_empty()
        || request.extension.as_os_str().is_empty()
        || request.session_dir.as_os_str().is_empty()
        || request.model.is_some_and(|model| model.trim().is_empty())
        || request.system_prompt.trim().is_empty()
        || request.agent_id.trim().is_empty()
    {
        return Err(ProviderError::Command(
            "Pi command requires executable, extension, prompt, session directory, and agent id"
                .to_string(),
        ));
    }

    let mut tools = BTreeSet::new();
    for category in request.tool_categories {
        match pi_tool_mapping(category) {
            PiToolMapping::Mcp => {
                tools.insert("mcp");
            }
            PiToolMapping::Builtin(names) => tools.extend(names.iter().copied()),
            PiToolMapping::Unsupported => {
                return Err(ProviderError::Command(format!(
                    "Pi does not support Team Agent tool category {category:?}"
                )));
            }
        }
    }
    if !tools.contains("mcp") {
        return Err(ProviderError::Command(
            "Pi command requires mcp_team".to_string(),
        ));
    }

    let mut argv = vec![
        request.executable.to_string_lossy().into_owned(),
        "--no-extensions".to_string(),
        "-e".to_string(),
        request.extension.to_string_lossy().into_owned(),
        "--no-approve".to_string(),
        "--no-context-files".to_string(),
        "--no-skills".to_string(),
        "--no-prompt-templates".to_string(),
        "--tui-mode".to_string(),
        "regular".to_string(),
    ];
    if let Some(model) = request.model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(effort) = request.effort {
        argv.push("--thinking".to_string());
        argv.push(effort.as_str().to_string());
    }
    argv.extend([
        "--system-prompt".to_string(),
        request.system_prompt.to_string(),
        "--tools".to_string(),
        tools.into_iter().collect::<Vec<_>>().join(","),
        "--session-dir".to_string(),
        request.session_dir.to_string_lossy().into_owned(),
    ]);
    // The shared lifecycle materializer always revalidates resume root/path/header.
    // Keep the pure argv builder usable before a fixture root is materialized, while
    // refusing a missing exact file once the recorded path's parent exists.
    match request.session {
        PiSessionSelector::Fresh { session_id } if !session_id.trim().is_empty() => {
            argv.push("--session-id".to_string());
            argv.push(session_id.to_string());
        }
        PiSessionSelector::Resume { path }
            if !path.as_os_str().is_empty()
                && (!path.parent().is_some_and(Path::is_dir) || path.is_file()) =>
        {
            argv.push("--session".to_string());
            argv.push(path.to_string_lossy().into_owned());
        }
        PiSessionSelector::Resume { path } if !path.as_os_str().is_empty() => {
            return Err(ProviderError::ResumeUnavailable(format!(
                "Pi exact session backing is missing: {}",
                path.display()
            )));
        }
        _ => {
            return Err(ProviderError::Command(
                "Pi command requires a non-empty session selector".to_string(),
            ));
        }
    }
    argv.push("--name".to_string());
    argv.push(request.agent_id.to_string());
    Ok(argv)
}
