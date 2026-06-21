//! lifecycle::launch —— 冷启 / quick-start / 危险审批探测 + add/fork / plan 起步与推进。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::{load_runtime_state, save_runtime_state};
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use super::*;

// ── lifecycle::launch —— 冷启 / quick-start / 危险审批探测 ──────────────────

/// `launch(spec_path, dry_run, auto_approve, skip_profile_smoke)`(`launch/core.py:29`)。
/// 冷启全队:路由 tasks、resolve 权限/危险审批门、session 冲突检查(冲突 →
/// `SessionConflict` 拒绝不 kill)、按 startup 顺序起每个 worker、捕获 session、开显示、
/// 写 state/team_state、attach leader receiver。
pub fn launch(
    spec_path: &Path,
    dry_run: bool,
    auto_approve: bool,
    skip_profile_smoke: bool,
) -> Result<LaunchReport, LifecycleError> {
    // CP-1: bind the spawn backend to the per-team socket (derived from the run workspace, the same
    // path the daemon/CLI derive from) so spawn + later has_session/inject/kill all hit one server.
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let transport = crate::tmux_backend::TmuxBackend::for_workspace(&team_workspace(team_dir));
    launch_with_transport(
        spec_path,
        dry_run,
        auto_approve,
        skip_profile_smoke,
        &transport,
    )
}

pub fn launch_with_transport(
    spec_path: &Path,
    dry_run: bool,
    auto_approve: bool,
    skip_profile_smoke: bool,
    transport: &dyn Transport,
) -> Result<LaunchReport, LifecycleError> {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let workspace = team_workspace(team_dir);
    launch_with_transport_in_workspace(
        &workspace,
        spec_path,
        dry_run,
        auto_approve,
        skip_profile_smoke,
        transport,
    )
}

pub fn launch_with_transport_in_workspace(
    workspace: &Path,
    spec_path: &Path,
    dry_run: bool,
    auto_approve: bool,
    skip_profile_smoke: bool,
    transport: &dyn Transport,
) -> Result<LaunchReport, LifecycleError> {
    let _ = skip_profile_smoke;
    if !spec_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "spec path not found: {}",
            spec_path.display()
        )));
    }
    let text = std::fs::read_to_string(spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", spec_path.display())))?;
    let spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    let session_name = spec_session_name(&spec);
    let safety = effective_runtime_config(&spec)?;
    if safety.enabled && !safety.inherited && !auto_approve && !dry_run {
        return Err(LifecycleError::DangerousApprovalRequired(
            "runtime dangerous_auto_approve is enabled".to_string(),
        ));
    }
    if !dry_run && transport_has_session(transport, &session_name) {
        return Err(LifecycleError::SessionConflict(format!(
            "tmux session already exists: {}",
            session_name.as_str()
        )));
    }
    let permissions = spec_agents(&spec)
        .into_iter()
        .map(|agent| PermissionSummary {
            agent_id: agent,
            raw: serde_json::json!({"source": "compiled_spec"}),
        })
        .collect::<Vec<_>>();
    write_launch_permission_audit(workspace, &safety)?;
    let routes = spec_routes(&spec);
    let started = if dry_run {
        Vec::new()
    } else {
        let started = spawn_agents(
            workspace,
            spec_path,
            &spec,
            &session_name,
            &safety,
            transport,
        )?;
        persist_spawn_agent_state(
            workspace,
            spec_path,
            &spec,
            &session_name,
            transport,
            &started,
            &safety,
        )?;
        started
    };
    // 0.3.28 Step 1: topology invariant guard (warn-only during migration).
    // Logs each violation to stderr; never panics. Promoted to hard error at
    // Step 10 once Steps 2–9 have eliminated structural co-location.
    if !dry_run {
        if let Ok(state_for_check) = crate::state::persist::load_runtime_state(workspace) {
            let violations = crate::layout::sessions::assert_topology_invariants(&state_for_check, &spec);
            crate::layout::sessions::log_topology_violations(&violations);
        }
    }
    Ok(LaunchReport {
        session_name,
        started,
        dry_run,
        tmux_endpoint: transport.tmux_endpoint(),
        routes,
        permissions,
        safety,
        leader_receiver_attached: false,
        session_capture_incomplete_agents: Vec::new(),
    })
}

fn spawn_agents(
    workspace: &Path,
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
    safety: &DangerousApproval,
    transport: &dyn Transport,
) -> Result<Vec<StartedAgent>, LifecycleError> {
    // E5 解耦:team_dir(角色定义 + profiles 所在)≠ spec_path.parent()(spec 已迁出到 .team/runtime)。
    // 优先取 state.team_dir(角色目录),回落 spec_path.parent()(legacy 同目录布局)。
    let team_dir_buf = crate::state::persist::load_runtime_state(workspace)
        .ok()
        .and_then(|state| {
            state
                .get("team_dir")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        });
    let team_dir = team_dir_buf
        .as_deref()
        .unwrap_or_else(|| spec_path.parent().unwrap_or_else(|| Path::new(".")));
    let runtime_fast = matches!(
        spec.get("runtime").and_then(|v| v.get("fast")),
        Some(Value::Bool(true))
    );
    let display_backend = spec_display_backend(spec);
    let active_agent_ids = spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| {
            if agent_is_paused(agent) {
                None
            } else {
                agent.get("id").and_then(Value::as_str).map(AgentId::new)
            }
        })
        .collect::<Vec<_>>();
    let layout_plan = if display_backend == DisplayBackend::Adaptive {
        adaptive_layout_plan(&active_agent_ids, ADAPTIVE_LAYOUT_MAX_PER_WINDOW)
    } else {
        Vec::new()
    };
    let mut started = Vec::new();
    for agent in spec_agent_values(spec) {
        let Some(agent_id_raw) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        if agent_is_paused(agent) {
            continue;
        }
        let agent_id = AgentId::new(agent_id_raw);
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .and_then(parse_provider)
            .unwrap_or(Provider::Codex);
        let auth_mode = agent
            .get("auth_mode")
            .and_then(Value::as_str)
            .and_then(parse_auth_mode)
            .unwrap_or(AuthMode::Subscription);
        let model = agent.get("model").and_then(Value::as_str);
        let adapter = crate::provider::get_adapter(provider);
        // Contract C / F6.4: pass the COMPILED agent context (resolved role/system prompt,
        // tools list, per-worker MCP config) into command construction so a real worker
        // has both the role instruction AND the callable Team Agent MCP capability.
        // probe5 RED proved that `build_command(.., None, None, ..)` left the worker
        // without `report_result`; placeholders are substituted at spawn time.
        let command_agent = crate::lifecycle::worker_command_context::WorkerCommandAgent::from_yaml(
            agent,
            Some(agent_id_raw),
            provider,
        );
        let system_prompt =
            crate::lifecycle::worker_command_context::compile_worker_system_prompt(&command_agent)?;
        let tools = crate::lifecycle::worker_command_context::resolved_tool_strings_for_command(
            &command_agent,
            provider,
            safety,
        )?;
        let resolved_tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
        let mcp_team_id = runtime_team_key_for_spec(spec_path, spec, session_name);
        let mcp_config = adapter
            .mcp_config(auth_mode)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?;
        let mcp_config = resolve_mcp_config(mcp_config, workspace, agent_id_raw, &mcp_team_id);
        let mcp_config_path = write_worker_mcp_config_for_provider(
            workspace,
            agent_id_raw,
            &mcp_config,
            Some(provider),
        )?;
        let profile_dir = team_dir.join("profiles");
        let profile_launch =
            crate::lifecycle::profile_launch::prepare_provider_profile_launch_with_profile_dir(
                workspace,
                agent_id_raw,
                agent,
                Some(&profile_dir),
                Some(&mcp_config),
            )?;
        let command_model = profile_launch.command_overrides.model.as_deref().or(model);
        let mut plan = adapter
            .build_command_plan(crate::provider::ProviderCommandContext {
                auth_mode,
                mcp_config: Some(&mcp_config),
                system_prompt: Some(system_prompt.as_str()),
                model: command_model,
                tools: &resolved_tool_refs,
                profile_launch: Some(&profile_launch),
                // Layer 1 self-healing (architect probe 2026-06-22): expose
                // agent_id as a display-name hint so Claude / Copilot
                // adapters can pass `--name <agent_id>`. Codex has no
                // equivalent flag and ignores the hint.
                agent_id_hint: Some(agent_id_raw),
            })
            .map_err(|e| LifecycleError::Provider(e.to_string()))?;
        if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
            point_native_mcp_config_at_file(&mut plan.argv, provider, &mcp_config_path);
        }
        // C-A-4 cr verdict v2 — Copilot BYOK(compatible_api)硬性校验:
        // "A model is required for BYOK"(help-providers 原文)。检查 agent
        // 的 model 来源:角色 spec.model > profile COPILOT_MODEL(经 env_overlay)
        // > --model 旗(本 worker 路径不在 argv 后追加用户 --model)。三者全空 → 报错
        // 含 "model" 字面,失败信息透传给 leader。
        if matches!(provider, Provider::Copilot) && auth_mode == AuthMode::CompatibleApi {
            let has_model = model.is_some_and(|s| !s.is_empty())
                || profile_launch.command_overrides.model.as_deref().is_some_and(|s| !s.is_empty())
                || profile_launch
                    .env_overlay
                    .get("COPILOT_MODEL")
                    .is_some_and(|v| !v.is_empty());
            if !has_model {
                return Err(LifecycleError::RequirementUnmet(
                    "copilot BYOK profile requires a model (set COPILOT_MODEL, agent.model, or --model)"
                        .to_string(),
                ));
            }
        }
        // §B1 + C-7-1 + C-6-2 + C-3-2 cr verdict v2 — Copilot launch-time argv 注入:
        //   -n <agent_id>      会话命名(main-help:104)→ resume-by-name + 人查 双键
        //   -C <workspace>     双保险 cwd(main-help:55-56),防 shell 包装意外
        //   --log-dir <path>   per-worker 定向日志(help-logging)→ 故障期可读 + N18 隔离
        //   --log-level info   配套日志级别
        //   --disable-mcp-server <n>...  C-3-2 残留 MCP server 按名禁(扫 mcp list)
        if matches!(provider, Provider::Copilot) {
            plan.argv.push("-n".to_string());
            plan.argv.push(agent_id_raw.to_string());
            plan.argv.push("-C".to_string());
            plan.argv.push(workspace.to_string_lossy().to_string());
            let log_dir = workspace
                .join(".team")
                .join("logs")
                .join("copilot")
                .join(agent_id_raw);
            std::fs::create_dir_all(&log_dir).map_err(|e| {
                LifecycleError::StatePersist(format!("{}: {e}", log_dir.display()))
            })?;
            plan.argv.push("--log-dir".to_string());
            plan.argv.push(log_dir.to_string_lossy().to_string());
            plan.argv.push("--log-level".to_string());
            plan.argv.push("info".to_string());
            // C-3-2/C-3-3 cr verdict v2 — spawn 前扫 `copilot mcp list` 找用户全局/
            // workspace 的 MCP 残留,对每个非 team_orchestrator server 追加
            // --disable-mcp-server <name>,并落 mcp-residual.txt + event。
            apply_copilot_mcp_residual_disables(
                &workspace,
                agent_id_raw,
                &mut plan.argv,
                &log_dir,
            )?;
        }
        fill_spawn_placeholders_full(&mut plan.argv, workspace, agent_id_raw, Some(&mcp_team_id));
        let mut env =
            inherited_env_with_team_overrides(workspace, agent_id_raw, Some(&mcp_team_id));
        apply_profile_launch_env(&mut env, &profile_launch);
        apply_mcp_auto_approval_env(&mut env, &safety);
        // Python providers.py:145 + launch/core.py:253 — fresh launch runs the worker
        // with cwd=workspace, same as the RS fork/add and restart paths.
        let env_unset: Vec<String> = profile_launch.env_unset.iter().cloned().collect();
        // BUG / C-1-2 / C-6-1 cr verdict — Copilot system_prompt 走 spawn env overlay +
        // per-worker AGENTS.md(B2 灵魂件降级):写
        //   <workspace>/.team/runtime/copilot-instructions/<agent_id>/AGENTS.md
        // 全文 == compile_worker_system_prompt 输出,并通过 spawn env
        // `COPILOT_CUSTOM_INSTRUCTIONS_DIRS=<该目录>` 让 copilot CLI 加载。
        // **禁** silent 写 ~/.copilot/AGENTS.md(C-1-2)+ **禁** -i 作首条消息(C-1-5)。
        if matches!(provider, Provider::Copilot) {
            apply_copilot_instructions_overlay(
                workspace,
                agent_id_raw,
                system_prompt.as_str(),
                &mut env,
            )?;
            // C-A-6 cr verdict v2 — Copilot worker env 全量继承下,用户 shell 的
            // COPILOT_GITHUB_TOKEN / GH_TOKEN / GITHUB_TOKEN 会穿透 + 按 cmd-login 实证
            // **优先于凭据库**(可能静默改变 auth 通道)。一期只观测不剥除(剥除是
            // 行为变更,cr 裁);命中任一就发 warn event 让 user 可见。
            let mut passthrough: Vec<String> = Vec::new();
            for key in ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
                if env.get(key).is_some_and(|v| !v.is_empty()) {
                    passthrough.push(key.to_string());
                }
            }
            if !passthrough.is_empty() {
                let event_log = crate::event_log::EventLog::new(workspace);
                let _ = event_log.write(
                    "provider.copilot.token_passthrough_warning",
                    serde_json::json!({
                        "agent_id": agent_id_raw,
                        "tokens": passthrough,
                        "reason": "user shell GITHUB_TOKEN family takes precedence over copilot credential store (cmd-login)",
                    }),
                );
            }
        }
        // E6 层1 实证3 + 诊断留痕:落最终 worker argv(spawn 前的真实形态)。
        // 任何"--session-id 预定 UUID 没生效"必须能从 events.jsonl 回答:argv 里到底有没有它。
        // 抽出 --session-id 值单列,方便和盘上 ~/.claude/projects/<cwd> 实际落的 UUID 对账。
        {
            let session_id_in_argv = plan
                .argv
                .iter()
                .position(|a| a == "--session-id")
                .and_then(|i| plan.argv.get(i + 1))
                .cloned();
            let event_log = crate::event_log::EventLog::new(workspace);
            let _ = event_log.write(
                "provider.worker.spawn_argv",
                serde_json::json!({
                    "agent_id": agent_id_raw,
                    "provider": provider,
                    "argv": plan.argv,
                    "session_id_in_argv": session_id_in_argv,
                    "expected_session_id": plan.expected_session_id.as_ref().map(|s| s.as_str()),
                }),
            );
        }
        // 0.3.28 Step 4b: replaced the `adaptive_layout_plan` 3-pane tiling
        // with Python-parity 1-window-per-agent placement. Window name =
        // `agent_id`; first worker creates the session via spawn_first,
        // subsequent workers each get a new window via spawn_into. No splits
        // in the worker session — Step 8's `assert_overlay_call_site` would
        // catch any drift if a split call snuck back in. The `placement`
        // variable is set to None to signal "no adaptive layout" to all
        // downstream consumers (display dict, layout_window persistence).
        let placement: Option<LayoutPlacement> = None;
        let window = WindowName::new(agent_id_raw);
        let spawn = if started.is_empty() {
            transport.spawn_first_with_env_unset(
                session_name,
                &window,
                &plan.argv,
                workspace,
                &env,
                &env_unset,
            )
        } else {
            transport.spawn_into_with_env_unset(
                session_name,
                &window,
                &plan.argv,
                workspace,
                &env,
                &env_unset,
            )
        }
        .map_err(|e| LifecycleError::Transport(e.to_string()))?;
        let _ = adapter.handle_startup_prompts(
            transport,
            &Target::Pane(spawn.pane_id.clone()),
            30,
            0.5,
        );
        // Python launch/core.py:235-237 — runtime.fast toggles the provider's fast mode
        // after spawn; provider specifics live behind the adapter (F032).
        if runtime_fast {
            let _ = adapter.enable_fast_mode(transport, &Target::Pane(spawn.pane_id.clone()));
        }
        if matches!(transport.liveness(&spawn.pane_id), Ok(PaneLiveness::Dead)) {
            continue;
        }
        if placement.is_some() {
            configure_adaptive_pane_title(
                workspace,
                transport,
                session_name,
                &window,
                &spawn.pane_id,
                agent_id_raw,
            );
        }
        let display = if placement.is_some() {
            WorkerDisplay::Adaptive {
                status: DisplayStatus::Opened,
                window: Some(spawn.window.clone()),
                workspace_window: None,
                pane_id: Some(spawn.pane_id.clone()),
                pane_title: Some(agent_id_raw.to_string()),
                target: Some(spawn.pane_id.as_str().to_string()),
                target_worker_session: Some(session_name.as_str().to_string()),
                linked_session: None,
                leader_session: Some(session_name.clone()),
                display_session: None,
                fallback: None,
            }
        } else {
            WorkerDisplay::Blocked {
                reason: AdaptiveBlockReason::NotImplementedThisPlatform,
            }
        };
        started.push(StartedAgent {
            agent_id,
            start_mode: StartMode::Fresh,
            target: spawn.pane_id.as_str().to_string(),
            session_id: None,
            rollout_path: None,
            pending_session_id: plan.expected_session_id.clone(),
            claude_config_dir: profile_launch.claude_config_dir.clone(),
            provider_projects_root: plan
                .provider_projects_root
                .clone()
                .or_else(|| profile_launch.claude_projects_root.clone()),
            managed_mcp_config: plan.managed_mcp_config || profile_launch.managed_mcp_config,
            layout_window: placement
                .as_ref()
                .map(|placement| placement.layout_window.clone()),
            layout_index: placement.as_ref().map(|placement| placement.layout_index),
            pane_index: placement.as_ref().map(|placement| placement.pane_index),
            display,
        });
    }
    Ok(started)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayoutPlacement {
    pub agent_id: AgentId,
    pub layout_window: WindowName,
    pub layout_index: usize,
    pub pane_index: usize,
    pub starts_window: bool,
}

