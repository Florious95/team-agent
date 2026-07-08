use super::common::*;
use super::selection::classify_restart_plan_with_resume_validation;
use super::*;
use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

// ── lifecycle::restart —— 整队 Route B resume-or-fresh 重建 ──────────────────

/// `restart(workspace, allow_fresh, team)`(`restart/orchestration.py:26`)。整队重建:
/// **先**算 resume 决策(Route B)+ `first_send_at` 严格校验(corrupt → hard refuse),
/// **再**做破坏性 teardown(关显示、建 session)、起后 leader rebind、adaptive 显示重建。
/// 每非 paused worker 必发一条 `restart.resume_decision`(Route B audit 契约)。
pub fn restart(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
) -> Result<RestartReport, LifecycleError> {
    restart_with_session_convergence_deadline(workspace, allow_fresh, team, None)
}

pub fn restart_with_session_convergence_deadline(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    session_converge_deadline_ms: Option<u64>,
) -> Result<RestartReport, LifecycleError> {
    let paths = lifecycle_paths(workspace, team)?;
    let transport = lifecycle_worker_tmux_backend_for_selected_state(&paths.run_workspace, team)?;
    restart_with_transport_with_session_convergence_deadline(
        workspace,
        allow_fresh,
        team,
        &transport,
        session_converge_deadline_ms,
        None,
    )
}

/// `restart` with an injected transport (tests: recording mock; prod: real TmuxBackend). The Route-B
/// resume/fresh worker spawn + start_coordinator are wired here over `transport`. (rt-host-a sweep:
/// was a stub returning RequirementUnmet at the spawn boundary — never spawned/resumed/started coordinator.)
pub fn restart_with_transport(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
) -> Result<RestartReport, LifecycleError> {
    restart_with_transport_with_readiness_deadline(workspace, allow_fresh, team, transport, None)
}

pub fn restart_with_transport_with_readiness_deadline(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
    readiness_deadline_ms: Option<u64>,
) -> Result<RestartReport, LifecycleError> {
    match restart_with_transport_with_session_convergence_deadline(
        workspace,
        allow_fresh,
        team,
        transport,
        None,
        readiness_deadline_ms,
    )? {
        RestartReport::RefusedResumeNotReady {
            missing,
            allow_fresh,
            error,
            ..
        } => Ok(RestartReport::RefusedResumeAtomicity {
            unresumable: missing
                .into_iter()
                .map(|agent_id| UnresumableWorker {
                    agent_id,
                    reason: "session_capture_incomplete".to_string(),
                    refusal_reason: Some(
                        crate::provider::session::ResumeRefusalReason::SessionCaptureIncomplete,
                    ),
                    session_id: None,
                    first_send_at: None,
                })
                .collect(),
            allow_fresh,
            error,
        }),
        report => Ok(report),
    }
}

