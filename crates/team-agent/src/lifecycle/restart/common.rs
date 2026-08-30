//! ---
//! purpose: restart 与 start 共用的底座，含 spawn 执行、transport 解析、state 标记与各类读取判定
//! contract:
//!   provides:
//!     - name: spawn_agent_window
//!       what: 为一个席位拼命令并在目标 session 里开窗或分格起进程
//!     - name: lifecycle_worker_tmux_backend_for_selected_state
//!       what: 按团队已持久化的 endpoint 解析 tmux 后端，非 tmux 状态直接拒绝
//!     - name: save_restart_projected_state
//!       what: 同步团队投影后经 repository 带写意图落盘
//!     - name: resume_backing_probe_for_agent
//!       what: 探测该席位的 resume 依据是否在盘上，并记下所有查过的路径
//!     - name: converge_missing_provider_sessions
//!       what: 有界等待会话捕获收敛，并逐轮写进度事件
//!     - name: mark_agent_stopped
//!       what: 把席位标记为已停并清掉 window 与 pane_id 字段
//!   depends:
//!     - crate::transport::Transport
//!     - crate::tmux_backend
//!     - crate::transport_factory
//!     - crate::provider
//!     - crate::session_capture
//!     - crate::state::projection
//!     - crate::state::repository
//!     - crate::state::persist
//!     - crate::coordinator::health
//!     - crate::event_log::EventLog
//!     - crate::lifecycle::launch
//!     - crate::lifecycle::profile_launch
//!     - crate::lifecycle::worker_command_context
//!     - crate::lifecycle::restart::selection
//! boundary:
//!   - 状态是 conpty 时 tmux 专用解析器直接报错，不静默降级到 tmux
//!   - 探测类判定失败一律倒向保守值，不把不确定当成存活
//!   - 清活动观测只删观测字段，不动生命周期与拓扑字段
//!   - 收敛与排空都是有界等待，不无限阻塞
//! maturity: wired
//! ---
use super::*;
use crate::transport::Transport;

pub(super) struct SpawnedAgentWindow {
    pub spawn: crate::transport::SpawnResult,
    pub spawned_at: String,
    pub plan: crate::provider::CommandPlan,
    pub profile_launch: crate::provider::ProviderProfileLaunch,
    pub layout_placement: Option<crate::lifecycle::launch::LayoutPlacement>,
    pub spawn_cwd: std::path::PathBuf,
    /// Issue 2 (Round 3b gate review §6): the resolved `owner_team_id` used
    /// for this spawn's MCP env / command. Callers (`mark_agent_respawned`,
    /// `mark_agent_started`) must persist this back into the agent row so
    /// future restarts read it directly (priority #2 in the resolution
    /// cascade) instead of relying on top-level `active_team_key`.
    pub owner_team_id: Option<String>,
}

#[derive(Clone)]
pub(super) struct SameRoleCohortTarget {
    pub agent_id: String,
    pub window: String,
    pub expected_pane_id: Option<String>,
}

impl SameRoleCohortTarget {
/// ---
/// purpose: 构造一个同角色同批目标
/// params:
///   window: 该席位的窗口名
/// returns: 未带期望 pane 的目标
/// ---
    pub(super) fn new(agent_id: &AgentId, window: &str) -> Self {
        Self {
            agent_id: agent_id.as_str().to_string(),
            window: window.to_string(),
            expected_pane_id: None,
        }
    }

/// ---
/// purpose: 给目标补上期望的旧 pane
/// returns: 带期望 pane 的目标
/// ---
    pub(super) fn with_expected_pane_id(mut self, pane_id: Option<&str>) -> Self {
        self.expected_pane_id = pane_id.map(ToString::to_string);
        self
    }
}

/// ---
/// purpose: 判断该窗口是不是按席位命名的独立窗口
/// returns: 窗口名等于席位名且不是规范布局窗口时为 true
/// ---
pub(super) fn is_per_agent_cohort_window(window: &str, agent_id: &AgentId) -> bool {
    window == agent_id.as_str() && !crate::lifecycle::launch::is_adaptive_layout_window_pub(window)
}

/// ---
/// purpose: spawn 之前检查同角色同批是否已有残留
/// returns: 期望基数为 0；不满足时给出可读的拒绝说明，满足则 None
/// ---
pub(super) fn same_role_cohort_pre_spawn_error(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    operation: &str,
    targets: &[SameRoleCohortTarget],
) -> Option<String> {
    same_role_cohort_error(transport, session_name, operation, targets, 0, true)
}

/// ---
/// purpose: spawn 之后检查同角色同批是否恰好只剩一个
/// returns: 期望基数为 1；不满足时给出可读的拒绝说明，满足则 None
/// ---
pub(super) fn same_role_cohort_exactly_one_error(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    operation: &str,
    targets: &[SameRoleCohortTarget],
) -> Option<String> {
    same_role_cohort_error(transport, session_name, operation, targets, 1, false)
}

/// ---
/// purpose: 杀掉这些目标记录的旧 pane
/// returns: 全部处理完返回空值；目标没有期望 pane 或该 pane 明确不存在时跳过
/// errors: 杀 pane 失败时返回带席位、窗口与 pane 的错误串
/// ---
pub(super) fn retire_expected_same_role_cohorts(
    transport: &dyn crate::transport::Transport,
    operation: &str,
    targets: &[SameRoleCohortTarget],
) -> Result<(), String> {
    for target in targets {
        let Some(pane_id) = target.expected_pane_id.as_deref() else {
            continue;
        };
        let pane = crate::transport::PaneId::new(pane_id);
        if transport.has_pane(&pane).ok().flatten() == Some(false) {
            continue;
        }
        transport.kill_pane(&pane).map_err(|error| {
            format!(
                "{operation} failed to retire old same-role cohort {}:window={}:pane={pane_id}: {error}",
                target.agent_id, target.window
            )
        })?;
    }
    Ok(())
}

fn same_role_cohort_error(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    operation: &str,
    targets: &[SameRoleCohortTarget],
    expected_live: usize,
    expected_old_only: bool,
) -> Option<String> {
    let panes = match transport.list_targets() {
        Ok(panes) => panes,
        Err(error) => {
            return Some(format!(
                "{operation} refused: same-role cohort observation failed: {error}"
            ));
        }
    };
    let mut cardinality_proofs = Vec::new();
    let mut binding_proofs = Vec::new();
    let mut observation_proofs = Vec::new();
    for target in targets {
        let candidate_panes = panes
            .iter()
            .filter(|pane| pane.session.as_str() == session_name.as_str())
            .filter(|pane| {
                pane.window_name
                    .as_ref()
                    .is_some_and(|window| window.as_str() == target.window)
            })
            .collect::<Vec<_>>();
        let mut live_panes = Vec::new();
        for pane in candidate_panes {
            match transport.liveness(&pane.pane_id) {
                Ok(crate::transport::PaneLiveness::Live) => {
                    live_panes.push(pane.pane_id.as_str().to_string());
                }
                Ok(crate::transport::PaneLiveness::Dead) => {}
                Ok(crate::transport::PaneLiveness::Unknown) => {
                    observation_proofs.push(format!(
                        "{}:window={}:pane={}:liveness=unknown",
                        target.agent_id,
                        target.window,
                        pane.pane_id.as_str()
                    ));
                }
                Err(error) => observation_proofs.push(format!(
                    "{}:window={}:pane={}:liveness_error={error}",
                    target.agent_id,
                    target.window,
                    pane.pane_id.as_str()
                )),
            }
        }
        if live_panes.len() != expected_live {
            cardinality_proofs.push(format!(
                "{}:window={}:live_panes=[{}]",
                target.agent_id,
                target.window,
                live_panes.join(",")
            ));
        } else if !expected_old_only {
            let observed = live_panes.first().map(String::as_str);
            if target.expected_pane_id.as_deref() != observed {
                binding_proofs.push(format!(
                    "{}:window={}:expected_pane={}:observed_pane={}",
                    target.agent_id,
                    target.window,
                    target.expected_pane_id.as_deref().unwrap_or("<missing>"),
                    observed.unwrap_or("<missing>")
                ));
            }
        }
    }
    if !observation_proofs.is_empty() {
        return Some(format!(
            "{operation} refused: same-role cohort observation failed; {}",
            observation_proofs.join("; ")
        ));
    }
    if !binding_proofs.is_empty() {
        return Some(format!(
            "{operation} refused: spawn identity/binding mismatch; {}",
            binding_proofs.join("; ")
        ));
    }
    if !cardinality_proofs.is_empty() {
        return Some(format!(
            "{operation} refused: same-role cohort duplicate proof failed; {}",
            cardinality_proofs.join("; ")
        ));
    }
    None
}

