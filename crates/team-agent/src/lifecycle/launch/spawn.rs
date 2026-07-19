//! unit-8 (Stage 3) — `lifecycle::launch::spawn` phase boundary.
//!
//! Dedicated home for the worker spawn executor. Future commits migrate
//! `spawn_first_with_env_unset` and `spawn_into_with_env_unset` from
//! launch.rs:376-405 here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::profile_launch::parse_provider;
use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPhase {
    SpawnFirstWithEnvUnset,
    SpawnIntoWithEnvUnset,
}

impl SpawnPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFirstWithEnvUnset => "launch.spawn.first_with_env_unset",
            Self::SpawnIntoWithEnvUnset => "launch.spawn.into_with_env_unset",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_dotted_paths_under_launch_spawn() {
        assert!(SpawnPhase::SpawnFirstWithEnvUnset
            .as_str()
            .starts_with("launch.spawn."));
    }
}

pub(super) fn spawn_agents(
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
        // 0.4.x provider effort MVP step 4 + 7: resolve effort and emit
        // unsupported warning event when the spec asked for an effort the
        // provider can't satisfy.
        let agent_effort = provider_effort_for_spawn(agent, provider);
        if let Some(event_value) = provider_effort_event_if_dropped(agent, provider, agent_id_raw) {
            let _ = crate::event_log::EventLog::new(workspace)
                .write("provider.effort_unsupported", event_value);
        }
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
                effort: agent_effort,
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
                || profile_launch
                    .command_overrides
                    .model
                    .as_deref()
                    .is_some_and(|s| !s.is_empty())
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
            std::fs::create_dir_all(&log_dir)
                .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", log_dir.display())))?;
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
        let env_unset = crate::layout::worker_env::isolate_worker_spawn_env(
            provider,
            &mut env,
            extend_worker_env_unset_for_effort(
                profile_launch.env_unset.iter().cloned().collect(),
                provider,
            ),
        );
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
        let spawn_epoch = u64::try_from(started.len()).unwrap_or(u64::MAX);
        let spawned_at = spawn_timestamp_for_agent(u32::try_from(spawn_epoch).unwrap_or(u32::MAX));
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
                crate::event_log::PROVIDER_WORKER_SPAWN_ARGV,
                crate::event_log::provider_worker_spawn_argv_fields(serde_json::json!({
                    "agent_id": agent_id_raw,
                    "provider": provider,
                    "argv": plan.argv,
                    "session_id_in_argv": session_id_in_argv,
                    "expected_session_id": plan.expected_session_id.as_ref().map(|s| s.as_str()),
                    "spawn_cwd": workspace.to_string_lossy(),
                    "spawned_at": spawned_at.as_str(),
                    "source": "launch",
                    "spawn_epoch": spawn_epoch,
                })),
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
            spawned_at,
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