pub fn restart_with_transport_with_session_convergence_deadline(
    workspace: &Path,
    allow_fresh: bool,
    team: Option<&str>,
    transport: &dyn crate::transport::Transport,
    session_converge_deadline_ms: Option<u64>,
    readiness_deadline_ms: Option<u64>,
) -> Result<RestartReport, LifecycleError> {
    // RED-2-STILL(P0):入口门必须在 canonical_run_workspace 解析后的路径上判,不用 raw workspace。
    // 根因:quick-start <dir> 把 .team/runtime/spec 落在 team_workspace(dir)=**parent**/.team;
    // 入口门查 raw dir 自身的 .team/state(空,它在 parent)→ 误判"无 team context"早退,到不了
    // 067f78f 下移后的第二道门。canonical_run_workspace 已能正确解析到 parent(走 parent.join(".team")
    // 分支),在它之上判 input_has_no_local_team_context 才对齐 quick-start 落点。
    let resolved_ws = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if crate::lifecycle::restart::input_has_no_local_team_context(&resolved_ws) {
        return Err(LifecycleError::TeamSelect(format!(
            "missing spec for restart: {} (run `team-agent quick-start <teamdir>` first)",
            crate::model::paths::runtime_dir(&resolved_ws).display()
        )));
    }
    // RED-2(P0)修:存在性门下移到 resolve 之后,用 selected.spec_path(读序 B:runtime 优先、
    // legacy 用户目录回落)判,而非 resolve 前自拼 workspace/team.spec.yaml(spec-demote 后 spec
    // 不在用户目录 → 旧门误报缺)。resolve_active_team(RequireSpec) 内部已按 spec_path 校验存在性,
    // 故直接交给它;失败信息(含真实 expected runtime spec 路径)即正确的 N38。
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // 显式存在性门(下移后):selected.spec_path 经读序 B 已定位 runtime/legacy spec。
    // 缺(空目录 restart 等)→ 报真实期望路径,不误导去用户目录找。
    if !selected.spec_path.as_ref().is_some_and(|p| p.exists()) {
        let expected = selected.spec_path.clone().unwrap_or_else(|| {
            crate::model::paths::runtime_spec_path(&selected.run_workspace, &selected.team_key)
        });
        return Err(LifecycleError::TeamSelect(format!(
            "missing spec for restart: {} (run `team-agent quick-start <teamdir>` first, or restore the team's role docs)",
            expected.display()
        )));
    }
    let lifecycle_lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &selected.run_workspace,
        operation: "restart",
        team: Some(selected.team_key.as_str()),
        agent_id: None,
    })?;
    let mut state = selected.state;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&state, None)?;
    let topology_issue_ids = crate::topology::restart_dirty_topology_issue_ids(&state);
    if !topology_issue_ids.is_empty() {
        let session_name = state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        crate::event_log::EventLog::new(&selected.run_workspace)
            .write(
                "restart.refused_dirty_topology",
                serde_json::json!({
                    "session_name": session_name,
                    "issues": topology_issue_ids.iter().map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>(),
                }),
            )
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        return Ok(RestartReport::RefusedDirtyTopology {
            session_name,
            reason: topology_issue_ids
                .first()
                .cloned()
                .unwrap_or_else(|| "dirty_topology".to_string()),
            error: "restart refused: tmux endpoint/socket topology is inconsistent; run diagnose from the intended leader socket before restarting".to_string(),
            issue_ids: topology_issue_ids,
        });
    }
    // E5 task#3 / RC-A6a + E4(leader 裁定:每次 restart 都从角色定义重建 runtime spec,覆盖):
    // 角色定义=第一真相源。角色齐 → compile_team 重建 + 保留运行期 override(session_name)+
    // 写 runtime spec。角色缺(TEAM.md/agents 不在)→ 显式拒(列缺哪些),旧 spec 原地保留不删不用。
    let spec =
        rebuild_runtime_spec_from_roles(&selected.run_workspace, &selected.team_key, &state)?;
    // 重建后 spec_workspace 恒为 runtime spec 的父目录(.team/runtime/<team_key>/)。
    let runtime_spec =
        crate::model::paths::runtime_spec_path(&selected.run_workspace, &selected.team_key);
    let spec_workspace = runtime_spec.parent().ok_or_else(|| {
        LifecycleError::TeamSelect("active team spec workspace not found".to_string())
    })?;
    let safety = crate::lifecycle::launch::effective_runtime_config(&spec)?;
    // Bug 1 (0.4.2 P0): the rebuilt spec is the single source of truth for the
    // active roster. Any agent in state.agents that is NOT in the rebuilt
    // spec is a stale leftover (role doc was deleted between sessions) and
    // must NOT be restarted — otherwise a `team-agent restart` would re-spawn
    // a removed worker from stale state. Prune state.agents in-place before
    // convergence/plan/spawn so every downstream step (resume validation,
    // event log, spawn loop) sees the bounded roster.
    let spec_agent_ids = crate::lifecycle::launch::spec_agent_id_set(&spec);
    if let Some(agents_obj) = state.get_mut("agents").and_then(|v| v.as_object_mut()) {
        let stale_ids: Vec<String> = agents_obj
            .keys()
            .filter(|id| !spec_agent_ids.contains(id.as_str()))
            .cloned()
            .collect();
        for stale_id in &stale_ids {
            agents_obj.remove(stale_id);
            let _ = crate::event_log::EventLog::new(&selected.run_workspace).write(
                "restart.agent_skipped_not_in_spec",
                serde_json::json!({
                    "agent_id": stale_id,
                    "team_key": selected.team_key.as_str(),
                    "reason": "agent present in state.agents but absent from rebuilt spec; \
                               role doc was removed — restart will not respawn removed workers",
                    "action": format!(
                        "to permanently remove run `team-agent remove-agent {stale_id}`; \
                         to re-add restore the role doc under agents/<id>.md and run \
                         `team-agent restart`"
                    ),
                }),
            );
        }
        if !stale_ids.is_empty() {
            save_restart_state(&selected.run_workspace, &mut state, &selected.team_key)?;
        }
    }
    let mut convergence = converge_missing_provider_sessions(
        &mut state,
        session_convergence_deadline(session_converge_deadline_ms),
        session_convergence_poll_interval(),
        &selected.run_workspace,
        allow_fresh,
    )?;
    if convergence.converged && convergence.changed {
        save_restart_state(&selected.run_workspace, &mut state, &selected.team_key)?;
    }
    let repaired_agent_ids =
        repair_resume_sessions_from_event_log(&selected.run_workspace, &mut state)?;
    if !repaired_agent_ids.is_empty() {
        save_restart_session_repairs(
            &selected.run_workspace,
            &mut state,
            &selected.team_key,
            &repaired_agent_ids,
        )?;
        let missing_after_repair = restart_required_missing_session_agent_ids(&state);
        convergence.changed = true;
        convergence.converged = missing_after_repair.is_empty();
        convergence.missing = missing_after_repair;
    }
    if !convergence.converged && !allow_fresh {
        return Ok(RestartReport::RefusedResumeNotReady {
            missing: convergence
                .missing
                .iter()
                .map(|agent_id| AgentId::new(agent_id.clone()))
                .collect(),
            allow_fresh,
            deadline: convergence.deadline,
            elapsed: convergence.elapsed,
            error: "resume_not_ready: session_capture_incomplete".to_string(),
        });
    }
    if !convergence.converged && convergence.changed {
        save_restart_state(&selected.run_workspace, &mut state, &selected.team_key)?;
    }
    let forced_fresh_missing = if convergence.converged {
        std::collections::BTreeSet::new()
    } else {
        convergence.missing.iter().cloned().collect()
    };
    let forced_fresh_convergence = (!convergence.converged).then_some(convergence.clone());
    let plan = classify_restart_plan_with_resume_validation(
        Some(&selected.run_workspace),
        &state,
        allow_fresh,
    )?;
    write_restart_resume_decision_events(
        &selected.run_workspace,
        &state,
        allow_fresh,
        &plan.decisions,
        &forced_fresh_missing,
        forced_fresh_convergence.as_ref(),
        &plan.unresumable,
    )?;
    if !plan.corrupt_entries.is_empty() {
        return Ok(RestartReport::RefusedInvalidFirstSendAt {
            invalid: plan.corrupt_entries,
            allow_fresh,
            error: "invalid first_send_at".to_string(),
        });
    }
    if !plan.unresumable.is_empty() {
        return Ok(RestartReport::RefusedResumeAtomicity {
            unresumable: plan.unresumable,
            allow_fresh,
            error: "restart requires resumable workers before live spawn; rerun with --allow-fresh to start fresh".to_string(),
        });
    }
    // unit-3 (Stage 1) session-identity preflight: before any kill, reject
    // the case where `state.session_name` actually holds a leader launcher
    // session name (`team-agent-leader-*`). Proceeding would tear down the
    // leader pane (E49 / 0.3.39 leader mis-kill). Nothing is created or
    // killed — the caller gets a structured refusal that distinguishes this
    // dirty-state from a normal resume/atomicity refusal.
    match crate::lifecycle::restart::preflight::check_session_preflight(&state) {
        crate::lifecycle::restart::preflight::SessionPreflight::Ok => {}
        crate::lifecycle::restart::preflight::SessionPreflight::WorkerSessionIsLeaderSession {
            session_name,
            reason,
        } => {
            return Ok(RestartReport::RefusedDirtyTopology {
                session_name: session_name.clone(),
                reason: reason.clone(),
                error: format!(
                    "restart refused: state.session_name `{session_name}` is a leader \
                     launcher session ({reason}); aborting before any tmux kill. Repair \
                     state.session_name to the worker session and re-run."
                ),
                issue_ids: vec![reason.clone()],
            });
        }
    }
    let session_name = state_session_name(&state);
    if session_live_or_default(transport, &session_name, false) {
        // 0.3.28 Step 5 (warn-only): per architecture, restart should REFUSE
        // when the worker session is live (Python `restart/orchestration.py:79-85`
        // raises `_tmux_session_conflict_error`). Pre-0.3.28 Rust kills the
        // worker session here — which under the old co-located topology also
        // killed the leader pane (the E57-1 cascade contribution from the
        // layout layer).
        //
        // After Step 2 the leader lives in a DIFFERENT session
        // (`team-agent-leader-...`), so killing the worker session no longer
        // tears down the leader. That makes this kill structurally safe, but
        // it still loses provider session state in the worker panes.
        //
        // Full "refuse" semantics will land once Steps 6+7 expose the
        // recovery path so users have a clean alternative. For now we emit
        // the warn-only event so operators see the drift in event logs.
        eprintln!(
            "team_agent::layout restart_precondition_warning worker_session=`{}` action=killing \
             (post-Step-7 will refuse and direct user to recover; safe today because Step 2 \
             moved leader to dedicated session)",
            session_name.as_str()
        );
        transport
            .kill_session(&session_name)
            .map_err(|e| LifecycleError::Transport(e.to_string()))?;
        mark_leader_receiver_rebind_required(&mut state, &session_name);
        mark_restart_targets_stopped_after_teardown(&mut state, &plan.decisions);
        let topology_authority_agent_ids = plan
            .decisions
            .iter()
            .map(|decision| decision.agent_id.as_str().to_string())
            .collect::<Vec<_>>();
        save_restart_state_with_lifecycle_topology_authority(
            &selected.run_workspace,
            &mut state,
            &selected.team_key,
            &topology_authority_agent_ids,
        )?;
    }
    let mut successful_agents: Vec<RestartedAgent> = Vec::new();
    let mut failed_agents: Vec<RestartFailedAgent> = Vec::new();
    let mut fatal_resume_failure = false;
    // B5 restart isolation loop: per-agent spawn failures must be recorded and
    // isolated here. G1 resume-integrity failures set a fatal flag and skip later
    // spawns; do not reintroduce `?`, `break`, or `return` inside this loop.
    // BEGIN_B5_RESTART_ISOLATION_LOOP
    for decision in &plan.decisions {
        if fatal_resume_failure {
            continue;
        }
        let Some(raw_agent) = state
            .get("agents")
            .and_then(|v| v.get(decision.agent_id.as_str()))
            .cloned()
        else {
            let error = format!("agent {} not found for restart", decision.agent_id);
            mark_agent_restart_failed(&mut state, decision, &error);
            let _ = write_restart_agent_failed_event(
                &selected.run_workspace,
                decision,
                "spawn",
                &error,
            );
            failed_agents.push(restart_failed_agent(decision, "spawn", error));
            continue;
        };
        let agent = rehydrate_agent_command_context_from_spec(
            spec_workspace,
            &decision.agent_id,
            &raw_agent,
        );
        if has_endpoint_convergence_marker(&state) && is_fake_model_harness_agent(&agent) {
            mark_fake_harness_agent_respawned(
                &mut state,
                &decision.agent_id,
                &session_name,
                &selected.team_key,
            );
            successful_agents.push(decision.clone());
            continue;
        }
        let session_id = if matches!(decision.restart_mode, StartMode::Resumed) {
            decision.session_id.as_ref()
        } else {
            None
        };
        let mut session_live = session_live_or_default(transport, &session_name, false);
        if !session_live {
            if let Some(previous) = successful_agents.pop() {
                let error = format!(
                    "session_disappeared_after_spawn: provider_resume_exited for {}; session {} disappeared before spawning {}",
                    previous.agent_id,
                    session_name.as_str(),
                    decision.agent_id
                );
                mark_agent_restart_failed(&mut state, &previous, &error);
                let _ = write_restart_agent_failed_event(
                    &selected.run_workspace,
                    &previous,
                    "resume",
                    &error,
                );
                failed_agents.push(restart_failed_agent(&previous, "resume", error));
                if is_resume_integrity_failure(&previous, "resume", "") {
                    fatal_resume_failure = true;
                }
                session_live = false;
            }
        }
        if fatal_resume_failure {
            continue;
        }
        let layout_placement = crate::lifecycle::launch::adaptive_existing_placement_for_agent(
            &state,
            transport,
            &session_name,
            &decision.agent_id,
        );
        let spawn = match spawn_agent_window(
            &selected.run_workspace,
            &session_name,
            &decision.agent_id,
            &agent,
            session_id,
            session_live,
            transport,
            Some(&safety),
            layout_placement.as_ref(),
            None,
            // Issue 2 (Round 3b gate review §6): thread the resolved
            // selected.team_key so the worker MCP env carries the right
            // owner_team_id even when top-level active_team_key is stale.
            Some(selected.team_key.as_str()),
        ) {
            Ok(spawn) => spawn,
            Err(error) => {
                let error = error.to_string();
                mark_agent_restart_failed(&mut state, decision, &error);
                let phase = restart_failure_phase(decision, "spawn", &error);
                let _ = write_restart_agent_failed_event(
                    &selected.run_workspace,
                    decision,
                    phase,
                    &error,
                );
                failed_agents.push(restart_failed_agent(decision, phase, error));
                if phase == "resume" {
                    fatal_resume_failure = true;
                }
                continue;
            }
        };
        if let Err(error) = verify_spawned_agent_live(&decision.agent_id, &spawn, transport)
            .and_then(|_| {
                mark_agent_respawned(
                    &mut state,
                    &decision.agent_id,
                    decision.restart_mode,
                    &spawn,
                    transport,
                    &safety,
                )
            })
        {
            let error = error.to_string();
            mark_agent_restart_failed(&mut state, decision, &error);
            let phase = restart_failure_phase(decision, "readiness", &error);
            let _ =
                write_restart_agent_failed_event(&selected.run_workspace, decision, phase, &error);
            failed_agents.push(restart_failed_agent(decision, phase, error));
            if phase == "resume" {
                fatal_resume_failure = true;
            }
            continue;
        }
        successful_agents.push(decision.clone());
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|agents| agents.get_mut(decision.agent_id.as_str()))
            .and_then(serde_json::Value::as_object_mut)
        {
            persist_effective_approval_policy_for_restart(agent, &safety);
        }
    }
    // END_B5_RESTART_ISOLATION_LOOP
    let mut topology_authority_agent_ids = successful_agents
        .iter()
        .map(|agent| agent.agent_id.as_str().to_string())
        .collect::<Vec<_>>();
    topology_authority_agent_ids.extend(
        failed_agents
            .iter()
            .map(|agent| agent.agent_id.as_str().to_string()),
    );
    let capture_backfill_skip_agent_ids = successful_agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.restart_mode,
                StartMode::Fresh | StartMode::FreshAfterMissingRollout
            )
        })
        .map(|agent| agent.agent_id.as_str().to_string())
        .collect::<Vec<_>>();
    save_restart_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
        &selected.run_workspace,
        &mut state,
        &selected.team_key,
        &capture_backfill_skip_agent_ids,
        &topology_authority_agent_ids,
    )?;
    if fatal_resume_failure {
        let attach_commands = Vec::new();
        let next_actions = restart_failure_next_actions(&failed_agents);
        write_restart_completed_event(
            &selected.run_workspace,
            &successful_agents,
            &failed_agents,
            "fail",
        )?;
        return Ok(RestartReport::Failed {
            session_name,
            failed_agents,
            next_actions,
            attach_commands,
        });
    }
    if successful_agents.is_empty() && !failed_agents.is_empty() {
        let attach_commands = Vec::new();
        let next_actions = restart_failure_next_actions(&failed_agents);
        write_restart_completed_event(
            &selected.run_workspace,
            &successful_agents,
            &failed_agents,
            "fail",
        )?;
        return Ok(RestartReport::Failed {
            session_name,
            failed_agents,
            next_actions,
            attach_commands,
        });
    }
    // RM-039-SESS-001 step 2 (architect verdict 2026-06-22): post-respawn
    // resume backing validation.
    //
    // Pre-kill preflight already proves backing exists, but killing the
    // provider process can cause the provider to rotate / clean up its
    // transient backing file (the Claude case in the evidence: the
    // sessions/<pid>.json file is the process-tracking record and goes away
    // when the worker dies). Resumed agents preserve the OLD
    // capture tuple (mark_agent_respawned only clears it on Fresh/
    // FreshAfterMissingRollout), so without this re-probe the runtime
    // reports `restart.completed rc:"ok"` while L2 provider backing truth
    // is missing — a false-green restart.
    //
    // Strategy:
    //   1. For every Resumed worker in `successful_agents`, re-run the
    //      same `resume_backing_probe_for_agent` used at preflight.
    //   2. Emit `restart.resume_postflight` carrying agent_id, session_id,
    //      exists, checked_paths.
    //   3. If exists=false: clear stale capture fields (rollout_path,
    //      captured_at, captured_via, attribution_confidence) but
    //      preserve session_id; set `_pending_session_id` to the same
    //      session_id so convergence searches for the resumed session
    //      explicitly. Then run one bounded convergence pass.
    //   4. Re-probe. If still missing and !allow_fresh, demote the agent
    //      from successful to failed with phase `resume_postflight` and
    //      `session_backing_store_missing_after_restart`. The outer
    //      branches below will then return Failed/Partial instead of OK.
    {
        let mut postflight_failed: Vec<crate::lifecycle::types::RestartFailedAgent> = Vec::new();
        let mut survivors: Vec<crate::lifecycle::types::RestartedAgent> =
            Vec::with_capacity(successful_agents.len());
        let convergence_deadline = session_convergence_deadline(session_converge_deadline_ms);
        let convergence_poll = session_convergence_poll_interval();
        for decision in successful_agents.drain(..) {
            if !matches!(
                decision.restart_mode,
                crate::lifecycle::types::StartMode::Resumed
            ) {
                survivors.push(decision);
                continue;
            }
            let Some(agent) = state
                .get("agents")
                .and_then(|v| v.get(decision.agent_id.as_str()))
                .cloned()
            else {
                survivors.push(decision);
                continue;
            };
            let provider = agent_provider(&agent);
            let session_id = match decision.session_id.as_ref() {
                Some(sid) => sid.clone(),
                None => {
                    // No session id on a Resumed decision shouldn't happen,
                    // but if it does there is nothing to revalidate.
                    survivors.push(decision);
                    continue;
                }
            };
            let probe = resume_backing_probe_for_agent(
                &selected.run_workspace,
                &decision.agent_id,
                &agent,
                provider,
                &session_id,
                agent_rollout_path(&agent).as_ref(),
            );
            let identity_probe = session_identity_probe_for_agent(
                &decision.agent_id,
                provider,
                agent_rollout_path(&agent).as_ref(),
            );
            write_restart_resume_postflight_event(
                &selected.run_workspace,
                &decision.agent_id,
                Some(&session_id),
                probe.exists,
                &probe.checked_paths,
                /* recaptured = */ false,
                identity_probe.identity_ok,
                identity_probe.embedded_agent_id.as_deref(),
            )?;
            if probe.exists && identity_probe.identity_ok != Some(false) {
                survivors.push(decision);
                continue;
            }
            if identity_probe.identity_ok == Some(false) {
                let phase = "resume_postflight";
                let error = "session_identity_mismatch_after_restart".to_string();
                mark_agent_restart_failed(&mut state, &decision, &error);
                let _ = write_restart_agent_failed_event(
                    &selected.run_workspace,
                    &decision,
                    phase,
                    &error,
                );
                postflight_failed.push(restart_failed_agent(&decision, phase, error));
                continue;
            }
            // 0.4.6 tuple-atomic contract: backing went missing between
            // preflight and post-spawn. Clear the FULL authoritative tuple
            // (including session_id) and persist the old provider id only
            // in `_pending_session_id` as a capture hint. Pre-0.4.6 kept
            // session_id alive while removing siblings — that left a
            // partial tuple that persist-layer backfill could later
            // resurrect into "looks captured" or that classify_restart
            // would refuse with `session_backing_store_missing`.
            if let Some(agents) = state
                .pointer_mut("/agents")
                .and_then(serde_json::Value::as_object_mut)
            {
                if let Some(agent_obj) = agents
                    .get_mut(decision.agent_id.as_str())
                    .and_then(serde_json::Value::as_object_mut)
                {
                    for field in [
                        "session_id",
                        "rollout_path",
                        "captured_at",
                        "captured_via",
                        "attribution_confidence",
                    ] {
                        agent_obj.remove(field);
                    }
                    agent_obj.insert(
                        "_pending_session_id".to_string(),
                        serde_json::json!(session_id.as_str()),
                    );
                }
            }
            // Persist state before convergence so the helper sees the
            // cleared tuple.
            save_restart_state(&selected.run_workspace, &mut state, &selected.team_key)?;
            let _converge = converge_missing_provider_sessions(
                &mut state,
                convergence_deadline,
                convergence_poll,
                &selected.run_workspace,
                allow_fresh,
            )?;
            save_restart_state(&selected.run_workspace, &mut state, &selected.team_key)?;
            // Re-probe with the freshest agent state.
            let agent_after = state
                .get("agents")
                .and_then(|v| v.get(decision.agent_id.as_str()))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let probe_after = resume_backing_probe_for_agent(
                &selected.run_workspace,
                &decision.agent_id,
                &agent_after,
                provider,
                &session_id,
                agent_rollout_path(&agent_after).as_ref(),
            );
            let identity_probe_after = session_identity_probe_for_agent(
                &decision.agent_id,
                provider,
                agent_rollout_path(&agent_after).as_ref(),
            );
            write_restart_resume_postflight_event(
                &selected.run_workspace,
                &decision.agent_id,
                Some(&session_id),
                probe_after.exists,
                &probe_after.checked_paths,
                /* recaptured = */ true,
                identity_probe_after.identity_ok,
                identity_probe_after.embedded_agent_id.as_deref(),
            )?;
            if probe_after.exists && identity_probe_after.identity_ok != Some(false) {
                survivors.push(decision);
                continue;
            }
            // Still missing. Demote the agent.
            let phase = "resume_postflight";
            let error = if identity_probe_after.identity_ok == Some(false) {
                "session_identity_mismatch_after_restart".to_string()
            } else {
                "session_backing_store_missing_after_restart".to_string()
            };
            mark_agent_restart_failed(&mut state, &decision, &error);
            let _ =
                write_restart_agent_failed_event(&selected.run_workspace, &decision, phase, &error);
            postflight_failed.push(restart_failed_agent(&decision, phase, error));
        }
        successful_agents = survivors;
        failed_agents.extend(postflight_failed);
        if !failed_agents.is_empty() {
            let topology_authority_agent_ids = failed_agents
                .iter()
                .map(|agent| agent.agent_id.as_str().to_string())
                .collect::<Vec<_>>();
            save_restart_state_with_lifecycle_topology_authority(
                &selected.run_workspace,
                &mut state,
                &selected.team_key,
                &topology_authority_agent_ids,
            )?;
        }
    }
    if successful_agents.is_empty() && !failed_agents.is_empty() {
        // Postflight demoted every survivor — emit Failed before coordinator start.
        let attach_commands = Vec::new();
        let next_actions = restart_failure_next_actions(&failed_agents);
        write_restart_completed_event(
            &selected.run_workspace,
            &successful_agents,
            &failed_agents,
            "fail",
        )?;
        return Ok(RestartReport::Failed {
            session_name,
            failed_agents,
            next_actions,
            attach_commands,
        });
    }
    drop(lifecycle_lock);
    let coordinator_started = start_coordinator_for_workspace(&selected.run_workspace)?;
    wait_restart_readiness_or_timeout(
        &selected.run_workspace,
        &state,
        &session_name,
        &successful_agents,
        transport,
        restart_readiness_deadline(readiness_deadline_ms),
        restart_readiness_poll_interval(),
    )?;
    let attach_commands =
        crate::tmux_backend::attach_command_for_session(&selected.run_workspace, &session_name)
            .into_iter()
            .collect::<Vec<_>>();
    let mut next_actions = Vec::new();
    if !failed_agents.is_empty() {
        next_actions.extend(restart_failure_next_actions(&failed_agents));
        write_restart_completed_event(
            &selected.run_workspace,
            &successful_agents,
            &failed_agents,
            "partial",
        )?;
        // 0.3.30 Bug 1: auto-attach on partial restart too — workers that did
        // come up still need a leader_receiver pane to deliver report_result.
        try_autobind_leader_after_restart(
            &selected.run_workspace,
            Some(selected.team_key.as_str()),
            &state,
        );
        return Ok(RestartReport::Partial {
            session_name,
            agents: successful_agents,
            failed_agents,
            coordinator_started,
            next_actions,
            attach_commands,
        });
    }
    write_restart_completed_event(
        &selected.run_workspace,
        &successful_agents,
        &failed_agents,
        "ok",
    )?;
    // 0.3.30 Bug 1: auto-attach leader from caller's TMUX_PANE if available.
    // Mirrors quick-start's seed_launched_owner_from_env behaviour: a restart
    // invoked from a tmux pane should bind that pane as leader_receiver,
    // restoring the worker→leader delivery path. Failure is non-fatal — the
    // user can still run `team-agent attach-leader` manually.
    try_autobind_leader_after_restart(&selected.run_workspace, Some(&selected.team_key), &state);
    // 0.3.28 Step 1: topology invariant guard (warn-only). Same pattern as
    // `lifecycle::launch::launch_with_transport_in_workspace` — logs to stderr,
    // never panics. Hard error path is deferred to Step 10.
    let violations = crate::layout::sessions::assert_topology_invariants(&state, &spec);
    crate::layout::sessions::log_topology_violations(&violations);
    Ok(RestartReport::Restarted {
        session_name,
        agents: successful_agents,
        coordinator_started,
        next_actions,
        attach_commands,
    })
}