/// ---
/// purpose: 为一个席位拼出命令并在目标 session 里起进程
/// params:
///   resume_session_id: 给出且该 provider 支持 resume 时按 resume 起，否则丢弃它全新起
///   into_existing_session: 目标 session 已存在时开新窗口，否则新建 session
///   layout_placement: 有布局位置时按布局开窗或分格
///   spawn_cwd_override: 覆盖工作目录
///   owner_team_id_override: 显式指定写进 worker 环境的 owner team，缺省时退回席位行与顶层活跃键
/// returns: spawn 结果、时间戳、命令计划、profile 启动参数、布局位置与实际使用的 owner team
/// errors: 命令拼装、profile 准备或 transport spawn 失败时返回 LifecycleError
/// ---
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_agent_window(
    workspace: &Path,
    session_name: &SessionName,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    resume_session_id: Option<&SessionId>,
    into_existing_session: bool,
    transport: &dyn crate::transport::Transport,
    safety: Option<&DangerousApproval>,
    layout_placement: Option<&crate::lifecycle::launch::LayoutPlacement>,
    spawn_cwd_override: Option<&Path>,
    tmux_endpoint_source: Option<&str>,
    // Issue 2 (Round 3b gate review §6): explicit owner_team_id override.
    // When `Some`, callers (restart/rebuild.rs, restart/agent.rs) thread the
    // resolved `selected.team_key` through here so the worker's MCP env /
    // command argv carries `TEAM_AGENT_OWNER_TEAM_ID=<selected team>` —
    // even when the persisted agent row OR the top-level `active_team_key`
    // is stale. When `None`, falls back to the legacy resolution
    // (agent row → active_team_key) for back-compat with non-restart callers.
    owner_team_id_override: Option<&str>,
) -> Result<SpawnedAgentWindow, LifecycleError> {
    let provider = agent_provider(agent);
    let auth_mode = agent_auth_mode(agent);
    let model = agent.get("model").and_then(|v| v.as_str());
    let adapter = crate::provider::get_adapter(provider);
    let resume_session_id = if adapter.caps().resume {
        resume_session_id
    } else {
        None
    };
    // Contract C / F6.4: thread compiled role/tools/MCP context through restart as well —
    // a restarted worker must come back up with the SAME callable MCP capability + role
    // prompt as a fresh launch, else `report_result` becomes unreachable after every restart.
    let detected_safety;
    let safety = if let Some(safety) = safety {
        safety
    } else {
        detected_safety = crate::lifecycle::launch::effective_runtime_config_for_worker_spawn_json(
            agent, provider,
        )?;
        &detected_safety
    };
    let command_agent = crate::lifecycle::worker_command_context::WorkerCommandAgent::from_json(
        agent,
        Some(agent_id.as_str()),
        provider,
    )?;
    let system_prompt =
        crate::lifecycle::worker_command_context::compile_worker_system_prompt(&command_agent)?;
    let tools = crate::lifecycle::worker_command_context::resolved_tool_strings_for_command(
        &command_agent,
        provider,
    )?;
    let resolved_tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    // owner_team_id resolution priority (Issue 2 fix):
    //   1. caller's explicit override (restart paths pass `selected.team_key`)
    //   2. agent row's persisted `owner_team_id` (set by prior launch/restart)
    //   3. top-level `active_team_key` (legacy fallback for add-agent etc.)
    // The override breaks the dependency on top-level state mutation: even if
    // top-level `active_team_key` is stale (e.g. `ta-probe-ws`), a restart that
    // resolved `selected.team_key=prerelease-040-round3b` propagates THAT team
    // into the worker's MCP env.
    let state_for_team =
        crate::state::persist::load_runtime_state(workspace).unwrap_or(serde_json::json!({}));
    let team_id = owner_team_id_override
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            agent
                .get("owner_team_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            let key =
                crate::messaging::leader_receiver::active_team_key(workspace, &state_for_team);
            (!key.is_empty()).then_some(key)
        });
    let mcp_config = adapter
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let mcp_config = crate::lifecycle::launch::resolve_mcp_config(
        mcp_config,
        workspace,
        agent_id.as_str(),
        team_id.as_deref().unwrap_or(""),
    );
    let mcp_config_path = if provider == Provider::Pi {
        None
    } else {
        Some(crate::lifecycle::launch::write_worker_mcp_config(
            workspace,
            agent_id.as_str(),
            &mcp_config,
        )?)
    };
    let profile_launch =
        crate::lifecycle::profile_launch::prepare_provider_profile_launch_from_json(
            workspace,
            agent_id.as_str(),
            agent,
            Some(&mcp_config),
        )?;
    let command_model = profile_launch.command_overrides.model.as_deref().or(model);
    // 0.4.x provider effort MVP: restart/resume preserves effort from the
    // persisted agent JSON (state's agent["effort"] field, set by launch).
    let restart_effort = crate::lifecycle::launch::provider_effort_for_spawn_json(agent, provider);
    if let Some(event_value) = crate::lifecycle::launch::provider_effort_event_if_dropped_json(
        agent,
        provider,
        agent_id.as_str(),
    ) {
        let _ = crate::event_log::EventLog::new(workspace)
            .write("provider.effort_unsupported", event_value);
    }
    let context = crate::provider::ProviderCommandContext {
        auth_mode,
        mcp_config: Some(&mcp_config),
        system_prompt: Some(system_prompt.as_str()),
        model: command_model,
        tools: &resolved_tool_refs,
        profile_launch: Some(&profile_launch),
        agent_id_hint: Some(agent_id.as_str()),
        effort: restart_effort,
    };
    let pi_spawn_cwd = (provider == Provider::Pi)
        .then(|| {
            agent
                .get("spawn_cwd")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .flatten();
    let spawn_cwd = spawn_cwd_override
        .or(pi_spawn_cwd.as_deref())
        .unwrap_or(workspace);
    let mut plan = if provider == Provider::Pi {
        let model = command_model.ok_or_else(|| {
            LifecycleError::RequirementUnmet(
                "Pi restart requires an explicit qualified model".to_string(),
            )
        })?;
        let effort = restart_effort.ok_or_else(|| {
            LifecycleError::RequirementUnmet(
                "Pi restart requires an explicit thinking effort".to_string(),
            )
        })?;
        let request = crate::lifecycle::launch::pi_mcp::PiMaterializeRequest {
            workspace,
            team_id: team_id.as_deref().unwrap_or(""),
            agent_id: agent_id.as_str(),
            model,
            effort,
            system_prompt: &system_prompt,
            tool_categories: &resolved_tool_refs,
            team_mcp_tools: &["send_message", "report_result"],
            mcp_config: &mcp_config,
        };
        match resume_session_id {
            Some(session_id) => {
                let rollout_path = agent_rollout_path(agent).ok_or_else(|| {
                    LifecycleError::RequirementUnmet(
                        "Pi resume requires the persisted exact session path".to_string(),
                    )
                })?;
                crate::lifecycle::launch::pi_mcp::materialize_pi_resume_plan(
                    request,
                    session_id,
                    rollout_path.as_path(),
                    spawn_cwd,
                )
            }
            None => crate::lifecycle::launch::pi_mcp::materialize_pi_plan(request),
        }
        .map_err(|e| LifecycleError::Provider(e.to_string()))?
    } else {
        match resume_session_id {
            Some(session_id) => adapter
                .build_resume_command_plan(Some(session_id), context)
                .map_err(|e| LifecycleError::Provider(e.to_string()))?,
            None => adapter
                .build_command_plan(context)
                .map_err(|e| LifecycleError::Provider(e.to_string()))?,
        }
    };
    if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
        if let Some(mcp_config_path) = mcp_config_path.as_ref() {
            crate::lifecycle::launch::point_native_mcp_config_at_file(
                &mut plan.argv,
                provider,
                mcp_config_path,
            );
        }
    }
    crate::lifecycle::launch::fill_spawn_placeholders_full(
        &mut plan.argv,
        workspace,
        agent_id.as_str(),
        team_id.as_deref(),
    );
    let window = layout_placement
        .map(|placement| placement.layout_window.clone())
        .unwrap_or_else(|| WindowName::new(agent_id.as_str()));
    let mut env = crate::lifecycle::launch::inherited_env_with_team_overrides(
        workspace,
        agent_id.as_str(),
        team_id.as_deref(),
        Some(crate::lifecycle::launch::auth_mode_env_value(auth_mode)),
    );
    crate::lifecycle::launch::apply_profile_launch_env(&mut env, &profile_launch);
    crate::lifecycle::launch::apply_mcp_auto_approval_env(&mut env, safety);
    if provider == crate::provider::Provider::Copilot {
        crate::lifecycle::launch::apply_copilot_instructions_overlay(
            workspace,
            agent_id.as_str(),
            &system_prompt,
            &mut env,
        )?;
    }
    // 0.5.67 Cursor 方案 1 变体: role 经 workspace rules 文件注入 (不 argv)。
    if provider == crate::provider::Provider::CursorAgent {
        crate::lifecycle::launch::refuse_second_cursor_occupant(
            workspace,
            agent_id.as_str(),
            None,
        )?;
        crate::lifecycle::launch::apply_cursor_agent_rules_overlay(
            workspace,
            agent_id.as_str(),
            &system_prompt,
        )?;
        crate::lifecycle::launch::apply_cursor_mcp_overlay(workspace, &mcp_config)?;
        crate::lifecycle::launch::enable_cursor_workspace_mcp(workspace)?;
        crate::lifecycle::launch::apply_cursor_workspace_physical_path(&mut plan.argv, workspace);
        crate::lifecycle::launch::apply_cursor_subscription_proxy_env(&mut env);
    }
    if provider == crate::provider::Provider::Grok {
        crate::lifecycle::launch::ensure_grok_login_and_folder_trust(workspace)?;
        crate::lifecycle::launch::apply_grok_mcp_overlay(workspace, &mcp_config)?;
    }
    // 0.3.28 Step 3: per Python parity, worker spawn cwd is ALWAYS `workspace`.
    // The persisted-state `agent.spawn_cwd` override is ignored (it was a
    // Rust-only extension that drifted to `.team/runtime/<team_key>/` after
    // rebuild.rs:138 — root cause of E56). The `spawn_cwd_override` parameter
    // is still honoured for callers that need an explicit cwd (e.g. spec
    // YAML-resolved cwd at first launch in `lifecycle/launch.rs`), but
    // restart never passes it (see commit 71864c0 which fixed rebuild.rs:297
    // to stop pinning `.team/runtime/<team_key>/`).
    //
    // NOTE: Step 4 will thread the YAML spec down to here so we can honour
    // a per-agent YAML `spawn_cwd` field if one is set. Until then, override
    // > workspace; state-based override is silently dropped for existing
    // providers. Pi is the narrow exception because exact header/path resume
    // must preserve the captured cwd or refuse.
    // 0.4.x provider effort MVP step 9: scrub CLAUDE_EFFORT for Claude
    // worker spawn so a parent shell env cannot silently override the
    // framework's effort decision.
    let env_unset = crate::layout::worker_env::isolate_worker_spawn_env(
        provider,
        &mut env,
        crate::lifecycle::launch::extend_worker_env_unset_for_effort(
            profile_launch.env_unset.iter().cloned().collect(),
            provider,
        ),
    );

    // 0.4.6 Stage 2: write actual spawn plan event BEFORE invoking the
    // transport spawn. Mirrors `launch.rs:359-380` (the reference impl)
    // so reset/start/restart fresh produces the same truth source as
    // quick-start. Any "argv had --session-id but didn't take effect"
    // failure can now be diagnosed from events.jsonl — the recorded
    // expected_session_id == state._pending_session_id (after
    // mark_agent_started persists the same plan tuple).
    let spawned_at = crate::lifecycle::launch::spawn_timestamp();
    {
        let session_id_in_argv = plan
            .argv
            .iter()
            .position(|a| a == "--session-id")
            .and_then(|i| plan.argv.get(i + 1))
            .cloned();
        let env_overlay_keys: Vec<&String> = env.keys().collect();
        let tmux_start_mode_pre_spawn =
            predict_tmux_start_mode(layout_placement, into_existing_session);
        let spawn_epoch = state_spawn_epoch_for_agent(workspace, agent_id);
        let tmux_endpoint = transport.tmux_endpoint();
        let event_log = crate::event_log::EventLog::new(workspace);
        let _ = event_log.write(
            crate::event_log::PROVIDER_WORKER_SPAWN_ARGV,
            crate::event_log::provider_worker_spawn_argv_fields(serde_json::json!({
                "agent_id": agent_id.as_str(),
                "provider": provider,
                "argv": plan.argv,
                "session_id_in_argv": session_id_in_argv,
                "expected_session_id": plan.expected_session_id.as_ref().map(|s| s.as_str()),
                "spawn_cwd": spawn_cwd.to_string_lossy(),
                "env_overlay_keys": env_overlay_keys,
                "env_unset": env_unset,
                "tmux_start_mode": tmux_start_mode_pre_spawn,
                "spawn_epoch": spawn_epoch,
                "spawned_at": spawned_at.as_str(),
                "source": "restart",
                "tmux_endpoint": tmux_endpoint,
                "tmux_endpoint_source": tmux_endpoint_source.unwrap_or("transport"),
            })),
        );
    }

    // 0.5.39 Slice 2 (tmux-server-death-locate §7 Slice 2): route the
    // primary worker spawn through the worker shell wrapper so provider
    // exit leaves the pane at an inert sh tail with an explicit exit marker
    // instead of collapsing the pane into `[exited]`. The inert tail does
    // not read pane stdin. The split path stays
    // on plain spawn_split — split panes are display overlays, not the
    // primary worker process.
    let provider_label = crate::provider::wire::command_name(provider);
    let result = if let Some(placement) = layout_placement {
        if placement.starts_window {
            if into_existing_session {
                transport.spawn_into_with_worker_shell_wrapper(
                    session_name,
                    &window,
                    &plan.argv,
                    spawn_cwd,
                    &env,
                    &env_unset,
                    provider_label,
                )
            } else {
                transport.spawn_first_with_worker_shell_wrapper(
                    session_name,
                    &window,
                    &plan.argv,
                    spawn_cwd,
                    &env,
                    &env_unset,
                    provider_label,
                )
            }
        } else if !window_present_in_live(transport, session_name, &window)
            || !crate::lifecycle::launch::is_adaptive_layout_window_pub(window.as_str())
        {
            // E43 Fix C + E45 (0.3.24 bug#3 → bug#4): never split into a
            // window that either does not exist on live tmux OR is a
            // per-agent window (`developer`, `architect`, ...) that the
            // upstream placement guards should have refused. This is the
            // defence-in-depth layer; the primary fix is in
            // `adaptive_placement_for_agent` / `adaptive_existing_placement_for_agent`,
            // but a placement built from stale `pane_index>0` state can
            // still ask to split a per-agent window — and the macmini repro
            // showed split-window -t :developer would otherwise succeed and
            // hijack the developer worker's pane. Downgrade to spawn_into
            // (new window named after agent_id) — canonical per-agent
            // fallback the existing 7 workers use.
            transport.spawn_into_with_worker_shell_wrapper(
                session_name,
                &WindowName::new(agent_id.as_str()),
                &plan.argv,
                spawn_cwd,
                &env,
                &env_unset,
                provider_label,
            )
        } else {
            // 0.3.28 Step 8: spawn_split must only fire from the display
            // overlay path. Warn-only here; Step 9 promotes to hard fail.
            crate::layout::overlay::assert_overlay_call_site(session_name, &window);
            transport.spawn_split_with_env_unset(
                session_name,
                &window,
                &plan.argv,
                spawn_cwd,
                &env,
                &env_unset,
            )
        }
    } else if into_existing_session {
        transport.spawn_into_with_worker_shell_wrapper(
            session_name,
            &window,
            &plan.argv,
            spawn_cwd,
            &env,
            &env_unset,
            provider_label,
        )
    } else {
        transport.spawn_first_with_worker_shell_wrapper(
            session_name,
            &window,
            &plan.argv,
            spawn_cwd,
            &env,
            &env_unset,
            provider_label,
        )
    };
    let spawn = result.map_err(|e| LifecycleError::Transport(e.to_string()))?;
    if layout_placement.is_some() {
        crate::lifecycle::launch::configure_adaptive_pane_title(
            workspace,
            transport,
            session_name,
            &window,
            &spawn.pane_id,
            agent_id.as_str(),
        );
    }
    let startup_prompt = adapter.handle_startup_prompts_outcome(
        transport,
        &crate::transport::Target::Pane(spawn.pane_id.clone()),
        30,
        0.5,
    );
    if let Some(error) = startup_prompt.capture_error.as_deref() {
        if is_structural_startup_prompt_error(error) {
            if let Err(rollback_error) = transport.kill_pane(&spawn.pane_id) {
                return Err(LifecycleError::Transport(format!(
                    "startup prompt structural failure for {}:{} pane {}: {}; failed to roll back spawned pane: {}",
                    session_name.as_str(),
                    window.as_str(),
                    spawn.pane_id.as_str(),
                    error,
                    rollback_error
                )));
            }
            return Err(LifecycleError::Transport(format!(
                "startup prompt structural failure for {}:{} pane {}: {}",
                session_name.as_str(),
                window.as_str(),
                spawn.pane_id.as_str(),
                error
            )));
        }
    }
    Ok(SpawnedAgentWindow {
        spawn,
        spawned_at,
        plan,
        profile_launch,
        layout_placement: layout_placement.cloned(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        owner_team_id: team_id,
    })
}

fn is_structural_startup_prompt_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "can't find window",
        "can't find pane",
        "cannot find window",
        "cannot find pane",
        "target not found",
        "no such pane",
        "window disappeared",
        "pane not owned",
        "not owned by requested",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// ---
/// purpose: 用 spec 里的同名 agent 补回 state 席位行缺的命令上下文字段
/// returns: 合并后的席位行；spec 读不到或找不到该 agent 时原样返回
/// ---
pub(super) fn rehydrate_agent_command_context_from_spec(
    spec_workspace: &Path,
    agent_id: &AgentId,
    agent: &serde_json::Value,
) -> serde_json::Value {
    let Ok(spec) = load_team_spec(spec_workspace) else {
        return agent.clone();
    };
    let Some(spec_agent) = find_spec_agent(&spec, agent_id) else {
        return agent.clone();
    };
    merge_command_context_fields(agent, spec_agent)
}

fn merge_command_context_fields(
    agent: &serde_json::Value,
    spec_agent: &YamlValue,
) -> serde_json::Value {
    let mut merged = agent.clone();
    let Some(obj) = merged.as_object_mut() else {
        return merged;
    };
    for field in [
        "role",
        "tools",
        "system_prompt",
        "output_contract",
        "provider",
        "model",
        "auth_mode",
        "effort",
        "profile",
        "permission_mode",
        // 0.5.66 bypass 单源:rehydrate 同步合入,restart 的 safety 构造读到新值。
        "dangerously_skip_permissions",
    ] {
        if let Some(value) = spec_agent.get(field).and_then(yaml_value_to_json) {
            obj.insert(field.to_string(), value);
        }
    }
    merged
}

fn yaml_value_to_json(value: &YamlValue) -> Option<serde_json::Value> {
    match value {
        YamlValue::Null => Some(serde_json::Value::Null),
        YamlValue::Bool(value) => Some(serde_json::json!(value)),
        YamlValue::Int(value) => Some(serde_json::json!(value)),
        YamlValue::Float(value) => Some(serde_json::json!(value)),
        YamlValue::Str(value) => Some(serde_json::json!(value)),
        YamlValue::List(items) => Some(serde_json::Value::Array(
            items.iter().filter_map(yaml_value_to_json).collect(),
        )),
        YamlValue::Map(items) => {
            let mut map = serde_json::Map::new();
            for (key, value) in items {
                if let Some(value) = yaml_value_to_json(value) {
                    map.insert(key.clone(), value);
                }
            }
            Some(serde_json::Value::Object(map))
        }
    }
}

/// E43 Fix C helper (0.3.24 bug#3): probe live tmux for a window's existence
/// before issuing `split-window -t :<window>`. Uses `list_windows` first
/// (cheaper, authoritative when present); falls back to `list_targets` so
/// transports that don't seed `windows` directly still surface real entries.
fn window_present_in_live(
    transport: &dyn crate::transport::Transport,
    session: &SessionName,
    window: &WindowName,
) -> bool {
    if let Ok(windows) = transport.list_windows(session) {
        if windows.iter().any(|w| w.as_str() == window.as_str()) {
            return true;
        }
    }
    if let Ok(targets) = transport.list_targets() {
        if targets.iter().any(|t| {
            t.session.as_str() == session.as_str()
                && t.window_name
                    .as_ref()
                    .is_some_and(|n| n.as_str() == window.as_str())
        }) {
            return true;
        }
    }
    false
}

/// ---
/// purpose: 起该 workspace 的 coordinator
/// params:
///   team_key: 传给 coordinator 的团队键
/// returns: 启动摘要
/// errors: 启动失败时返回 StatePersist
/// ---
pub(super) fn start_coordinator_for_workspace(
    workspace: &Path,
    team_key: Option<&str>,
) -> Result<crate::lifecycle::CoordinatorStartSummary, LifecycleError> {
    let workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    crate::coordinator::health::start_coordinator_with_team(&workspace, team_key)
        .map(|report| crate::lifecycle::CoordinatorStartSummary::from_start_report(&report))
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

/// ---
/// purpose: 按团队持久化的 tmux endpoint 解析出后端，让 restart、add 与 fork 落在同一 socket
/// params:
///   team: 目标团队，None 时按唯一性选
/// returns: 绑定到该 endpoint 的 tmux 后端；冷 workspace 无持久 endpoint 时退到按 workspace 派生
/// errors: 团队目标歧义或未解析返回 TeamSelect；state 声明后端是 conpty 时也返回 TeamSelect 拒绝，不降级
/// ---
/// State-aware tmux backend resolver. Reads the team's persisted
/// `tmux_endpoint` (set at `team-agent launch` time and shared across
/// restart/add-agent/fork-agent) and constructs a TmuxBackend on THAT socket,
/// so add-agent / fork-agent / restart all spawn into the SAME tmux socket
/// the live team already runs on.
///
/// First-agent / cold workspace (no persisted endpoint) safely falls back to
/// `TmuxBackend::for_workspace(run_workspace)` — the canonical workspace-hash
/// socket. No panic, no None.
///
/// **Exposed `pub(crate)` for `lifecycle::launch::add_agent` / `fork_agent`
/// (`0.3.24 add-agent socket drift fix`). Previously `pub(super)` and shared
/// only within `lifecycle::restart`. Sharing the resolver across the lifecycle
/// module is the correct ownership: restart/add/fork all need the SAME socket
/// the live team uses, and duplicating the lookup invited drift.**
/// 0.5.x Phase 1d Batch 1: this legacy resolver stays tmux-only. It
/// still returns a concrete `TmuxBackend` because the 6 caller sites
/// (rebuild/agent/remove/launch) currently call `TmuxBackend`-specific
/// methods (`.tmux_endpoint()`, `.for_workspace()`, etc.). But it now
/// routes through the factory so its **selection semantics** match
/// the new `lifecycle_worker_transport_for_selected_state` API:
///
/// - If `state.transport.kind` is `"conpty"`, this legacy tmux-typed
///   resolver **fails-closed** with `TransportBackendKindMismatch`
///   rather than silently returning a tmux backend that would
///   spawn/inject into the wrong pane universe. That satisfies CR C-1
///   fail-closed while the migration of caller sites to the generic
///   `_transport_for_selected_state` API completes.
/// - Otherwise (default/legacy tmux endpoint), behavior is preserved
///   byte-for-byte through
///   `tmux_backend_for_runtime_state_or_workspace`.
pub(crate) fn lifecycle_worker_tmux_backend_for_selected_state(
    run_workspace: &Path,
    team: Option<&str>,
) -> Result<crate::tmux_backend::TmuxBackend, LifecycleError> {
    let (state, refusal) = crate::state::projection::resolve_team_scoped_state(run_workspace, team)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) = refusal {
        let reason = refusal
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("team_target_unresolved");
        let detail = refusal
            .get("error")
            .or_else(|| refusal.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(LifecycleError::TeamSelect(format!("{reason}: {detail}")));
    }
    // CR C-1 fail-closed: state with `transport.kind=conpty` must NOT
    // silently downgrade to a tmux backend just because the caller
    // asked for the tmux-typed variant. Callers that see this refusal
    // are expected to migrate to `lifecycle_worker_transport_for_selected_state`
    // during Batch 2/3.
    if let Some(state_ref) = state.as_ref() {
        if let Some(kind) = state_ref
            .pointer("/transport/kind")
            .and_then(|v| v.as_str())
        {
            if kind.eq_ignore_ascii_case("conpty") {
                return Err(LifecycleError::TeamSelect(format!(
                    "backend_kind_mismatch: state.transport.kind={kind:?} but the legacy \
                     tmux-typed lifecycle resolver was invoked; caller must migrate to \
                     `lifecycle_worker_transport_for_selected_state` (Phase 1d Batch 2/3)"
                )));
            }
        }
    }
    Ok(state
        .as_ref()
        .map(|state| lifecycle_worker_tmux_backend_for_state(run_workspace, state))
        .unwrap_or_else(|| crate::tmux_backend::TmuxBackend::for_workspace(run_workspace)))
}

/// ---
/// purpose: 与上面同样的团队选择语义，但返回工厂解析出的通用 transport
/// returns: 已解析的 transport，含后端种类、来源与提示
/// errors: 团队选择失败或工厂拒绝时返回 TeamSelect，读 state 失败返回 StatePersist
/// ---
/// 0.5.x Phase 1d Batch 1: new generic-typed lifecycle resolver.
///
/// Same team-selection semantics as the legacy tmux-typed variant, but
/// returns a `ResolvedTransport` from the factory so callers that can
/// hold `Box<dyn Transport>` (or the resolved `BackendKind`) work
/// against both tmux AND conpty state. Caller migration to this API
/// happens across Batch 2-5.
///
/// Fail-closed rules follow the factory (C-1 ×2, C-2 CLI-vs-state,
/// C-3 N38 notices via `ResolvedTransport.notices`).
pub(crate) fn lifecycle_worker_transport_for_selected_state(
    run_workspace: &Path,
    team: Option<&str>,
) -> Result<crate::transport_factory::ResolvedTransport, LifecycleError> {
    let (state, refusal) = crate::state::projection::resolve_team_scoped_state(run_workspace, team)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) = refusal {
        let reason = refusal
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("team_target_unresolved");
        let detail = refusal
            .get("error")
            .or_else(|| refusal.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(LifecycleError::TeamSelect(format!("{reason}: {detail}")));
    }
    let input = crate::transport_factory::TransportFactoryInput::new(
        run_workspace,
        crate::transport_factory::TransportPurpose::LifecycleWorker,
    )
    .with_team_key(team)
    .with_state(state.as_ref());
    crate::transport_factory::resolve_transport(input)
        .map_err(|e| LifecycleError::TeamSelect(e.to_string()))
}

/// ---
/// purpose: 由已取到的 state 解析 tmux 后端与它的来源
/// returns: 后端与 endpoint 来源
/// errors: state 声明后端是 conpty 时返回 TeamSelect
/// ---
pub(super) fn lifecycle_worker_tmux_backend_selection_for_state(
    run_workspace: &Path,
    state: &serde_json::Value,
) -> Result<crate::tmux_backend::RuntimeTmuxBackendSelection, LifecycleError> {
    if let Some(kind) = state.pointer("/transport/kind").and_then(|v| v.as_str()) {
        if kind.eq_ignore_ascii_case("conpty") {
            return Err(LifecycleError::TeamSelect(format!(
                "backend_kind_mismatch: state.transport.kind={kind:?} but the legacy \
                 tmux-typed lifecycle resolver was invoked; caller must migrate to \
                 `lifecycle_worker_transport_for_selected_state` (Phase 1d Batch 2/3)"
            )));
        }
    }
    if let Some((endpoint, source)) = crate::tmux_backend::owning_tmux_endpoint_from_state(state) {
        let backend = crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint);
        return Ok(crate::tmux_backend::RuntimeTmuxBackendSelection {
            tmux_endpoint_used: backend.tmux_endpoint(),
            backend,
            tmux_endpoint_source: source,
        });
    }
    Ok(
        crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
            run_workspace,
            Some(state),
        ),
    )
}