pub(crate) fn adaptive_layout_plan(
    agent_ids: &[AgentId],
    max_per_window: usize,
) -> Vec<LayoutPlacement> {
    let max_per_window = max_per_window.max(1);
    agent_ids
        .iter()
        .enumerate()
        .map(|(idx, agent_id)| {
            let layout_index = idx / max_per_window;
            let pane_index = idx % max_per_window;
            LayoutPlacement {
                agent_id: agent_id.clone(),
                layout_window: WindowName::new(format!("team-w{}", layout_index + 1)),
                layout_index,
                pane_index,
                starts_window: pane_index == 0,
            }
        })
        .collect()
}

pub(crate) const ADAPTIVE_LAYOUT_MAX_PER_WINDOW: usize = 3;

pub(crate) fn state_uses_adaptive_layout(state: &serde_json::Value) -> bool {
    state
        .get("display_backend")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|backend| backend == "adaptive")
        || state
            .get("runtime")
            .and_then(|runtime| runtime.get("display_backend"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|backend| backend == "adaptive")
        || state
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|agents| {
                agents.values().any(|agent| {
                    agent
                        .get("layout_window")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|window| !window.is_empty())
                })
            })
}

pub(crate) fn adaptive_placement_for_agent(
    state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    agent_id: &AgentId,
) -> Option<LayoutPlacement> {
    if !state_uses_adaptive_layout(state) {
        return None;
    }
    // E43 Fix A (0.3.24 bug#3, demo-director blocker): cross-check live panes
    // by both pane_id AND window_name. Real-machine state can carry a stale
    // `layout_window` residue (e.g. "team-w2") while the live pane for that
    // agent actually lives under a DIFFERENT window_name (e.g. "architect").
    // The prior pane_id-only check let the stale claim slip through, and the
    // resulting placement asked spawn_agent_window to split a phantom window.
    let live_targets = transport.list_targets().unwrap_or_default();
    let live_panes: BTreeSet<String> = live_targets
        .iter()
        .map(|pane| pane.pane_id.as_str().to_string())
        .collect();
    // Map pane_id → window_name for the window-name cross-check.
    let live_pane_window: BTreeMap<String, String> = live_targets
        .iter()
        .filter_map(|pane| {
            pane.window_name
                .as_ref()
                .map(|name| (pane.pane_id.as_str().to_string(), name.as_str().to_string()))
        })
        .collect();
    let live_windows: BTreeSet<String> = transport
        .list_windows(session_name)
        .unwrap_or_default()
        .into_iter()
        .map(|window| window.as_str().to_string())
        .collect();
    let mut windows: BTreeMap<usize, (String, usize)> = BTreeMap::new();
    if let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) {
        for (id, agent) in agents {
            if id == agent_id.as_str() {
                continue;
            }
            let Some(window) = agent
                .get("layout_window")
                .or_else(|| agent.get("window"))
                .and_then(serde_json::Value::as_str)
                .filter(|window| !window.is_empty())
            else {
                continue;
            };
            // E45 (0.3.24 bug#4): only canonical `team-w<N>[-suffix]` windows
            // count as adaptive layout windows. Per-agent windows (named
            // after agent_id like `developer`) are NOT layout windows even if
            // the state's `layout_window`/`layout_index` carries them — those
            // are residue from a prior launch or explicit per-agent topology.
            // Treating them as adaptive caused add-agent to split into the
            // developer worker's pane on macmini (split-window -t :developer
            // succeeded → 2 panes in developer window, no new window for
            // demo-director).
            if !is_adaptive_layout_window(window) {
                continue;
            }
            let pane_id = agent
                .get("pane_id")
                .and_then(serde_json::Value::as_str);
            let pane_live = pane_id.is_some_and(|pane| live_panes.contains(pane));
            // E43 Fix A: window_name match — when the agent's pane is live,
            // its live window_name MUST equal the claimed `window`; otherwise
            // the claim is stale residue from a respawn/rename and must not
            // count toward the layout map.
            let pane_window_matches = pane_id
                .and_then(|pane| live_pane_window.get(pane))
                .is_some_and(|name| name == window);
            if pane_live && !pane_window_matches {
                continue;
            }
            if !pane_live && (!live_panes.is_empty() || !live_windows.contains(window)) {
                continue;
            }
            let layout_index = agent
                .get("layout_index")
                .and_then(serde_json::Value::as_u64)
                .map(|idx| idx as usize)
                .or_else(|| parse_team_layout_index(window))
                .unwrap_or(windows.len());
            let entry = windows
                .entry(layout_index)
                .or_insert_with(|| (window.to_string(), 0));
            entry.1 = entry.1.saturating_add(1);
        }
    }
    if let Some((&layout_index, (window, count))) = windows.iter().next_back() {
        if *count < ADAPTIVE_LAYOUT_MAX_PER_WINDOW {
            return Some(LayoutPlacement {
                agent_id: agent_id.clone(),
                layout_window: WindowName::new(window.clone()),
                layout_index,
                pane_index: *count,
                starts_window: false,
            });
        }
    }
    // E45 (0.3.24 bug#4): when the live session has NO real adaptive layout
    // window (the topology is effectively per-agent, even though state says
    // display_backend=adaptive), DO NOT synthesise a fresh `team-w<N>`
    // window — that would force the new agent into an adaptive-layout pane
    // shape the rest of the session does not actually use. Return None so
    // the caller (`start_agent_at_paths` → `spawn_agent_window`) falls back
    // to its non-placement path, which opens a new window named after the
    // agent_id (canonical per-agent pattern, matches the existing 7 workers).
    if windows.is_empty() {
        return None;
    }
    let next_index = windows.keys().next_back().map(|idx| idx + 1).unwrap_or(0);
    let base = format!("team-w{}", next_index + 1);
    Some(LayoutPlacement {
        agent_id: agent_id.clone(),
        layout_window: unique_layout_window_name(&base, &live_windows),
        layout_index: next_index,
        pane_index: 0,
        starts_window: true,
    })
}

pub(crate) fn adaptive_existing_placement_for_agent(
    state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    agent_id: &AgentId,
) -> Option<LayoutPlacement> {
    if !state_uses_adaptive_layout(state) {
        return None;
    }
    let agent = state.get("agents")?.get(agent_id.as_str())?;
    let window = agent
        .get("layout_window")
        .or_else(|| agent.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|window| !window.is_empty())?;
    // E45 (0.3.24 bug#4): existing-placement is meaningless for a per-agent
    // window name (e.g. `developer`). Return None and let the caller fall
    // back to its non-placement spawn path, which opens / reuses a window
    // named after the agent_id. Only canonical `team-w<N>` windows are
    // honored as existing adaptive placements.
    if !is_adaptive_layout_window(window) {
        return None;
    }
    let layout_index = agent
        .get("layout_index")
        .and_then(serde_json::Value::as_u64)
        .map(|idx| idx as usize)
        .or_else(|| parse_team_layout_index(window))
        .unwrap_or(0);
    let desired_pane_index = agent
        .get("pane_index")
        .and_then(serde_json::Value::as_u64)
        .map(|idx| idx as usize)
        .unwrap_or(0);
    let live_windows: BTreeSet<String> = transport
        .list_windows(session_name)
        .unwrap_or_default()
        .into_iter()
        .map(|window| window.as_str().to_string())
        .collect();
    if !live_windows.contains(window) {
        // E43 Fix B (0.3.24 bug#3): the claimed `layout_window` is NOT in
        // live_windows — it's stale residue. The session is effectively
        // per-agent (live windows are named after agent_ids), so fall back
        // to a new window named after the agent_id itself instead of
        // synthesising a fresh phantom-named window the next spawn would
        // try (and fail) to split.
        return Some(LayoutPlacement {
            agent_id: agent_id.clone(),
            layout_window: WindowName::new(agent_id.as_str()),
            layout_index,
            pane_index: 0,
            starts_window: true,
        });
    }
    let existing_panes = transport
        .list_targets()
        .unwrap_or_default()
        .into_iter()
        .filter(|pane| {
            pane.window_name.as_ref().is_some_and(|name| name.as_str() == window)
                && agent
                    .get("pane_id")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|agent_pane| agent_pane != pane.pane_id.as_str())
        })
        .count();
    Some(LayoutPlacement {
        agent_id: agent_id.clone(),
        layout_window: WindowName::new(window),
        layout_index,
        pane_index: existing_panes,
        starts_window: false,
    })
}

fn parse_team_layout_index(window: &str) -> Option<usize> {
    window
        .strip_prefix("team-w")
        .and_then(|rest| rest.split('-').next())
        .and_then(|raw| raw.parse::<usize>().ok())
        .and_then(|idx| idx.checked_sub(1))
}

/// E45 (0.3.24 bug#4, demo-director second-layer drift): a window name is a
/// REAL adaptive layout window only when it matches the canonical
/// `team-w<N>[-suffix]` shape (i.e. `parse_team_layout_index` returns Some).
/// Per-agent window names like `developer` / `architect` / `demo-director`
/// are NOT adaptive layout windows even if state happens to carry them in
/// the agent's `layout_window` or `layout_index` field — they are residue
/// from a prior launch / explicit per-agent topology. Treating them as
/// adaptive caused the macmini repro: add-agent demo-director split into
/// the developer worker's window @453 instead of opening its own.
fn is_adaptive_layout_window(window: &str) -> bool {
    parse_team_layout_index(window).is_some()
}

/// Crate-public wrapper for the defensive guard at
/// `restart/common.rs::spawn_agent_window`. Same semantics as the private
/// helper above; promoted to `pub(crate)` so the spawn-time defence-in-depth
/// layer can refuse to split into a per-agent window even if a stale
/// placement asks for it.
pub(crate) fn is_adaptive_layout_window_pub(window: &str) -> bool {
    is_adaptive_layout_window(window)
}

fn unique_layout_window_name(base: &str, live_windows: &BTreeSet<String>) -> WindowName {
    if !live_windows.contains(base) {
        return WindowName::new(base);
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !live_windows.contains(&candidate) {
            return WindowName::new(candidate);
        }
    }
    unreachable!("unbounded suffix search always returns")
}

fn persist_spawn_agent_state(
    workspace: &Path,
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
    transport: &dyn Transport,
    started: &[StartedAgent],
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    let state_path = crate::state::persist::runtime_state_path(workspace);
    let mut state = if state_path.exists() {
        let text = std::fs::read_to_string(&state_path)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", state_path.display())))?;
        serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", state_path.display())))?
    } else {
        serde_json::json!({"agents": {}})
    };
    let team_id = runtime_team_key_for_spec(spec_path, spec, session_name);
    let worker_tmux_socket = launched_worker_tmux_socket(transport, workspace);
    drop_worker_pane_seeded_owner(&mut state, &team_id, started, worker_tmux_socket.as_deref());
    // Only persist running state for agents whose spawn still has a live target.
    let live_windows: BTreeSet<String> = transport
        .list_windows(session_name)
        .unwrap_or_default()
        .into_iter()
        .map(|w| w.as_str().to_string())
        .collect();
    let live_started_agents: BTreeSet<String> = started
        .iter()
        .map(|agent| agent.agent_id.as_str().to_string())
        .collect();
    let pane_pids_by_agent = pane_pids_by_started_agent(transport, started);
    // E5 解耦:profiles 随**角色定义**(team_dir),不随 spec(已迁出到 .team/runtime)。
    // 优先 state.team_dir(角色目录),回落 spec_path.parent()(legacy 同目录布局)。
    let profile_dir = state
        .get("team_dir")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|dir| Path::new(dir).join("profiles"))
        .unwrap_or_else(|| spec_path.parent().unwrap_or(workspace).join("profiles"));
    let mut agents = serde_json::Map::new();
    let mut spawn_index = 0_u32;
    for agent in spec_agent_values(spec) {
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .and_then(parse_provider)
            .unwrap_or(Provider::Codex);
        if agent_is_paused(agent) {
            let mut paused = serde_json::Map::new();
            paused.insert("status".to_string(), serde_json::json!("paused"));
            paused.insert("provider".to_string(), serde_json::json!(provider));
            agents.insert(id.to_string(), serde_json::Value::Object(paused));
            continue;
        }
        let started_agent = started.iter().find(|agent| agent.agent_id.as_str() == id);
        let window = started_agent
            .and_then(|started| started.layout_window.as_ref())
            .map(WindowName::as_str)
            .or_else(|| agent.get("window").and_then(Value::as_str))
            .unwrap_or(id);
        if !live_started_agents.contains(id)
            || (!live_windows.is_empty() && !live_windows.contains(window))
        {
            let mut failed = serde_json::Map::new();
            failed.insert("status".to_string(), serde_json::json!("spawn_failed"));
            failed.insert("provider".to_string(), serde_json::json!(provider));
            failed.insert("agent_id".to_string(), serde_json::json!(id));
            failed.insert("window".to_string(), serde_json::json!(window));
            failed.insert(
                "reason".to_string(),
                serde_json::json!("tmux window not present after spawn"),
            );
            agents.insert(id.to_string(), serde_json::Value::Object(failed));
            continue;
        }
        let pane_pid = pane_pids_by_agent.get(id).copied();
        let spawned_at = spawn_timestamp_for_agent(spawn_index);
        spawn_index = spawn_index.saturating_add(1);
        agents.insert(
            id.to_string(),
            running_agent_state(
                agent,
                id,
                provider,
                workspace,
                workspace,
                &spawned_at,
                &team_id,
                Some(agent_id_to_pane_id(started, id)),
                pane_pid,
                safety,
                started_agent,
                Some(&profile_dir),
            )?,
        );
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("agents".to_string(), serde_json::Value::Object(agents));
    } else {
        let mut obj = serde_json::Map::new();
        obj.insert("agents".to_string(), serde_json::Value::Object(agents));
        state = serde_json::Value::Object(obj);
    }
    save_launched_team_state_for_key(workspace, &state, Some(&team_id))
}

fn pane_pids_by_started_agent(
    transport: &dyn Transport,
    started: &[StartedAgent],
) -> BTreeMap<String, u32> {
    let panes = transport.list_targets().unwrap_or_default();
    started
        .iter()
        .filter_map(|agent| {
            panes
                .iter()
                .find(|pane| pane.pane_id.as_str() == agent.target)
                .and_then(|pane| pane.pane_pid)
                .map(|pid| (agent.agent_id.as_str().to_string(), pid))
        })
        .collect()
}

fn agent_id_to_pane_id<'a>(started: &'a [StartedAgent], agent_id: &str) -> &'a str {
    started
        .iter()
        .find(|agent| agent.agent_id.as_str() == agent_id)
        .map(|agent| agent.target.as_str())
        .unwrap_or("")
}

fn save_launched_team_state(
    workspace: &Path,
    launched: &serde_json::Value,
) -> Result<(), LifecycleError> {
    save_launched_team_state_for_key(workspace, launched, None)
}

fn save_launched_team_state_for_key(
    workspace: &Path,
    launched: &serde_json::Value,
    team_key: Option<&str>,
) -> Result<(), LifecycleError> {
    let existing = load_runtime_state(workspace).unwrap_or_else(|_| serde_json::json!({}));
    let launched_key = team_key
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::state::projection::team_state_key(launched));
    let mut launched = launched.clone();
    if let Some(obj) = launched.as_object_mut() {
        obj.insert(
            "active_team_key".to_string(),
            serde_json::Value::String(launched_key.clone()),
        );
        obj.entry("is_external_leader".to_string())
            .or_insert(serde_json::Value::Bool(false));
    }
    promote_launched_binding_from_team_entry(&mut launched, &launched_key);
    preserve_existing_leader_topology(&existing, &launched_key, &mut launched);
    drop_foreign_seeded_owner(&existing, &launched_key, &mut launched);
    drop_bare_worker_seeded_owner(&mut launched, &launched_key);
    let merged = if team_key.is_some() {
        merge_workspace_team_state_with_key(&existing, &launched, &launched_key)
    } else {
        crate::state::projection::merge_workspace_team_state(&existing, &launched)
    };
    let mut projected = crate::state::projection::project_top_level_view(&merged, &launched_key);
    drop_unbound_top_level_owner(&mut projected);
    save_runtime_state(workspace, &projected)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn preserve_existing_leader_topology(
    existing: &serde_json::Value,
    launched_key: &str,
    launched: &mut serde_json::Value,
) {
    let Some(obj) = launched.as_object_mut() else {
        return;
    };
    let existing_team = existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(launched_key))
        .unwrap_or(existing);
    for key in ["is_external_leader", "leader_client"] {
        if !obj.contains_key(key) {
            if let Some(value) = existing_team.get(key).or_else(|| existing.get(key)) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
}

fn drop_bare_worker_seeded_owner(launched: &mut serde_json::Value, launched_key: &str) {
    if has_positive_caller_leader_env() {
        return;
    }
    let pane = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if pane.ends_with("-first") {
        seed_unbound_launched_owner(launched, launched_key);
    }
}

fn merge_workspace_team_state_with_key(
    existing: &serde_json::Value,
    launched: &serde_json::Value,
    launched_key: &str,
) -> serde_json::Value {
    let mut launched_obj = launched.as_object().cloned().unwrap_or_default();
    let mut teams = existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let launched_entry = crate::state::projection::compact_team_state(launched);
    if !existing
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|session| !session.is_empty())
    {
        teams.insert(launched_key.to_string(), launched_entry);
        launched_obj.insert("teams".to_string(), serde_json::Value::Object(teams));
        return serde_json::Value::Object(launched_obj);
    }

    let existing_key = explicit_active_team_key(existing)
        .unwrap_or_else(|| crate::state::projection::team_state_key(existing));
    if existing_key == launched_key {
        let mut teams = existing
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        teams.insert(launched_key.to_string(), launched_entry);
        launched_obj.insert("teams".to_string(), serde_json::Value::Object(teams));
        return serde_json::Value::Object(launched_obj);
    }

    let mut merged = existing.as_object().cloned().unwrap_or_default();
    let mut teams = merged
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    teams
        .entry(existing_key)
        .or_insert_with(|| crate::state::projection::compact_team_state(existing));
    teams.insert(launched_key.to_string(), launched_entry);
    merged.insert("teams".to_string(), serde_json::Value::Object(teams));
    serde_json::Value::Object(merged)
}

