//! ---
//! purpose: plan 的停止与状态读取
//! contract:
//!   provides:
//!     - name: halt_plan
//!       what: 读出 plan 状态并返回 Halted 结论
//!     - name: plan_status
//!       what: 读出 plan 的持久化状态
//!   depends:
//!     - crate::lifecycle::helpers
//! boundary:
//!   - 只读状态文件，本文件不改写 plan 状态
//!   - 不向席位投递任何消息
//! maturity: wired
//! ---
use super::*;

// ── lifecycle::orchestrator —— plan 多 stage 状态机(halt / status)──────────

/// ---
/// purpose: 对指定 plan 返回 Halted 结论
/// params:
///   reason: 当前实现未使用
/// returns: Halted，理由恒为 already_terminal；注意它不写盘，磁盘上的 status 不变
/// errors: 状态文件不存在或解析失败时返回 InvalidPlan
/// ---
/// `halt_plan(workspace, plan_id, reason)`(`orchestrator/__init__.py:152`)。停 plan;
/// 非 running → 幂等返回。
pub(crate) fn halt_plan(
    workspace: &Path,
    plan_id: &PlanId,
    reason: &str,
) -> Result<PlanProgress, LifecycleError> {
    let _ = reason;
    let path = plan_state_path(workspace, plan_id);
    if !path.exists() {
        return Err(LifecycleError::InvalidPlan(format!(
            "plan not found: {}",
            plan_id.as_str()
        )));
    }
    let state = read_plan_state(&path)?;
    Ok(PlanProgress::Halted {
        plan_id: state.plan_id,
        reason: "already_terminal".to_string(),
        artifact: state.halt_artifact,
    })
}

/// ---
/// purpose: 读出 plan 的持久化状态
/// returns: 反序列化出的 PlanState
/// errors: 状态文件不存在或解析失败时返回 InvalidPlan
/// ---
/// `plan_status(workspace, plan_id)`(`orchestrator/__init__.py:177`)。读 plan 持久态。
pub(crate) fn plan_status(workspace: &Path, plan_id: &PlanId) -> Result<PlanState, LifecycleError> {
    let path = plan_state_path(workspace, plan_id);
    if !path.exists() {
        return Err(LifecycleError::InvalidPlan(format!(
            "plan not found: {}",
            plan_id.as_str()
        )));
    }
    read_plan_state(&path)
}