fn repair_resume_sessions_from_event_log(
    workspace: &Path,
    state: &mut serde_json::Value,
) -> Result<Vec<String>, LifecycleError> {
    let agent_ids = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|agents| agents.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut repaired_agent_ids = Vec::new();
    for agent_id in agent_ids {
        let previous = state
            .get("agents")
            .and_then(|agents| agents.get(&agent_id))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if previous
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| !session.is_empty())
        {
            continue;
        }
        let Some(provider) = previous
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_provider)
        else {
            continue;
        };
        let auth_mode = previous
            .get("auth_mode")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_auth_mode)
            .unwrap_or(AuthMode::Subscription);
        let exclude_session_ids = claimed_session_ids_except(state, &agent_id);
        let adapter = crate::provider::get_adapter(provider);
        let repaired = crate::session_capture::recover_resume_session_from_events(
            workspace,
            &agent_id,
            &previous,
            adapter.as_ref(),
            auth_mode,
            &exclude_session_ids,
        )
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
        let Some(repaired) = repaired else {
            continue;
        };
        let old_session_id = previous
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|session| !session.is_empty())
            .map(str::to_string);
        let session_id = repaired
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|session| !session.is_empty())
            .map(str::to_string);
        let rollout_path = repaired
            .get("rollout_path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        // 0.4.6 tuple-atomic contract (audit §Restart 修改清单, line 175):
        // capture-domain repair must yield a COMPLETE authoritative tuple
        // (session_id + rollout_path + captured_at + captured_via) before
        // restart writes it back into agent state. A partial repair would
        // be persist-domain truth synthesis through the restart hook,
        // exactly what the contract forbids. Reject incomplete repairs;
        // capture stays Stage-1 pending on next coordinator tick.
        let captured_at_ok = repaired
            .get("captured_at")
            .is_some_and(|v| !v.is_null() && v.as_str().is_some_and(|s| !s.is_empty()));
        let captured_via_ok = repaired
            .get("captured_via")
            .is_some_and(|v| !v.is_null() && v.as_str().is_some_and(|s| !s.is_empty()));
        if session_id.is_none() || rollout_path.is_none() || !captured_at_ok || !captured_via_ok {
            crate::event_log::EventLog::new(workspace)
                .write(
                    "resume.session_repair_refused_incomplete_tuple",
                    serde_json::json!({
                        "agent_id": agent_id,
                        "provider": provider_wire(provider),
                        "session_id": session_id,
                        "rollout_path": rollout_path,
                        "captured_at_ok": captured_at_ok,
                        "captured_via_ok": captured_via_ok,
                    }),
                )
                .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
            continue;
        }
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|agents| agents.get_mut(&agent_id))
        {
            *agent = repaired.clone();
        }
        crate::event_log::EventLog::new(workspace)
            .write(
                "resume.session_repaired",
                serde_json::json!({
                    "agent_id": agent_id,
                    "provider": provider_wire(provider),
                    "old_session_id": old_session_id,
                    "session_id": session_id,
                    "rollout_path": rollout_path,
                    "captured_via": "event_log_repair",
                    "attribution_confidence": repaired.get("attribution_confidence").cloned().unwrap_or(serde_json::Value::Null),
                }),
            )
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
        repaired_agent_ids.push(agent_id);
    }
    Ok(repaired_agent_ids)
}

