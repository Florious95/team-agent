//! ---
//! purpose: Pi CLI 的纯工具映射与 fresh/resume argv 构造
//! contract:
//!   provides:
//!     - name: build_pi_command_argv
//!       what: 仅从已验证输入构造保留 direct Pi 配置的 Team Agent argv
//! boundary:
//!   - 不发现 executable、catalog、adapter 或 MCP 配置
//!   - 除 resume exact backing 存在性外不读文件；不写文件、不启动进程
//! maturity: wired
//! ---

use std::path::Path;

use crate::model::enums::ProviderEffort;
use crate::provider::ProviderError;

#[derive(Debug, Clone, Copy)]
pub(crate) enum PiSessionSelector<'a> {
    Fresh { session_id: &'a str },
    Resume { path: &'a Path },
}

pub(crate) struct PiCommandRequest<'a> {
    pub extension: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Option<ProviderEffort>,
    pub system_prompt: &'a str,
    /// Team-owned session root for isolated seats; `None` preserves Pi's native default.
    pub session_dir: Option<&'a Path>,
    pub session: PiSessionSelector<'a>,
    pub agent_id: &'a str,
}

/// ---
/// purpose: 从已验证 semantic request 构造仅追加 Team Agent MCP/提示/session 的 Pi argv
/// returns: exact ordered fresh/resume argv，argv[0] 固定为裸 `pi`
/// errors: 必需字段时返回 ProviderError
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

    // Executable discovery/preflight belongs to the lifecycle materializer;
    // the actual command must remain PATH-resolved at spawn time.
    let mut argv = vec![
        "pi".to_string(),
        "-e".to_string(),
        request.extension.to_string_lossy().into_owned(),
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
        "--append-system-prompt".to_string(),
        request.system_prompt.to_string(),
    ]);
    if let Some(session_dir) = request.session_dir {
        argv.extend([
            "--session-dir".to_string(),
            session_dir.to_string_lossy().into_owned(),
        ]);
    }
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
