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

pub(super) fn agent_is_paused(agent: &Value) -> bool {
    matches!(agent.get("paused"), Some(Value::Bool(true)))
}

pub(crate) fn spawn_timestamp() -> String {
    match std::env::var("TEAM_AGENT_TEST_FIXED_SPAWNED_AT") {
        Ok(value) => value,
        Err(_) => chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
            .to_string(),
    }
}

pub(super) fn spawn_timestamp_for_agent(offset_micros: u32) -> String {
    if offset_micros == 0 {
        return spawn_timestamp();
    }
    match std::env::var("TEAM_AGENT_TEST_FIXED_SPAWNED_AT") {
        Ok(value) => chrono::DateTime::parse_from_rfc3339(&value)
            .map(|dt| {
                (dt.with_timezone(&chrono::Utc)
                    + chrono::Duration::microseconds(i64::from(offset_micros)))
                .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
                .to_string()
            })
            .unwrap_or(value),
        Err(_) => spawn_timestamp(),
    }
}

pub(crate) fn fill_spawn_placeholders(argv: &mut [String], workspace: &Path, agent_id: &str) {
    fill_spawn_placeholders_full(argv, workspace, agent_id, None);
}

/// #229 B-layer worker env contract (`worker_spawn_inherits_parent_process_env_for_proxy_and_ca`):
/// every worker `transport.spawn_first/into` MUST receive an env map that is the **complete**
/// `team-agent` process environ (so the child sees the user's PATH ordering, HTTP_PROXY /
/// HTTPS_PROXY / ALL_PROXY / NO_PROXY, NODE_EXTRA_CA_CERTS / SSL_CERT_FILE / CURL_CA_BUNDLE /
/// REQUESTS_CA_BUNDLE / GIT_SSL_CAINFO, plus any wrapper-sourced vars), **then** overlay the
/// Team Agent identity three-tuple. This equals POSIX "child inherits parent environ" — the same
/// behavior the user gets when typing `codex` from their own shell. Zero hardcoded paths, zero
/// wrapper-name assumptions, generic across providers.
///
/// `TMUX` / `TMUX_PANE` are stripped because they bind the inherited shell to the **launching**
/// tmux pane; leaving them in would point worker-side tmux integrations at the wrong pane.
pub(crate) fn inherited_env_with_team_overrides(
    workspace: &Path,
    agent_id: &str,
    team_id: Option<&str>,
) -> BTreeMap<String, String> {
    // 0.3.28 Step 3: delegate to `layout::worker_env::worker_spawn_env` which
    // implements Python's whitelist semantics (`providers.py:130-145`). The
    // whitelist:
    //   * Keeps PATH-like vars, locale, provider creds (CLAUDE_*/OPENAI_*/
    //     COPILOT_*/GH_*/GEMINI_*/GOOGLE_*) + posix identifiers only.
    //   * Strips ALL `TEAM_AGENT_LEADER_*` and
    //     `TEAM_AGENT_MACHINE_FINGERPRINT` (leader identity contamination,
    //     E60 root).
    //   * Strips `TEAM_AGENT_TEAM_ID` (the leader's team_id — re-injected
    //     as `TEAM_AGENT_OWNER_TEAM_ID` for the worker).
    //   * Strips `COPILOT_DISABLE_TERMINAL_TITLE` (re-injected per-agent by
    //     `apply_copilot_instructions_overlay` based on the WORKER's provider).
    //   * Strips `TMUX` / `TMUX_PANE` (would attach worker to leader's pane).
    crate::layout::worker_env::worker_spawn_env(std::env::vars(), workspace, agent_id, team_id)
}