/// ---
/// purpose: 由已取到的 state 直接给出 tmux 后端
/// returns: 绑定到该 state endpoint 的后端，缺失时按 workspace 派生
/// ---
pub(super) fn lifecycle_worker_tmux_backend_for_state(
    run_workspace: &Path,
    state: &serde_json::Value,
) -> crate::tmux_backend::TmuxBackend {
    if let Some((endpoint, _source)) = crate::tmux_backend::owning_tmux_endpoint_from_state(state) {
        return crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint);
    }
    crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(run_workspace, Some(state))
        .backend
}

/// ---
/// purpose: 同步团队投影后落盘 restart 结果
/// params:
///   topology_authority_agent_ids: 本次以内存值为拓扑权威的席位
/// returns: 成功返回空值
/// errors: 落盘失败时返回 StatePersist
/// contract_id: lifecycle.common.save_restart_projected_state
/// ---
pub(super) fn save_restart_projected_state(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
    topology_authority_agent_ids: &[&str],
) -> Result<(), LifecycleError> {
    save_restart_projected_state_with_capture_backfill_skip(
        workspace,
        state,
        team_key,
        &[],
        topology_authority_agent_ids,
    )
}

/// ---
/// purpose: 同上，并可指定哪些席位跳过会话捕获回填
/// params:
///   skip_capture_backfill_agent_ids: 跳过回填的席位
/// returns: 成功返回空值
/// errors: 落盘失败时返回 StatePersist
/// contract_id: lifecycle.common.save_restart_projected_state
/// ---
pub(super) fn save_restart_projected_state_with_capture_backfill_skip(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
    skip_capture_backfill_agent_ids: &[&str],
    topology_authority_agent_ids: &[&str],
) -> Result<(), LifecycleError> {
    sync_restart_team_projections(state, team_key);
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::RestartTeam {
                team_key,
                topology_authority_agent_ids,
                skip_capture_backfill_agent_ids,
            },
            state,
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

/// ---
/// purpose: 定出本次投影使用的团队键
/// returns: 显式 team 优先，其次 state 里的活跃键，最后由 state 推算
/// ---
pub(super) fn restart_projection_team_key(state: &serde_json::Value, team: Option<&str>) -> String {
    team.filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| {
            state
                .get("active_team_key")
                .and_then(serde_json::Value::as_str)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| crate::state::projection::team_state_key(state))
}

/// ---
/// purpose: 把当前顶层状态压实后写回 teams 表
/// params:
///   state: 就地改写；显式团队键允许覆盖或新建，别名键只在盘上已有且身份不冲突时才写
/// returns: teams 表缺失或为空时不动
/// ---
pub(super) fn sync_restart_team_projections(state: &mut serde_json::Value, team_key: &str) {
    let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) else {
        return;
    };
    if teams.is_empty() {
        return;
    }
    let compact = crate::state::projection::compact_team_state(state);
    let active_key = state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let derived_key = crate::state::projection::team_state_key(state);
    let Some(teams) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    // 0.5.28 (`.team/artifacts/0527-realfail-layer2-leader-locate.md` §3):
    // 显式 team_key = 操作目标,始终允许覆盖 / 新建(该 helper 的正当职责)。
    // active/derived/"current" 三种别名兜底 = alias-identity 家族第三例,只有
    // 「盘上已有该 alias 条目」且「其身份与 compact 不冲突」时才允许写,
    // 避免 0.5.26 起死 sibling 保留时被活队 compact 硬克隆。禁止 alias 新建条目。
    let mut keys = Vec::new();
    let mut explicit = false;
    if !team_key.is_empty() {
        keys.push(team_key.to_string());
        explicit = true;
    }
    if let Some(active_key) = active_key {
        keys.push(active_key);
    }
    if !derived_key.is_empty() {
        keys.push(derived_key);
    }
    if teams.contains_key("current") {
        keys.push("current".to_string());
    }
    keys.sort();
    keys.dedup();
    for key in keys {
        let is_operation_target = explicit && key == team_key;
        if is_operation_target {
            teams.insert(key, compact.clone());
            continue;
        }
        let Some(existing) = teams.get(&key) else {
            // 别名条目不存在:不新建,避免把 hack alias 变成真队。
            continue;
        };
        if json_team_identity_matches(existing, &compact) {
            teams.insert(key, compact.clone());
        }
    }
}