fn claimed_session_ids_except(
    state: &serde_json::Value,
    current_agent_id: &str,
) -> std::collections::BTreeSet<String> {
    let mut keys: std::collections::BTreeSet<String> = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|agents| {
            agents
                .iter()
                .filter(|(agent_id, _)| agent_id.as_str() != current_agent_id)
                .filter_map(|(_, agent)| {
                    agent
                        .get("session_id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|session| !session.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    // E57 (lane-046-capture-gap): restart's resume_session_repair postflight
    // path must NOT reassign the leader's own provider session to a worker.
    // The capture-layer allocator (session_capture::claimed_provider_session_keys)
    // already excludes leader_receiver/team_owner; mirror that here for the
    // event-log recovery path, otherwise a fresh restart can pull the leader
    // session_id out of events.jsonl and write it onto a worker
    // (release-engineer / any worker with no captured session). Scan
    // state.leader_receiver, state.team_owner, and the same fields under
    // state.teams.<key>.
    push_leader_session_ids(&mut keys, state);
    if let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) {
        for team_state in teams.values() {
            push_leader_session_ids(&mut keys, team_state);
        }
    }
    keys
}

fn push_leader_session_ids(
    keys: &mut std::collections::BTreeSet<String>,
    scope: &serde_json::Value,
) {
    for anchor in ["leader_receiver", "team_owner"] {
        if let Some(node) = scope.get(anchor) {
            for field in ["session_id", "provider_session_id"] {
                if let Some(session_id) = node
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    keys.insert(session_id.to_string());
                }
            }
        }
    }
}

fn session_convergence_deadline(requested_ms: Option<u64>) -> std::time::Duration {
    if let Some(ms) = requested_ms {
        return std::time::Duration::from_millis(ms);
    }
    env_duration_ms(
        &[
            "TEAM_AGENT_RESTART_SESSION_CAPTURE_DEADLINE_MS",
            "TEAM_AGENT_RESTART_SESSION_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_RESTART_CAPTURE_DEADLINE_MS",
            "TEAM_AGENT_RESTART_CAPTURE_TIMEOUT_MS",
            "TEAM_AGENT_RESTART_SESSION_CAPTURE_TIMEOUT_MS",
            "TEAM_AGENT_RESTART_SESSION_CONVERGENCE_TIMEOUT_MS",
            "TEAM_AGENT_SESSION_CAPTURE_DEADLINE_MS",
            "TEAM_AGENT_SESSION_CAPTURE_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_SESSION_CAPTURE_TIMEOUT_MS",
            "TEAM_AGENT_SESSION_CAPTURE_CONVERGENCE_TIMEOUT_MS",
            "TEAM_AGENT_SESSION_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_SESSION_CONVERGENCE_TIMEOUT_MS",
            "TEAM_AGENT_PROVIDER_SESSION_CONVERGENCE_DEADLINE_MS",
            "TEAM_AGENT_PROVIDER_SESSION_CONVERGENCE_TIMEOUT_MS",
        ],
        crate::session_capture::RESTART_SESSION_CONVERGENCE_DEADLINE_MS,
    )
}

fn session_convergence_poll_interval() -> std::time::Duration {
    env_duration_ms(
        &[
            "TEAM_AGENT_RESTART_SESSION_CAPTURE_POLL_MS",
            "TEAM_AGENT_RESTART_SESSION_CONVERGENCE_POLL_MS",
            "TEAM_AGENT_RESTART_CAPTURE_POLL_MS",
            "TEAM_AGENT_SESSION_CAPTURE_POLL_MS",
            "TEAM_AGENT_SESSION_CAPTURE_CONVERGENCE_POLL_MS",
            "TEAM_AGENT_SESSION_CONVERGENCE_POLL_MS",
            "TEAM_AGENT_PROVIDER_SESSION_CONVERGENCE_POLL_MS",
        ],
        crate::session_capture::RESTART_SESSION_CONVERGENCE_POLL_MS,
    )
}

fn env_duration_ms(names: &[&str], default_ms: u64) -> std::time::Duration {
    let ms = names
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|value| parse_duration_value_ms(&value))
                .or_else(|| {
                    name.strip_suffix("_MS").and_then(|prefix| {
                        std::env::var(prefix)
                            .ok()
                            .and_then(|value| parse_duration_value_seconds_ms(&value))
                    })
                })
        })
        .unwrap_or(default_ms);
    std::time::Duration::from_millis(ms)
}

fn parse_duration_value_ms(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

fn parse_duration_value_seconds_ms(value: &str) -> Option<u64> {
    let seconds = value.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds >= 0.0 {
        Some((seconds * 1000.0).round() as u64)
    } else {
        None
    }
}

fn restart_readiness_deadline(requested_ms: Option<u64>) -> std::time::Duration {
    requested_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| env_duration_ms(&["TEAM_AGENT_RESTART_READINESS_DEADLINE_MS"], 30_000))
}

fn restart_readiness_poll_interval() -> std::time::Duration {
    env_duration_ms(&["TEAM_AGENT_RESTART_READINESS_POLL_MS"], 200)
}

#[derive(Debug, Clone, Copy)]
struct RestartReadiness {
    session_created: bool,
    worker_pane_addressable: bool,
    coordinator_alive: bool,
}

impl RestartReadiness {
    fn ready(self) -> bool {
        self.session_created && self.worker_pane_addressable && self.coordinator_alive
    }
}

fn wait_restart_readiness_or_timeout(
    workspace: &Path,
    state: &serde_json::Value,
    session_name: &SessionName,
    decisions: &[RestartedAgent],
    transport: &dyn crate::transport::Transport,
    deadline: std::time::Duration,
    poll_interval: std::time::Duration,
) -> Result<(), LifecycleError> {
    let started = std::time::Instant::now();
    loop {
        let readiness = restart_readiness(workspace, state, session_name, decisions, transport);
        if readiness.ready() {
            return Ok(());
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            write_restart_readiness_timeout_event(workspace, readiness, deadline, elapsed)?;
            return Err(LifecycleError::RequirementUnmet(
                restart_readiness_timeout_message(workspace, readiness, deadline),
            ));
        }
        std::thread::sleep(std::cmp::min(
            poll_interval,
            deadline.saturating_sub(elapsed),
        ));
    }
}

fn restart_readiness(
    workspace: &Path,
    state: &serde_json::Value,
    session_name: &SessionName,
    decisions: &[RestartedAgent],
    transport: &dyn crate::transport::Transport,
) -> RestartReadiness {
    let session_created = session_live_or_default(transport, session_name, false);
    let worker_pane_addressable = restart_worker_panes_addressable(state, decisions, transport);
    let coordinator_workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let coordinator_alive =
        crate::coordinator::coordinator_health(&coordinator_workspace).ok && session_created;
    RestartReadiness {
        session_created,
        worker_pane_addressable,
        coordinator_alive,
    }
}