pub(crate) fn apply_mcp_auto_approval_env(
    env: &mut BTreeMap<String, String>,
    safety: &DangerousApproval,
) {
    for key in [
        "TEAM_AGENT_LEADER_BYPASS",
        "TEAM_AGENT_LEADER_BYPASS_SOURCE",
        "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
        "TEAM_AGENT_LEADER_BYPASS_FLAG",
        "TEAM_AGENT_MCP_AUTO_APPROVE",
        "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
    ] {
        env.remove(key);
    }
    if safety.enabled
        && matches!(safety.source, DangerousApprovalSource::LeaderProcess)
        && safety.inherited
    {
        env.insert("TEAM_AGENT_LEADER_BYPASS".to_string(), "1".to_string());
        env.insert(
            "TEAM_AGENT_LEADER_BYPASS_SOURCE".to_string(),
            "leader_process".to_string(),
        );
        if let Some(provider) = safety.provider.as_deref() {
            env.insert(
                "TEAM_AGENT_LEADER_BYPASS_PROVIDER".to_string(),
                provider.to_string(),
            );
        }
        if let Some(flag) = safety.flag.as_deref() {
            env.insert(
                "TEAM_AGENT_LEADER_BYPASS_FLAG".to_string(),
                flag.to_string(),
            );
        }
        env.insert(
            "TEAM_AGENT_MCP_AUTO_APPROVE".to_string(),
            "team_orchestrator".to_string(),
        );
        env.insert(
            "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE".to_string(),
            "leader_bypass".to_string(),
        );
    } else {
        env.insert("TEAM_AGENT_LEADER_BYPASS".to_string(), "0".to_string());
    }
}

/// BUG / B2 灵魂件 + C-1-2 + C-6-1 cr verdict — Copilot per-worker AGENTS.md
/// 写入 + `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` 注入。
///
/// 目录布局:`<workspace>/.team/runtime/copilot-instructions/<agent_id>/AGENTS.md`
///   * 含 `<agent_id>` segment(C-6-2 per-agent isolation,N18 精神)
///   * 文件内容 ≡ `compile_worker_system_prompt` 输出(B2 ps/文件双验法)
///   * **禁** silent 写全局 `~/.copilot/AGENTS.md`(C-1-2 grep guard)
///
/// 失败回 `LifecycleError::StatePersist` 以与既有 state 持久化错误同源,
/// 不 silent 吞(MUST-NOT-13 诚实)。
pub(crate) fn apply_copilot_instructions_overlay(
    workspace: &Path,
    agent_id: &str,
    system_prompt: &str,
    env: &mut BTreeMap<String, String>,
) -> Result<(), LifecycleError> {
    let dir = workspace
        .join(".team")
        .join("runtime")
        .join("copilot-instructions")
        .join(agent_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", dir.display())))?;
    let agents_md = dir.join("AGENTS.md");
    std::fs::write(&agents_md, system_prompt.as_bytes())
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", agents_md.display())))?;
    env.insert(
        "COPILOT_CUSTOM_INSTRUCTIONS_DIRS".to_string(),
        dir.to_string_lossy().to_string(),
    );
    // ★ C-4 P0(N39 红线 / MUST-12) — copilot config 默认 `updateTerminalTitle=true`
    // 会改 tmux window 名(help-config 原文)。tmux window 名是框架定位 agent 的
    // anchor(window==agent_id);copilot 静默改写 → 寻址 / kill / 保护集 三处同源
    // 派生漂移 → B5 protected_set 误判、MUST-12 pane 身份失锚、N39 同源派生破。
    // 漏关后果定级为【B5 leader 误杀同级 incident】,绝不允许 silent 跳过。
    // 主案:env `COPILOT_DISABLE_TERMINAL_TITLE=1`(help-config 原文 "Can also be
    // disabled via the COPILOT_DISABLE_TERMINAL_TITLE environment variable")。
    env.insert(
        "COPILOT_DISABLE_TERMINAL_TITLE".to_string(),
        "1".to_string(),
    );
    Ok(())
}

/// C-3-2/C-3-3 cr verdict v2 — Copilot spawn 前调 `copilot mcp list` 扫用户全局
/// `~/.copilot/mcp-config.json` 与 workspace `.mcp.json` 的 MCP 残留;对每个非
/// `team_orchestrator` server 追加 `--disable-mcp-server <name>`(main-help:72-73)
/// 并落 `<log_dir>/mcp-residual.txt` + emit `provider.copilot.mcp_residual_detected`
/// event(MUST-NOT-13 诚实记录,非 silent)。
///
/// 失败回 `LifecycleError::StatePersist`,不 silent 吞;`copilot mcp list` 自身
/// 无法运行(命令缺失 / 退出码非零)时,仅记 `mcp-residual.txt` 的 unavailable
/// 行,不阻断 spawn(provider 一期 subscription-only,工具链可能未完全就绪)。
pub(super) fn apply_copilot_mcp_residual_disables(
    workspace: &Path,
    agent_id: &str,
    argv: &mut Vec<String>,
    log_dir: &Path,
) -> Result<(), LifecycleError> {
    let listing = std::process::Command::new("copilot")
        .arg("mcp")
        .arg("list")
        .output();
    let residual_path = log_dir.join("mcp-residual.txt");
    match listing {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            std::fs::write(&residual_path, &text).map_err(|e| {
                LifecycleError::StatePersist(format!("{}: {e}", residual_path.display()))
            })?;
            let residual_servers = parse_copilot_mcp_list_server_names(&text);
            let non_orchestrator: Vec<String> = residual_servers
                .iter()
                .filter(|name| name.as_str() != "team_orchestrator")
                .cloned()
                .collect();
            for name in &non_orchestrator {
                argv.push("--disable-mcp-server".to_string());
                argv.push(name.clone());
            }
            if !non_orchestrator.is_empty() {
                let event_log = crate::event_log::EventLog::new(workspace);
                let _ = event_log.write(
                    "provider.copilot.mcp_residual_detected",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "residual_servers": non_orchestrator,
                        "log_path": residual_path.to_string_lossy(),
                    }),
                );
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            std::fs::write(
                &residual_path,
                format!(
                    "copilot mcp list exit={:?} stderr={stderr}\n",
                    out.status.code()
                ),
            )
            .map_err(|e| {
                LifecycleError::StatePersist(format!("{}: {e}", residual_path.display()))
            })?;
        }
        Err(e) => {
            std::fs::write(
                &residual_path,
                format!("copilot mcp list unavailable: {e}\n"),
            )
            .map_err(|e| {
                LifecycleError::StatePersist(format!("{}: {e}", residual_path.display()))
            })?;
        }
    }
    Ok(())
}