#[cfg(test)]
mod merge_workspace_team_state_with_key_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_top_level_existing_session_preserves_existing_teams() {
        let existing = json!({
            "session_name": "",
            "active_team_key": "parent",
            "teams": {
                "parent": {
                    "session_name": "team-parent",
                    "agents": {"parent_worker": {"status": "running"}}
                }
            }
        });
        let launched = json!({
            "session_name": "team-child",
            "agents": {"child_worker": {"status": "running"}}
        });
        let merged = merge_workspace_team_state_with_key(&existing, &launched, "child");
        assert_eq!(
            merged.pointer("/teams/parent/session_name"),
            Some(&json!("team-parent")),
            "existing.teams must survive even when existing.session_name is empty: {merged}"
        );
        assert_eq!(
            merged.pointer("/teams/child/session_name"),
            Some(&json!("team-child")),
            "launched team must still be inserted under its runtime key: {merged}"
        );
    }
}

fn promote_launched_binding_from_team_entry(launched: &mut serde_json::Value, launched_key: &str) {
    let entry = launched
        .get("teams")
        .and_then(|teams| teams.get(launched_key))
        .cloned();
    let Some(entry) = entry else {
        return;
    };
    let Some(obj) = launched.as_object_mut() else {
        return;
    };
    for key in ["leader_receiver", "team_owner", "owner_epoch"] {
        if !obj.contains_key(key) {
            if let Some(value) = entry.get(key) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
}

fn drop_unbound_top_level_owner(state: &mut serde_json::Value) {
    let pane = state
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if pane.starts_with('%') || pane.chars().all(|ch| ch.is_ascii_digit()) && !pane.is_empty() {
        return;
    }
    if let Some(obj) = state.as_object_mut() {
        obj.remove("leader_receiver");
        obj.remove("team_owner");
        obj.remove("owner_epoch");
    }
}

fn drop_foreign_seeded_owner(
    existing: &serde_json::Value,
    launched_key: &str,
    launched: &mut serde_json::Value,
) {
    let Some(pane) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return;
    };
    if owner_pane_belongs_to_other_team(existing, launched_key, pane) {
        let replacement = unbound_launched_owner(launched, launched_key);
        if let Some(obj) = launched.as_object_mut() {
            if let Some(owner) = replacement {
                obj.insert("team_owner".to_string(), owner);
            } else {
                obj.remove("team_owner");
            }
            obj.remove("owner_epoch");
        }
    }
}

fn drop_worker_pane_seeded_owner(
    launched: &mut serde_json::Value,
    launched_key: &str,
    started: &[StartedAgent],
    worker_tmux_socket: Option<&str>,
) {
    let Some(pane) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return;
    };
    let leader_pane = std::env::var("TEAM_AGENT_LEADER_PANE_ID")
        .ok()
        .filter(|value| !value.is_empty());
    let tmux_pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.is_empty());
    let has_leader_identity_env = has_positive_caller_leader_env();
    let seeded_from_bare_tmux = !has_leader_identity_env && tmux_pane.as_deref() == Some(pane);
    let caller_tmux_socket = crate::tmux_backend::socket_name_from_tmux_env();
    if seeded_from_bare_tmux
        && (tmux_sockets_match_or_unknown(caller_tmux_socket.as_deref(), worker_tmux_socket)
            || pane.ends_with("-first"))
        && seeded_pane_looks_like_worker(pane, started)
    {
        seed_unbound_launched_owner(launched, launched_key);
    }
}

fn seeded_pane_looks_like_worker(pane: &str, started: &[StartedAgent]) -> bool {
    pane.ends_with("-first")
        || started.iter().any(|agent| {
            pane == agent.target
                || pane.starts_with(agent.target.as_str())
                || agent.target.starts_with(pane)
        })
}

fn launched_worker_tmux_socket(transport: &dyn Transport, workspace: &Path) -> Option<String> {
    if matches!(transport.kind(), crate::transport::BackendKind::Tmux) {
        Some(crate::tmux_backend::socket_name_for_workspace(workspace))
    } else {
        None
    }
}

fn tmux_sockets_match_or_unknown(caller_socket: Option<&str>, worker_socket: Option<&str>) -> bool {
    match (caller_socket, worker_socket) {
        (Some(caller), Some(worker)) => caller == worker,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn env_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|value| !value.is_empty())
}

/// B-7 / 036b — TEAM_AGENT_LEADER_PANE_ID 主动路径 fail-fast helper。
/// 入口形态(N38 三行式):
///   error  : `TEAM_AGENT_LEADER_PANE_ID points at a dead/absent pane: %<id>`
///   action : `unset TEAM_AGENT_LEADER_PANE_ID, or set it to a live tmux pane id`
///   log    : `TEAM_AGENT_LEADER_PANE_ID=%<id>`
/// env 未设(或空)→ Ok(())。
/// env 设了但 pane 可判定为 Dead/Absent → Err(RequirementUnmet)。
/// 真实 tmux 后端跨所有现存 tmux socket server 探测:TEAM_AGENT_LEADER_PANE_ID 是用户
/// override 指针,不归属当前 team socket。
/// probe 返 Unknown 不挡(被动路径降级):本主动路径只对【显式 Dead/Absent】fail-fast,
/// MUST-17 不过度设计 / unset 走 pass-through(b7_unset_leader_pane_env_passes_through 守)。
pub(crate) fn validate_active_leader_pane_env(
    transport: &dyn Transport,
) -> Result<(), LifecycleError> {
    validate_active_leader_pane_env_with_workspaces(transport, &[])
}

pub(crate) fn validate_active_leader_pane_env_with_workspace(
    transport: &dyn Transport,
    workspace: Option<&Path>,
) -> Result<(), LifecycleError> {
    let workspaces = workspace.into_iter().collect::<Vec<_>>();
    validate_active_leader_pane_env_with_workspaces(transport, &workspaces)
}

pub(crate) fn validate_active_leader_pane_env_with_workspaces(
    transport: &dyn Transport,
    workspaces: &[&Path],
) -> Result<(), LifecycleError> {
    let pane_id_raw = match std::env::var("TEAM_AGENT_LEADER_PANE_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(()),
    };
    let pane = crate::transport::PaneId::new(&pane_id_raw);
    if !is_tmux_pane_id_format(&pane) {
        write_invalid_leader_pane_env_warning(workspaces, &pane_id_raw);
        return Ok(());
    }
    let failure = match leader_pane_env_state_for_validation(transport, &pane) {
        LeaderPaneEnvState::Dead => Some("dead"),
        LeaderPaneEnvState::Absent => Some("absent"),
        LeaderPaneEnvState::Live | LeaderPaneEnvState::Unknown => None,
    };
    let Some(reason) = failure else {
        return Ok(());
    };
    Err(LifecycleError::RequirementUnmet(format!(
        "TEAM_AGENT_LEADER_PANE_ID points at a {reason} pane: {pane_id_raw}\n\
         action: unset TEAM_AGENT_LEADER_PANE_ID, or set it to a live tmux pane id\n\
         log: TEAM_AGENT_LEADER_PANE_ID={pane_id_raw}"
    )))
}

fn write_invalid_leader_pane_env_warning(workspaces: &[&Path], pane_id_raw: &str) {
    let message = "invalid pane id format, skipping validation";
    let mut wrote = false;
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for workspace in workspaces {
        let key = workspace.to_string_lossy().to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        match crate::event_log::EventLog::new(workspace).write(
            "leader_pane_env.validation_warning",
            serde_json::json!({
                "env": "TEAM_AGENT_LEADER_PANE_ID",
                "value": pane_id_raw,
                "warning": message,
            }),
        ) {
            Ok(_) => wrote = true,
            Err(err) => errors.push(format!("{key}: {err}")),
        }
    }
    if !wrote {
        eprintln!("TEAM_AGENT_LEADER_PANE_ID={pane_id_raw}: {message}");
        if !errors.is_empty() {
            eprintln!(
                "TEAM_AGENT_LEADER_PANE_ID warning event write failed: {}",
                errors.join("; ")
            );
        }
    }
}

fn warn_ignored_owner_team_id(workspace: &Path, team_dir: &Path, runtime_team_key: &str) {
    let Ok(Some(ignored)) = crate::compiler::ignored_owner_team_id_from_team_md(team_dir) else {
        return;
    };
    eprintln!("Warning: ignored TEAM.md {}={}", ignored.field, ignored.value);
    eprintln!("Reason: owner identity is the canonical runtime team key ({runtime_team_key}), not TEAM.md front matter");
    eprintln!("Action: remove {} from TEAM.md", ignored.field);
    if let Err(err) = crate::event_log::EventLog::new(workspace).write(
        "spec.field_ignored",
        serde_json::json!({
            "field": ignored.field,
            "source": team_dir.join("TEAM.md").to_string_lossy().to_string(),
            "value": ignored.value,
            "warning": "ignored user-set owner_team_id",
            "reason": "owner identity is derived from the canonical runtime team key",
            "action": "remove owner_team_id from TEAM.md",
            "runtime_team_key": runtime_team_key,
        }),
    ) {
        eprintln!("Warning: spec.field_ignored event write failed: {err}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaderPaneEnvState {
    Live,
    Dead,
    Absent,
    Unknown,
}

fn leader_pane_env_state_for_validation(
    transport: &dyn Transport,
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    if !is_tmux_pane_id_format(pane) {
        return LeaderPaneEnvState::Unknown;
    }
    if transport.probes_real_tmux_socket_roots() {
        return active_leader_pane_state_across_tmux_sockets(pane);
    }
    active_leader_pane_state(transport, pane)
}

fn is_tmux_pane_id_format(pane: &crate::transport::PaneId) -> bool {
    let pane = pane.as_str();
    pane.len() > 1 && pane.starts_with('%') && pane[1..].chars().all(|ch| ch.is_ascii_digit())
}

fn active_leader_pane_state_across_tmux_sockets(
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    let endpoints = crate::tmux_backend::tmux_socket_endpoints();
    let transports = endpoints
        .iter()
        .map(|endpoint| crate::tmux_backend::TmuxBackend::for_tmux_endpoint(endpoint))
        .collect::<Vec<_>>();
    active_leader_pane_state_across_transports(
        transports.iter().map(|transport| transport as &dyn Transport),
        pane,
    )
}

pub(crate) fn active_leader_pane_state_across_transports<'a>(
    transports: impl IntoIterator<Item = &'a dyn Transport>,
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    let mut found_absent = false;
    let mut found_dead = false;
    for transport in transports {
        match active_leader_pane_state(transport, pane) {
            LeaderPaneEnvState::Live => return LeaderPaneEnvState::Live,
            LeaderPaneEnvState::Dead => found_dead = true,
            LeaderPaneEnvState::Absent => found_absent = true,
            LeaderPaneEnvState::Unknown => {}
        }
    }
    if found_dead {
        LeaderPaneEnvState::Dead
    } else if found_absent {
        LeaderPaneEnvState::Absent
    } else {
        LeaderPaneEnvState::Unknown
    }
}

fn active_leader_pane_state(
    transport: &dyn Transport,
    pane: &crate::transport::PaneId,
) -> LeaderPaneEnvState {
    match transport.has_pane(pane) {
        Ok(Some(true)) => return LeaderPaneEnvState::Live,
        Ok(Some(false)) => return LeaderPaneEnvState::Absent,
        Ok(None) | Err(_) => {}
    }
    match transport.liveness(pane) {
        Ok(crate::transport::PaneLiveness::Live) => LeaderPaneEnvState::Live,
        Ok(crate::transport::PaneLiveness::Dead) => LeaderPaneEnvState::Dead,
        Ok(crate::transport::PaneLiveness::Unknown) | Err(_) => LeaderPaneEnvState::Unknown,
    }
}

fn seed_unbound_launched_owner(launched: &mut serde_json::Value, launched_key: &str) {
    let Some(owner) = unbound_launched_owner(launched, launched_key) else {
        return;
    };
    let Some(provider) = owner
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .filter(|provider| !provider.is_empty())
    else {
        return;
    };
    let owner_epoch = 1u64;
    let receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "unbound",
        "provider": provider,
        "leader_session_uuid": owner.get("leader_session_uuid").cloned().unwrap_or(serde_json::Value::Null),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    if let Some(obj) = launched.as_object_mut() {
        obj.insert("leader_receiver".to_string(), receiver);
        obj.insert("team_owner".to_string(), owner);
        obj.insert("owner_epoch".to_string(), serde_json::json!(owner_epoch));
    }
}

fn unbound_launched_owner(
    launched: &serde_json::Value,
    launched_key: &str,
) -> Option<serde_json::Value> {
    let provider = unbound_launched_provider(launched)?;
    let machine_fingerprint = launched
        .get("team_owner")
        .and_then(|owner| owner.get("machine_fingerprint"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let workspace = launched
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let uuid = crate::model::ids::LeaderSessionUuid::derive(
        machine_fingerprint,
        workspace,
        &os_user,
        launched_key,
    )
    .ok()?;
    Some(serde_json::json!({
        "provider": provider,
        "machine_fingerprint": machine_fingerprint,
        "leader_session_uuid": uuid.as_str(),
        "owner_epoch": 1u64,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": os_user,
    }))
}

fn unbound_launched_provider(launched: &serde_json::Value) -> Option<String> {
    if let Some(provider) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("provider"))
        .and_then(serde_json::Value::as_str)
        .filter(|provider| !provider.is_empty())
        .and_then(parse_provider)
        .and_then(provider_wire_string)
    {
        return Some(provider);
    }
    let pane = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())?;
    let target = PaneId::new(pane);
    attributed_provider_for_pane_across_tmux_sockets(&target).and_then(provider_wire_string)
}

fn provider_wire_string(provider: Provider) -> Option<String> {
    serde_json::to_value(provider)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

fn attributed_provider_for_pane_across_tmux_sockets(pane: &PaneId) -> Option<Provider> {
    crate::tmux_backend::tmux_socket_endpoints()
        .into_iter()
        .filter_map(|endpoint| {
            crate::tmux_backend::TmuxBackend::for_tmux_endpoint(&endpoint)
                .list_targets()
                .ok()
        })
        .flatten()
        .find(|info| info.pane_id == *pane)
        .and_then(|info| crate::leader::attribute_pane_provider(&info))
}

fn caller_provider_for_seed_with_lookup(
    caller: &crate::state::owner_gate::CallerIdentity,
    lookup_pane_provider: impl Fn(&PaneId) -> Option<Provider>,
) -> Option<String> {
    if !caller.provider.is_empty() {
        if let Some(provider) = parse_provider(&caller.provider).and_then(provider_wire_string) {
            return Some(provider);
        }
    }
    (!caller.pane_id.is_empty())
        .then(|| PaneId::new(&caller.pane_id))
        .and_then(|pane| lookup_pane_provider(&pane))
        .and_then(provider_wire_string)
}

#[cfg(test)]
mod e22_unbound_owner_provider_tests {
    use super::*;
    use crate::state::owner_gate::CallerIdentity;

    #[test]
    fn unbound_owner_preserves_explicit_copilot_provider() {
        let mut launched = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "team_owner": {
                "provider": "copilot",
                "machine_fingerprint": "machine"
            }
        });

        seed_unbound_launched_owner(&mut launched, "team-e22");

        assert_eq!(
            launched
                .pointer("/team_owner/provider")
                .and_then(serde_json::Value::as_str),
            Some("copilot")
        );
        assert_eq!(
            launched
                .pointer("/leader_receiver/provider")
                .and_then(serde_json::Value::as_str),
            Some("copilot")
        );
    }

    #[test]
    fn unbound_owner_without_attributed_provider_does_not_default_codex() {
        let mut launched = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "team_owner": {
                "machine_fingerprint": "machine"
            }
        });

        seed_unbound_launched_owner(&mut launched, "team-e22");

        assert!(
            launched.get("leader_receiver").is_none(),
            "unattributed unbound owner must not seed a codex receiver: {launched}"
        );
        assert!(
            launched
                .pointer("/team_owner/provider")
                .and_then(serde_json::Value::as_str)
                != Some("codex"),
            "unattributed unbound owner must not silently become codex: {launched}"
        );
    }

    fn caller(provider: &str, pane_id: &str) -> CallerIdentity {
        CallerIdentity {
            pane_id: pane_id.to_string(),
            provider: provider.to_string(),
            machine_fingerprint: "machine".to_string(),
            leader_session_uuid: "leader-uuid".to_string(),
            leader_session_uuid_source: "derived".to_string(),
        }
    }

    #[test]
    fn env_seed_attributes_in_tmux_node_form_copilot_from_caller_pane() {
        let mut state = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "leader": {"provider": "copilot"},
        });

        assert!(seed_launched_owner_from_caller_with_provider_lookup(
            &mut state,
            caller("", "%0"),
            |pane| (pane.as_str() == "%0").then_some(Provider::Copilot),
        ));

        assert_eq!(
            state
                .pointer("/team_owner/provider")
                .and_then(serde_json::Value::as_str),
            Some("copilot")
        );
        assert_eq!(
            state
                .pointer("/leader_receiver/provider")
                .and_then(serde_json::Value::as_str),
            Some("copilot")
        );
    }

    #[test]
    fn env_seed_unknown_caller_pane_does_not_default_codex() {
        let mut state = serde_json::json!({
            "workspace": "/tmp/team-agent-e22",
            "leader": {"provider": "copilot"},
        });

        assert!(seed_launched_owner_from_caller_with_provider_lookup(
            &mut state,
            caller("", "%0"),
            |_| None,
        ));
        assert_eq!(
            state
                .pointer("/team_owner/pane_id")
                .and_then(serde_json::Value::as_str),
            Some("%0")
        );
        assert_eq!(
            state
                .pointer("/leader_receiver/pane_id")
                .and_then(serde_json::Value::as_str),
            Some("%0")
        );
        assert!(
            state
                .pointer("/leader_receiver/provider")
                .and_then(serde_json::Value::as_str)
                != Some("codex"),
            "unknown caller pane must not silently seed a codex receiver: {state}"
        );
        assert!(
            state
                .pointer("/team_owner/provider")
                .and_then(serde_json::Value::as_str)
                != Some("codex"),
            "unknown caller pane must not silently become codex: {state}"
        );
    }
}