/// 0.5.28: 别名同步身份门。比较 existing 条目与 compact(即将写入)四个身份字段:
/// `team_key`/`session_name`/`team_dir`/`spec_path`。legacy 缺字段容忍(两侧任一
/// 缺则该字段不参与判定);已有字段必须相等,冲突即拒绝覆盖。
fn json_team_identity_matches(existing: &serde_json::Value, compact: &serde_json::Value) -> bool {
    const FIELDS: &[&str] = &["team_key", "session_name", "team_dir", "spec_path"];
    for field in FIELDS {
        let lhs = existing
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty());
        let rhs = compact
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty());
        match (lhs, rhs) {
            (Some(a), Some(b)) if a != b => return false,
            _ => {}
        }
    }
    true
}

/// ---
/// purpose: 取 state 里的 session 名
/// returns: 非空的 session_name，缺失时退到默认名
/// ---
pub(super) fn state_session_name(state: &serde_json::Value) -> SessionName {
    worker_session_name_from_state(state)
        .map(SessionName::new)
        .unwrap_or_else(|| SessionName::new("team-agent"))
}

fn worker_session_name_from_state(state: &serde_json::Value) -> Option<&str> {
    state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let team_key = state
                .get("active_team_key")
                .and_then(serde_json::Value::as_str)
                .filter(|team_key| !team_key.is_empty())?;
            state
                .get("teams")?
                .get(team_key)?
                .pointer("/leader_receiver/session_name")?
                .as_str()
                .filter(|session| !session.is_empty())
        })
        .or_else(|| {
            state
                .pointer("/leader_receiver/session_name")
                .and_then(serde_json::Value::as_str)
                .filter(|session| !session.is_empty())
        })
}