/// 解析 `copilot mcp list` 输出取 server 名集合(te 真机实证 v2,1.0.59 形态):
/// ```text
/// User servers:
///   foo (local)
///   bar (http)
/// Builtin servers:
///   github-mcp-server (local)
/// ```
/// 或空集形态(te 真机实证 fake HOME 无 mcp-config.json):
/// ```text
/// No MCP servers configured.
///
/// Add a server with:
///   copilot mcp add <name> -- <command> [args...]
///   copilot mcp add --transport http <name> <url>
/// ```
///
/// 规则:
/// 1. 首行含 "No MCP servers configured" → 立即返空(避免把 "Add a server with"
///    段下的 help 行误识为 server)
/// 2. 段标题行(非缩进、以 `:` 结尾):只有 *servers:* 后缀的段(User/Builtin/
///    Workspace servers:)才进 server-listing 模式;其余段(如 "Add a server with:")
///    进 skip 模式直到下个 servers: 段或文档结束
/// 3. servers: 段内的缩进行取首段 token,剥 ` (local)`/` (http)`/` (sse)` 后缀
/// 4. 空行 / 不识别行容忍跳过(诚实降级:漏识 = silent 残留,在 mcp-residual.txt
///    全量落盘留证)
pub(super) fn parse_copilot_mcp_list_server_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_servers_section = false;
    for line in text.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.is_empty() {
            continue;
        }
        // C-3-2 fix(te 真机实证):空集 sentinel 立即返空。
        if trimmed_end
            .trim_start()
            .starts_with("No MCP servers configured")
        {
            return Vec::new();
        }
        // 段标题行(非缩进):决定后续缩进行是否取 server 名。"*servers:" 是
        // listing 段(User/Builtin/Workspace),其它段都 skip(如 "Add a server with:"
        // 下面的 help 命令缩进行)。
        if !(line.starts_with(' ') || line.starts_with('\t')) {
            let lower = trimmed_end.to_ascii_lowercase();
            in_servers_section = lower.trim_end_matches(':').ends_with("servers");
            continue;
        }
        if !in_servers_section {
            continue;
        }
        let trimmed = trimmed_end.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let mut token = trimmed.split_whitespace().next().unwrap_or("").to_string();
        // 剥常见装饰后缀(实测形如 "(local)"/"(http)"/"(sse)" 是独立 whitespace
        // 分隔的 token,首段 token 通常不带括号;若实际 copilot 把括号粘连首段
        // token,这里多做一次后缀剥离守护)。
        if let Some(idx) = token.find('(') {
            token.truncate(idx);
        }
        token = token.trim_end_matches(':').trim().to_string();
        if token.is_empty() {
            continue;
        }
        out.push(token);
    }
    out
}