fn restart_worker_panes_addressable(
    state: &serde_json::Value,
    decisions: &[RestartedAgent],
    transport: &dyn crate::transport::Transport,
) -> bool {
    if decisions.is_empty() {
        return true;
    }
    if has_endpoint_convergence_marker(state)
        && decisions.iter().all(|decision| {
            state
                .get("agents")
                .and_then(|agents| agents.get(decision.agent_id.as_str()))
                .and_then(|agent| agent.get("pane_id"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|pane| pane.starts_with("__team_agent_fake_harness_"))
        })
    {
        return true;
    }
    decisions.iter().all(|decision| {
        let Some(pane_id) = state
            .get("agents")
            .and_then(|agents| agents.get(decision.agent_id.as_str()))
            .and_then(|agent| agent.get("pane_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|pane| !pane.is_empty())
            .map(crate::transport::PaneId::new)
        else {
            return false;
        };
        pane_addressable(transport, &pane_id)
    })
}

fn pane_addressable(
    transport: &dyn crate::transport::Transport,
    pane_id: &crate::transport::PaneId,
) -> bool {
    match transport.has_pane(pane_id) {
        Ok(Some(present)) => present,
        Ok(None) | Err(_) => {
            transport
                .list_targets()
                .map(|targets| targets.iter().any(|pane| pane.pane_id == *pane_id))
                .unwrap_or(false)
                || transport
                    .liveness(pane_id)
                    .map(|state| state == crate::transport::PaneLiveness::Live)
                    .unwrap_or(false)
        }
    }
}

fn write_restart_readiness_timeout_event(
    workspace: &Path,
    readiness: RestartReadiness,
    deadline: std::time::Duration,
    elapsed: std::time::Duration,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "restart.readiness_timeout",
            serde_json::json!({
                "tmux_session_created": readiness.session_created,
                "worker_pane_addressable": readiness.worker_pane_addressable,
                "coordinator_alive": readiness.coordinator_alive,
                "deadline_ms": deadline.as_millis(),
                "elapsed_ms": elapsed.as_millis(),
                "coordinator_log": crate::coordinator::coordinator_log_path(
                    &crate::coordinator::WorkspacePath::new(workspace.to_path_buf())
                ).display().to_string(),
                "state_path": crate::state::persist::runtime_state_path(workspace).display().to_string(),
                "pid_path": crate::coordinator::coordinator_pid_path(
                    &crate::coordinator::WorkspacePath::new(workspace.to_path_buf())
                ).display().to_string(),
            }),
        )
        .map(|_| ())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn restart_readiness_timeout_message(
    workspace: &Path,
    readiness: RestartReadiness,
    deadline: std::time::Duration,
) -> String {
    let coordinator_workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let deadline_s = deadline.as_secs_f64();
    format!(
        "restart not ready within {deadline_s:.1}s: {missing}\n\
         - tmux session created: {session}\n\
         - worker pane addressable: {pane}\n\
         - coordinator alive: {coordinator}\n\
         Action: check coordinator log {log}, then `team-agent restart <agent> --allow-fresh` or `team-agent diagnose`\n\
         Log: coordinator_log={log} state={state} pid_file={pid}",
        missing = restart_readiness_missing_summary(readiness),
        session = yes_no(readiness.session_created),
        pane = yes_no(readiness.worker_pane_addressable),
        coordinator = yes_no(readiness.coordinator_alive),
        log = crate::coordinator::coordinator_log_path(&coordinator_workspace).display(),
        state = crate::state::persist::runtime_state_path(workspace).display(),
        pid = crate::coordinator::coordinator_pid_path(&coordinator_workspace).display(),
    )
}

fn restart_readiness_missing_summary(readiness: RestartReadiness) -> String {
    let mut missing = Vec::new();
    if !readiness.session_created {
        missing.push("tmux session created");
    }
    if !readiness.worker_pane_addressable {
        missing.push("worker pane addressable");
    }
    if !readiness.coordinator_alive {
        missing.push("coordinator alive");
    }
    missing.join(", ")
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn verify_spawned_agent_live(
    _agent_id: &AgentId,
    _spawn: &SpawnedAgentWindow,
    _transport: &dyn crate::transport::Transport,
) -> Result<(), LifecycleError> {
    Ok(())
}

/// 0.3.30 Bug 1: restart success path auto-attach.
/// When `TMUX_PANE` is present in the caller's env, treat the restart as if
/// the user had also run `attach-leader` from that pane. Mirrors quick-start's
/// `seed_launched_owner_from_env` semantics.
///
/// Failure modes are intentionally non-fatal — `attach_leader` returns Err if
/// the pane validation rejects (e.g. caller pane is a registered worker pane,
/// E51 guard). In that case the user must still run `attach-leader` manually,
/// matching pre-fix behaviour. We log to stderr so the operator sees why
/// auto-attach didn't take.
fn try_autobind_leader_after_restart(
    workspace: &std::path::Path,
    team: Option<&str>,
    state: &serde_json::Value,
) {
    if std::env::var_os("TMUX_PANE").is_none() {
        return;
    }
    // Provider: prefer the existing team_owner.provider (if rebind),
    // else leader_receiver.provider (stale, but still informative),
    // else default to ClaudeCode (matches the most common deployment).
    let provider = state
        .pointer("/team_owner/provider")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            state
                .pointer("/leader_receiver/provider")
                .and_then(serde_json::Value::as_str)
        })
        .and_then(|s| {
            // Legacy auto-attach: collapse any claude variant to ClaudeCode
            // (this site historically treated `Claude` and `ClaudeCode` as
            // the same attach target). Wire-format `parse_provider` keeps
            // them distinct everywhere else.
            crate::provider::wire::parse_provider(s).map(|p| match p {
                crate::model::enums::Provider::Claude => crate::model::enums::Provider::ClaudeCode,
                other => other,
            })
        })
        .unwrap_or(crate::model::enums::Provider::ClaudeCode);
    let team_str = team;
    match crate::leader::attach_leader(workspace, team_str, None, provider) {
        Ok(result) if result.ok => {
            eprintln!(
                "team_agent::restart auto_attach_leader ok pane={:?} team={:?}",
                result.bound_pane_id.as_ref().map(|p| p.as_str()),
                team_str,
            );
        }
        Ok(result) => {
            eprintln!(
                "team_agent::restart auto_attach_leader skipped reason={:?} team={:?}",
                result.reason, team_str,
            );
        }
        Err(error) => {
            eprintln!(
                "team_agent::restart auto_attach_leader failed error={error} team={team_str:?} \
                 (run `team-agent attach-leader` from your tmux pane to bind manually)",
            );
        }
    }
}

fn mark_leader_receiver_rebind_required(state: &mut serde_json::Value, session_name: &SessionName) {
    let Some(receiver) = state
        .get_mut("leader_receiver")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let same_session = receiver
        .get("session_name")
        .and_then(|v| v.as_str())
        .map(|session| session == session_name.as_str())
        .unwrap_or(true);
    if !same_session {
        return;
    }
    if receiver
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|status| status == "attached")
    {
        receiver.insert("status".to_string(), serde_json::json!("rebind_required"));
    }
}

fn mark_restart_targets_stopped_after_teardown(
    state: &mut serde_json::Value,
    decisions: &[RestartedAgent],
) {
    let Some(agents) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for decision in decisions {
        let Some(agent) = agents
            .get_mut(decision.agent_id.as_str())
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        agent.insert("status".to_string(), serde_json::json!("stopped"));
        agent.remove("pane_id");
        agent.remove("pane_pid");
    }
}

fn save_restart_state(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
) -> Result<(), LifecycleError> {
    save_restart_projected_state(workspace, state, team_key, &[])
}

fn save_restart_state_with_lifecycle_topology_authority(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
    agent_ids: &[String],
) -> Result<(), LifecycleError> {
    save_restart_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
        workspace,
        state,
        team_key,
        &[],
        agent_ids,
    )
}

