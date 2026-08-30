//! ---
//! purpose: 把 verify_context_fork 的 Result 翻译成三态结局(Verified/Pending/Rejected)，并给 pending 态一条推进到失败的判据
//! contract:
//!   provides:
//!     - name: ContextForkOutcome
//!       what: fork 三态:Verified(带证明) / Pending(带可交回捕获通道的扫描上下文) / Rejected(provider 错误)
//!     - name: PendingContextFork
//!       what: 超时未验证时保留的续查材料:源会话 id、目标席位、spawned_at、CaptureSessionContext
//!     - name: observe_context_fork
//!       what: 调 verify_context_fork 并把 Timeout 折成 Pending 而不是错误
//!     - name: transition_pending_context_fork
//!       what: 纯判据——pending_context_fork 是否该被推进成 transcript_missing
//!   requires:
//!     - name: crate::provider::session_scan::CaptureSessionContext
//!       what: Pending 携带的续查上下文类型
//! boundary:
//!   - 不轮询、不读磁盘;所有 I/O 都在被调用的 verify_context_fork 里
//!   - 不写 state、不发事件——只产出结局值，落盘由捕获通道做
//!   - Pending 不是成功也不是失败，绝不折进另外两态
//! maturity: wired
//! ---
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct PendingContextFork {
    pub source_session_id: SessionId,
    pub target_agent: String,
    pub spawned_at: String,
    pub scanner_context: crate::provider::session_scan::CaptureSessionContext,
}

#[derive(Debug)]
pub(crate) enum ContextForkOutcome {
    Verified(ContextForkProof),
    Pending(PendingContextFork),
    Rejected(ProviderError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextForkPendingFailure {
    TranscriptMissing,
}

/// ---
/// purpose: 判定一个停在 pending_context_fork 的席位是否该被改判为 transcript_missing
/// params:
///   triggered: 该席位是否已出现过触发事件(有过交互/结果/pane 输出)
///   grace_expired: 宽限窗口是否已过
/// returns: 两者同时为真才给 Some(TranscriptMissing);否则 None = 继续 pending
/// contract:
///   provides:
///     - name: transition_pending_context_fork
///       what: 纯布尔判据，无 I/O、无时钟
/// boundary:
///   - 不自己计算 triggered / grace_expired——两个事实由调用方(capture.rs)从 agent 行算好后传入
///   - 只表达「该改判」，不负责写 capture_state
/// ---
pub(crate) fn transition_pending_context_fork(
    triggered: bool,
    grace_expired: bool,
) -> Option<ContextForkPendingFailure> {
    (triggered && grace_expired).then_some(ContextForkPendingFailure::TranscriptMissing)
}

/// ---
/// purpose: 跑一次 fork 验证并把结果收敛成三态，超时不当错误而是留成可续查的 Pending
/// params:
///   provider: 转交 verify_context_fork 分派
///   source_session_id: 源会话 id，同时写进 Pending 供后续比对
///   plan: expected_session_id 与 provider_projects_root 会被复制进 Pending 的扫描上下文
///   before: spawn 前 backing 基线
///   expected_backing_path: 精确快照路径，透传
///   source_agent_id: 透传给 verify_context_fork(当前该参数不参与判定)
///   agent_id: 目标席位 id;既用于 codex 身份比对，也写进 Pending.target_agent
///   spawn_cwd: 目标席位工作目录，透传并写进 Pending 的扫描上下文
///   spawned_at: 时间边界，透传并写进 Pending
///   deadline: 轮询预算
/// returns: Verified(proof) / Pending(续查材料) / Rejected(ProviderError)
/// contract:
///   provides:
///     - name: observe_context_fork
///       what: 只做结果翻译与 Pending 材料装配，不新增任何判定
/// boundary:
///   - Pending 里的 CaptureSessionContext 一律 pane_id=None、pane_pid=None——本函数不掌握 pane 事实
///   - 不重试、不写状态;是否再验由捕获通道决定
/// ---
pub(crate) fn observe_context_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    source_agent_id: &str,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> ContextForkOutcome {
    match verify_context_fork(
        provider,
        source_session_id,
        plan,
        before,
        expected_backing_path,
        source_agent_id,
        agent_id,
        spawn_cwd,
        spawned_at,
        deadline,
    ) {
        Ok(proof) => ContextForkOutcome::Verified(proof),
        Err(ContextForkTermination::Timeout { .. }) => {
            ContextForkOutcome::Pending(PendingContextFork {
                source_session_id: source_session_id.clone(),
                target_agent: agent_id.to_string(),
                spawned_at: spawned_at.to_string(),
                scanner_context: crate::provider::session_scan::CaptureSessionContext {
                    agent_id: agent_id.to_string(),
                    spawn_cwd: spawn_cwd.to_path_buf(),
                    pane_id: None,
                    pane_pid: None,
                    spawned_at: Some(spawned_at.to_string()),
                    expected_session_id: plan.expected_session_id.clone(),
                    provider_projects_root: plan.provider_projects_root.clone(),
                },
            })
        }
        Err(ContextForkTermination::Rejected(error)) => ContextForkOutcome::Rejected(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convergence_deadlines_are_provider_specific() {
        assert_eq!(
            context_fork_convergence_deadline(Provider::Claude),
            Duration::from_secs(45)
        );
        assert_eq!(
            context_fork_convergence_deadline(Provider::Codex),
            Duration::from_secs(10)
        );
        assert_eq!(
            context_fork_convergence_deadline(Provider::Copilot),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn codex_without_new_backing_is_typed_pending() {
        let root =
            std::env::temp_dir().join(format!("ta-codex-fork-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: None,
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Codex, &plan);
        let outcome = observe_context_fork(
            Provider::Codex,
            &SessionId::new("source-session"),
            &plan,
            &before,
            None,
            "source",
            "fork",
            &root,
            "2026-07-24T00:00:00Z",
            Duration::ZERO,
        );
        let ContextForkOutcome::Pending(pending) = outcome else {
            panic!("missing backing must remain a typed pending fork")
        };
        assert_eq!(pending.source_session_id.as_str(), "source-session");
        assert_eq!(pending.target_agent, "fork");
        let _ = std::fs::remove_dir_all(&root);
    }
}