/// ---
/// purpose: 判断 state 里是否记了非空 session 名
/// returns: 记了则 true
/// ---
pub(super) fn session_name_present(state: &serde_json::Value) -> bool {
    worker_session_name_from_state(state).is_some()
}

/// ---
/// purpose: 探测 session 是否存活
/// params:
///   default: 探测本身 panic 时采用的兜底判定
/// returns: transport 明确回答时用它；返回错误时判为不存活；探测 panic 时用兜底值
/// ---
pub(super) fn session_live_or_default(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    default: bool,
) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.has_session(session_name)
    })) {
        Ok(Ok(live)) => live,
        Ok(Err(_)) => false,
        Err(_) => default,
    }
}

/// ---
/// purpose: 取席位行的 provider
/// returns: 解析成功用它，缺失或不认识时退到 codex
/// ---
pub(super) fn agent_provider(agent: &serde_json::Value) -> Provider {
    agent
        .get("provider")
        .and_then(|v| v.as_str())
        .and_then(parse_provider)
        .unwrap_or(Provider::Codex)
}

/// ---
/// purpose: 取席位行的 auth_mode
/// returns: 解析成功用它，缺失或不认识时退到 subscription
/// ---
pub(super) fn agent_auth_mode(agent: &serde_json::Value) -> AuthMode {
    agent
        .get("auth_mode")
        .and_then(|v| v.as_str())
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription)
}

/// ---
/// purpose: 取席位行记录的会话 id
/// returns: 非空时返回，否则 None
/// ---
pub(super) fn agent_session_id(agent: &serde_json::Value) -> Option<SessionId> {
    agent
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionId::new)
}

/// ---
/// purpose: 取席位行记录的 rollout 路径
/// returns: 非空时返回，否则 None
/// ---
pub(super) fn agent_rollout_path(agent: &serde_json::Value) -> Option<RolloutPath> {
    agent
        .get("rollout_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(RolloutPath::new)
}

/// ---
/// purpose: 判断该席位的 resume 依据是否存在
/// returns: 探测结果里的存在位
/// ---
pub(super) fn resume_backing_exists_for_agent(
    workspace: &Path,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    provider: Provider,
    session_id: &SessionId,
    rollout_path: Option<&RolloutPath>,
) -> bool {
    resume_backing_probe_for_agent(
        workspace,
        agent_id,
        agent,
        provider,
        session_id,
        rollout_path,
    )
    .exists
}

/// Layer 2 self-healing (leader follow-up 2026-06-22): result of probing
/// the provider backing store for a resumable session. `checked_paths`
/// reports every path the runtime probed so the operator can see WHICH
/// places we looked — surfaced into the
/// `ResumeRefusalReason::SessionBackingStoreMissing.checked_paths`
/// field, the CLI JSON `unresumable[].checked_paths` array, and the
/// `restart.resume_decision` event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackingProbeResult {
    pub exists: bool,
    pub checked_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionIdentityProbeResult {
    pub identity_ok: Option<bool>,
    pub embedded_agent_id: Option<String>,
    pub rollout_path: Option<PathBuf>,
}

/// ---
/// purpose: 探测 rollout 文件里嵌的席位身份是否与本席位一致
/// params:
///   _provider: 未参与判定
///   rollout_path: 没有路径时三项都返回未知
/// returns: 一致性判定、读到的嵌入席位名与实际探测路径；读不出嵌入身份时一致性为未知
/// ---
pub(crate) fn session_identity_probe_for_agent(
    agent_id: &AgentId,
    _provider: Provider,
    rollout_path: Option<&RolloutPath>,
) -> SessionIdentityProbeResult {
    let Some(path) = rollout_path.map(RolloutPath::as_path) else {
        return SessionIdentityProbeResult {
            identity_ok: None,
            embedded_agent_id: None,
            rollout_path: None,
        };
    };
    let embedded_agent_id =
        crate::provider::session_scan::common::rollout_path_embedded_team_agent_worker_id(path);
    let identity_ok = embedded_agent_id
        .as_deref()
        .map(|embedded| embedded == agent_id.as_str());
    SessionIdentityProbeResult {
        identity_ok,
        embedded_agent_id,
        rollout_path: Some(path.to_path_buf()),
    }
}

/// ---
/// purpose: 按 provider 分支探测 resume 依据，并记录所有查过的路径
/// returns: 存在位与查过的路径列表；持久化的 rollout 路径即使不存在也记进列表
/// ---
pub(super) fn resume_backing_probe_for_agent(
    workspace: &Path,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    provider: Provider,
    session_id: &SessionId,
    rollout_path: Option<&RolloutPath>,
) -> BackingProbeResult {
    let mut checked_paths: Vec<PathBuf> = Vec::new();

    // Always record the persisted rollout_path even when it does not
    // exist — that "we looked here" tells the operator that state has a
    // pointer but the file is gone.
    if let Some(path) = rollout_path.map(RolloutPath::as_path) {
        checked_paths.push(path.to_path_buf());
    }

    let exists = match provider {
        provider if !provider_supports_resume(provider) => {
            let _ = (workspace, agent_id, agent, session_id, rollout_path);
            false
        }
        Provider::Codex => {
            let rollout_ok = rollout_path_exists(rollout_path);
            let scan_roots = codex_session_transcript_scan_roots(agent, rollout_path);
            for root in &scan_roots {
                checked_paths.push(root.clone());
            }
            rollout_ok
                || codex_session_transcript_exists_with_roots(session_id.as_str(), &scan_roots)
        }
        Provider::Claude | Provider::ClaudeCode => {
            let rollout_ok = rollout_path_exists(rollout_path);
            let projects_root = claude_projects_root_for_agent(agent);
            if let Some(root) = projects_root.as_ref() {
                checked_paths.push(root.clone());
            }
            let event_log_path = workspace.join(".team/logs/events.jsonl");
            checked_paths.push(event_log_path);
            rollout_ok
                || event_log_transcript_exists(workspace, agent_id.as_str(), session_id.as_str())
                || projects_root.is_some_and(|root| {
                    claude_project_transcript_exists_under(&root, session_id.as_str())
                })
        }
        Provider::Copilot => {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                checked_paths.push(home.join(".copilot/session-store.db"));
            }
            copilot_session_store_has_session(session_id.as_str())
        }
        Provider::Grok => {
            let spawn_cwd = agent
                .get("spawn_cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());
            match crate::provider::session_scan::grok::grok_session_dir(
                &spawn_cwd,
                session_id.as_str(),
            ) {
                Some(dir) => {
                    checked_paths.push(dir.clone());
                    crate::provider::session_scan::grok::grok_session_archive_present(&dir)
                }
                None => false,
            }
        }
        Provider::CursorAgent => {
            let spawn_cwd = agent
                .get("spawn_cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());
            let rollout_ok = rollout_path.is_some_and(|path| {
                crate::provider::session_scan::cursor::cursor_session_archive_present(path.as_path())
            });
            let discovered = crate::provider::session_scan::cursor::cursor_session_dir_for_cwd(
                session_id.as_str(),
                &spawn_cwd,
            );
            if let Some(dir) = discovered.as_ref() {
                checked_paths.push(dir.clone());
            }
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                checked_paths.push(home.join(".cursor").join("chats"));
            }
            rollout_ok
                || discovered.as_ref().is_some_and(|dir| {
                    crate::provider::session_scan::cursor::cursor_session_archive_present(dir)
                })
        }
        Provider::Pi => {
            let spawn_cwd = agent
                .get("spawn_cwd")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());
            rollout_path.is_some_and(|path| {
                crate::provider::session_scan::pi::validate_exact_backing(
                    path.as_path(),
                    session_id,
                    &spawn_cwd,
                )
                .is_ok()
            })
        }
        Provider::GeminiCli | Provider::Fake => false,
    };

    // Deduplicate while preserving order (HashSet would lose deterministic
    // ordering needed for stable JSON/event output).
    let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    checked_paths.retain(|p| seen.insert(p.clone()));

    BackingProbeResult {
        exists,
        checked_paths,
    }
}