fn owner_pane_belongs_to_other_team(
    existing: &serde_json::Value,
    launched_key: &str,
    pane: &str,
) -> bool {
    existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| {
            teams.iter().any(|(key, team)| {
                key != launched_key
                    && team
                        .get("team_owner")
                        .and_then(|owner| owner.get("pane_id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(pane)
            })
        })
}

fn running_agent_state(
    agent: &Value,
    id: &str,
    provider: Provider,
    workspace: &Path,
    spawn_cwd: &Path,
    spawned_at: &str,
    team_id: &str,
    pane_id: Option<&str>,
    pane_pid: Option<u32>,
    safety: &DangerousApproval,
    started_agent: Option<&StartedAgent>,
    profile_dir: Option<&Path>,
) -> Result<serde_json::Value, LifecycleError> {
    let model = agent.get("model").and_then(Value::as_str);
    let auth_mode = agent
        .get("auth_mode")
        .and_then(Value::as_str)
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription);
    let profile = agent
        .get("profile")
        .map(yaml_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let window = started_agent
        .and_then(|started| started.layout_window.as_ref())
        .map(WindowName::as_str)
        .or_else(|| agent.get("window").and_then(Value::as_str))
        .unwrap_or(id);
    let mcp_config = crate::provider::get_adapter(provider)
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let mcp_config = resolve_mcp_config(mcp_config, workspace, id, team_id);
    let mcp_config_path =
        write_worker_mcp_config_for_provider(workspace, id, &mcp_config, Some(provider))?;
    let mut state = serde_json::Map::new();
    state.insert("status".to_string(), serde_json::json!("running"));
    state.insert("provider".to_string(), serde_json::json!(provider));
    state.insert("agent_id".to_string(), serde_json::json!(id));
    state.insert(
        "model".to_string(),
        model.map_or(serde_json::Value::Null, |m| serde_json::json!(m)),
    );
    state.insert("auth_mode".to_string(), serde_json::json!(auth_mode));
    state.insert("profile".to_string(), profile);
    if agent.get("profile").is_some() {
        if let Some(profile_dir) = profile_dir {
            state.insert(
                "_profile_dir".to_string(),
                serde_json::json!(profile_dir.to_string_lossy().to_string()),
            );
        }
    }
    state.insert("window".to_string(), serde_json::json!(window));
    state.insert(
        "mcp_config".to_string(),
        serde_json::json!(mcp_config_path.to_string_lossy().to_string()),
    );
    state.insert(
        "permissions".to_string(),
        permissions_json(agent, id, provider)
            .map_err(|e| LifecycleError::Compile(e.to_string()))?,
    );
    persist_effective_approval_policy(&mut state, safety);
    state.insert("session_id".to_string(), serde_json::Value::Null);
    state.insert("rollout_path".to_string(), serde_json::Value::Null);
    state.insert("captured_at".to_string(), serde_json::Value::Null);
    state.insert("captured_via".to_string(), serde_json::Value::Null);
    state.insert(
        "attribution_confidence".to_string(),
        serde_json::Value::Null,
    );
    if let Some(started_agent) = started_agent {
        persist_started_agent_plan_state(&mut state, started_agent);
        if let Some(layout_window) = started_agent.layout_window.as_ref() {
            state.insert(
                "layout_window".to_string(),
                serde_json::json!(layout_window.as_str()),
            );
        }
        if let Some(layout_index) = started_agent.layout_index {
            state.insert("layout_index".to_string(), serde_json::json!(layout_index));
        }
        if let Some(pane_index) = started_agent.pane_index {
            state.insert("pane_index".to_string(), serde_json::json!(pane_index));
        }
        if !matches!(started_agent.display, WorkerDisplay::Blocked { .. }) {
            state.insert(
                "display".to_string(),
                serde_json::to_value(&started_agent.display)
                    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?,
            );
        }
    }
    state.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(spawn_cwd.to_string_lossy().to_string()),
    );
    state.insert("spawned_at".to_string(), serde_json::json!(spawned_at));
    if let Some(pane_id) = pane_id.filter(|pane| !pane.is_empty()) {
        state.insert("pane_id".to_string(), serde_json::json!(pane_id));
    }
    if let Some(pane_pid) = pane_pid {
        state.insert("pane_pid".to_string(), serde_json::json!(pane_pid));
    }
    Ok(serde_json::Value::Object(state))
}

pub(crate) fn effective_approval_policy(safety: &DangerousApproval) -> serde_json::Value {
    serde_json::json!({
        "enabled": safety.enabled,
        "source": dangerous_approval_source_str(safety.source),
        "inherited": safety.inherited,
        "explicit_yes_confirmed": safety.enabled && matches!(safety.source, DangerousApprovalSource::RuntimeConfig),
        "provider": safety.provider,
        "flag": safety.flag,
        "worker_capability_above_leader": safety.worker_capability_above_leader,
    })
}

pub(crate) fn persist_effective_approval_policy(
    agent_state: &mut serde_json::Map<String, serde_json::Value>,
    safety: &DangerousApproval,
) {
    agent_state.insert(
        "effective_approval_policy".to_string(),
        effective_approval_policy(safety),
    );
}

fn dangerous_approval_source_str(source: DangerousApprovalSource) -> &'static str {
    match source {
        DangerousApprovalSource::RuntimeConfig => "runtime_config",
        DangerousApprovalSource::LeaderProcess => "leader_process",
        DangerousApprovalSource::Disabled => "disabled",
    }
}

pub(crate) fn resolve_mcp_config(
    config: crate::provider::McpConfig,
    workspace: &Path,
    agent_id: &str,
    team_id: &str,
) -> crate::provider::McpConfig {
    crate::provider::McpConfig {
        raw: resolve_mcp_placeholders(config.raw, workspace, agent_id, team_id),
    }
}

fn resolve_mcp_placeholders(
    value: serde_json::Value,
    workspace: &Path,
    agent_id: &str,
    team_id: &str,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(
            s.replace("{workspace}", &workspace.to_string_lossy())
                .replace("{agent_id}", agent_id)
                .replace("{team_id}", team_id),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| resolve_mcp_placeholders(item, workspace, agent_id, team_id))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        key,
                        resolve_mcp_placeholders(value, workspace, agent_id, team_id),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

pub(crate) fn write_worker_mcp_config(
    workspace: &Path,
    agent_id: &str,
    config: &crate::provider::McpConfig,
) -> Result<PathBuf, LifecycleError> {
    write_worker_mcp_config_for_provider(workspace, agent_id, config, None)
}

/// C-3-4 cr verdict v2 — Copilot 的 mcp config schema 字段名是 `transport`
/// (实测 cmd-mcp-add 原文取值 stdio|http|sse),不是 canonical 的 `type`。当
/// provider==Copilot 时写出文件前先做 type→transport 翻译;其它 provider 不动。
/// 文件路径同 canonical `<ws>/.team/runtime/mcp/<agent_id>.json`,因为 launch
/// 路径会用 `--additional-mcp-config @<file>` 直指它。
pub(crate) fn write_worker_mcp_config_for_provider(
    workspace: &Path,
    agent_id: &str,
    config: &crate::provider::McpConfig,
    provider: Option<Provider>,
) -> Result<PathBuf, LifecycleError> {
    let path = workspace
        .join(".team/runtime/mcp")
        .join(format!("{agent_id}.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let raw = if matches!(provider, Some(Provider::Copilot)) {
        copilot_translate_mcp_servers(&config.raw)
    } else {
        config.raw.clone()
    };
    let body = serde_json::to_string_pretty(&serde_json::json!({"mcpServers": raw}))
        .map_err(|e| LifecycleError::StatePersist(format!("serialize mcp config: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// C-3-4 cr verdict v2 — McpConfig.raw 是 `{name: {type, command, args, env}}` 形;
/// copilot mcp add schema 取 `transport` 替 `type`(stdio|http|sse 同值)。仅
/// 字段名变换,其余字段全保留。
fn copilot_translate_mcp_servers(raw: &serde_json::Value) -> serde_json::Value {
    let Some(servers) = raw.as_object() else {
        return raw.clone();
    };
    let mut translated = serde_json::Map::new();
    for (name, server) in servers {
        let Some(obj) = server.as_object() else {
            translated.insert(name.clone(), server.clone());
            continue;
        };
        let mut out = serde_json::Map::new();
        for (key, value) in obj {
            if key == "type" {
                out.insert("transport".to_string(), value.clone());
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
        translated.insert(name.clone(), serde_json::Value::Object(out));
    }
    serde_json::Value::Object(translated)
}

pub(crate) fn point_native_mcp_config_at_file(
    argv: &mut [String],
    provider: Provider,
    path: &Path,
) {
    match provider {
        Provider::Claude | Provider::ClaudeCode => {
            let Some(index) = argv.iter().position(|arg| arg == "--mcp-config") else {
                return;
            };
            if let Some(value) = argv.get_mut(index.saturating_add(1)) {
                *value = path.to_string_lossy().to_string();
            }
        }
        // §C1 note: copilot `--additional-mcp-config` 接受 `@file`,直接指向既有
        // `.team/runtime/mcp/<agent>.json`(launch 路径 write_worker_mcp_config 已写)。
        // 既避免 inline JSON 包 mcpServers wrapper 的语义错位,也更利于 ps 验法。
        Provider::Copilot => {
            let Some(index) = argv.iter().position(|arg| arg == "--additional-mcp-config") else {
                return;
            };
            if let Some(value) = argv.get_mut(index.saturating_add(1)) {
                *value = format!("@{}", path.to_string_lossy());
            }
        }
        _ => {}
    }
}

fn permissions_json(
    agent: &Value,
    id: &str,
    provider: Provider,
) -> Result<serde_json::Value, crate::model::ModelError> {
    let tools = agent.get("tools").and_then(Value::as_list).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let resolved = permissions::resolve_permissions(&AgentPermissionInput {
        id: Some(AgentId::new(id)),
        provider,
        role: agent
            .get("role")
            .and_then(Value::as_str)
            .map(str::to_string),
        tools,
    })?;
    let mut out = serde_json::Map::new();
    out.insert("agent_id".to_string(), serde_json::json!(id));
    out.insert("provider".to_string(), serde_json::json!(provider));
    out.insert(
        "tools".to_string(),
        serde_json::json!(resolved.sorted_tool_strings()),
    );
    out.insert(
        "resolved_tools".to_string(),
        serde_json::Value::Array(
            resolved
                .resolved_tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "tool": tool.tool,
                        "enforcement": tool.enforcement,
                    })
                })
                .collect(),
        ),
    );
    out.insert(
        "has_prompt_only".to_string(),
        serde_json::json!(resolved.has_prompt_only),
    );
    Ok(serde_json::Value::Object(out))
}

fn agent_is_paused(agent: &Value) -> bool {
    matches!(agent.get("paused"), Some(Value::Bool(true)))
}

fn spawn_timestamp() -> String {
    match std::env::var("TEAM_AGENT_TEST_FIXED_SPAWNED_AT") {
        Ok(value) => value,
        Err(_) => chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
            .to_string(),
    }
}

fn spawn_timestamp_for_agent(offset_micros: u32) -> String {
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
    crate::layout::worker_env::worker_spawn_env(
        std::env::vars(),
        workspace,
        agent_id,
        team_id,
    )
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
        env.insert("TEAM_AGENT_LEADER_BYPASS_SOURCE".to_string(), "leader_process".to_string());
        if let Some(provider) = safety.provider.as_deref() {
            env.insert("TEAM_AGENT_LEADER_BYPASS_PROVIDER".to_string(), provider.to_string());
        }
        if let Some(flag) = safety.flag.as_deref() {
            env.insert("TEAM_AGENT_LEADER_BYPASS_FLAG".to_string(), flag.to_string());
        }
        env.insert("TEAM_AGENT_MCP_AUTO_APPROVE".to_string(), "team_orchestrator".to_string());
        env.insert("TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE".to_string(), "leader_bypass".to_string());
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
    env.insert("COPILOT_DISABLE_TERMINAL_TITLE".to_string(), "1".to_string());
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
fn apply_copilot_mcp_residual_disables(
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
                format!("copilot mcp list exit={:?} stderr={stderr}\n", out.status.code()),
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
fn parse_copilot_mcp_list_server_names(text: &str) -> Vec<String> {
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

fn persist_started_agent_plan_state(
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

fn is_posix_shell_identifier(name: &str) -> bool {
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

fn spec_team_id(spec: &Value) -> Option<String> {
    spec.get("team")
        .and_then(|v| v.get("id").or_else(|| v.get("name")))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.get("name").and_then(Value::as_str).map(str::to_string))
}

fn explicit_active_team_key(state: &serde_json::Value) -> Option<String> {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|team| !team.is_empty())
        .map(str::to_string)
}

fn runtime_team_key_for_spec(spec_path: &Path, spec: &Value, session_name: &SessionName) -> String {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let state = serde_json::json!({
        "team_dir": team_dir.to_string_lossy(),
        "spec_path": spec_path.to_string_lossy(),
        "session_name": session_name.as_str(),
        "team": spec.get("team").map(yaml_value_to_json).unwrap_or(serde_json::Value::Null),
    });
    crate::state::projection::team_state_key(&state)
}

fn transport_has_session(transport: &dyn Transport, session_name: &SessionName) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.has_session(session_name)
    })) {
        Ok(Ok(live)) => live,
        Ok(Err(_)) | Err(_) => false,
    }
}

fn spec_display_backend(spec: &Value) -> DisplayBackend {
    let requested = spec
        .get("runtime")
        .and_then(|runtime| runtime.get("display_backend"))
        .and_then(Value::as_str)
        .and_then(|backend| {
            serde_json::from_value::<DisplayBackend>(serde_json::json!(backend)).ok()
        });
    crate::lifecycle::display::resolve_display_backend(requested, None).backend
}

fn parse_provider(raw: &str) -> Option<Provider> {
    match raw {
        "claude" => Some(Provider::Claude),
        "claude_code" => Some(Provider::ClaudeCode),
        "codex" => Some(Provider::Codex),
        "copilot" => Some(Provider::Copilot),
        "gemini_cli" => Some(Provider::GeminiCli),
        "fake" => Some(Provider::Fake),
        _ => None,
    }
}

fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}

fn quick_start_requested_team_key<'a>(
    team_id: Option<&'a str>,
    name: Option<&'a str>,
) -> Option<&'a str> {
    team_id.or(name).filter(|team| !team.is_empty())
}

struct QuickStartDepth {
    parent_team_key: Option<String>,
    team_depth: u64,
}