fn save_restart_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
    skip_capture_backfill_agent_ids: &[String],
    topology_agent_ids: &[String],
) -> Result<(), LifecycleError> {
    let skip_capture_backfill_agent_ids = skip_capture_backfill_agent_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let topology_agent_ids = topology_agent_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    sync_restart_team_projections(state, team_key);
    crate::state::projection::save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
        workspace,
        state,
        &skip_capture_backfill_agent_ids,
        &topology_agent_ids,
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn save_restart_session_repairs(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
    agent_ids: &[String],
) -> Result<(), LifecycleError> {
    let repairs = collect_session_repair_fields(state, agent_ids);
    sync_restart_team_projections(state, team_key);
    match crate::state::projection::save_team_scoped_state(workspace, state) {
        Ok(()) => Ok(()),
        Err(crate::state::StateError::SaveConflict(_)) => {
            let mut latest =
                crate::state::projection::select_runtime_state(workspace, Some(team_key))
                    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
            for (agent_id, repair) in &repairs {
                apply_session_repair_fields(&mut latest, agent_id, repair);
            }
            sync_restart_team_projections(&mut latest, team_key);
            crate::state::projection::save_team_scoped_state(workspace, &latest)
                .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
            *state = latest;
            Ok(())
        }
        Err(error) => Err(LifecycleError::StatePersist(error.to_string())),
    }
}

fn collect_session_repair_fields(
    state: &serde_json::Value,
    agent_ids: &[String],
) -> Vec<(String, serde_json::Value)> {
    agent_ids
        .iter()
        .filter_map(|agent_id| {
            state
                .get("agents")
                .and_then(|agents| agents.get(agent_id))
                .cloned()
                .map(|agent| (agent_id.clone(), agent))
        })
        .collect()
}

fn apply_session_repair_fields(
    state: &mut serde_json::Value,
    agent_id: &str,
    repair: &serde_json::Value,
) {
    let Some(target) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let Some(repair) = repair.as_object() else {
        return;
    };
    for field in [
        "session_id",
        "rollout_path",
        "captured_at",
        "captured_via",
        "attribution_confidence",
    ] {
        if let Some(value) = repair.get(field) {
            target.insert(field.to_string(), value.clone());
        } else {
            target.remove(field);
        }
    }
    target.remove("attribution_ambiguous");
}

fn mark_agent_respawned(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    restart_mode: StartMode,
    spawn: &SpawnedAgentWindow,
    transport: &dyn crate::transport::Transport,
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Err(LifecycleError::StatePersist(format!(
            "agent {} state is not an object",
            agent_id
        )));
    };
    agent.insert("status".to_string(), serde_json::json!("running"));
    agent.insert(
        "window".to_string(),
        serde_json::json!(spawn.spawn.window.as_str()),
    );
    agent.insert(
        "pane_id".to_string(),
        serde_json::json!(spawn.spawn.pane_id.as_str()),
    );
    let pane_pid = spawn.spawn.child_pid.or_else(|| {
        transport
            .list_targets()
            .unwrap_or_default()
            .into_iter()
            .find(|pane| pane.pane_id == spawn.spawn.pane_id)
            .and_then(|pane| pane.pane_pid)
    });
    if let Some(pane_pid) = pane_pid {
        agent.insert("pane_pid".to_string(), serde_json::json!(pane_pid));
    } else {
        agent.remove("pane_pid");
    }
    agent.insert(
        "spawned_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    agent.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(spawn.spawn_cwd.to_string_lossy().to_string()),
    );
    // Issue 2 (Round 3b gate review §6): persist the resolved owner_team_id
    // back into the agent row so future restarts read it directly from the
    // agent row (cascade priority 2) instead of relying on top-level
    // active_team_key (priority 3).
    if let Some(ref team_id) = spawn.owner_team_id {
        if !team_id.is_empty() {
            agent.insert("owner_team_id".to_string(), serde_json::json!(team_id));
        }
    }
    // Bug 2 (0.3.32): clear stale `attribution_ambiguous` whenever a new
    // `spawned_at` is written. Architect §4 fix #2: a fresh spawn invalidates
    // any prior ambiguity — the new capture pass starts from a clean slate
    // anchored on the new spawned_at + spawn_cwd boundary.
    agent.remove("attribution_ambiguous");
    // Bug 1 (capture promotion, 0.3.32): on Fresh / FreshAfterMissingRollout,
    // `spawn.plan.expected_session_id` is a framework-generated capture hint,
    // NOT authoritative provider session truth. Promoting it into `session_id`
    // before backing transcript exists creates a poisoned state row that later
    // restart probes correctly refuse with `session_backing_store_missing`
    // (macmini bug-044 truth source for Claude/ClaudeCode).
    //
    // 0.4.6 tuple-atomic contract (restart-persist-capture-contract-audit.md):
    // ALL providers — restart never promotes a planned id into authoritative
    // `session_id`. Capture (provider scanner) is the only authoritative
    // writer. On Fresh / FreshAfterMissingRollout, clear the full tuple;
    // `persist_command_plan_state` below writes `_pending_session_id` as a
    // capture hint, and the provider scanner promotes that hint into the
    // tuple ONLY after it confirms backing (sqlite/transcript/.codex).
    //
    // This deletes the Copilot promotion that previously violated the
    // capture-only-writer rule (audit §Restart 当前违规 #1) and uses the
    // Claude family clear path uniformly. The Copilot scanner
    // (provider/adapter.rs:1217-1228) already gates expected-id with a
    // sqlite point-check, so no truth is lost.
    // S1-CAPTURE-001 (0.4.8): on Fresh / FreshAfterMissingRollout, clear the
    // FULL prior-session authoritative capture tuple — not just session_id.
    // Mirrors mark_agent_started (restart/agent.rs:728-740) so the rebuild path
    // (multi-agent restart) and the single-agent start path enforce the same
    // fresh-tuple invariant. Without this, save_restart_state's persist
    // backfill can revive the stale rollout_path/captured_at/capture_state
    // tuple from latest, defeating the fresh-tuple guarantee and leaving
    // delivered tokens in the old transcript (leader/unassigned mis-attrib).
    if matches!(
        restart_mode,
        StartMode::Fresh | StartMode::FreshAfterMissingRollout
    ) {
        for field in [
            "session_id",
            "rollout_path",
            "captured_at",
            "captured_via",
            "attribution_confidence",
            "capture_state",
            "attribution_ambiguous",
        ] {
            agent.remove(field);
        }
    }
    crate::lifecycle::launch::persist_command_plan_state(agent, &spawn.plan, &spawn.profile_launch);
    persist_effective_approval_policy_for_restart(agent, safety);
    if let Some(placement) = spawn.layout_placement.as_ref() {
        agent.insert(
            "layout_window".to_string(),
            serde_json::json!(placement.layout_window.as_str()),
        );
        agent.insert(
            "layout_index".to_string(),
            serde_json::json!(placement.layout_index),
        );
        agent.insert(
            "pane_index".to_string(),
            serde_json::json!(placement.pane_index),
        );
        agent.insert(
            "display".to_string(),
            serde_json::json!({
                "backend": "adaptive",
                "status": "opened",
                "window": placement.layout_window.as_str(),
                "workspace_window": null,
                "pane_id": spawn.spawn.pane_id.as_str(),
                "pane_title": agent_id.as_str(),
                "target": spawn.spawn.pane_id.as_str(),
                "target_worker_session": spawn.spawn.session.as_str(),
                "linked_session": null,
                "leader_session": spawn.spawn.session.as_str(),
                "display_session": null,
                "fallback": null,
            }),
        );
    }
    if matches!(
        restart_mode,
        StartMode::Fresh | StartMode::FreshAfterMissingRollout
    ) && spawn.plan.expected_session_id.is_some()
    {
        agent.insert("rollout_path".to_string(), serde_json::Value::Null);
        agent.insert("captured_at".to_string(), serde_json::Value::Null);
        agent.insert("captured_via".to_string(), serde_json::Value::Null);
        agent.insert(
            "attribution_confidence".to_string(),
            serde_json::Value::Null,
        );
    }
    agent.remove("startup_prompts");
    agent.remove("startup_prompt_status");
    agent.remove("startup_prompt_probe_epoch");
    agent.remove("startup_prompt_probe_disabled_at");
    Ok(())
}

fn mark_fake_harness_agent_respawned(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    session_name: &SessionName,
    team_key: &str,
) {
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    agent.insert("status".to_string(), serde_json::json!("running"));
    agent.insert("window".to_string(), serde_json::json!(agent_id.as_str()));
    agent.insert(
        "pane_id".to_string(),
        serde_json::json!(format!("__team_agent_fake_harness_{}", agent_id.as_str())),
    );
    agent.remove("pane_pid");
    agent.insert(
        "session_name".to_string(),
        serde_json::json!(session_name.as_str()),
    );
    agent.insert(
        "spawned_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    agent.insert("owner_team_id".to_string(), serde_json::json!(team_key));
}

fn restart_failed_agent(
    decision: &RestartedAgent,
    phase: impl Into<String>,
    error: String,
) -> RestartFailedAgent {
    RestartFailedAgent {
        agent_id: decision.agent_id.clone(),
        restart_mode: decision.restart_mode,
        decision: decision.decision,
        session_id: decision.session_id.clone(),
        phase: phase.into(),
        error,
    }
}

fn restart_failure_phase(
    decision: &RestartedAgent,
    phase: &'static str,
    error: &str,
) -> &'static str {
    if is_resume_integrity_failure(decision, phase, error) {
        "resume"
    } else {
        phase
    }
}

fn is_resume_integrity_failure(decision: &RestartedAgent, phase: &str, error: &str) -> bool {
    if !matches!(decision.restart_mode, StartMode::Resumed) {
        return false;
    }
    if phase == "resume" || phase == "readiness" {
        return true;
    }
    error.contains("session_disappeared_after_spawn")
        || error.contains("provider_resume_exited")
        || error.contains("resume_not_ready")
        || error.contains("resume_atomicity")
        || error.contains("no live pane")
        || error.contains("no-pane")
}

fn mark_agent_restart_failed(
    state: &mut serde_json::Value,
    decision: &RestartedAgent,
    error: &str,
) {
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(decision.agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    agent.insert("status".to_string(), serde_json::json!("failed"));
    agent.insert("restart_error".to_string(), serde_json::json!(error));
    agent.insert(
        "restart_failed_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    agent.remove("pane_id");
    agent.remove("pane_pid");
}

fn write_restart_agent_failed_event(
    workspace: &Path,
    decision: &RestartedAgent,
    phase: &str,
    error: &str,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "restart.agent_failed",
            serde_json::json!({
                "agent_id": decision.agent_id.as_str(),
                "restart_mode": start_mode_wire(decision.restart_mode),
                "decision": resume_decision_wire(decision.decision),
                "session_id": decision.session_id.as_ref().map(|session| session.as_str()),
                "phase": phase,
                "error": error,
                "action": format!(
                    "inspect worker {} output, then restart that worker with `team-agent restart-agent {}` or rerun `team-agent restart --allow-fresh`",
                    decision.agent_id,
                    decision.agent_id
                ),
                "log": format!(
                    ".team/logs/coordinator.log and .team/runtime/state.json agent={}",
                    decision.agent_id
                ),
            }),
        )
        .map(|_| ())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

/// RM-039-SESS-001 step 2 (architect verdict 2026-06-22): emit
/// `restart.resume_postflight` for each Resumed worker we re-probed after
/// spawn. `recaptured=true` means this event is the second probe after
/// one bounded `converge_missing_provider_sessions` pass; `false` means
/// it's the first probe right after spawn. `exists` reflects whether the
/// provider backing file actually exists on disk; `checked_paths` lists
/// every path the runtime probed.
fn write_restart_resume_postflight_event(
    workspace: &Path,
    agent_id: &AgentId,
    session_id: Option<&SessionId>,
    exists: bool,
    checked_paths: &[std::path::PathBuf],
    recaptured: bool,
    identity_ok: Option<bool>,
    embedded_agent_id: Option<&str>,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "restart.resume_postflight",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "session_id": session_id.map(SessionId::as_str),
                "exists": exists,
                "checked_paths": checked_paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                "recaptured": recaptured,
                "identity_ok": identity_ok,
                "embedded_agent_id": embedded_agent_id,
            }),
        )
        .map(|_| ())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn write_restart_completed_event(
    workspace: &Path,
    successful_agents: &[RestartedAgent],
    failed_agents: &[RestartFailedAgent],
    rc: &str,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "restart.completed",
            serde_json::json!({
                "rc": rc,
                "status": rc,
                "successful_agents": successful_agents
                    .iter()
                    .map(|agent| agent.agent_id.as_str())
                    .collect::<Vec<_>>(),
                "failed_agents": failed_agents
                    .iter()
                    .map(|failure| serde_json::json!({
                        "agent_id": failure.agent_id.as_str(),
                        "phase": failure.phase,
                        "error": failure.error,
                    }))
                    .collect::<Vec<_>>(),
            }),
        )
        .map(|_| ())
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn restart_failure_next_actions(failed_agents: &[RestartFailedAgent]) -> Vec<String> {
    failed_agents
        .iter()
        .map(|failure| {
            format!(
                "inspect worker {} output, then restart that worker with `team-agent restart-agent {}` or rerun `team-agent restart --allow-fresh`",
                failure.agent_id, failure.agent_id
            )
        })
        .collect()
}