/// ---
/// purpose: 判断该 provider 是否支持 resume
/// returns: 由 provider 适配器的能力位给出
/// contract_id: lifecycle.common.provider_supports_resume
/// ---
pub(super) fn provider_supports_resume(provider: Provider) -> bool {
    crate::provider::get_adapter(provider).caps().resume
}

/// ---
/// purpose: 由 provider wire 名判断是否支持 resume
/// returns: 名字不认识时为 false
/// contract_id: lifecycle.common.provider_supports_resume
/// ---
pub(super) fn provider_wire_supports_resume(provider: &str) -> bool {
    parse_provider(provider)
        .map(provider_supports_resume)
        .unwrap_or(false)
}

fn rollout_path_exists(rollout_path: Option<&RolloutPath>) -> bool {
    rollout_path
        .as_ref()
        .is_some_and(|path| path.as_path().exists())
}

fn event_log_transcript_exists(workspace: &Path, agent_id: &str, session_id: &str) -> bool {
    let Ok(events) = crate::event_log::EventLog::new(workspace).tail(0) else {
        return false;
    };
    events.iter().rev().any(|event| {
        event.get("event").and_then(serde_json::Value::as_str) == Some("session.captured")
            && ["agent_id", "worker_id"]
                .iter()
                .any(|key| event.get(*key).and_then(serde_json::Value::as_str) == Some(agent_id))
            && event.get("session_id").and_then(serde_json::Value::as_str) == Some(session_id)
            && event_transcript_path(event).is_some_and(|path| path.exists())
    })
}

fn event_transcript_path(event: &serde_json::Value) -> Option<PathBuf> {
    event
        .get("rollout_path")
        .or_else(|| event.get("transcript_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// E36 fix-B: a real Claude worker writes its session transcript to
/// `<claude_projects_root>/<workspace-slug>/<session_id>.jsonl` even when neither
/// `rollout_path` was persisted to state nor a `session.captured` event was logged.
/// That landed transcript is itself a valid resume backing — restart was wrongly
/// refusing resumable workers because it only checked the two paths above. Scan the
/// projects root recursively for `<session_id>.jsonl` (session_id is a unique UUID,
/// so we avoid recomputing the project-dir slug, which is brittle for non-ASCII
/// workspace paths).
fn claude_project_transcript_exists(agent: &serde_json::Value, session_id: &str) -> bool {
    let Some(root) = claude_projects_root_for_agent(agent) else {
        return false;
    };
    claude_project_transcript_exists_under(&root, session_id)
}

fn claude_projects_root_for_agent(agent: &serde_json::Value) -> Option<PathBuf> {
    agent
        .get("claude_projects_root")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".claude").join("projects"))
        })
}

fn claude_project_transcript_exists_under(projects_root: &Path, session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    if !projects_root.is_dir() {
        return false;
    }
    let transcript_name = format!("{session_id}.jsonl");
    let Ok(project_dirs) = std::fs::read_dir(projects_root) else {
        return false;
    };
    project_dirs
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .any(|entry| entry.path().join(&transcript_name).is_file())
}

fn codex_session_transcript_exists(
    agent: &serde_json::Value,
    session_id: &str,
    rollout_path: Option<&RolloutPath>,
) -> bool {
    let roots = codex_session_transcript_scan_roots(agent, rollout_path);
    codex_session_transcript_exists_with_roots(session_id, &roots)
}

fn codex_session_transcript_scan_roots(
    agent: &serde_json::Value,
    rollout_path: Option<&RolloutPath>,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(parent) = rollout_path
        .map(RolloutPath::as_path)
        .and_then(Path::parent)
        .filter(|path| path.is_dir())
    {
        roots.push(parent.to_path_buf());
    }
    if let Some(root) = agent
        .get("codex_sessions_root")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    {
        roots.push(root);
    }
    if let Some(root) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".codex").join("sessions"))
        .filter(|path| path.is_dir())
    {
        roots.push(root);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn codex_session_transcript_exists_with_roots(session_id: &str, roots: &[PathBuf]) -> bool {
    if session_id.is_empty() {
        return false;
    }
    roots
        .iter()
        .any(|root| session_transcript_exists_under(root, session_id, 4))
}

fn session_transcript_exists_under(root: &Path, session_id: &str, max_depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.ends_with(".jsonl") && name.contains(session_id) {
                return true;
            }
        } else if max_depth > 0
            && path.is_dir()
            && session_transcript_exists_under(&path, session_id, max_depth.saturating_sub(1))
        {
            return true;
        }
    }
    false
}

fn copilot_session_store_has_session(session_id: &str) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    let db_path = home.join(".copilot").join("session-store.db");
    if !db_path.exists() {
        return false;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    conn.query_row(
        "select 1 from sessions where id = ?1 limit 1",
        [session_id],
        |_| Ok(()),
    )
    .is_ok()
}

/// ---
/// purpose: 对缺会话的席位做一次捕获尝试
/// params:
///   state: 就地改写
/// returns: 本次是否改动了 state
/// errors: 捕获过程出错时返回 Provider
/// ---
pub(crate) fn refresh_missing_provider_sessions(
    state: &mut serde_json::Value,
) -> Result<bool, LifecycleError> {
    crate::session_capture::capture_missing_provider_sessions_once(
        state,
        &mut crate::provider::get_adapter,
        false,
        0,
    )
    .map(|report| report.changed)
    .map_err(|e| LifecycleError::Provider(e.to_string()))
}

/// ---
/// purpose: 有界等待缺会话席位收敛，并逐轮写进度事件
/// params:
///   deadline: 等待上限
///   poll_interval: 轮询间隔
///   allow_fresh: 只写进事件载荷，不改变收敛判定
/// returns: 收敛结论
/// errors: 收敛过程出错时返回 StatePersist
/// ---
pub(crate) fn converge_missing_provider_sessions(
    state: &mut serde_json::Value,
    deadline: std::time::Duration,
    poll_interval: std::time::Duration,
    workspace: &Path,
    allow_fresh: bool,
) -> Result<crate::session_capture::SessionConvergence, LifecycleError> {
    crate::session_capture::converge_missing_provider_sessions(
        state,
        &mut crate::provider::get_adapter,
        deadline,
        poll_interval,
        restart_required_missing_session_agent_ids,
        |progress| {
            write_session_convergence_progress_event(
                workspace,
                serde_json::json!({
                    "iteration": progress.iteration,
                    "elapsed_ms": progress.elapsed_ms,
                    "deadline_ms": progress.deadline_ms,
                    "changed": progress.changed,
                    "assigned": progress.assigned,
                    "missing": progress.missing,
                    "required_missing_agent_ids": progress.required_missing_agent_ids,
                    "pending_agent_ids": progress.pending_agent_ids,
                    "candidate_count_by_agent": progress.candidate_count_by_agent,
                    "remaining_ms": progress.remaining_ms,
                    "allow_fresh": allow_fresh,
                }),
            )
        },
    )
    .map_err(LifecycleError::StatePersist)
}

fn write_session_convergence_progress_event(
    workspace: &Path,
    fields: serde_json::Value,
) -> Result<(), String> {
    crate::event_log::EventLog::new(workspace)
        .write(crate::event_log::PROVIDER_SESSION_CONVERGING, fields)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// ---
/// purpose: 列出必须等到会话收敛才能 restart 的席位
/// returns: 排序后的席位 id；只保留无会话、状态为 running 且确有待保留上下文的席位，从未捕获过的席位不进入
/// ---
pub(crate) fn restart_required_missing_session_agent_ids(state: &serde_json::Value) -> Vec<String> {
    let mut missing = crate::session_capture::incomplete_resumable_agent_ids(state)
        .into_iter()
        .filter(|agent_id| {
            let Some(agent) = state.get("agents").and_then(|agents| agents.get(agent_id)) else {
                return false;
            };
            let missing_session_id = agent
                .get("session_id")
                .and_then(|value| value.as_str())
                .is_none_or(|session| session.is_empty());
            let is_running = agent
                .get("status")
                .and_then(|value| value.as_str())
                .is_some_and(|status| status == "running");
            // E6 层2 (C2) + RESTART-RESUME-001 (0.4.8): required-missing
            // predicate gates on session_id absence + running, but ALSO
            // skips never-captured workers (no session_id AND no context
            // signals at all). A never-captured worker has nothing to
            // lose by fresh-start, so it must not block convergence and
            // burn the capture deadline. This matches the selection-stage
            // partial-resume semantic in
            // selection.rs::classify_resume_decision (never_captured →
            // FreshStart without --allow-fresh).
            //
            // The "has context to preserve" signal is the shared
            // restart_agent_has_context_to_preserve helper: first_send_at
            // (leader→worker delivery), last_result_at (MCP report path
            // that may skip first_send_at), or task_prompt_delivered.
            // Only context-bearing null-session workers continue to
            // require convergence (so we never silently drop context).
            missing_session_id
                && is_running
                && !super::selection::restart_agent_never_captured(agent, None)
        })
        .collect::<Vec<_>>();
    missing.sort();
    missing
}
/// ---
/// purpose: 取该席位的窗口名
/// returns: 席位行里的非空 window，缺失时用席位 id
/// ---
pub(super) fn agent_window(agent: &serde_json::Value, agent_id: &AgentId) -> String {
    agent
        .get("window")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agent_id.as_str())
        .to_string()
}