fn quick_start_depth_guard(
    workspace: &Path,
    _agents_dir: &Path,
    requested_team: Option<&str>,
    _strict_real_runtime: bool,
) -> Result<QuickStartDepth, LifecycleError> {
    let env_parent = std::env::var("TEAM_AGENT_OWNER_TEAM_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let parent = env_parent;
    let Some(parent) = parent else {
        let state = crate::state::persist::load_runtime_state(workspace)
            .unwrap_or_else(|_| serde_json::json!({}));
        let ambiguous_nested_intent = requested_team.is_some_and(|team| {
            looks_ambiguous_child_team_key(team) || looks_grandchild_team_key(team)
        });
        if has_live_runtime_teams(&state) && ambiguous_nested_intent {
            if requested_team.is_some_and(looks_grandchild_team_key) {
                if let Some(parent_key) = infer_parent_team_from_active_state(&state) {
                    let parent_state =
                        crate::state::projection::project_top_level_view(&state, &parent_key);
                    let parent_depth = parent_state
                        .get("team_depth")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1);
                    return Ok(QuickStartDepth {
                        parent_team_key: Some(parent_key),
                        team_depth: parent_depth.saturating_add(1),
                    });
                }
            }
            return Err(LifecycleError::RequirementUnmet(
                "cannot infer parent team for nested quick-start; pass an explicit worker/subleader owner context"
                    .to_string(),
            ));
        }
        return Ok(QuickStartDepth {
            parent_team_key: None,
            team_depth: 1,
        });
    };
    let state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    let parent_key = crate::state::projection::resolve_owner_team_id(&state, &parent)
        .canonical_key()
        .map(str::to_string)
        .unwrap_or(parent);
    let parent_state = crate::state::projection::project_top_level_view(&state, &parent_key);
    let parent_depth = parent_state
        .get("team_depth")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let team_depth = parent_depth.saturating_add(1);
    Ok(QuickStartDepth {
        parent_team_key: Some(parent_key),
        team_depth,
    })
}

fn infer_parent_team_from_active_state(state: &serde_json::Value) -> Option<String> {
    let active = explicit_active_team_key(state)?;
    let team = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(&active))?;
    team_has_running_agent(team).then_some(active)
}

fn has_live_runtime_teams(state: &serde_json::Value) -> bool {
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.values().any(team_has_running_agent))
}

fn team_has_running_agent(team: &serde_json::Value) -> bool {
    team.get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                agent.get("status").and_then(serde_json::Value::as_str) == Some("running")
            })
        })
}

fn looks_ambiguous_child_team_key(team: &str) -> bool {
    let team = team.trim().to_ascii_lowercase();
    team != "child"
        && (team.starts_with("child-")
            || team.starts_with("child_")
            || team.starts_with("child.")
            || team.starts_with("child"))
}

fn looks_grandchild_team_key(team: &str) -> bool {
    let team = team.trim().to_ascii_lowercase();
    team == "grandchild"
        || team.starts_with("grandchild-")
        || team.starts_with("grandchild_")
        || team.starts_with("grandchild.")
        || team.starts_with("grandchild")
}

fn annotate_team_depth(
    state: &mut serde_json::Value,
    parent_team_key: Option<&str>,
    team_depth: u64,
) {
    let Some(obj) = state.as_object_mut() else {
        return;
    };
    obj.insert("team_depth".to_string(), serde_json::json!(team_depth));
    if let Some(parent) = parent_team_key.filter(|value| !value.is_empty()) {
        obj.insert("parent_team_key".to_string(), serde_json::json!(parent));
    }
}

fn annotate_persisted_team_depth(
    workspace: &Path,
    team_key: &str,
    parent_team_key: Option<&str>,
    team_depth: u64,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let Some(team) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team_key))
    else {
        return Ok(());
    };
    annotate_team_depth(team, parent_team_key, team_depth);
    crate::state::persist::save_runtime_state(workspace, &state)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

fn runtime_state_has_quick_start_team(state: &serde_json::Value, team: &str) -> bool {
    explicit_active_team_key(state).as_deref() == Some(team)
        || state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|teams| {
                teams.contains_key(team)
                    || teams
                        .values()
                        .any(|entry| json_team_identity_matches(entry, team))
            })
        || crate::state::projection::team_state_key(state) == team
        || json_team_identity_matches(state, team)
        || state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| session == team || session.strip_prefix("team-") == Some(team))
}

fn json_team_identity_matches(state: &serde_json::Value, team: &str) -> bool {
    state
        .get("team")
        .and_then(|value| value.get("id").or_else(|| value.get("name")))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value == team)
        || state
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == team)
}

/// `quick_start(agents_dir, name, yes, fresh, team_id)`(`diagnose/quick_start.py:18`)。
/// 面向用户的零配置入口:编译 team_dir → `launch` → autobind leader receiver → 起
/// coordinator → `wait_ready` 轮询就绪。归入 lifecycle module(不与 diagnose 混)。
pub fn quick_start(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_in_workspace(&workspace, agents_dir, name, yes, fresh, team_id)
}

pub fn quick_start_in_workspace(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = explicit_quick_start_workspace(workspace);
    let transport = quick_start_tmux_backend(&workspace);
    quick_start_with_transport_in_workspace(
        &workspace,
        agents_dir,
        name,
        yes,
        fresh,
        team_id,
        &transport,
    )
}

pub fn quick_start_in_workspace_with_display(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
    open_display: bool,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = explicit_quick_start_workspace(workspace);
    let transport = quick_start_tmux_backend(&workspace);
    quick_start_with_transport_in_workspace_with_display(
        &workspace,
        agents_dir,
        name,
        yes,
        fresh,
        team_id,
        &transport,
        open_display,
    )
}

pub(crate) fn quick_start_tmux_backend(workspace: &Path) -> crate::tmux_backend::TmuxBackend {
    if let Some(endpoint) = crate::tmux_backend::socket_name_from_tmux_env() {
        crate::tmux_backend::TmuxBackend::for_tmux_endpoint(&endpoint)
    } else {
        crate::tmux_backend::TmuxBackend::for_workspace(workspace)
    }
}

pub(crate) fn selected_tmux_socket_source(
    transport: &dyn Transport,
    workspace: &Path,
) -> Option<&'static str> {
    let endpoint = transport.tmux_endpoint()?;
    if crate::tmux_backend::socket_name_from_tmux_env().as_deref() == Some(endpoint.as_str()) {
        Some("leader_env")
    } else if endpoint == crate::tmux_backend::socket_name_for_workspace(workspace) {
        Some("workspace")
    } else {
        None
    }
}

pub(crate) fn configure_adaptive_pane_title(
    workspace: &Path,
    transport: &dyn Transport,
    session_name: &SessionName,
    window: &WindowName,
    pane: &PaneId,
    agent_id: &str,
) {
    if let Err(error) =
        transport.configure_adaptive_pane_title(session_name, window, pane, agent_id)
    {
        let message = format!("adaptive layout pane title failed for {agent_id}: {error}");
        eprintln!("Warning: {message}");
        if let Err(event_error) = crate::event_log::EventLog::new(workspace).write(
            "adaptive_layout.pane_title_failed",
            serde_json::json!({
                "agent_id": agent_id,
                "session": session_name.as_str(),
                "window": window.as_str(),
                "pane_id": pane.as_str(),
                "warning": message,
            }),
        ) {
            eprintln!("Warning: adaptive_layout.pane_title_failed event write failed: {event_error}");
        }
    }
}

fn explicit_quick_start_workspace(workspace: &Path) -> PathBuf {
    std::fs::canonicalize(workspace).unwrap_or_else(|_| {
        if workspace.is_absolute() {
            workspace.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(workspace)
        }
    })
}

/// `quick_start` with an injected transport — tests inject a recording mock so the REAL spawn path
/// (launch dry_run=false → spawn_agents) is asserted without a live tmux; prod uses the real TmuxBackend.
pub fn quick_start_with_transport(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    let workspace = team_workspace(agents_dir);
    quick_start_with_transport_in_workspace(
        &workspace, agents_dir, name, yes, fresh, team_id, transport,
    )
}

pub fn quick_start_with_transport_in_workspace(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    quick_start_with_transport_in_workspace_with_display(
        workspace, agents_dir, name, yes, fresh, team_id, transport, true,
    )
}

