//! leader::start — leader_start_plan / start_leader / leader_session_name(派生 tmux session 名)。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::provider::{get_adapter, Provider};
use crate::tmux_backend::TmuxBackend;
use crate::transport::{SessionName, Transport};

use super::helpers::{
    provider_wire, resolve_workspace_for_hash, sanitize_session_folder, sha1_hex_prefix,
};
use super::owner_bind::leader_identity_context;
use super::{LeaderError, LeaderStartMode, LeaderStartPlan};

// ── leader::start — leader_start_plan / start_leader / session 名 ──

/// `leader_start_plan`(card §46;`__init__.py:82`)。计算 leader 启动计划
/// (exec in-TMUX / new tmux session / attach existing)。provider 未安装 → `Err(Start)`。
pub fn leader_start_plan(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
) -> Result<LeaderStartPlan, LeaderError> {
    if attach_session.is_some() && !confirm_attach {
        return Err(LeaderError::Start("--attach-session requires --confirm".to_string()));
    }
    if attach_existing && !confirm_attach {
        return Err(LeaderError::Start("attach existing leader session requires confirm".to_string()));
    }
    let adapter = get_adapter(provider);
    if !adapter.is_installed() {
        let command_name = provider_command_name(provider);
        return Err(LeaderError::Start(format!(
            "Provider {} command '{}' not found",
            provider_wire(provider),
            command_name
        )));
    }
    let identity = leader_identity_context(workspace, None, None)?;
    let session_name = attach_session
        .cloned()
        .or_else(|| Some(leader_session_name(provider, workspace)));
    let in_tmux = std::env::var_os("TMUX").is_some();
    if !in_tmux {
        ensure_tmux_installed()?;
    }
    let existing_session = if !in_tmux && !attach_existing && attach_session.is_none() {
        match session_name.as_ref() {
            Some(session) => tmux_session_exists(workspace, session)?,
            None => false,
        }
    } else {
        false
    };
    let mode = if in_tmux {
        LeaderStartMode::ExecProvider
    } else if attach_existing || attach_session.is_some() || existing_session {
        LeaderStartMode::AttachExisting
    } else {
        LeaderStartMode::NewTmuxSession
    };
    let mut leader_env = BTreeMap::new();
    leader_env.insert("TEAM_AGENT_LEADER_PROVIDER".to_string(), provider_wire(provider).to_string());
    leader_env.insert(
        "TEAM_AGENT_LEADER_SESSION_UUID".to_string(),
        identity.leader_session_uuid.as_str().to_string(),
    );
    leader_env.insert(
        "TEAM_AGENT_MACHINE_FINGERPRINT".to_string(),
        identity.machine_fingerprint.clone(),
    );
    leader_env.insert(
        "TEAM_AGENT_WORKSPACE".to_string(),
        identity.workspace_abspath.to_string_lossy().into_owned(),
    );
    leader_env.insert("TEAM_AGENT_TEAM_ID".to_string(), identity.team_id.as_str().to_string());
    let argv = start_argv(mode, provider, provider_args, workspace, session_name.as_ref(), &leader_env)?;
    let plan_env = if mode == LeaderStartMode::ExecProvider {
        merged_exec_env(&leader_env)
    } else {
        leader_env.clone()
    };
    Ok(LeaderStartPlan {
        mode,
        provider,
        workspace: resolve_workspace_for_hash(workspace),
        session_name,
        argv,
        leader_env: plan_env,
        identity: Some(identity),
        detached: false,
    })
}

/// `start_leader`(card §46;`__init__.py:60`)。计算并执行 leader 启动计划(spawn + 信号处理)。
/// 进程退出后 `Err`/退出码经 caller 处理(此处返 `Result` 替代 Python 的 `SystemExit`)。
pub fn start_leader(
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    attach_existing: bool,
    confirm_attach: bool,
    attach_session: Option<&SessionName>,
) -> Result<(), LeaderError> {
    let plan = leader_start_plan(
        provider,
        provider_args,
        workspace,
        attach_existing,
        confirm_attach,
        attach_session,
    )?;
    crate::event_log::EventLog::new(workspace).write(
        super::LeaderEvent::LeaderStart.name(),
        serde_json::json!({
            "provider": super::helpers::provider_wire(plan.provider),
            "mode": serde_json::to_value(plan.mode)?,
            "session_name": plan.session_name.as_ref().map(|s| s.as_str().to_string()),
        }),
    )?;
    Ok(())
}