pub(crate) fn apply_profile_launch_env(
    env: &mut BTreeMap<String, String>,
    profile_launch: &crate::provider::ProviderProfileLaunch,
) {
    for key in &profile_launch.env_unset {
        env.remove(key);
    }
    env.extend(profile_launch.env_overlay.clone());
}

pub(super) fn persist_started_agent_plan_state(
    state: &mut serde_json::Map<String, serde_json::Value>,
    started_agent: &StartedAgent,
) {
    if let Some(session_id) = started_agent.pending_session_id.as_ref() {
        state.insert(
            "_pending_session_id".to_string(),
            serde_json::json!(session_id.as_str()),
        );
    }
    if let Some(root) = started_agent.provider_projects_root.as_ref() {
        state.insert(
            "claude_projects_root".to_string(),
            serde_json::json!(root.to_string_lossy().to_string()),
        );
    }
    if started_agent.managed_mcp_config {
        state.insert("managed_mcp_config".to_string(), serde_json::json!(true));
    }
    if started_agent.managed_mcp_config
        || started_agent.claude_config_dir.is_some()
        || started_agent.provider_projects_root.is_some()
    {
        state.insert(
            "profile_launch".to_string(),
            serde_json::json!({
                "managed_mcp_config": started_agent.managed_mcp_config,
                "claude_config_dir": started_agent.claude_config_dir.as_ref().map(|path| path.to_string_lossy().to_string()),
                "claude_projects_root": started_agent.provider_projects_root.as_ref().map(|path| path.to_string_lossy().to_string()),
            }),
        );
    }
}

pub(crate) fn persist_command_plan_state(
    state: &mut serde_json::Map<String, serde_json::Value>,
    plan: &crate::provider::CommandPlan,
    profile_launch: &crate::provider::ProviderProfileLaunch,
) {
    if let Some(session_id) = plan.expected_session_id.as_ref() {
        state.insert(
            "_pending_session_id".to_string(),
            serde_json::json!(session_id.as_str()),
        );
    }
    let projects_root = plan
        .provider_projects_root
        .as_ref()
        .or(profile_launch.claude_projects_root.as_ref());
    if let Some(root) = projects_root {
        state.insert(
            "claude_projects_root".to_string(),
            serde_json::json!(root.to_string_lossy().to_string()),
        );
    }
    let managed_mcp_config = plan.managed_mcp_config || profile_launch.managed_mcp_config;
    if managed_mcp_config {
        state.insert("managed_mcp_config".to_string(), serde_json::json!(true));
    }
    if managed_mcp_config || profile_launch.claude_config_dir.is_some() || projects_root.is_some() {
        state.insert(
            "profile_launch".to_string(),
            serde_json::json!({
                "managed_mcp_config": managed_mcp_config,
                "claude_config_dir": profile_launch.claude_config_dir.as_ref().map(|path| path.to_string_lossy().to_string()),
                "claude_projects_root": projects_root.map(|path| path.to_string_lossy().to_string()),
            }),
        );
    }
}

pub(super) fn is_posix_shell_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Same as [`fill_spawn_placeholders`] plus `{team_id}` substitution everywhere it
/// appears as a SUBSTRING (the MCP config encodes it as `mcp_servers.team_orchestrator
/// .env.TEAM_AGENT_OWNER_TEAM_ID="{team_id}"`, embedded inside `-c key=value` strings,
/// so a token-equality replace would miss it).
pub(crate) fn fill_spawn_placeholders_full(
    argv: &mut [String],
    workspace: &Path,
    agent_id: &str,
    team_id: Option<&str>,
) {
    let workspace_text = workspace.to_string_lossy().to_string();
    let team_text = team_id.unwrap_or("").to_string();
    for arg in argv {
        if arg == "{workspace}" {
            *arg = workspace_text.clone();
        } else if arg == "{agent_id}" {
            *arg = agent_id.to_string();
        } else if arg.contains("{workspace}")
            || arg.contains("{agent_id}")
            || arg.contains("{team_id}")
        {
            *arg = arg
                .replace("{workspace}", &workspace_text)
                .replace("{agent_id}", agent_id)
                .replace("{team_id}", &team_text);
        }
    }
}