pub fn quick_start_with_transport_in_workspace_with_display(
    workspace: &Path,
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
    open_display: bool,
) -> Result<QuickStartReport, LifecycleError> {
    // B-7 / 036b N38 三行 fail-fast — TEAM_AGENT_LEADER_PANE_ID 主动路径在 quick-start
    // 入口验活;死/缺(Dead)的 pane 必须明确报错,不可 silent bind 到 spawner /
    // owner_bind / lease / display 任一消费点。被动路径(display/seed 等)各自走
    // 降级+event,不在这里挡。错误三行式:error(含 pane id 字面)/action(unset
    // 或修 env)/log(env var 名)。
    let team_workspace = team_workspace(agents_dir);
    let warning_workspaces = [workspace, team_workspace.as_path()];
    validate_active_leader_pane_env_with_workspaces(transport, &warning_workspaces)?;
    if !agents_dir.exists() {
        return Err(LifecycleError::Compile(format!(
            "agents dir not found: {}",
            agents_dir.display()
        )));
    }
    let workspace = workspace.to_path_buf();
    let mut spec = crate::compiler::compile_team(agents_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    override_spec_workspace(&mut spec, &workspace);
    if !open_display {
        override_spec_display_backend(&mut spec, "none");
    }
    let requested_team = quick_start_requested_team_key(team_id, name)
        .map(str::to_string)
        .or_else(|| spec_team_id(&spec));
    let explicit_team_key = quick_start_requested_team_key(team_id, name).map(str::to_string);
    let team_depth = quick_start_depth_guard(
        &workspace,
        agents_dir,
        requested_team.as_deref(),
        matches!(transport.kind(), crate::transport::BackendKind::Tmux),
    )?;
    if team_depth.team_depth > 2 {
        let parent = team_depth.parent_team_key.as_deref().unwrap_or("");
        return Err(LifecycleError::RequirementUnmet(format!(
            "team nesting depth limit exceeded: parent_team_key={parent} parent_depth={} max_depth=2",
            team_depth.team_depth.saturating_sub(1)
        )));
    }
    if !fresh {
        let state_path = crate::state::persist::runtime_state_path(&workspace);
        if state_path.exists() {
            let state = crate::state::persist::load_runtime_state(&workspace)
                .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
            if requested_team
                .as_deref()
                .is_none_or(|team| runtime_state_has_quick_start_team(&state, team))
            {
                let session_name = state
                    .get("session_name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(SessionName::new);
                let attach_commands = session_name
                    .as_ref()
                    .map(|session| {
                        let windows = quick_start_attach_window_names(&state);
                        attach_commands_for_runtime_windows(
                            state
                                .get("tmux_endpoint")
                                .and_then(serde_json::Value::as_str)
                                .or_else(|| {
                                    state.get("tmux_socket").and_then(serde_json::Value::as_str)
                                }),
                            &workspace,
                            session,
                            windows.iter().map(String::as_str),
                        )
                    })
                    .unwrap_or_default();
                let mut next_actions = vec![
                    "run restart to resume the existing team or pass --fresh to replace it"
                        .to_string(),
                ];
                if session_name.is_some() {
                    if crate::tmux_backend::socket_probe_missing_for_workspace(&workspace) {
                        next_actions.push(crate::tmux_backend::socket_missing_hint_for_workspace(
                            &workspace,
                        ));
                    }
                    next_actions.extend(attach_commands.iter().cloned());
                }
                return Ok(QuickStartReport::ExistingRuntime {
                    team: requested_team.clone(),
                    session_name,
                    state_path: Some(state_path),
                    next_actions,
                    attach_commands,
                });
            }
        }
    }
    // CR-040/042: repeated quick-start from one template with distinct --team-id/--name
    // must NOT collide on the template-derived tmux session. Override the compiled
    // spec's runtime.session_name with one derived from the REQUESTED team identity
    // so launch_with_transport (which reads runtime.session_name) spawns into an
    // isolated session per requested team.
    if let Some(requested) = requested_team.as_deref() {
        override_spec_session_name(&mut spec, &format!("team-{requested}"));
    }
    let session_name = spec_session_name(&spec);
    // team_key 身份源 = team_dir(agents_dir).name(角色定义目录),不依赖 spec 落点。
    let state_team_key = explicit_team_key.clone().unwrap_or_else(|| {
        runtime_team_key_for_spec(&agents_dir.join("team.spec.yaml"), &spec, &session_name)
    });
    warn_ignored_owner_team_id(workspace.as_path(), agents_dir, &state_team_key);
    // E5 spec 迁移:spec 写到 .team/runtime/<team_key>/(中间产物,绝不落用户目录 agents_dir)。
    // Bug2:原子写(tmp+rename),避免半截 spec。
    let spec_path = crate::model::paths::runtime_spec_path(&workspace, &state_team_key);
    write_spec_atomic(&spec_path, &spec)?;
    let _store = crate::message_store::MessageStore::open(&workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let resolved_spec_path =
        std::fs::canonicalize(&spec_path).unwrap_or_else(|_| spec_path.clone());
    let mut state = initial_runtime_state(&spec, &resolved_spec_path, &workspace, agents_dir);
    annotate_runtime_tmux_endpoint(&mut state, transport, &workspace);
    save_launched_team_state_for_key(&workspace, &state, Some(&state_team_key))?;
    annotate_persisted_team_depth(
        &workspace,
        &state_team_key,
        team_depth.parent_team_key.as_deref(),
        team_depth.team_depth,
    )?;
    // FIX (rt-host-a real-machine finding): dry_run=false so launch_with_transport calls spawn_agents
    // and really creates the tmux session + worker windows (was hardcoded true → never spawned, which
    // also starved the coordinator: no session → first tick TmuxSessionMissing → run_daemon loop exits).
    let mut launch =
        launch_with_transport_in_workspace(&workspace, &spec_path, false, yes, true, transport)?;
    annotate_persisted_team_depth(
        &workspace,
        &state_team_key,
        team_depth.parent_team_key.as_deref(),
        team_depth.team_depth,
    )?;
    launch.leader_receiver_attached =
        launched_team_receiver_is_attached(&workspace, &state_team_key);
    launch.session_capture_incomplete_agents =
        quick_start_session_capture_incomplete_agents(&workspace, &state_team_key);
    let coordinator_workspace = crate::coordinator::WorkspacePath::new(workspace.clone());
    let coordinator_started = crate::coordinator::start_coordinator(&coordinator_workspace)
        .map(|report| report.ok)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let coordinator_action = if coordinator_started {
        "coordinator started"
    } else {
        "coordinator not started"
    };
    // BUG-7: build an honest readiness verdict from the post-spawn runtime state.
    // - If persist_spawn_agent_state (BUG-2 fix) marked any agent non-running, the
    //   team is observably Degraded.
    // - Otherwise the framework cannot itself verify that the worker's MCP tool set
    //   loaded successfully (provider-side codex/claude schema rejections happen
    //   asynchronously after spawn), so the verdict is PendingToolLoad — never
    //   bare Ready.
    let worker_readiness = quick_start_worker_readiness(&workspace, &state_team_key);
    let attach_windows = load_runtime_state(&workspace)
        .ok()
        .map(|state| {
            attach_window_names_with_managed_leader(&state, started_attach_window_names(&launch.started))
        })
        .unwrap_or_else(|| started_attach_window_names(&launch.started));
    let attach_commands = attach_commands_for_runtime_windows(
        launch.tmux_endpoint.as_deref(),
        &workspace,
        &session_name,
        attach_windows.iter().map(String::as_str),
    );
    let mut next_actions = vec![format!(
        "team compiled; real spawn is behind the transport/provider boundary; {coordinator_action}"
    )];
    if crate::tmux_backend::socket_probe_missing_for_workspace(&workspace) {
        next_actions.push(crate::tmux_backend::socket_missing_hint_for_workspace(&workspace));
    }
    next_actions.extend(attach_commands.iter().cloned());
    let display_backend = state
        .get("display_backend")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none")
        .to_string();
    Ok(QuickStartReport::Ready {
        session_name,
        launch: Box::new(launch),
        next_actions,
        attach_commands,
        display_backend,
        worker_readiness,
    })
}

/// Annotate `state.tmux_endpoint` / `state.tmux_socket` (and `tmux_socket_source`)
/// from the active transport. Originally only called at `launch_with_transport`
/// init time; **`0.3.24` opened this to `pub(crate)` so restart/add/fork can keep
/// the persisted endpoint synchronized with the transport they actually used,
/// closing the silent socket-drift gap** (single state-save path; no parallel
/// "annotate after spawn" race with coordinator).
pub(crate) fn annotate_runtime_tmux_endpoint(state: &mut serde_json::Value, transport: &dyn Transport, workspace: &Path) {
    let Some(endpoint) = transport.tmux_endpoint() else {
        return;
    };
    let endpoint_for_state = if Path::new(&endpoint).is_absolute() || endpoint == "default" {
        endpoint.clone()
    } else if endpoint == crate::tmux_backend::socket_name_for_workspace(workspace) {
        crate::tmux_backend::socket_path_for_workspace(workspace)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| endpoint.clone())
    } else {
        crate::tmux_backend::socket_path_for_name(&endpoint)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| endpoint.clone())
    };
    if let Some(obj) = state.as_object_mut() {
        obj.insert("tmux_endpoint".to_string(), serde_json::json!(endpoint_for_state));
        obj.insert(
            "tmux_socket".to_string(),
            obj.get("tmux_endpoint")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        if let Some(source) = selected_tmux_socket_source(transport, workspace) {
            obj.insert("tmux_socket_source".to_string(), serde_json::json!(source));
        }
    }
}

fn attach_commands_for_runtime_windows<'a>(
    endpoint: Option<&str>,
    workspace: &Path,
    session_name: &SessionName,
    window_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let windows = window_names.into_iter().collect::<Vec<_>>();
    let attach = if let Some(endpoint) = endpoint.filter(|endpoint| !endpoint.is_empty()) {
        if Path::new(endpoint).is_absolute() {
            windows
                .iter()
                .map(|window_name| {
                    format!(
                        "tmux -S {} attach -t {}:{}",
                        endpoint,
                        session_name.as_str(),
                        window_name
                    )
                })
                .collect::<Vec<_>>()
        } else {
            crate::tmux_backend::attach_commands_for_windows(
                workspace,
                session_name,
                windows.iter().copied(),
            )
        }
    } else {
        crate::tmux_backend::attach_commands_for_windows(
            workspace,
            session_name,
            windows.iter().copied(),
        )
    };
    attach
}

fn started_attach_window_names(started: &[StartedAgent]) -> Vec<String> {
    let mut windows = started
        .iter()
        .map(|started| {
            started
                .layout_window
                .as_ref()
                .map(|window| window.as_str().to_string())
                .unwrap_or_else(|| started.agent_id.as_str().to_string())
        })
        .collect::<Vec<_>>();
    windows.sort();
    windows.dedup();
    windows
}

pub(crate) fn attach_window_names_for_state_agents<'a>(
    state: &serde_json::Value,
    agent_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let windows = agent_ids
        .into_iter()
        .map(|agent_id| {
            state
                .get("agents")
                .and_then(serde_json::Value::as_object)
                .and_then(|agents| agents.get(agent_id))
                .and_then(|agent| {
                    agent
                        .get("layout_window")
                        .or_else(|| agent.get("window"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|window| !window.is_empty())
                })
                .unwrap_or(agent_id)
                .to_string()
        })
        .collect::<Vec<_>>();
    attach_window_names_with_managed_leader(state, windows)
}

fn quick_start_attach_window_names(state: &serde_json::Value) -> Vec<String> {
    let windows = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|(agent_id, agent)| {
                    agent
                        .get("window")
                        .and_then(serde_json::Value::as_str)
                        .filter(|window| !window.is_empty())
                        .map(str::to_string)
                        .or_else(|| Some(agent_id.clone()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    attach_window_names_with_managed_leader(state, windows)
}

fn attach_window_names_with_managed_leader(
    state: &serde_json::Value,
    mut windows: Vec<String>,
) -> Vec<String> {
    if state_uses_managed_leader(state) {
        windows.push("leader".to_string());
    }
    windows.sort();
    windows.dedup();
    windows
}

fn state_uses_managed_leader(state: &serde_json::Value) -> bool {
    crate::state::projection::state_is_managed_leader(state)
}

/// BUG-7 helper: derive a [`QuickStartReadiness`] verdict from the just-written
/// runtime state. Reads `agents[*].status`; any non-`running` agent flips the
/// verdict to `Degraded { unhealthy_agents }` (sorted, deduped); otherwise
/// `PendingToolLoad` — never bare Ready. State read failure is treated as
/// PendingToolLoad rather than fabricated success.
fn quick_start_worker_readiness(workspace: &Path, team_key: &str) -> QuickStartReadiness {
    let Ok(state) = load_runtime_state(workspace) else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    let Some(agents) = team_state
        .get("agents")
        .and_then(serde_json::Value::as_object)
    else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let all_spawned = !agents.is_empty();
    let leader_receiver_attached = launched_team_receiver_is_attached(workspace, team_key);
    let all_attached_receiver = leader_receiver_attached;
    let mut unhealthy: Vec<String> = agents
        .iter()
        .filter_map(|(id, agent)| {
            let status = agent.get("status").and_then(serde_json::Value::as_str);
            match status {
                Some("running") => None,
                _ => Some(id.clone()),
            }
        })
        .collect();
    if !unhealthy.is_empty() {
        unhealthy.sort();
        unhealthy.dedup();
        QuickStartReadiness::Degraded {
            unhealthy_agents: unhealthy,
        }
    } else {
        let incomplete_agents =
            crate::session_capture::incomplete_interacted_resumable_agent_ids(team_state);
        let all_resumable_have_session = incomplete_agents.is_empty();
        let _readiness_ready = all_spawned && all_attached_receiver && all_resumable_have_session;
        QuickStartReadiness::PendingToolLoad
    }
}

fn quick_start_session_capture_incomplete_agents(workspace: &Path, team_key: &str) -> Vec<String> {
    let Ok(state) = load_runtime_state(workspace) else {
        return Vec::new();
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    crate::session_capture::incomplete_interacted_resumable_agent_ids(team_state)
}

pub(crate) fn launched_team_receiver_is_attached(workspace: &Path, team_key: &str) -> bool {
    let Ok(state) = load_runtime_state(workspace) else {
        return true;
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    if team_state.get("leader_receiver").is_none() {
        return crate::state::projection::state_is_external_leader(team_state);
    }
    if team_uses_fake_model_harness(team_state) {
        return true;
    }
    leader_receiver_is_attached(team_state)
}

fn team_uses_fake_model_harness(team_state: &serde_json::Value) -> bool {
    team_state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            !agents.is_empty()
                && agents.values().all(|agent| {
                    agent.get("model").and_then(serde_json::Value::as_str) == Some("fake")
                })
        })
}

fn leader_receiver_is_attached(team_state: &serde_json::Value) -> bool {
    let Some(receiver) = team_state.get("leader_receiver") else {
        return false;
    };
    let status = receiver
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let pane_id = receiver
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| receiver.get("pane").and_then(serde_json::Value::as_str))
        .unwrap_or("");
    status == "attached" && !pane_id.is_empty() && pane_id != "__team_agent_unbound__"
}

/// `detect_inherited_dangerous_permissions`(`launch/config.py`):扫进程祖先链找
/// `--dangerously-*` flag,产出危险审批继承态。launch 在 inherited=false 且无 --yes 时拒。
pub fn detect_dangerous_approval() -> Result<DangerousApproval, LifecycleError> {
    if let Ok(raw) = std::env::var("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON") {
        let argv_tokens = serde_json::from_str::<Vec<String>>(&raw).map_err(|e| {
            LifecycleError::StatePersist(format!("invalid test ancestry argv: {e}"))
        })?;
        return Ok(detect_dangerous_approval_in_argv(&argv_tokens)
            .unwrap_or_else(disabled_dangerous_approval));
    }
    for argv_tokens in process_ancestry_argv(std::process::id()) {
        if let Some(detected) = detect_dangerous_approval_in_argv(&argv_tokens) {
            return Ok(detected);
        }
    }
    Ok(disabled_dangerous_approval())
}

fn detect_dangerous_approval_in_argv(argv_tokens: &[String]) -> Option<DangerousApproval> {
    let argv0 = argv_tokens.first().map(String::as_str).unwrap_or("");
    let ancestry_binary_name = binary_name(argv0);
    for token in argv_tokens {
        for (provider, flag) in dangerous_leader_flags() {
            if token == flag {
                let unexpected_binary =
                    !binary_matches_provider(provider, ancestry_binary_name.as_deref());
                return Some(DangerousApproval {
                    enabled: true,
                    source: DangerousApprovalSource::LeaderProcess,
                    inherited: true,
                    provider: Some((*provider).to_string()),
                    flag: Some((*flag).to_string()),
                    worker_capability_above_leader: false,
                    ancestry_binary_name,
                    unexpected_binary,
                });
            }
        }
    }
    None
}

fn dangerous_leader_flags() -> &'static [(&'static str, &'static str)] {
    &[
        ("claude", "--dangerously-skip-permissions"),
        ("claude", "--dangerously-skip-permission"),
        ("codex", "--dangerously-bypass-approvals-and-sandbox"),
        // 0.3.27 P1 (E54 symptom 2): copilot's full-permission flags. Without
        // these entries, a copilot leader running with --allow-all / --yolo would
        // NOT propagate bypass to claude/codex workers because
        // detect_dangerous_approval scans the coordinator's ancestor argv and
        // only matches entries in THIS table.
        ("copilot", "--allow-all"),
        ("copilot", "--yolo"),
    ]
}

fn binary_matches_provider(provider: &str, binary: Option<&str>) -> bool {
    match (provider, binary) {
        ("codex", Some("codex")) => true,
        ("claude", Some("claude" | "claude-code" | "claude_code")) => true,
        // 0.3.27 P1 (E54): copilot binary detection for dangerous-approval scan.
        ("copilot", Some("copilot" | "github-copilot" | "gh-copilot")) => true,
        _ => false,
    }
}

fn binary_name(argv0: &str) -> Option<String> {
    Path::new(argv0)
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn process_ancestry_argv(pid: u32) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut current = pid;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..12 {
        if current == 0 || !seen.insert(current) {
            break;
        }
        if let Some(argv_tokens) = process_argv_tokens(current) {
            out.push(argv_tokens);
        }
        let Some(parent) = process_parent_pid(current) else {
            break;
        };
        if parent <= 1 || parent == current {
            break;
        }
        current = parent;
    }
    out
}

#[cfg(target_os = "linux")]
fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv_tokens = String::from_utf8_lossy(&bytes)
        .split('\0')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

#[cfg(target_os = "macos")]
fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    use std::mem::size_of;

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        i32::try_from(pid).ok()?,
    ];
    let mut size = 0usize;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size <= size_of::<libc::c_int>() {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size <= size_of::<libc::c_int>() {
        return None;
    }
    let argc = i32::from_ne_bytes(buf.get(..size_of::<libc::c_int>())?.try_into().ok()?) as usize;
    let mut offset = size_of::<libc::c_int>();
    while offset < size && buf[offset] != 0 {
        offset += 1;
    }
    while offset < size && buf[offset] == 0 {
        offset += 1;
    }
    let raw = String::from_utf8_lossy(&buf[offset..size]);
    let argv_tokens = raw
        .split('\0')
        .filter(|token| !token.is_empty())
        .take(argc)
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let argv_tokens = text
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

fn process_parent_pid(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

/// `add_agent(workspace, agent_id, role_file_path, open_display, team)`
/// (`lifecycle/operations.py:143`)。动态 role doc 编译进 spec + 起 worker;失败**字节级回滚**
/// spec_yaml / workspace_state / **team_state.md** / role_file(Gap 15.11),每步发
/// `lifecycle.add_step_*` 事件(顺序被测试锁死)。
pub fn add_agent(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
) -> Result<AddAgentReport, LifecycleError> {
    let selected = match crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    ) {
        Ok(selected) => selected,
        Err(_) if workspace.join("TEAM.md").exists() => {
            // **0.3.24 add-agent socket drift fix**: even on the TEAM.md fallback
            // path (no spec yet), prefer the state-aware resolver. It reads the
            // team's persisted `tmux_endpoint` (set at `team-agent launch` time)
            // and routes the new agent's spawn to the SAME tmux socket the live
            // team uses. Cold workspaces / first-agent paths safely fall back to
            // `TmuxBackend::for_workspace(team_workspace)` inside the resolver.
            let team_ws = team_workspace(workspace);
            let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
                &team_ws, team,
            )
            .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&team_ws));
            return add_agent_with_transport(
                workspace,
                agent_id,
                role_file_path,
                open_display,
                team,
                &transport,
            );
        }
        Err(error) => return Err(LifecycleError::TeamSelect(error.to_string())),
    };
    // E5 §3:compile_team 要角色定义目录(team_dir),不是 spec 落点(spec_workspace=runtime)。
    let team_dir = selected.team_dir;
    // **0.3.24 add-agent socket drift fix**: route to the live team's persisted
    // tmux endpoint (NOT the workspace-hash for_workspace socket). Without this,
    // `add-agent` spawns into an orphan socket (e.g. `ta-<hash>/termclaud`) while
    // the live team runs on its persisted default socket — the leader can't see
    // the new window, state never registers, and the orphaned `claude` process
    // floats forever (macmini repro: `demo-director` startup blocker).
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(selected.team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    add_agent_with_transport_at_paths(
        &selected.run_workspace,
        &team_dir,
        agent_id,
        role_file_path,
        open_display,
        Some(selected.team_key.as_str()),
        &transport,
    )
}

/// `add_agent` with an injected transport — after the recompile+write, wires the new worker spawn
/// (via start_agent_with_transport) + start_coordinator (rt-host-a sweep: recompiled but never spawned).
pub fn add_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    add_agent_with_transport_at_paths(
        &run_workspace,
        workspace,
        agent_id,
        role_file_path,
        open_display,
        team,
        transport,
    )
}

fn add_agent_with_transport_at_paths(
    run_workspace: &Path,
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let runtime_state = crate::state::persist::load_runtime_state(run_workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let canonical_team_key = team
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| explicit_active_team_key(&runtime_state))
        .unwrap_or_else(|| crate::state::projection::team_state_key(&runtime_state));
    let owner_state =
        crate::state::projection::select_runtime_state(run_workspace, Some(&canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    ensure_owner_allowed_for_state(&owner_state, Some(agent_id))?;
    if !role_file_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "role file not found: {}",
            role_file_path.display()
        )));
    }
    if runtime_agent_exists(&owner_state, agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    // E5 Bug1:不再 copy role 文件进 <team_dir>/agents(自拷贝 O_TRUNC 截断反模式)。
    // 就地读外部 role 文档编译,注入 base team spec 的 agents/routing。role 文件留在原处。
    let mut spec = crate::compiler::compile_team(team_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    override_spec_workspace(&mut spec, run_workspace);
    let workspace_s = spec
        .get("team")
        .and_then(|team| team.get("workspace"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| team_dir.to_str().unwrap_or_default())
        .to_string();
    let team_meta = crate::compiler::read_front_matter(&team_dir.join("TEAM.md"))
        .map(|(meta, _)| meta)
        .unwrap_or(Value::Null);
    let compiled = crate::compiler::compile_role_agent(role_file_path, &team_meta, &workspace_s)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    if compiled.id != agent_id.as_str() {
        return Err(LifecycleError::Compile(format!(
            "role file declares name '{}' but add-agent id is '{}'",
            compiled.id, agent_id
        )));
    }
    inject_agent_into_spec(&mut spec, compiled.agent, &compiled.id)?;
    let safety = effective_runtime_config(&spec)?;
    // E5 spec 迁移:重编译的 spec 原子写到 .team/runtime/<team_key>/(不落用户目录 team_dir)。
    let spec_path = crate::model::paths::runtime_spec_path(run_workspace, &canonical_team_key);
    // E42 (0.3.24 P0): capture pre-write bytes for atomic rollback. If anything
    // downstream of write_spec_atomic + upsert_agent_state_from_role + spawn
    // fails, restore the prior bytes so the canonical spec / runtime state never
    // get a half-written row that disagrees with what remove-agent can see.
    let pre_spec_text = match std::fs::read_to_string(&spec_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(LifecycleError::StatePersist(format!("read spec: {e}"))),
    };
    let pre_runtime_state = crate::state::persist::load_runtime_state(run_workspace).ok();
    write_spec_atomic(&spec_path, &spec)?;
    let (meta, _) = crate::compiler::read_front_matter(role_file_path)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    // upsert writes status="starting" (E42) — start_agent_at_paths::mark_agent_started
    // promotes to "running" on Ok. If anything fails between here and the Ok
    // return below, rollback restores the captured pre-bytes.
    if let Err(error) = upsert_agent_state_from_role(
        run_workspace,
        &canonical_team_key,
        agent_id,
        &meta,
        role_file_path,
        &safety,
    ) {
        rollback_add_agent_atomic(
            run_workspace,
            &spec_path,
            pre_spec_text.as_deref(),
            pre_runtime_state.as_ref(),
            agent_id,
        );
        return Err(error);
    }
    let started = match crate::lifecycle::restart::start_agent_at_paths(
        run_workspace,
        team_dir,
        agent_id,
        false,
        open_display,
        true,
        Some(&canonical_team_key),
        transport,
    ) {
        Ok(started) => started,
        Err(error) => {
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                pre_runtime_state.as_ref(),
                agent_id,
            );
            return Err(error);
        }
    };
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { env, .. } => (env, StartMode::Noop),
        StartAgentOutcome::Paused { .. } => {
            rollback_add_agent_atomic(
                run_workspace,
                &spec_path,
                pre_spec_text.as_deref(),
                pre_runtime_state.as_ref(),
                agent_id,
            );
            return Err(LifecycleError::RequirementUnmet(format!(
                "added agent {agent_id} is paused"
            )));
        }
    };
    Ok(AddAgentReport {
        env,
        start_mode,
        role_file: role_file_path.to_path_buf(),
    })
}

/// E42 (0.3.24 P0, double-spec deadlock): best-effort atomic rollback for a
/// failed add-agent. Restores the canonical spec to its pre-write bytes (or
/// removes the file if it didn't exist), and restores runtime state to its
/// pre-write JSON (so the half-written `status:starting` row is gone). The
/// caller propagates the ORIGINAL operation error after rollback; rollback
/// errors are swallowed (best-effort, no panic).
fn rollback_add_agent_atomic(
    run_workspace: &Path,
    spec_path: &Path,
    pre_spec_text: Option<&str>,
    pre_runtime_state: Option<&serde_json::Value>,
    agent_id: &AgentId,
) {
    if let Some(text) = pre_spec_text {
        let _ = std::fs::write(spec_path, text);
    } else {
        let _ = std::fs::remove_file(spec_path);
    }
    if let Some(state) = pre_runtime_state {
        let _ = crate::state::persist::save_runtime_state(run_workspace, state);
    } else {
        // No prior runtime state — drop just the agent we added (load → strip → save).
        if let Ok(mut state) = crate::state::persist::load_runtime_state(run_workspace) {
            if let Some(agents) = state
                .get_mut("agents")
                .and_then(serde_json::Value::as_object_mut)
            {
                agents.remove(agent_id.as_str());
            }
            let _ = crate::state::persist::save_runtime_state(run_workspace, &state);
        }
    }
}