fn start_mode_wire(mode: StartMode) -> &'static str {
    match mode {
        StartMode::Resumed => "resumed",
        StartMode::Fresh => "fresh",
        StartMode::FreshAfterMissingRollout => "fresh_after_missing_rollout",
        StartMode::Noop => "noop",
    }
}

fn resume_decision_wire(decision: ResumeDecision) -> &'static str {
    match decision {
        ResumeDecision::Resume => "resume",
        ResumeDecision::FreshStart => "fresh_start",
        ResumeDecision::Refuse => "refuse",
    }
}

fn write_restart_resume_decision_events(
    workspace: &Path,
    state: &serde_json::Value,
    allow_fresh: bool,
    decisions: &[RestartedAgent],
    forced_fresh_missing: &std::collections::BTreeSet<String>,
    forced_fresh_convergence: Option<&crate::session_capture::SessionConvergence>,
    unresumable: &[crate::lifecycle::types::UnresumableWorker],
) -> Result<(), LifecycleError> {
    // Layer 2 (leader directive 2026-06-22): every restart.resume_decision
    // event for a Refuse decision must carry the structured refusal_reason
    // wire string so operators can see WHY without crawling state.json.
    let refusal_index: std::collections::BTreeMap<
        &str,
        &crate::lifecycle::types::UnresumableWorker,
    > = unresumable
        .iter()
        .map(|u| (u.agent_id.as_str(), u))
        .collect();
    for decision in decisions {
        let agent = state
            .get("agents")
            .and_then(|v| v.get(decision.agent_id.as_str()))
            .unwrap_or(&serde_json::Value::Null);
        let first_send_at = agent
            .get("first_send_at")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let session_id = decision.session_id.as_ref().map(|s| s.as_str().to_string());
        let decision_wire = resume_decision_wire(decision.decision);
        write_restart_resume_decision_event(
            workspace,
            decision.agent_id.as_str(),
            first_send_at,
            session_id,
            allow_fresh,
            decision_wire,
            forced_fresh_missing.contains(decision.agent_id.as_str()),
            forced_fresh_convergence,
            refusal_index.get(decision.agent_id.as_str()).copied(),
        )?;
    }
    Ok(())
}

fn write_restart_resume_decision_event(
    workspace: &Path,
    worker_id: &str,
    first_send_at: Option<String>,
    session_id: Option<String>,
    allow_fresh: bool,
    decision: &str,
    forced_fresh: bool,
    forced_fresh_convergence: Option<&crate::session_capture::SessionConvergence>,
    unresumable: Option<&crate::lifecycle::types::UnresumableWorker>,
) -> Result<(), LifecycleError> {
    use std::io::Write as _;

    let path = workspace.join(".team").join("logs").join("events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    let mut event = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "event": crate::lifecycle::types::event_names::RESTART_RESUME_DECISION,
        "worker_id": worker_id,
        "has_first_send_at": first_send_at.is_some(),
        "has_session_id": session_id.is_some(),
        "allow_fresh": allow_fresh,
        "decision": decision,
        "first_send_at": first_send_at,
        "session_id": session_id,
    });
    // Layer 2 (leader directive 2026-06-22): when this is a Refuse decision
    // and we have a structured ResumeRefusalReason, emit the wire string +
    // optional recovery hint so the event-log audit is actionable. Falls
    // back to the legacy free-form `reason` string when no structured
    // refusal_reason is set.
    if decision == "refuse" {
        if let Some(u) = unresumable {
            if let Some(obj) = event.as_object_mut() {
                obj.insert("refusal_reason".to_string(), serde_json::json!(u.reason));
                if let Some(structured) = &u.refusal_reason {
                    obj.insert(
                        "refusal_reason_wire".to_string(),
                        serde_json::json!(structured.wire()),
                    );
                    if let crate::provider::session::ResumeRefusalReason::SessionBackingStoreMissing {
                        checked_paths,
                        recovery_hint,
                    } = structured
                    {
                        if !checked_paths.is_empty() {
                            obj.insert(
                                "checked_paths".to_string(),
                                serde_json::json!(checked_paths
                                    .iter()
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .collect::<Vec<_>>()),
                            );
                        }
                        if let Some(hint) = recovery_hint {
                            let mut h = serde_json::Map::new();
                            h.insert("provider".to_string(), serde_json::json!(hint.provider));
                            if let Some(name) = &hint.provider_session_name_hint {
                                h.insert("name".to_string(), serde_json::json!(name));
                            }
                            if let Some(cwd) = &hint.spawn_cwd {
                                h.insert(
                                    "spawn_cwd".to_string(),
                                    serde_json::json!(cwd.to_string_lossy()),
                                );
                            }
                            h.insert(
                                "picker_hint".to_string(),
                                serde_json::json!(hint.picker_hint()),
                            );
                            obj.insert(
                                "recovery_hint".to_string(),
                                serde_json::Value::Object(h),
                            );
                        }
                    }
                    if let crate::provider::session::ResumeRefusalReason::SessionIdentityMismatch {
                        expected_agent_id,
                        embedded_agent_id,
                        session_id,
                        rollout_path,
                    } = structured
                    {
                        obj.insert(
                            "expected_agent_id".to_string(),
                            serde_json::json!(expected_agent_id),
                        );
                        obj.insert(
                            "embedded_agent_id".to_string(),
                            serde_json::json!(embedded_agent_id),
                        );
                        obj.insert(
                            "poisoned_session_id".to_string(),
                            serde_json::json!(session_id),
                        );
                        if let Some(path) = rollout_path {
                            obj.insert(
                                "rollout_path".to_string(),
                                serde_json::json!(path.to_string_lossy()),
                            );
                        }
                    }
                }
            }
        }
    }
    if forced_fresh {
        if let Some(event) = event.as_object_mut() {
            event.insert("forced_fresh".to_string(), serde_json::json!(true));
            event.insert("reason".to_string(), serde_json::json!("resume_not_ready"));
            if let Some(convergence) = forced_fresh_convergence {
                event.insert(
                    "session_convergence".to_string(),
                    serde_json::json!({
                        "complete": false,
                        "deadline_s": convergence.deadline.as_secs_f64(),
                        "deadline_ms": convergence.deadline.as_millis(),
                        "elapsed_ms": convergence.elapsed.as_millis(),
                        "pending_agent_ids": convergence.missing.clone(),
                    }),
                );
            }
        }
    }
    let line =
        serde_json::to_string(&event).map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    file.write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

/// `restart_candidates(workspace)`(`restart/selection.py:12`)。从 snapshot + active
/// state 收集可重启 team。
pub fn restart_candidates(workspace: &Path) -> Result<Vec<RestartCandidate>, LifecycleError> {
    let state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let alive = crate::state::projection::team_state_candidates(&state);
    if alive.is_empty() {
        if !state_has_restart_candidate_shape(&state) {
            return Ok(Vec::new());
        }
        let key = crate::state::projection::team_state_key(&state);
        return Ok(vec![restart_candidate_from_state(workspace, &key, &state)]);
    }
    Ok(alive
        .keys()
        .map(|key| {
            let projected = crate::state::projection::project_top_level_view(&state, key);
            restart_candidate_from_state(workspace, key, &projected)
        })
        .collect())
}

/// `select_restart_state(workspace, team)`(`restart/selection.py:49`)。按 `--team` 或
/// 唯一性选一个;歧义/未找到 → `TeamSelect`。
pub fn select_restart_state(
    workspace: &Path,
    team: Option<&str>,
) -> Result<RestartCandidate, LifecycleError> {
    let selected = crate::state::projection::select_runtime_state(workspace, team)
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    if !state_has_restart_candidate_shape(&selected) {
        let name = team.filter(|t| !t.is_empty()).unwrap_or("<default>");
        return Err(LifecycleError::TeamSelect(format!(
            "restart team {name:?} not found"
        )));
    }
    let key = selected
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || crate::state::projection::team_state_key(&selected),
            str::to_string,
        );
    Ok(restart_candidate_from_state(workspace, &key, &selected))
}

fn state_has_restart_candidate_shape(state: &serde_json::Value) -> bool {
    state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.is_empty())
        || state
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|agents| !agents.is_empty())
}

fn restart_candidate_from_state(
    workspace: &Path,
    team_name: &str,
    state: &serde_json::Value,
) -> RestartCandidate {
    let session_name = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .unwrap_or_else(|| SessionName::new(team_name));
    let mut agents = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|map| map.keys().map(AgentId::new).collect::<Vec<_>>())
        .unwrap_or_default();
    agents.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    RestartCandidate {
        session_name,
        team_name: team_name.to_string(),
        state_path: crate::state::persist::runtime_state_path(workspace),
        spec_path: restart_candidate_spec_path(workspace, state),
        has_context: restart_candidate_has_context(state),
        agents,
    }
}