/// `leader_session_name`(card §48;`__init__.py:186`)。确定派生 tmux session 名
/// `team-agent-leader-<provider>-<folder>-<sha1[:8]>`(workspace.resolve() 的 sha1 前 8 hex)。
pub fn leader_session_name(provider: Provider, workspace: &Path) -> SessionName {
    let resolved = resolve_workspace_for_hash(workspace);
    let folder_raw = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");
    let folder = sanitize_session_folder(folder_raw);
    let hash = sha1_hex_prefix(resolved.to_string_lossy().as_bytes(), 8);
    SessionName::new(format!(
        "team-agent-leader-{}-{folder}-{hash}",
        provider_wire(provider)
    ))
}

fn start_argv(
    mode: LeaderStartMode,
    provider: Provider,
    provider_args: &[String],
    workspace: &Path,
    session_name: Option<&SessionName>,
    leader_env: &BTreeMap<String, String>,
) -> Result<Vec<String>, LeaderError> {
    let provider_cmd = provider_command_name(provider).to_string();
    match mode {
        LeaderStartMode::ExecProvider => {
            let mut argv = vec![provider_cmd];
            argv.extend(provider_args.iter().cloned());
            Ok(argv)
        }
        LeaderStartMode::AttachExisting => {
            let Some(session) = session_name else {
                return Err(LeaderError::Start("attach session missing".to_string()));
            };
            Ok(vec![
                "tmux".to_string(),
                "attach-session".to_string(),
                "-t".to_string(),
                session.as_str().to_string(),
            ])
        }
        LeaderStartMode::NewTmuxSession => {
            let Some(session) = session_name else {
                return Err(LeaderError::Start("leader session missing".to_string()));
            };
            let resolved_workspace = resolve_workspace_for_hash(workspace);
            let mut exports = leader_export_assignments(leader_env);
            if let Some(path) = std::env::var_os("PATH").and_then(|p| p.into_string().ok()) {
                exports.push(shlex_quote(&format!("PATH={path}")));
            }
            let mut provider_argv = vec![provider_cmd];
            provider_argv.extend(provider_args.iter().cloned());
            let shell = format!(
                "cd {} && export {} && exec {}",
                shlex_quote(&resolved_workspace.to_string_lossy()),
                exports.join(" "),
                shell_join(&provider_argv)
            );
            Ok(vec![
                "tmux".to_string(),
                "new-session".to_string(),
                "-s".to_string(),
                session.as_str().to_string(),
                "-n".to_string(),
                provider_wire(provider).to_string(),
                "-c".to_string(),
                resolved_workspace.to_string_lossy().into_owned(),
                "sh".to_string(),
                "-lc".to_string(),
                shell,
            ])
        }
    }
}

fn ensure_tmux_installed() -> Result<(), LeaderError> {
    match Command::new("tmux").arg("-V").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) | Err(_) => Err(LeaderError::Start(
            "tmux is not installed; install tmux 3.3+ or start the leader from an existing tmux pane"
                .to_string(),
        )),
    }
}

fn provider_command_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        Provider::GeminiCli => "gemini",
        Provider::Fake => "fake",
    }
}

fn tmux_session_exists(workspace: &Path, session: &SessionName) -> Result<bool, LeaderError> {
    TmuxBackend::for_workspace(workspace)
        .has_session(session)
        .map_err(|e| LeaderError::Start(format!("tmux has-session failed: {e}")))
}

fn leader_export_assignments(leader_env: &BTreeMap<String, String>) -> Vec<String> {
    [
        "TEAM_AGENT_LEADER_PROVIDER",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_MACHINE_FINGERPRINT",
        "TEAM_AGENT_WORKSPACE",
        "TEAM_AGENT_TEAM_ID",
    ]
    .iter()
    .filter_map(|key| {
        leader_env
            .get(*key)
            .map(|value| shlex_quote(&format!("{key}={value}")))
    })
    .collect()
}

fn merged_exec_env(leader_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.extend(leader_env.iter().map(|(key, value)| (key.clone(), value.clone())));
    env
}

fn shell_join(args: &[String]) -> String {
    args.iter().map(|arg| shlex_quote(arg)).collect::<Vec<_>>().join(" ")
}

fn shlex_quote(raw: &str) -> String {
    if !raw.is_empty()
        && raw
            .bytes()
            .all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'@' | b'%' | b'_' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                    )
            })
    {
        raw.to_string()
    } else {
        format!("'{}'", raw.replace('\'', "'\"'\"'"))
    }
}
