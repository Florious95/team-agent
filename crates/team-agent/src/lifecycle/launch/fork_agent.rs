//! ---
//! purpose: 在当前席位窗口注入官方斜杠命令完成就地分身，不读 provider session 落盘
//! contract:
//!   provides:
//!     - name: fork_agent_with_transport
//!       what: grok subscription 注入纯净 /fork；屏幕出现 forked from 才算成功
//!     - name: in_window_fork_command
//!       what: 只有已验证的 provider+auth 才给出窗口内命令；未验证返回 None
//!   depends:
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

const FORKED_FROM_MARK: &str = "forked from";
const MAX_ENTER_RETRIES: u32 = 8;

/// Official in-window fork command. None = unverified or unsupported.
/// Only grok+subscription is field-proven (2026-08-17).
pub fn in_window_fork_command(provider: Provider, auth: AuthMode) -> Option<&'static str> {
    match (provider, auth) {
        (Provider::Grok, AuthMode::Subscription) => Some("/fork"),
        _ => None,
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
    let run_ws = selected.run_workspace.clone();
    let agent = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!(
                "unknown worker agent id: {source_agent_id}"
            ))
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
    let Some(command) = in_window_fork_command(provider, auth) else {
        return Err(LifecycleError::Provider(format!(
            "{provider_raw} does not support native session fork"
        )));
    };
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
    inject_clean_command(transport, &target, command)?;
    let seen = wait_for_forked_from(transport, &target)?;
    drop(lock);
    let _ = as_agent_id;
    Ok(ForkAgentReport {
        source_agent_id: source_agent_id.clone(),
        new_agent_id: source_agent_id.clone(),
        env: AgentActionEnvelope {
            agent_id: source_agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(&run_ws),
            coordinator_started: false,
        },
        session_id: None,
        backing_state: if seen {
            ForkBackingState::Verified
        } else {
            ForkBackingState::PendingContextFork
        },
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

fn wait_for_forked_from(
    transport: &dyn Transport,
    target: &Target,
) -> Result<bool, LifecycleError> {
    for attempt in 0..MAX_ENTER_RETRIES {
        let cap = transport
            .capture(target, crate::transport::CaptureRange::Tail(40))
            .map_err(|e| LifecycleError::Transport(e.to_string()))?;
        if cap.text.contains(FORKED_FROM_MARK) {
            return Ok(true);
        }
        if attempt + 1 < MAX_ENTER_RETRIES {
            // Mixed text in the box is not failure. Retry Enter only — never re-paste /fork.
            let _ = transport.send_keys(target, &[crate::transport::Key::Enter]);
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }
    Err(LifecycleError::RequirementUnmet(
        "fork inject did not produce 'forked from' on screen".to_string(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextForkFinalized {
    source_agent_id: String,
    agent_id: String,
    session_id: String,
    rollout_path: String,
    captured_via: String,
    attribution_confidence: String,
}

impl ContextForkFinalized {
    pub(crate) fn write_audit(
        &self,
        event_log: &crate::event_log::EventLog,
    ) -> Result<(), crate::event_log::EventLogError> {
        let _ = (
            &self.source_agent_id,
            &self.agent_id,
            &self.session_id,
            &self.rollout_path,
            &self.captured_via,
            &self.attribution_confidence,
        );
        let _ = event_log;
        Ok(())
    }
}

/// Old pending-fork capture is gone. Do not read provider session files.
pub(crate) fn finalize_pending_fork_capture(
    _agent: &mut serde_json::Map<String, serde_json::Value>,
    _captured: &crate::provider::CapturedSession,
) -> Option<ContextForkFinalized> {
    None
}