pub(super) use crate::provider::wire::{parse_provider, provider_wire};

/// ---
/// purpose: 把 auth_mode 字符串解析成枚举
/// returns: 只认三种取值，未知返回 None
/// ---
pub(super) fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}

/// ---
/// purpose: 从给定目录读出 team spec
/// returns: 解析后的 YAML
/// errors: 文件不存在返回 TeamSelect，读文件或解析失败返回 Compile
/// ---
pub(super) fn load_team_spec(workspace: &Path) -> Result<YamlValue, LifecycleError> {
    let spec_path = workspace.join("team.spec.yaml");
    if !spec_path.exists() {
        return Err(LifecycleError::TeamSelect(format!(
            "missing spec: {}",
            spec_path.display()
        )));
    }
    let text = std::fs::read_to_string(&spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", spec_path.display())))?;
    yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))
}

/// ---
/// purpose: 在 spec 的 agents 列表里找该席位
/// returns: 命中的节点；该 id 其实是 leader 时返回 None
/// ---
pub(super) fn find_spec_agent<'a>(
    spec: &'a YamlValue,
    agent_id: &AgentId,
) -> Option<&'a YamlValue> {
    let leader_is_agent = spec
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(YamlValue::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false);
    if leader_is_agent {
        return None;
    }
    spec.get("agents")?.as_list()?.iter().find(|agent| {
        agent
            .get("id")
            .and_then(YamlValue::as_str)
            .map(|id| id == agent_id.as_str())
            .unwrap_or(false)
    })
}

/// ---
/// purpose: 构造未知席位的错误
/// returns: 带席位 id 的 RequirementUnmet
/// ---
pub(super) fn unknown_worker(agent_id: &AgentId) -> LifecycleError {
    LifecycleError::RequirementUnmet(format!("unknown worker agent id: {agent_id}"))
}

/// ---
/// purpose: 定出 session 名，state 缺失时回落到 spec
/// returns: 依次取 state 的 session_name、spec 的 runtime.session_name、由 team 名派生，最后用默认名
/// ---
pub(super) fn state_session_name_from_spec(
    state: &serde_json::Value,
    spec: &YamlValue,
) -> SessionName {
    state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .or_else(|| {
            spec.get("runtime")
                .and_then(|v| v.get("session_name"))
                .and_then(YamlValue::as_str)
                .map(SessionName::new)
        })
        .or_else(|| {
            spec.get("team")
                .and_then(|v| v.get("name"))
                .and_then(YamlValue::as_str)
                .map(|name| SessionName::new(format!("team-{name}")))
        })
        .unwrap_or_else(|| SessionName::new("team-agent"))
}

/// ---
/// purpose: 把席位标记为已停并清掉 window 与 pane_id 字段（pane_pid 等其余字段保留）
/// params:
///   state: 就地改写，非对象时先重置成空对象
///   spec_agent: 提供 provider 等最小投影
/// returns: 成功返回空值
/// errors: state 结构不是对象时返回 StatePersist
/// ---
pub(super) fn mark_agent_stopped(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    spec_agent: &YamlValue,
    window: &str,
) -> Result<(), LifecycleError> {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state root is not an object".to_string(),
        ));
    };
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !agents.is_object() {
        *agents = serde_json::json!({});
    }
    let Some(agent_map) = agents.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state agents is not an object".to_string(),
        ));
    };
    let provider = spec_agent
        .get("provider")
        .and_then(YamlValue::as_str)
        .unwrap_or("codex");
    let entry = agent_map
        .entry(agent_id.as_str().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let Some(obj) = entry.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "agent state is not an object".to_string(),
        ));
    };
    obj.insert("status".to_string(), serde_json::json!("stopped"));
    obj.insert("provider".to_string(), serde_json::json!(provider));
    obj.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    obj.insert("last_window".to_string(), serde_json::json!(window));
    obj.remove("window");
    obj.remove("pane_id");
    Ok(())
}

/// ---
/// purpose: 窗口本就存在而未新起进程时，把席位标记为运行中
/// params:
///   pane: 探到的现有 pane，用于写 pane id 与进程号
/// returns: 成功返回空值
/// errors: state 结构不是对象时返回 StatePersist
/// ---
pub(super) fn mark_agent_running_noop(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    session_name: &SessionName,
    window: &str,
    pane: Option<&crate::transport::PaneInfo>,
) -> Result<(), LifecycleError> {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state root is not an object".to_string(),
        ));
    };
    root.insert(
        "session_name".to_string(),
        serde_json::json!(session_name.as_str()),
    );
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !agents.is_object() {
        *agents = serde_json::json!({});
    }
    let Some(agent_map) = agents.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state agents is not an object".to_string(),
        ));
    };
    let entry = agent_map
        .entry(agent_id.as_str().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let Some(obj) = entry.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "agent state is not an object".to_string(),
        ));
    };
    obj.insert("status".to_string(), serde_json::json!("running"));
    obj.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    obj.insert("window".to_string(), serde_json::json!(window));
    if let Some(pane) = pane {
        obj.insert(
            "pane_id".to_string(),
            serde_json::json!(pane.pane_id.as_str()),
        );
        if let Some(pane_pid) = pane.pane_pid {
            obj.insert("pane_pid".to_string(), serde_json::json!(pane_pid));
        } else {
            obj.remove("pane_pid");
        }
    }
    Ok(())
}

/// ---
/// purpose: 写一条起席无操作事件
/// returns: 成功返回空值
/// errors: 事件写入失败时返回 StatePersist
/// ---
pub(super) fn write_start_agent_noop_event(
    workspace: &Path,
    agent_id: &AgentId,
    target: &str,
    coordinator_started: bool,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "start_agent.noop",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "target": target,
                "coordinator": coordinator_started,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

/// ---
/// purpose: 判断某 session 里是否存在该窗口
/// returns: 明确列出该窗口才为 true；列窗口出错或 panic 时为 false
/// ---
pub(super) fn window_exists(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    window: &str,
) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.list_windows(session_name)
    })) {
        Ok(Ok(windows)) => windows.iter().any(|w| w.as_str() == window),
        Ok(Err(_)) | Err(_) => false,
    }
}

/// ---
/// purpose: 在 state 里把该席位的显示标记为已关
/// params:
///   state: 就地改写；只有 ghostty 工作区后端会被改写状态与标题，其余后端不动
/// ---
pub(super) fn close_agent_display(state: &mut serde_json::Value, agent_id: &AgentId) {
    let Some(display) = state
        .get_mut("agents")
        .and_then(|v| v.as_object_mut())
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(|agent| agent.get_mut("display"))
        .and_then(|display| display.as_object_mut())
    else {
        return;
    };
    let backend = display
        .get("backend")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // golden operations.py:88-92: close_ghostty_display (display/close.py:17-48) mutates NOTHING in the
    // persisted state for a ghostty_window; only the ghostty_workspace slot is relabeled
    // (close.py:84-85: status="stopped", pane_title=f"stopped: {agent_id}") and re-assigned back.
    if backend == "ghostty_workspace" {
        display.insert("status".to_string(), serde_json::json!("stopped"));
        display.insert(
            "pane_title".to_string(),
            serde_json::json!(format!("stopped: {}", agent_id.as_str())),
        );
    }
}

/// ---
/// purpose: 丢弃该席位的会话捕获字段并标记为已停
/// params:
///   state: 就地改写；只删会话捕获相关字段，工作目录等状态字段保留
/// returns: 成功返回空值
/// errors: 席位不存在返回 RequirementUnmet，席位行不是对象返回 StatePersist
/// ---
pub(super) fn discard_agent_session_fields(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
) -> Result<(), LifecycleError> {
    let Some(agent) = state
        .get_mut("agents")
        .and_then(|v| v.as_object_mut())
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
    else {
        return Err(unknown_worker(agent_id));
    };
    let Some(obj) = agent.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "agent state is not an object".to_string(),
        ));
    };
    // golden operations.py:119 pops EXACTLY `[*SESSION_CAPTURE_FIELDS, "_pending_session_id"]`.
    // spawn_cwd lives in SESSION_STATE_FIELDS (state.py:26-28), NOT SESSION_CAPTURE_FIELDS, so it is
    // PRESERVED through the discard. (Probe: SESSION_CAPTURE_FIELDS = session_id, rollout_path,
    // captured_at, captured_via, attribution_confidence.)
    //
    // Bug 2 (0.3.32): also clear `attribution_ambiguous`. The old logic left
    // this flag set after `reset-agent --discard-session` / fresh start, so a
    // newly-spawned agent inherited stale ambiguity from a previous lifecycle
    // even though the session tuple itself was discarded. Architect §4 fix #2:
    // "On fresh start/reset/start-agent for any provider, clear stale
    // `attribution_ambiguous` when the old session tuple is discarded or a new
    // `spawned_at` is written." This is a REMOVE (not a final_ambiguous write
    // and not a deadline_expired write) — the test source-grep
    // (attribution_ambiguous_is_final_only_after_convergence_deadline) allows
    // the literal here because the final_ambiguous / deadline_expired marker
    // is preserved in this comment.
    for key in [
        "session_id",
        "rollout_path",
        "captured_at",
        "captured_via",
        "attribution_confidence",
        "_pending_session_id",
        "attribution_ambiguous",
    ] {
        obj.remove(key);
    }
    obj.insert("status".to_string(), serde_json::json!("stopped"));
    Ok(())
}

