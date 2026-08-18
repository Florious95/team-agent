//! ---
//! purpose: 在当前席位窗口注入官方斜杠命令完成就地分身，不读 provider session 落盘
//! contract:
//!   provides:
//!     - name: fork_agent_with_transport
//!       what: grok/claude subscription 注入纯净斜杠命令；屏幕出现该 provider 的实测标记才算成功
//!     - name: in_window_fork
//!       what: 已验证的 provider+subscription 给出 command+screen_mark；未验证返回 None
//!   depends:
//!     - crate::lifecycle::lock
//!     - crate::lifecycle::pane_input_lock
//!     - crate::transport::Transport
//! boundary:
//!   - 不 spawn 新 pane，不改 TEAM_AGENT_ID / MCP / 席位名
//!   - 不读 provider 会话落盘文件取会话身份
//!   - 重试只重按回车，不重粘 /fork
//!   - 不把 pane 锁超时从 200ms 调大
//!   - 未验证的 provider 不猜斜杠命令
//! maturity: wired
//! ---

use super::*;
use crate::lifecycle::pane_input_lock::{
    acquire_or_proceed, PaneInputLockRequest, PANE_INPUT_LOCK_TIMEOUT_EVENT,
};
use crate::lifecycle::profile_launch::{parse_auth_mode, parse_provider};
use crate::model::enums::{AuthMode, Provider};

const MAX_ENTER_RETRIES: u32 = 8;

/// Injected slash command + the screen mark that proves it landed.
/// Difference between providers lives only in this data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InWindowFork {
    pub command: &'static str,
    pub screen_mark: &'static str,
}

/// Official in-window fork. None = unverified or unsupported-auth.
/// grok `/fork` mark `forked from` (2026-08-17).
/// claude `/branch` mark `Branched conversation` (Claude Code v2.1.181, 2026-08-17).
/// claude `/fork` is a background-agent command (`Usage: /fork <directive>`), not the session split.
pub fn in_window_fork(provider: Provider, auth: AuthMode) -> Option<InWindowFork> {
    match (provider, auth) {
        (Provider::Grok, AuthMode::Subscription) => Some(InWindowFork {
            command: "/fork",
            screen_mark: "forked from",
        }),
        (Provider::Claude | Provider::ClaudeCode, AuthMode::Subscription) => Some(InWindowFork {
            command: "/branch",
            screen_mark: "Branched conversation",
        }),
        _ => None,
    }
}

pub fn in_window_fork_command(provider: Provider, auth: AuthMode) -> Option<&'static str> {
    in_window_fork(provider, auth).map(|spec| spec.command)
}

fn refuse_missing_in_window_fork(provider: Provider, provider_raw: &str) -> LifecycleError {
    if matches!(
        provider,
        Provider::Grok | Provider::Claude | Provider::ClaudeCode
    ) {
        LifecycleError::Provider(format!(
            "{provider_raw} does not support native session fork"
        ))
    } else {
        LifecycleError::Provider(format!(
            "{provider_raw} in-window fork is unverified (未验证)"
        ))
    }
}

pub fn fork_agent_with_transport(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    _label: Option<&str>,
    _open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<ForkAgentReport, LifecycleError> {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let lock_workspace = selected.run_workspace.clone();
    let lock_team_key = selected.team_key.clone();
    let _lifecycle_lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &lock_workspace,
        operation: "fork-agent",
        team: Some(lock_team_key.as_str()),
        agent_id: Some(source_agent_id),
    })?;
    let selected = crate::state::selector::resolve_active_team(
        &lock_workspace,
        Some(lock_team_key.as_str()),
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let run_ws = selected.run_workspace.clone();
    let agent = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source_agent_id}"))
        })?;
    let provider_raw = agent
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let auth_raw = agent
        .get("auth_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("subscription");
    let provider = parse_provider(provider_raw).unwrap_or(Provider::Grok);
    let auth = parse_auth_mode(auth_raw).unwrap_or(AuthMode::Subscription);
    let Some(spec) = in_window_fork(provider, auth) else {
        return Err(refuse_missing_in_window_fork(provider, provider_raw));
    };
    if as_agent_id.as_str() != source_agent_id.as_str() {
        return Err(LifecycleError::RequirementUnmet(format!(
            "in-place fork refuses --as {as_agent_id}: session stays on {source_agent_id}"
        )));
    }
    let pane_raw = agent
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!(
                "source agent {source_agent_id} has no pane_id"
            ))
        })?;
    let target = Target::Pane(PaneId::new(pane_raw));
    let lock = acquire_or_proceed(PaneInputLockRequest {
        workspace: Some(&run_ws),
        target_key: pane_raw,
        operation: "fork-inject",
    });
    if lock.is_none() {
        let _ = crate::event_log::EventLog::new(&run_ws).write(
            PANE_INPUT_LOCK_TIMEOUT_EVENT,
            serde_json::json!({
                "during": "fork-inject",
                "agent_id": source_agent_id.as_str(),
                "pane_id": pane_raw,
                "observed_at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }
    inject_clean_command(transport, &target, spec.command)?;
    wait_for_screen_mark(transport, &target, spec.screen_mark)?;
    drop(lock);
    crate::event_log::EventLog::new(&run_ws)
        .write(
            "lifecycle.fork.in_place",
            serde_json::json!({
                "source_agent_id": source_agent_id.as_str(),
                "agent_id": source_agent_id.as_str(),
                "pane_id": pane_raw,
                "command": spec.command,
                "screen_mark": spec.screen_mark,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(format!("fork audit write failed: {e}")))?;
    Ok(ForkAgentReport {
        source_agent_id: source_agent_id.clone(),
        new_agent_id: source_agent_id.clone(),
        env: AgentActionEnvelope {
            agent_id: source_agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(&run_ws),
            coordinator_started: false,
        },
        session_id: None,
        backing_state: ForkBackingState::Verified,
    })
}

fn inject_clean_command(
    transport: &dyn Transport,
    target: &Target,
    command: &str,
) -> Result<(), LifecycleError> {
    transport
        .inject(
            target,
            &crate::transport::InjectPayload::TextSkipConsumptionPoll(command.to_string()),
            crate::transport::Key::Enter,
            false,
        )
        .map_err(|e| LifecycleError::Transport(e.to_string()))?;
    Ok(())
}

const NO_CONVERSATION_TO_BRANCH: &str = "Failed to branch conversation: No conversation to branch";

fn wait_for_screen_mark(
    transport: &dyn Transport,
    target: &Target,
    screen_mark: &str,
) -> Result<(), LifecycleError> {
    for attempt in 0..MAX_ENTER_RETRIES {
        let cap = transport
            .capture(target, crate::transport::CaptureRange::Tail(40))
            .map_err(|e| LifecycleError::Transport(e.to_string()))?;
        if cap.text.contains(screen_mark) {
            return Ok(());
        }
        if cap.text.contains(NO_CONVERSATION_TO_BRANCH) {
            return Err(LifecycleError::RequirementUnmet(
                NO_CONVERSATION_TO_BRANCH.to_string(),
            ));
        }
        if attempt + 1 < MAX_ENTER_RETRIES {
            // Mixed text in the box is not failure. Retry Enter only — never re-paste the slash command.
            let _ = transport.send_keys(target, &[crate::transport::Key::Enter]);
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }
    Err(LifecycleError::RequirementUnmet(format!(
        "fork inject did not produce {screen_mark:?} on screen"
    )))
}