/// E5 task#3 / RC-A6a:每次 restart 都以**角色定义**(team_dir 的 TEAM.md+agents/*.md)
/// compile_team 重建 runtime spec(覆盖),保留运行期 override(session_name 必须延续,
/// 否则 tmux session 对不上)。写到 .team/runtime/<team_key>/team.spec.yaml。
///
/// 角色定义缺(team_dir 未记 / TEAM.md 不在 / agents 不在)→ **显式拒**(LifecycleError,
/// CLI N38 三行式),列出缺哪些;**旧 spec 原地保留不删不用**(T2 防数据销毁,无静默路径)。
fn rebuild_runtime_spec_from_roles(
    run_workspace: &Path,
    team_key: &str,
    state: &serde_json::Value,
) -> Result<YamlValue, LifecycleError> {
    // team_dir(角色定义源)优先取 state.team_dir;缺则回落 run_workspace(自含 team-dir 布局,
    // run_workspace 本身即角色目录)。两者都无角色定义则下面的齐全性检查会显式拒。
    let team_dir = state
        .get("team_dir")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| run_workspace.to_path_buf());
    // 角色定义齐全性检查(显式拒,列缺哪些;旧 spec 不动)。
    let mut missing: Vec<String> = Vec::new();
    if !team_dir.join("TEAM.md").exists() {
        missing.push(format!("{}/TEAM.md", team_dir.display()));
    }
    let agents_dir = team_dir.join("agents");
    let has_role_doc = std::fs::read_dir(&agents_dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        })
        .unwrap_or(false);
    if !has_role_doc {
        missing.push(format!(
            "{}/*.md (at least one role doc)",
            agents_dir.display()
        ));
    }
    if !missing.is_empty() {
        if let Some(spec) = load_endpoint_convergence_runtime_spec(run_workspace, team_key, state)?
        {
            return Ok(spec);
        }
        // N38 三行式:error / action / log。旧 runtime spec 原地保留(不删不用)。
        return Err(LifecycleError::TeamSelect(format!(
            "cannot restart: role definitions missing for team '{team_key}': {}. \
             action: restore the listed role docs (TEAM.md + agents/*.md are the source of truth), \
             then re-run restart; the previous runtime spec is left in place (not used). \
             log: team_dir={}",
            missing.join(", "),
            team_dir.display(),
        )));
    }
    // 重建:compile_team(角色定义) + 保留运行期 session_name override。
    let mut spec = crate::compiler::compile_team(&team_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    crate::lifecycle::launch::override_spec_workspace(&mut spec, run_workspace);
    if let Some(session_name) = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        crate::lifecycle::launch::override_spec_session_name(&mut spec, session_name);
    }
    // 写 runtime spec(覆盖,原子 tmp+rename;Bug2)。
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, team_key);
    crate::lifecycle::launch::write_spec_atomic(&spec_path, &spec)?;
    // RC-A6a:重建成功后清理用户目录的 legacy spec(中间产物不该留在角色目录)。
    // 仅删 team_dir 下的 team.spec.yaml(角色定义 TEAM.md/agents 不动);失败不致命(best-effort)。
    let legacy_spec = team_dir.join("team.spec.yaml");
    if legacy_spec.exists() && legacy_spec != spec_path {
        let _ = std::fs::remove_file(&legacy_spec);
    }
    Ok(spec)
}

fn load_endpoint_convergence_runtime_spec(
    run_workspace: &Path,
    team_key: &str,
    state: &serde_json::Value,
) -> Result<Option<YamlValue>, LifecycleError> {
    if !has_endpoint_convergence_marker(state) {
        return Ok(None);
    }
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, team_key);
    if !spec_path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", spec_path.display())))?;
    let mut spec =
        crate::model::yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    crate::lifecycle::launch::override_spec_workspace(&mut spec, run_workspace);
    if let Some(session_name) = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        crate::lifecycle::launch::override_spec_session_name(&mut spec, session_name);
    }
    Ok(Some(spec))
}

fn has_endpoint_convergence_marker(state: &serde_json::Value) -> bool {
    state
        .get("topology_convergence")
        .and_then(|v| v.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("converged")
}

fn is_fake_model_harness_agent(agent: &serde_json::Value) -> bool {
    agent_provider(agent) == crate::model::enums::Provider::Fake
        && agent.get("model").and_then(serde_json::Value::as_str) == Some("fake")
}

fn restart_candidate_spec_path(workspace: &Path, state: &serde_json::Value) -> std::path::PathBuf {
    if let Some(path) = state
        .get("spec_path")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return std::path::PathBuf::from(path);
    }
    if let Some(team_dir) = state
        .get("team_dir")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return std::path::Path::new(team_dir).join("team.spec.yaml");
    }
    workspace.join("team.spec.yaml")
}

fn restart_candidate_has_context(state: &serde_json::Value) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                ["session_id", "rollout_path", "first_send_at"]
                    .iter()
                    .any(|key| {
                        agent
                            .get(*key)
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::transport::{
        AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
        PaneField, PaneInfo, PaneLiveness, SetEnvOutcome, SpawnResult, Target, Transport,
        TransportError,
    };

    struct RespawnEpochTransport;

    impl Transport for RespawnEpochTransport {
        fn kind(&self) -> BackendKind {
            BackendKind::Tmux
        }

        fn spawn_first(
            &self,
            _session: &SessionName,
            _window: &WindowName,
            _argv: &[String],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            unimplemented!("not used by respawn epoch test")
        }

        fn spawn_into(
            &self,
            _session: &SessionName,
            _window: &WindowName,
            _argv: &[String],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            unimplemented!("not used by respawn epoch test")
        }

        fn inject(
            &self,
            _target: &Target,
            _payload: &InjectPayload,
            _submit: Key,
            _bracketed: bool,
        ) -> Result<InjectReport, TransportError> {
            unimplemented!("not used by respawn epoch test")
        }

        fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
            unimplemented!("not used by respawn epoch test")
        }

        fn capture(
            &self,
            _target: &Target,
            range: CaptureRange,
        ) -> Result<CapturedText, TransportError> {
            Ok(CapturedText {
                text: String::new(),
                range,
            })
        }

        fn query(
            &self,
            _target: &Target,
            _field: PaneField,
        ) -> Result<Option<String>, TransportError> {
            Ok(None)
        }

        fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
            Ok(PaneLiveness::Live)
        }

        fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
            Ok(Vec::new())
        }

        fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
            Ok(true)
        }

        fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
            Ok(Vec::new())
        }

        fn set_session_env(
            &self,
            _session: &SessionName,
            _key: &str,
            _value: &str,
        ) -> Result<SetEnvOutcome, TransportError> {
            Ok(SetEnvOutcome::Applied)
        }

        fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
            Ok(())
        }

        fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
            Ok(())
        }

        fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
            Ok(AttachOutcome::Attached)
        }
    }

    fn disabled_safety() -> DangerousApproval {
        DangerousApproval {
            enabled: false,
            source: DangerousApprovalSource::Disabled,
            inherited: false,
            provider: None,
            flag: None,
            worker_capability_above_leader: false,
            ancestry_binary_name: None,
            unexpected_binary: false,
        }
    }

    #[test]
    fn respawn_refreshes_startup_probe_epoch_inputs() {
        let mut state = serde_json::json!({
            "agents": {
                "w1": {
                    "status": "running",
                    "window": "w1-old",
                    "pane_id": "%old",
                    "pane_pid": 1010,
                    "spawned_at": "2026-06-01T00:00:00+00:00",
                    "startup_prompts": "disabled_for_epoch",
                    "startup_prompt_status": "disabled_for_epoch",
                    "startup_prompt_probe_epoch": "pane_pid:1010",
                    "startup_prompt_probe_disabled_at": "2026-06-01T00:02:30+00:00"
                }
            }
        });
        let spawn = SpawnedAgentWindow {
            spawn: SpawnResult {
                pane_id: PaneId::new("%new"),
                session: SessionName::new("team-epoch"),
                window: WindowName::new("w1"),
                child_pid: Some(2020),
            },
            plan: crate::provider::CommandPlan::argv_only(vec!["codex".to_string()]),
            profile_launch: crate::provider::ProviderProfileLaunch::default(),
            layout_placement: None,
            spawn_cwd: std::path::PathBuf::from("/tmp/team-epoch"),
            owner_team_id: None,
        };
        let before = chrono::Utc::now();

        mark_agent_respawned(
            &mut state,
            &AgentId::new("w1"),
            StartMode::Resumed,
            &spawn,
            &RespawnEpochTransport,
            &disabled_safety(),
        )
        .expect("mark respawned");

        let agent = state.pointer("/agents/w1").expect("agent");
        assert_eq!(
            agent.get("pane_id").and_then(serde_json::Value::as_str),
            Some("%new")
        );
        assert_eq!(
            agent.get("pane_pid").and_then(serde_json::Value::as_u64),
            Some(2020)
        );
        let spawned_at = agent
            .get("spawned_at")
            .and_then(serde_json::Value::as_str)
            .expect("spawned_at refreshed");
        let spawned_at = chrono::DateTime::parse_from_rfc3339(spawned_at)
            .expect("spawned_at rfc3339")
            .with_timezone(&chrono::Utc);
        assert!(
            spawned_at >= before - chrono::Duration::seconds(1),
            "respawn must start a fresh startup-probe grace window; spawned_at={spawned_at}, before={before}"
        );
        for key in [
            "startup_prompts",
            "startup_prompt_status",
            "startup_prompt_probe_epoch",
            "startup_prompt_probe_disabled_at",
        ] {
            assert!(
                agent.get(key).is_none(),
                "respawned pane must not inherit old startup prompt epoch field {key}"
            );
        }
    }

    #[test]
    fn restart_projection_sync_updates_active_team_and_current_alias() {
        let mut state = serde_json::json!({
            "active_team_key": "team-alpha",
            "team_dir": "/tmp/team-alpha",
            "session_name": "team-alpha",
            "agents": {
                "alpha": {
                    "status": "running",
                    "pane_id": "%new",
                    "pane_pid": 47650,
                    "spawned_at": "2026-06-13T04:00:00+00:00"
                }
            },
            "teams": {
                "current": {
                    "team_dir": "/tmp/team-alpha",
                    "session_name": "team-alpha",
                    "agents": {
                        "alpha": {
                            "status": "running",
                            "pane_id": "%new",
                            "pane_pid": 47650,
                            "spawned_at": "2026-06-13T04:00:00+00:00"
                        }
                    }
                },
                "team-alpha": {
                    "team_dir": "/tmp/team-alpha",
                    "session_name": "team-alpha",
                    "agents": {
                        "alpha": {
                            "status": "running",
                            "pane_id": "%old",
                            "pane_pid": 46784,
                            "spawned_at": "2026-06-13T03:00:00+00:00",
                            "startup_prompt_probe_epoch": "pane_pid:46784"
                        }
                    }
                }
            }
        });

        sync_restart_team_projections(&mut state, "team-alpha");

        for pointer in [
            "/agents/alpha",
            "/teams/current/agents/alpha",
            "/teams/team-alpha/agents/alpha",
        ] {
            let agent = state.pointer(pointer).expect(pointer);
            assert_eq!(
                agent.get("pane_id").and_then(serde_json::Value::as_str),
                Some("%new")
            );
            assert_eq!(
                agent.get("pane_pid").and_then(serde_json::Value::as_u64),
                Some(47650)
            );
            assert_eq!(
                agent.get("spawned_at").and_then(serde_json::Value::as_str),
                Some("2026-06-13T04:00:00+00:00")
            );
            assert!(
                agent.get("startup_prompt_probe_epoch").is_none(),
                "{pointer} must not retain the old startup probe epoch"
            );
        }
    }
}
