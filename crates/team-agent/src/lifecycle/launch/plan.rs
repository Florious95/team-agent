use super::*;

// ── lifecycle::orchestrator —— plan 多 stage 状态机(起步与推进)─────────────

/// `start_plan(workspace, plan_path, start)`(`orchestrator/__init__.py:26`)。load_plan
/// (YAML 校验)→ 持久化 PlanState → 派发首 stage。对已 running/halted/completed 幂等返回。
pub fn start_plan(
    workspace: &Path,
    plan_path: &Path,
    start: bool,
) -> Result<PlanProgress, LifecycleError> {
    if !plan_path.exists() {
        return Err(LifecycleError::InvalidPlan(format!(
            "plan not found: {}",
            plan_path.display()
        )));
    }
    let text = std::fs::read_to_string(plan_path).map_err(|e| {
        LifecycleError::InvalidPlan(format!("{}: {e}", plan_path.display()))
    })?;
    let plan = yaml::loads(&text).map_err(|e| LifecycleError::InvalidPlan(e.to_string()))?;
    let plan_id = plan_id_from_plan(plan_path, &plan)?;
    let state_path = plan_state_path(workspace, &plan_id);
    if state_path.exists() {
        return plan_progress_from_state(workspace, read_plan_state(&state_path)?);
    }

    let stages = plan_stages(&plan)?;
    if stages.is_empty() {
        return Err(LifecycleError::InvalidPlan(
            "plan has no stages".to_string(),
        ));
    }
    let _ = start;
    let current_stage = 1;
    let state = PlanState {
        plan_id,
        plan_path: plan_path.to_path_buf(),
        team: plan.get("team").and_then(Value::as_str).map(str::to_string),
        current_stage,
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_stages: Vec::new(),
        status: PlanStatus::Running,
        halt_reason: None,
        halt_artifact: None,
        stages,
        current_dispatch: None,
    };
    save_plan_state(workspace, &state)?;
    let state_path = plan_state_path(workspace, &state.plan_id);
    Ok(PlanProgress::Running {
        plan_id: state.plan_id,
        current_stage,
        state_path,
    })
}

/// `handle_report_result(workspace, envelope)`(`orchestrator/__init__.py:79`)。被 step11
/// report_result 路径调用以推进 stage;条件匹配 → 推进/完成,否则 `NoMatch`。
pub fn handle_report_result(
    workspace: &Path,
    envelope: &serde_json::Value,
) -> Result<PlanProgress, LifecycleError> {
    let dir = crate::model::paths::runtime_dir(workspace).join("orchestrator");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(PlanProgress::NoMatch);
    };
    for entry in entries {
        let entry = entry.map_err(|e| LifecycleError::InvalidPlan(format!("read plan dir: {e}")))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let state = read_plan_state(&path)?;
        if state.status != PlanStatus::Running {
            continue;
        }
        if let Some(next) = advance_plan_state(workspace, state, envelope)? {
            return Ok(next);
        }
    }
    Ok(PlanProgress::NoMatch)
}

fn plan_id_from_plan(plan_path: &Path, plan: &Value) -> Result<PlanId, LifecycleError> {
    if let Some(raw) = plan.get("id").and_then(Value::as_str) {
        return PlanId::parse(raw);
    }
    let raw = plan_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| LifecycleError::InvalidPlanId("plan path has no file stem".to_string()))?;
    PlanId::parse(raw)
}

fn plan_stages(plan: &Value) -> Result<Vec<PlanStage>, LifecycleError> {
    let Some(raw_stages) = plan.get("stages").and_then(Value::as_list) else {
        return Ok(Vec::new());
    };
    let mut stages = Vec::new();
    for raw in raw_stages {
        let id = raw
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| LifecycleError::InvalidPlan("stage id is required".to_string()))?
            .to_string();
        let condition = raw
            .get("on_result")
            .or_else(|| raw.get("advance_on"))
            .and_then(Value::as_str)
            .map(PlanCondition::parse)
            .transpose()?;
        stages.push(PlanStage {
            id,
            assignee: raw.get("assignee").and_then(Value::as_str).map(AgentId::new),
            prompt: raw.get("prompt").and_then(Value::as_str).map(str::to_string),
            on_result: condition,
            status: raw.get("status").and_then(Value::as_str).map(str::to_string),
        });
    }
    Ok(stages)
}

