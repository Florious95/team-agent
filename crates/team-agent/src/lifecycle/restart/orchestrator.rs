use super::*;

// ── lifecycle::orchestrator —— plan 多 stage 状态机(halt / status)──────────

/// `halt_plan(workspace, plan_id, reason)`(`orchestrator/__init__.py:152`)。停 plan;
/// 非 running → 幂等返回。
pub fn halt_plan(
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

/// `plan_status(workspace, plan_id)`(`orchestrator/__init__.py:177`)。读 plan 持久态。
pub fn plan_status(workspace: &Path, plan_id: &PlanId) -> Result<PlanState, LifecycleError> {
    let path = plan_state_path(workspace, plan_id);
    if !path.exists() {
        return Err(LifecycleError::InvalidPlan(format!(
            "plan not found: {}",
            plan_id.as_str()
        )));
    }
    read_plan_state(&path)
}
