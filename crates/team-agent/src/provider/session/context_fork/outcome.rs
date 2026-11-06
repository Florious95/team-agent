//! Pending native-session capture convergence policy.

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