fn upsert_agent_state_from_role(
    workspace: &Path,
    canonical_team_key: &str,
    agent_id: &AgentId,
    meta: &Value,
    dynamic_role_file: &Path,
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    let mut state =
        crate::state::projection::select_runtime_state(workspace, Some(canonical_team_key))
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    if !state.is_object() {
        state = serde_json::json!({});
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
    let provider = meta
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    let auth_mode = meta
        .get("auth_mode")
        .and_then(Value::as_str)
        .unwrap_or("subscription");
    let role = meta
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_else(|| agent_id.as_str());
    // E42 (0.3.24 P0, double-spec deadlock): persist the initial state row as
    // "starting" (not "running"). The caller (add_agent_with_transport_at_paths)
    // promotes to "running" only after start_agent_at_paths returns Running.
    // If the spawn fails, the rollback below removes the entry entirely.
    let mut entry = serde_json::json!({
        "provider": provider,
        "auth_mode": auth_mode,
        "role": role,
        "status": "starting",
        "dynamic_role_file": dynamic_role_file.to_string_lossy().to_string(),
    });
    if let Some(model) = meta.get("model").and_then(Value::as_str) {
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("model".to_string(), serde_json::json!(model));
            obj.insert("model_source".to_string(), serde_json::json!("role"));
        }
    }
    if let Some(profile) = meta.get("profile").and_then(Value::as_str) {
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("profile".to_string(), serde_json::json!(profile));
            if let Some(team_dir) = dynamic_role_file.parent().and_then(Path::parent) {
                obj.insert(
                    "_profile_dir".to_string(),
                    serde_json::json!(team_dir.join("profiles").to_string_lossy().to_string()),
                );
            }
            if !obj.contains_key("model_source") {
                obj.insert("model_source".to_string(), serde_json::json!("default"));
            }
        }
    }
    if let Some(obj) = entry.as_object_mut() {
        persist_effective_approval_policy(obj, safety);
    }
    agent_map.insert(agent_id.as_str().to_string(), entry);
    save_launched_team_state_for_key(workspace, &state, Some(canonical_team_key))
}

/// E5 Bug1:把 add-agent 就地编译出的 agent 条目注入 base team spec(`agents` 列表 +
/// `routing.rules` 加 `route-<id>`),复刻 [`compile_team`] 的路由规则形态。不落任何文件。
fn inject_agent_into_spec(
    spec: &mut Value,
    agent: Value,
    agent_id: &str,
) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = spec else {
        return Err(LifecycleError::Compile("spec is not a map".to_string()));
    };
    // agents 列表追加。
    match pairs.iter_mut().find(|(k, _)| k == "agents") {
        Some((_, Value::List(agents))) => {
            if !agents
                .iter()
                .any(|existing| yaml_agent_id(existing) == Some(agent_id))
            {
                agents.push(agent);
            }
        }
        _ => return Err(LifecycleError::Compile("spec.agents missing or not a list".to_string())),
    }
    // routing.rules 追加 route-<id>(与 compile_team 同形)。
    if let Some((_, Value::Map(routing))) = pairs.iter_mut().find(|(k, _)| k == "routing") {
        if let Some((_, Value::List(rules))) = routing.iter_mut().find(|(k, _)| k == "rules") {
            if !rules
                .iter()
                .any(|rule| yaml_route_assigns_to(rule) == Some(agent_id))
            {
                rules.push(Value::Map(vec![
                    ("id".to_string(), Value::Str(format!("route-{agent_id}"))),
                    (
                        "match".to_string(),
                        Value::Map(vec![(
                            "assignee".to_string(),
                            Value::List(vec![Value::Str(agent_id.to_string())]),
                        )]),
                    ),
                    ("assign_to".to_string(), Value::Str(agent_id.to_string())),
                    ("priority".to_string(), Value::Int(10)),
                ]));
            }
        }
    }
    Ok(())
}

fn runtime_agent_exists(state: &serde_json::Value, agent_id: &AgentId) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| agents.contains_key(agent_id.as_str()))
}

fn yaml_agent_id(agent: &Value) -> Option<&str> {
    let Value::Map(pairs) = agent else {
        return None;
    };
    pairs
        .iter()
        .find(|(key, _)| key == "id")
        .and_then(|(_, value)| match value {
            Value::Str(id) => Some(id.as_str()),
            _ => None,
        })
}

fn yaml_route_assigns_to(rule: &Value) -> Option<&str> {
    let Value::Map(pairs) = rule else {
        return None;
    };
    pairs
        .iter()
        .find(|(key, _)| key == "assign_to")
        .and_then(|(_, value)| match value {
            Value::Str(id) => Some(id.as_str()),
            _ => None,
        })
}

/// `fork_agent(workspace, source_agent_id, as_agent_id, ...)`(`lifecycle/operations.py:284`)。
/// native session fork(provider 须 supports_session_fork ∧ auth_mode!=compatible_api);
/// 失败回滚,每条失败臂 `adapter.cleanup_mcp`。
pub fn fork_agent(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    open_display: bool,
    team: Option<&str>,
) -> Result<ForkAgentReport, LifecycleError> {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // **0.3.24 add-agent socket drift fix** (same root cause): fork-agent must
    // also route to the live team's persisted tmux endpoint, not the workspace-
    // hash for_workspace socket. Same orphan-on-wrong-socket pathology.
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(selected.team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    fork_agent_with_transport(
        workspace,
        source_agent_id,
        as_agent_id,
        label,
        open_display,
        team,
        &transport,
    )
}

pub fn fork_agent_with_transport(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<ForkAgentReport, LifecycleError> {
    let _ = open_display;
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // E5 §3:team_dir(角色定义+profiles)恒用户目录。spec 读用 selector 解析的 spec_path
    // (读序 B:runtime 优先、legacy 回落),写恒走 runtime_spec_path(canonical 落点)。
    let fork_team_dir = selected.team_dir.clone();
    let read_spec_path = selected.spec_path.clone().ok_or_else(|| {
        LifecycleError::TeamSelect("active team spec not found".to_string())
    })?;
    let workspace = selected.run_workspace;
    let state = selected.state;
    ensure_owner_allowed_for_state(&state, Some(source_agent_id))?;
    let spec_path = crate::model::paths::runtime_spec_path(&workspace, &selected.team_key);
    let text = std::fs::read_to_string(&read_spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", read_spec_path.display())))?;
    let spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    if find_spec_agent(&spec, as_agent_id).is_some() || leader_id_matches(&spec, as_agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {as_agent_id}"
        )));
    }
    let source_agent = find_spec_agent(&spec, source_agent_id).ok_or_else(|| {
        LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source_agent_id}"))
    })?;
    let session_id = state
        .get("agents")
        .and_then(|v| v.get(source_agent_id.as_str()))
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(crate::provider::SessionId::new)
        .ok_or_else(|| {
            LifecycleError::Provider(format!(
                "cannot fork {source_agent_id}: source session_id is missing"
            ))
        })?;
    let session_name = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .unwrap_or_else(|| spec_session_name(&spec));
    if transport
        .list_windows(&session_name)
        .map(|windows| windows.iter().any(|w| w.as_str() == as_agent_id.as_str()))
        .unwrap_or(false)
    {
        return Err(LifecycleError::Transport(format!(
            "tmux window already exists for fork target: {}:{}",
            session_name.as_str(),
            as_agent_id.as_str()
        )));
    }
    let new_spec = append_forked_agent(&spec, source_agent, source_agent_id, as_agent_id, label)?;
    // validate 用角色定义目录的 team_workspace(校验 working_directory),非 spec 落点。
    let validate_ws = crate::model::paths::team_workspace(&fork_team_dir)
        .unwrap_or_else(|_| workspace.clone());
    crate::model::spec::validate_spec(&new_spec, &validate_ws)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    write_spec_atomic(&spec_path, &new_spec)?;
    let new_agent = find_spec_agent(&new_spec, as_agent_id).ok_or_else(|| {
        LifecycleError::RequirementUnmet(format!("unknown worker agent id: {as_agent_id}"))
    })?;
    let provider = new_agent
        .get("provider")
        .and_then(Value::as_str)
        .and_then(parse_provider)
        .unwrap_or(Provider::Codex);
    let auth_mode = new_agent
        .get("auth_mode")
        .and_then(Value::as_str)
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription);
    let adapter = crate::provider::get_adapter(provider);
    let provider_str = new_agent
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    if auth_mode == AuthMode::CompatibleApi || !adapter.caps().fork {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        return Err(LifecycleError::Provider(format!(
            "{provider_str} does not support native session fork"
        )));
    }
    let model = new_agent.get("model").and_then(Value::as_str);
    let safety = effective_runtime_config(&new_spec)?;
    let command_agent = crate::lifecycle::worker_command_context::WorkerCommandAgent::from_yaml(
        new_agent,
        Some(as_agent_id.as_str()),
        provider,
    );
    let system_prompt =
        crate::lifecycle::worker_command_context::compile_worker_system_prompt(&command_agent)?;
    let tools = crate::lifecycle::worker_command_context::resolved_tool_strings_for_command(
        &command_agent,
        provider,
        &safety,
    )?;
    let resolved_tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let fork_team = crate::messaging::leader_receiver::active_team_key(&workspace, &state);
    let mcp_config = adapter.mcp_config(auth_mode).map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        LifecycleError::Provider(e.to_string())
    })?;
    let mcp_config = resolve_mcp_config(mcp_config, &workspace, as_agent_id.as_str(), &fork_team);
    let mcp_config_path = write_worker_mcp_config_for_provider(
        &workspace,
        as_agent_id.as_str(),
        &mcp_config,
        Some(provider),
    )
    .map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        e
    })?;
    // E5 §3:profiles 随角色定义目录(team_dir),不随已迁出的 spec。
    let profile_dir = fork_team_dir.join("profiles");
    let profile_launch =
        crate::lifecycle::profile_launch::prepare_provider_profile_launch_with_profile_dir(
            &workspace,
            as_agent_id.as_str(),
            new_agent,
            Some(&profile_dir),
            Some(&mcp_config),
        )?;
    let command_model = profile_launch.command_overrides.model.as_deref().or(model);
    let mut plan = adapter
        .fork_plan(
            Some(&session_id),
            crate::provider::ProviderCommandContext {
                auth_mode,
                mcp_config: Some(&mcp_config),
                system_prompt: Some(system_prompt.as_str()),
                model: command_model,
                tools: &resolved_tool_refs,
                profile_launch: Some(&profile_launch),
                agent_id_hint: Some(as_agent_id.as_str()),
            },
        )
        .map_err(|e| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            LifecycleError::Provider(e.to_string())
        })?;
    if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
        point_native_mcp_config_at_file(&mut plan.argv, provider, &mcp_config_path);
    }
    fill_spawn_placeholders_full(
        &mut plan.argv,
        &workspace,
        as_agent_id.as_str(),
        Some(&fork_team),
    );
    let window = WindowName::new(as_agent_id.as_str());
    // fork inherits the parent agent's owner team via runtime state (`active_team_key`).
    let mut env =
        inherited_env_with_team_overrides(&workspace, as_agent_id.as_str(), Some(&fork_team));
    apply_profile_launch_env(&mut env, &profile_launch);
    apply_mcp_auto_approval_env(&mut env, &safety);
    // golden operations.py:336 -> _tmux_start_command_for_agent_window (runtime.py:1017-1020): branch on
    // _tmux_session_exists — an ABSENT session => new-session (spawn_first), present => new-window
    // (spawn_into). The Rust restart seam (restart.rs spawn_agent_window) uses the same branch.
    let session_live = transport.has_session(&session_name).unwrap_or(false);
    let env_unset: Vec<String> = profile_launch.env_unset.iter().cloned().collect();
    let spawn_result = if session_live {
        transport.spawn_into_with_env_unset(
            &session_name,
            &window,
            &plan.argv,
            &workspace,
            &env,
            &env_unset,
        )
    } else {
        transport.spawn_first_with_env_unset(
            &session_name,
            &window,
            &plan.argv,
            &workspace,
            &env,
            &env_unset,
        )
    };
    let spawn = spawn_result.map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        LifecycleError::Transport(e.to_string())
    })?;
    let old_state = state.clone();
    let mut next_state = state;
    upsert_forked_agent_state(
        &mut next_state,
        source_agent_id,
        as_agent_id,
        new_agent,
        &safety,
        &plan,
        &profile_launch,
        &spawn,
        &workspace,
        Some(&profile_dir),
    )?;
    if let Some(agent) = next_state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(as_agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    {
        persist_effective_approval_policy(agent, &safety);
    }
    if let Err(e) = maybe_fail_fork_after_spawn("save_runtime_state") {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
        );
        return Err(e);
    }
    if let Err(e) = save_runtime_state(&workspace, &next_state) {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
        );
        return Err(LifecycleError::StatePersist(e.to_string()));
    }
    if let Err(e) = maybe_fail_fork_after_spawn("start_coordinator") {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
        );
        return Err(e);
    }
    let coordinator_started = crate::coordinator::start_coordinator(
        &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()),
    )
    .map(|report| report.ok)
    .map_err(|e| {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
        );
        LifecycleError::StatePersist(e.to_string())
    })?;
    Ok(ForkAgentReport {
        source_agent_id: source_agent_id.clone(),
        new_agent_id: as_agent_id.clone(),
        env: AgentActionEnvelope {
            agent_id: as_agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(&workspace),
            coordinator_started,
        },
        session_id: None,
    })
}

fn rollback_fork_after_spawn(
    workspace: &Path,
    spec_path: &Path,
    spec_text: &str,
    old_state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    window: &WindowName,
    mcp_config_path: &Path,
    agent_id: &AgentId,
    profile_launch: &crate::provider::ProviderProfileLaunch,
) {
    let _ = transport.kill_window(&Target::SessionWindow {
        session: session_name.clone(),
        window: window.clone(),
    });
    let _ = std::fs::write(spec_path, spec_text.as_bytes());
    let _ = save_runtime_state(workspace, old_state);
    cleanup_fork_mcp_artifacts(workspace, agent_id, mcp_config_path, profile_launch);
}

fn maybe_fail_fork_after_spawn(step: &str) -> Result<(), LifecycleError> {
    let Ok(reason) = std::env::var("TEAM_AGENT_TEST_FAIL_FORK_AFTER_SPAWN") else {
        return Ok(());
    };
    if reason.is_empty() {
        return Ok(());
    }
    let should_fail = reason == step || (step == "start_coordinator" && reason == "coordinator");
    if !should_fail {
        return Ok(());
    }
    Err(LifecycleError::StatePersist(format!(
        "injected fork failure after spawn: {reason}"
    )))
}

fn cleanup_fork_mcp_artifacts(
    workspace: &Path,
    agent_id: &AgentId,
    mcp_config_path: &Path,
    profile_launch: &crate::provider::ProviderProfileLaunch,
) {
    let _ = std::fs::remove_file(mcp_config_path);
    let _ = std::fs::remove_file(
        workspace
            .join(".team/runtime/provider-env")
            .join(format!("{}.env", agent_id.as_str())),
    );
    if let Some(config_dir) = profile_launch.claude_config_dir.as_ref() {
        let _ = std::fs::remove_dir_all(config_dir.parent().unwrap_or(config_dir));
    }
}

fn leader_id_matches(spec: &Value, agent_id: &AgentId) -> bool {
    spec.get("leader")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false)
}

fn find_spec_agent<'a>(spec: &'a Value, agent_id: &AgentId) -> Option<&'a Value> {
    let leader_is_agent = spec
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false);
    if leader_is_agent {
        return None;
    }
    spec.get("agents")?.as_list()?.iter().find(|agent| {
        agent
            .get("id")
            .and_then(Value::as_str)
            .map(|id| id == agent_id.as_str())
            .unwrap_or(false)
    })
}