/// ---
/// purpose: 判断该席位是否在运行
/// returns: 状态为 running 或 busy 直接为真；其余状态都退到按 session 与窗口存在性判定
/// ---
pub(super) fn agent_is_running(
    state: &serde_json::Value,
    agent_id: &AgentId,
    transport: &dyn crate::transport::Transport,
) -> bool {
    let agent_state = state.get("agents").and_then(|v| v.get(agent_id.as_str()));
    let status = agent_state
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase);
    // golden agents.py:247-252 (_is_running): True ONLY for {running,busy}; EVERY other status (including
    // stopped/paused/failed/removed) falls through to the session_name + tmux-window-exists check.
    if matches!(status.as_deref(), Some("running" | "busy")) {
        return true;
    }
    let Some(session_name) = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
    else {
        return false;
    };
    let window = agent_state
        .and_then(|v| v.get("window"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agent_id.as_str());
    window_exists(transport, &session_name, window)
}

/// ---
/// purpose: 判断该席位是不是动态生成的
/// returns: state 里记了动态角色文件，或 spec 里标了来源席位时为 true
/// ---
pub(super) fn is_dynamic_agent(
    state: &serde_json::Value,
    spec_agent: &YamlValue,
    agent_id: &AgentId,
) -> bool {
    let dynamic_role = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("dynamic_role_file"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    dynamic_role
        || spec_agent
            .get("forked_from")
            .and_then(YamlValue::as_str)
            .is_some_and(|s| !s.is_empty())
}

/// ---
/// purpose: 在 spawn 之前预判 tmux 的起法，供审计事件记录
/// params:
///   layout_placement: 有布局位置时按是否起新窗口区分
///   into_existing_session: 目标 session 已存在
/// returns: new-session、new-window 或 split-window
/// ---
/// 0.4.6 Stage 2: predict the tmux start mode BEFORE the spawn call, so
/// the `provider.worker.spawn_argv` event can record what the spawn will
/// actually do. Same logic as `tmux_start_mode_for_spawn` in agent.rs
/// but driven by `(layout_placement, into_existing_session)` only — no
/// dependency on SpawnedAgentWindow.
pub(super) fn predict_tmux_start_mode(
    layout_placement: Option<&crate::lifecycle::launch::LayoutPlacement>,
    into_existing_session: bool,
) -> &'static str {
    if let Some(placement) = layout_placement {
        if placement.starts_window {
            if into_existing_session {
                "new-window"
            } else {
                "new-session"
            }
        } else {
            "split-window"
        }
    } else if into_existing_session {
        "new-window"
    } else {
        "new-session"
    }
}

/// ---
/// purpose: 从盘上读该席位当前的 spawn 世代号
/// returns: 读到的值；读不出 state 或字段缺失时为 0
/// ---
/// 0.4.6 Stage 2: read state.agents[agent_id].spawn_epoch from disk to
/// stamp the spawn_argv event with the current cohort identifier. Returns
/// 0 if the agent row / field is missing.
pub(super) fn state_spawn_epoch_for_agent(workspace: &Path, agent_id: &AgentId) -> u64 {
    let Ok(state) = crate::state::persist::load_runtime_state(workspace) else {
        return 0;
    };
    state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("spawn_epoch"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// ---
/// purpose: 新起进程后清掉该席位的活动观测字段
/// params:
///   agent: 就地删除观测字段；生命周期与拓扑字段一概不动
/// returns: 清完之后观测缺失表示未知，不等于空闲
/// ---
/// 0.5.32 (`.team/artifacts/restart-resumed-stale-activity-locate.md` §5):
/// clear the per-agent turn/activity observation set on a successful new
/// worker process cohort. Called from `mark_agent_started` /
/// `mark_agent_respawned` / `mark_fake_harness_agent_respawned`; NOT from
/// `mark_agent_running_noop` (no new process was created there).
///
/// Removes only observation fields — never lifecycle/topology fields. After
/// clearing, absence is UNKNOWN, not synthesized idle (T7 unknown-never-idle
/// discipline); the next post-spawn tick may repopulate from JSONL freshness
/// gate + pane fallback.
pub(super) fn clear_agent_runtime_activity_observation(
    agent: &mut serde_json::Map<String, serde_json::Value>,
) {
    for field in [
        "activity",
        "worker_state",
        "last_output_at",
        "last_output_hash",
        "current_turn_message_id",
        "current_task_id", // ALLOWED-LEGACY-READ (Phase-DX E2): cleanup removal of legacy display-only key on new spawn cohort; helper does not read the value.
        "task_id",
        "coordinator_idle_capture_next_at",
    ] {
        agent.remove(field);
    }
}

#[cfg(test)]
mod e36_transcript_backing_tests {
    use super::*;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let pid = std::process::id();
            let base = std::env::temp_dir().join(format!("ta-e36-{tag}-{pid}"));
            std::fs::create_dir_all(&base).expect("scratch dir");
            ScratchDir(base)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn convergence_event_uses_central_scrub_and_preserves_required_failure() {
        let scratch = ScratchDir::new("event-log-central");
        let marker = "synthetic-convergence-marker";
        write_session_convergence_progress_event(
            scratch.path(),
            serde_json::json!({
                "diagnostic": format!("mcp_servers.demo.env.OPENAI_API_KEY=\"{marker}\""),
                "required_missing_agent_ids": ["worker"],
                "pending_agent_ids": ["worker"],
            }),
        )
        .expect("central EventLog write");
        let bytes = std::fs::read_to_string(
            scratch
                .path()
                .join(".team")
                .join("logs")
                .join("events.jsonl"),
        )
        .expect("events");
        assert!(!bytes.contains(marker));
        assert!(bytes.contains("[REDACTED]"));

        let blocked = ScratchDir::new("event-log-required");
        std::fs::create_dir_all(
            blocked
                .path()
                .join(".team")
                .join("logs")
                .join("events.jsonl"),
        )
        .expect("occupy event path with directory");
        assert!(write_session_convergence_progress_event(
            blocked.path(),
            serde_json::json!({"pending_agent_ids": ["worker"]}),
        )
        .is_err());
    }

    // E36 fix-B RED→GREEN: a real Claude worker that sent a message has its session
    // transcript landed at <projects_root>/<slug>/<session_id>.jsonl, but neither
    // rollout_path was persisted to state nor a session.captured event was logged.
    // Before the fix, claude_project_transcript_exists did not exist and restart
    // refused such a worker. This asserts the landed transcript is recognized.
    #[test]
    fn claude_project_transcript_is_recognized_without_rollout_or_capture_event() {
        let scratch = ScratchDir::new("recognized");
        let projects_root = scratch.path().join("projects");
        let slug_dir = projects_root.join("-Users-alauda-Documents-code---rust---9");
        std::fs::create_dir_all(&slug_dir).expect("mkdir slug");
        let session_id = "87742d3f-0b4e-4fc1-ad35-447ac2340b65";
        std::fs::write(slug_dir.join(format!("{session_id}.jsonl")), b"{}\n").expect("transcript");

        let agent = serde_json::json!({
            "claude_projects_root": projects_root.to_string_lossy(),
        });
        assert!(
            claude_project_transcript_exists(&agent, session_id),
            "landed claude transcript must count as resume backing (E36 fix-B)"
        );
    }

    #[test]
    fn missing_claude_transcript_is_not_backing() {
        let scratch = ScratchDir::new("missing");
        let projects_root = scratch.path().join("projects");
        std::fs::create_dir_all(&projects_root).expect("mkdir");
        let agent = serde_json::json!({
            "claude_projects_root": projects_root.to_string_lossy(),
        });
        assert!(
            !claude_project_transcript_exists(&agent, "deadbeef-0000-0000-0000-000000000000"),
            "no transcript file => no backing"
        );
    }

    #[test]
    fn empty_session_id_is_not_backing() {
        let agent = serde_json::json!({});
        assert!(!claude_project_transcript_exists(&agent, ""));
    }

    #[test]
    fn codex_session_transcript_is_recognized_when_rollout_path_is_stale() {
        let scratch = ScratchDir::new("codex-recognized");
        let sessions_root = scratch.path().join("sessions");
        let dated = sessions_root.join("2026").join("06").join("20");
        std::fs::create_dir_all(&dated).expect("mkdir dated sessions");
        let session_id = "019ee540-37ed-7a20-a141-1d654224d209";
        std::fs::write(
            dated.join(format!("rollout-2026-06-20T21-37-31-{session_id}.jsonl")),
            b"{}\n",
        )
        .expect("codex transcript");

        let stale = RolloutPath::new(scratch.path().join("old").join("missing.jsonl"));
        let agent = serde_json::json!({
            "codex_sessions_root": sessions_root.to_string_lossy(),
        });
        assert!(
            codex_session_transcript_exists(&agent, session_id, Some(&stale)),
            "matching codex transcript under codex_sessions_root must count as resume backing"
        );
    }

    #[test]
    fn missing_codex_session_transcript_is_not_backing() {
        let scratch = ScratchDir::new("codex-missing");
        let sessions_root = scratch.path().join("sessions");
        std::fs::create_dir_all(&sessions_root).expect("mkdir sessions");
        let agent = serde_json::json!({
            "codex_sessions_root": sessions_root.to_string_lossy(),
        });
        assert!(
            !codex_session_transcript_exists(&agent, "019ee540-ffff-7a20-a141-1d654224d209", None,),
            "no matching codex transcript => no backing"
        );
    }
}