fn plan_progress_from_state(
    workspace: &Path,
    state: PlanState,
) -> Result<PlanProgress, LifecycleError> {
    match state.status {
        PlanStatus::Running => Ok(PlanProgress::Running {
            state_path: plan_state_path(workspace, &state.plan_id),
            current_stage: state.current_stage,
            plan_id: state.plan_id,
        }),
        PlanStatus::Completed => Ok(PlanProgress::Completed {
            plan_id: state.plan_id,
        }),
        PlanStatus::Halted => Ok(PlanProgress::Halted {
            plan_id: state.plan_id,
            reason: state
                .halt_reason
                .unwrap_or_else(|| "already_terminal".to_string()),
            artifact: state.halt_artifact,
        }),
    }
}

fn advance_plan_state(
    workspace: &Path,
    mut state: PlanState,
    envelope: &serde_json::Value,
) -> Result<Option<PlanProgress>, LifecycleError> {
    let Some(stage_index) = current_stage_index(&state) else {
        return Ok(None);
    };
    let Some(stage) = state.stages.get(stage_index) else {
        return Ok(None);
    };
    let condition = stage.on_result.as_ref().unwrap_or(&PlanCondition::Any);
    if !plan_condition_matches(condition, envelope) {
        return Ok(None);
    }
    let completed = stage.id.clone();
    if !state.completed_stages.iter().any(|id| id == &completed) {
        state.completed_stages.push(completed);
    }
    let next_index = stage_index.saturating_add(1);
    if let Some(next_stage) = state.stages.get(next_index) {
        let _ = next_stage;
        let current_stage = i64::try_from(next_index.saturating_add(1))
            .map_err(|e| LifecycleError::InvalidPlan(format!("stage index overflow: {e}")))?;
        state.current_stage = current_stage;
        save_plan_state(workspace, &state)?;
        let state_path = plan_state_path(workspace, &state.plan_id);
        Ok(Some(PlanProgress::Running {
            plan_id: state.plan_id,
            current_stage,
            state_path,
        }))
    } else {
        state.status = PlanStatus::Completed;
        state.current_stage = i64::try_from(state.stages.len().saturating_add(1))
            .map_err(|e| LifecycleError::InvalidPlan(format!("stage index overflow: {e}")))?;
        save_plan_state(workspace, &state)?;
        Ok(Some(PlanProgress::Completed {
            plan_id: state.plan_id,
        }))
    }
}

fn current_stage_index(state: &PlanState) -> Option<usize> {
    if state.current_stage > 0 {
        let current = usize::try_from(state.current_stage).ok()?.checked_sub(1)?;
        if current < state.stages.len() {
            return Some(current);
        }
    }
    state
        .stages
        .iter()
        .position(|stage| !state.completed_stages.iter().any(|id| id == &stage.id))
}

fn plan_condition_matches(condition: &PlanCondition, envelope: &serde_json::Value) -> bool {
    match condition {
        PlanCondition::Any => true,
        PlanCondition::FieldEq { field, value } => report_result_field(envelope, field)
            .map(|got| got == *value)
            .unwrap_or(false),
    }
}

fn report_result_field(envelope: &serde_json::Value, field: &str) -> Option<String> {
    envelope
        .get("report_result")
        .and_then(|v| v.get(field))
        .or_else(|| envelope.get(field))
        .and_then(json_scalar_to_string)
}

fn json_scalar_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
    }
}