fn append_forked_agent(
    spec: &Value,
    source_agent: &Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
) -> Result<Value, LifecycleError> {
    let mut new_agent = source_agent.clone();
    set_yaml_map_value(
        &mut new_agent,
        "id",
        Value::Str(as_agent_id.as_str().to_string()),
    )?;
    // golden operations.py:315 `str(label or new_agent.get("role") or as_agent_id)` — Python `or`
    // falsiness: an EMPTY-string label/role is falsy and falls through to the next tier.
    // The label IS the forked agent's new role (it feeds the identity prompt — B2 family).
    let role = label
        .filter(|s| !s.is_empty())
        .or_else(|| {
            new_agent
                .get("role")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| as_agent_id.as_str())
        .to_string();
    set_yaml_map_value(&mut new_agent, "role", Value::Str(role.clone()))?;
    set_yaml_map_value(
        &mut new_agent,
        "forked_from",
        Value::Str(source_agent_id.as_str().to_string()),
    )?;
    set_yaml_map_value(
        &mut new_agent,
        "preferred_for",
        Value::List(vec![
            Value::Str(as_agent_id.as_str().to_string()),
            Value::Str(role),
        ]),
    )?;

    let Value::Map(pairs) = spec else {
        return Err(LifecycleError::Compile(
            "spec root is not a map".to_string(),
        ));
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        if key == "agents" {
            let mut agents = value
                .as_list()
                .map(|items| items.to_vec())
                .unwrap_or_default();
            agents.push(new_agent.clone());
            out.push((key.clone(), Value::List(agents)));
        } else if key == "runtime" {
            out.push((key.clone(), runtime_with_startup_agent(value, as_agent_id)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    Ok(Value::Map(out))
}

fn set_yaml_map_value(value: &mut Value, key: &str, next: Value) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = value else {
        return Err(LifecycleError::Compile(
            "agent entry is not a map".to_string(),
        ));
    };
    if let Some((_, existing)) = pairs.iter_mut().find(|(k, _)| k == key) {
        *existing = next;
    } else {
        pairs.push((key.to_string(), next));
    }
    Ok(())
}

fn runtime_with_startup_agent(runtime: &Value, agent_id: &AgentId) -> Value {
    let Value::Map(pairs) = runtime else {
        return runtime.clone();
    };
    let mut out = Vec::new();
    let mut saw_startup = false;
    for (key, value) in pairs {
        if key == "startup_order" {
            saw_startup = true;
            let mut order = value
                .as_list()
                .map(|items| items.to_vec())
                .unwrap_or_default();
            let already_present = order.iter().any(|item| {
                item.as_str()
                    .map(|id| id == agent_id.as_str())
                    .unwrap_or(false)
            });
            if !already_present {
                order.push(Value::Str(agent_id.as_str().to_string()));
            }
            out.push((key.clone(), Value::List(order)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    if !saw_startup {
        out.push((
            "startup_order".to_string(),
            Value::List(vec![Value::Str(agent_id.as_str().to_string())]),
        ));
    }
    Value::Map(out)
}

fn upsert_forked_agent_state(
    state: &mut serde_json::Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    spec_agent: &Value,
    safety: &DangerousApproval,
    plan: &crate::provider::CommandPlan,
    profile_launch: &crate::provider::ProviderProfileLaunch,
    spawn: &crate::transport::SpawnResult,
    spawn_cwd: &Path,
    profile_dir: Option<&Path>,
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
        .and_then(Value::as_str)
        .unwrap_or("codex");
    let mut entry = serde_json::Map::new();
    entry.insert("status".to_string(), serde_json::json!("running"));
    entry.insert("provider".to_string(), serde_json::json!(provider));
    entry.insert(
        "agent_id".to_string(),
        serde_json::json!(as_agent_id.as_str()),
    );
    entry.insert(
        "window".to_string(),
        serde_json::json!(as_agent_id.as_str()),
    );
    entry.insert(
        "forked_from".to_string(),
        serde_json::json!(source_agent_id.as_str()),
    );
    entry.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(spawn_cwd.to_string_lossy().to_string()),
    );
    entry.insert(
        "pane_id".to_string(),
        serde_json::json!(spawn.pane_id.as_str()),
    );
    if let Some(pid) = spawn.child_pid {
        entry.insert("pane_pid".to_string(), serde_json::json!(pid));
    }
    for key in [
        "auth_mode",
        "model",
        "model_source",
        "profile",
        "_profile_dir",
        "role",
    ] {
        if let Some(value) = spec_agent.get(key) {
            entry.insert(key.to_string(), yaml_value_to_json(value));
        }
    }
    if spec_agent.get("profile").is_some() && !entry.contains_key("_profile_dir") {
        if let Some(profile_dir) = profile_dir {
            entry.insert(
                "_profile_dir".to_string(),
                serde_json::json!(profile_dir.to_string_lossy().to_string()),
            );
        }
    }
    entry.insert("session_id".to_string(), serde_json::Value::Null);
    entry.insert("rollout_path".to_string(), serde_json::Value::Null);
    entry.insert("captured_at".to_string(), serde_json::Value::Null);
    entry.insert("captured_via".to_string(), serde_json::Value::Null);
    entry.insert(
        "attribution_confidence".to_string(),
        serde_json::Value::Null,
    );
    persist_command_plan_state(&mut entry, plan, profile_launch);
    agent_map.insert(
        as_agent_id.as_str().to_string(),
        serde_json::Value::Object(entry),
    );
    if let Some(entry) = agent_map
        .get_mut(as_agent_id.as_str())
        .and_then(serde_json::Value::as_object_mut)
    {
        persist_effective_approval_policy(entry, safety);
    }
    Ok(())
}

pub(crate) fn ensure_owner_allowed(workspace: &Path) -> Result<(), LifecycleError> {
    let state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    ensure_owner_allowed_for_state(&state, None)
}

pub(crate) fn ensure_owner_allowed_for_state(
    state: &serde_json::Value,
    target_role: Option<&AgentId>,
) -> Result<(), LifecycleError> {
    struct NoopLiveness;
    impl crate::state::owner_gate::PaneLivenessProbe for NoopLiveness {
        fn liveness(&self, _pane_id: &str) -> crate::model::enums::PaneLiveness {
            crate::model::enums::PaneLiveness::Live
        }
    }

    let target_team = crate::state::projection::team_state_key(state);
    if caller_is_target_role_in_team(&target_team, target_role) {
        return Ok(());
    }
    let caller = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&target_team),
        None,
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) =
        crate::state::owner_gate::check_team_owner(state, &caller, false, &NoopLiveness)
    {
        return Err(LifecycleError::OwnerRefused(refusal.to_string()));
    }
    Ok(())
}

fn caller_is_target_role_in_team(target_team: &str, target_role: Option<&AgentId>) -> bool {
    let Some(target_role) = target_role else {
        return false;
    };
    std::env::var("TEAM_AGENT_ID").ok().as_deref() == Some(target_role.as_str())
        && std::env::var("TEAM_AGENT_TEAM_ID").ok().as_deref() == Some(target_team)
}

pub(crate) fn state_path(workspace: &Path) -> std::path::PathBuf {
    crate::state::persist::runtime_state_path(workspace)
}

fn initial_runtime_state(
    spec: &Value,
    spec_path: &Path,
    workspace: &Path,
    team_dir: &Path,
) -> serde_json::Value {
    let mut agents = serde_json::Map::new();
    for agent in spec_agent_values(spec) {
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("codex");
        let role = agent.get("role").and_then(Value::as_str).unwrap_or(id);
        let model = agent.get("model").and_then(Value::as_str);
        let auth_mode = agent.get("auth_mode").and_then(Value::as_str);
        let mut value = serde_json::json!({
            "provider": provider,
            "role": role,
        });
        if let Some(obj) = value.as_object_mut() {
            if let Some(model) = model {
                obj.insert("model".to_string(), serde_json::json!(model));
            }
            if let Some(auth_mode) = auth_mode {
                obj.insert("auth_mode".to_string(), serde_json::json!(auth_mode));
            }
        }
        agents.insert(id.to_string(), value);
    }
    let display_backend = spec_display_backend(spec);
    let mut state = serde_json::Map::new();
    state.insert(
        "spec_path".to_string(),
        serde_json::json!(spec_path.to_string_lossy().to_string()),
    );
    state.insert(
        "workspace".to_string(),
        serde_json::json!(workspace.to_string_lossy().to_string()),
    );
    state.insert(
        "team_dir".to_string(),
        serde_json::json!(team_dir.to_string_lossy().to_string()),
    );
    state.insert(
        "session_name".to_string(),
        serde_json::json!(spec_session_name(spec).as_str()),
    );
    state.insert(
        "leader".to_string(),
        spec.get("leader")
            .map(yaml_value_to_json)
            .unwrap_or(serde_json::Value::Null),
    );
    state.insert("agents".to_string(), serde_json::Value::Object(agents));
    state.insert("tasks".to_string(), spec_tasks_json(spec));
    state.insert(
        "display_backend".to_string(),
        serde_json::json!(display_backend),
    );
    state.insert("is_external_leader".to_string(), serde_json::json!(false));
    let mut state = serde_json::Value::Object(state);
    if !seed_launched_owner_from_env(&mut state) {
        let team_id = crate::state::projection::team_state_key(&state);
        seed_unbound_launched_owner(&mut state, &team_id);
    }
    state
}

fn seed_launched_owner_from_env(state: &mut serde_json::Value) -> bool {
    let team_id = crate::state::projection::team_state_key(state);
    let Ok(caller) = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&team_id),
        None,
    ) else {
        return false;
    };
    seed_launched_owner_from_caller_with_provider_lookup(
        state,
        caller,
        attributed_provider_for_pane_across_tmux_sockets,
    )
}

fn seed_launched_owner_from_caller_with_provider_lookup(
    state: &mut serde_json::Value,
    caller: crate::state::owner_gate::CallerIdentity,
    lookup_pane_provider: impl Fn(&PaneId) -> Option<Provider>,
) -> bool {
    if caller.pane_id.is_empty() {
        return false;
    }
    let provider = caller_provider_for_seed_with_lookup(&caller, lookup_pane_provider);
    let pane_id = caller.pane_id;
    let owner_epoch = 1u64;
    let mut owner = serde_json::json!({
        "pane_id": pane_id,
        "machine_fingerprint": caller.machine_fingerprint,
        "leader_session_uuid": caller.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
    });
    let mut receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "pane": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "leader_session_uuid": owner.get("leader_session_uuid").cloned().unwrap_or(serde_json::Value::Null),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    if let Some(provider) = provider.as_ref() {
        if let Some(owner) = owner.as_object_mut() {
            owner.insert("provider".to_string(), serde_json::json!(provider));
        }
        if let Some(receiver) = receiver.as_object_mut() {
            receiver.insert("provider".to_string(), serde_json::json!(provider));
        }
    }
    if let (Some(receiver), Some(socket)) = (
        receiver.as_object_mut(),
        crate::tmux_backend::socket_name_from_tmux_env(),
    ) {
        receiver.insert("tmux_socket".to_string(), serde_json::json!(socket));
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("leader_receiver".to_string(), receiver);
        obj.insert("team_owner".to_string(), owner);
        obj.insert("owner_epoch".to_string(), serde_json::json!(owner_epoch));
    }
    true
}

fn has_positive_caller_leader_env() -> bool {
    env_nonempty("TEAM_AGENT_LEADER_PANE_ID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE")
        || env_nonempty("TEAM_AGENT_LEADER_PROVIDER")
}

fn spec_tasks_json(spec: &Value) -> serde_json::Value {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| serde_json::Value::Array(tasks.iter().map(yaml_value_to_json).collect()))
        .unwrap_or_else(|| serde_json::json!([]))
}

fn yaml_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(v) => serde_json::json!(v),
        Value::Int(v) => serde_json::json!(v),
        Value::Float(v) => serde_json::json!(v),
        Value::Str(v) => serde_json::json!(v),
        Value::List(values) => {
            serde_json::Value::Array(values.iter().map(yaml_value_to_json).collect())
        }
        Value::Map(entries) => {
            let mut out = serde_json::Map::new();
            for (key, item) in entries {
                out.insert(key.clone(), yaml_value_to_json(item));
            }
            serde_json::Value::Object(out)
        }
    }
}

/// Set `runtime.session_name` on the compiled spec to `session_name`, creating the
/// `runtime` map and/or the `session_name` entry if absent. Used by quick-start to
/// derive the tmux session from the REQUESTED team identity (CR-040/042) rather
/// than the template's compiled-in name.
/// E5 Bug2(atomic 真修):原子写 runtime spec —— 写 `<spec>.tmp-<pid>` 再 rename 覆盖,
/// 避免崩溃/并发留下半截 spec(plain fs::write 会 in-place truncate 后逐字节写)。
/// rename 失败时清理 tmp,原 spec(若有)不动。
pub(crate) fn write_spec_atomic(spec_path: &Path, spec: &Value) -> Result<(), LifecycleError> {
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let tmp = spec_path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, yaml::dumps(spec))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", tmp.display())))?;
    if let Err(e) = std::fs::rename(&tmp, spec_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(LifecycleError::StatePersist(format!(
            "{}: {e}",
            spec_path.display()
        )));
    }
    Ok(())
}

pub(crate) fn override_spec_session_name(spec: &mut Value, session_name: &str) {
    override_spec_runtime_str(spec, "session_name", session_name);
}

pub(crate) fn override_spec_workspace(spec: &mut Value, workspace: &Path) {
    let workspace_s = workspace.to_string_lossy().to_string();
    let Value::Map(root) = spec else { return };
    if let Some((_, Value::Map(team))) = root.iter_mut().find(|(k, _)| k == "team") {
        if let Some((_, value)) = team.iter_mut().find(|(k, _)| k == "workspace") {
            *value = Value::Str(workspace_s.clone());
        }
    }
    if let Some((_, Value::List(agents))) = root.iter_mut().find(|(k, _)| k == "agents") {
        for agent in agents {
            if let Value::Map(fields) = agent {
                if let Some((_, value)) = fields.iter_mut().find(|(k, _)| k == "working_directory") {
                    *value = Value::Str(workspace_s.clone());
                }
            }
        }
    }
}

fn override_spec_display_backend(spec: &mut Value, display_backend: &str) {
    override_spec_runtime_str(spec, "display_backend", display_backend);
}

fn override_spec_runtime_str(spec: &mut Value, key: &str, value: &str) {
    let Value::Map(root) = spec else { return };
    let runtime_slot = root
        .iter_mut()
        .find(|(k, _)| k == "runtime")
        .map(|(_, v)| v);
    match runtime_slot {
        Some(Value::Map(runtime)) => {
            if let Some((_, existing)) = runtime.iter_mut().find(|(k, _)| k == key) {
                *existing = Value::Str(value.to_string());
            } else {
                runtime.push((key.to_string(), Value::Str(value.to_string())));
            }
        }
        Some(other) => {
            *other = Value::Map(vec![(key.to_string(), Value::Str(value.to_string()))]);
        }
        None => {
            root.push((
                "runtime".to_string(),
                Value::Map(vec![(key.to_string(), Value::Str(value.to_string()))]),
            ));
        }
    }
}

fn spec_session_name(spec: &Value) -> SessionName {
    if let Some(name) = spec
        .get("runtime")
        .and_then(|v| v.get("session_name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return SessionName::new(name);
    }
    // Python launch/core.py:56 — fallback derives from the team name, not a constant.
    let team_name = spec
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("agent");
    SessionName::new(format!("team-{team_name}"))
}

/// 0.3.28 layout step 1: pub re-export of `spec_session_name` for the new
/// `layout::sessions::worker_session_name` to delegate to. Single underlying
/// impl; this just widens visibility without duplicating logic.
pub fn worker_session_name_pub(spec: &Value) -> SessionName {
    spec_session_name(spec)
}

fn spec_agents(spec: &Value) -> Vec<AgentId> {
    spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| agent.get("id").and_then(Value::as_str).map(AgentId::new))
        .collect()
}

fn spec_agent_values(spec: &Value) -> Vec<&Value> {
    spec.get("agents")
        .and_then(Value::as_list)
        .map(|agents| agents.iter().collect())
        .unwrap_or_default()
}

fn spec_routes(spec: &Value) -> Vec<RoutingDecision> {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| {
            tasks
                .iter()
                .map(|task| {
                    let routed = crate::model::routing::route_task(spec, task);
                    RoutingDecision {
                        task_id: task.get("id").and_then(Value::as_str).map(str::to_string),
                        selected_agent: routed.agent_id,
                        reason: routed.reason,
                        manual_override: false,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn spec_default_assignee(spec: &Value) -> Option<AgentId> {
    spec.get("routing")
        .and_then(|v| v.get("default_assignee"))
        .and_then(Value::as_str)
        .map(AgentId::new)
        .or_else(|| spec_agents(spec).into_iter().next())
}

pub(crate) fn effective_runtime_config(spec: &Value) -> Result<DangerousApproval, LifecycleError> {
    let enabled = spec
        .get("runtime")
        .and_then(|v| v.get("dangerous_auto_approve"))
        .is_some_and(Value::is_truthy);
    if enabled {
        let leader = detect_dangerous_approval()?;
        Ok(DangerousApproval {
            enabled: true,
            source: DangerousApprovalSource::RuntimeConfig,
            inherited: false,
            provider: None,
            flag: None,
            worker_capability_above_leader: !leader.enabled,
            ancestry_binary_name: leader.ancestry_binary_name,
            unexpected_binary: false,
        })
    } else {
        Ok(detect_dangerous_approval()?)
    }
}

fn disabled_dangerous_approval() -> DangerousApproval {
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

pub(crate) fn effective_runtime_config_for_worker_spawn(
) -> Result<DangerousApproval, LifecycleError> {
    detect_dangerous_approval()
}

fn write_launch_permission_audit(
    workspace: &Path,
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "launch.permissions_resolved",
            serde_json::json!({
                "dangerous_auto_approve": safety.enabled,
                "dangerous_auto_approve_source": safety.source,
                "dangerous_auto_approve_inherited": safety.inherited,
                "dangerous_auto_approve_provider": safety.provider,
                "dangerous_auto_approve_flag": safety.flag,
                "worker_capability_above_leader": safety.worker_capability_above_leader,
                "ancestry_binary_name": safety.ancestry_binary_name,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if safety.unexpected_binary {
        crate::event_log::EventLog::new(workspace)
            .write(
                "dangerous_flag_in_unexpected_binary",
                serde_json::json!({
                    "provider": safety.provider,
                    "flag": safety.flag,
                    "ancestry_binary_name": safety.ancestry_binary_name,
                }),
            )
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    Ok(())
}

fn team_workspace(team_dir: &Path) -> PathBuf {
    crate::model::paths::team_workspace(team_dir).unwrap_or_else(|_| {
        team_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| team_dir.to_path_buf())
    })
}

mod plan;
pub use plan::{handle_report_result, start_plan};

// unit-8 (Stage 3) phase boundary modules. Established here as named
// homes for upcoming relocations from the launch.rs monolith. Each
// child carries a `*Phase` enum naming the canonical phase labels used
// in event logs and (future) the orchestrator's step dispatcher.
pub mod readiness;
pub mod spawn;
pub mod spec_state;
