//!
//! ProviderAdapter trait(per-provider 命令构造 + 能力面)+ registry facade + 占位实现。

use std::path::{Path, PathBuf};

use super::types::{
    CapturedSession, CommandPlan, McpConfig, ProviderCaps, ProviderCommandContext,
    ProviderError, SessionId, };
use super::{AuthMode, Provider};

pub use crate::provider::session_scan::{CaptureSessionContext, CapturedSessionCandidate};

// ===========================================================================
// TRAIT: ProviderAdapter (method SIGNATURES only — 无 body)
// ===========================================================================

/// 单一真相 per-provider 命令构造器 + 能力面(`provider_cli/adapter.py` `ProviderAdapter`
/// 基类的 Rust 等价)。claude/codex/gemini/fake 各一 impl;copilot/opencode 占位 impl
/// 调用即返 `ProviderError::CapabilityUnsupported`。
///
/// fallible 边界(§10):任何触碰 fs / 子进程 / provider 日志的方法返 `Result`;
/// 纯查询(caps)返值。**MUST-NOT-13**:trait 实现绝不调 provider client / network SDK。
pub trait ProviderAdapter {
    /// 此 adapter 对应的 provider(claude_code 归一后单变体)。
    fn provider(&self) -> Provider;

    /// 静态能力声明(doc §59)。
    fn caps(&self) -> ProviderCaps;

    /// CLI 是否已安装(doctor;`adapter.py` is_installed)。
    fn is_installed(&self) -> bool;




    /// Construct launch argv with the explicit permission-bypass policy.
    fn build_command_with_permissions(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        dangerously_skip_permissions: bool,
    ) -> Result<Vec<String>, ProviderError>;

    fn build_command_plan(
        &self,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        self.build_command_with_permissions(
            ctx.auth_mode,
            ctx.mcp_config,
            ctx.system_prompt,
            ctx.model,
            ctx.dangerously_skip_permissions,
        )
        .map(CommandPlan::argv_only)
    }

    /// 启动后从 provider session 日志捕获 session_id + rollout_path
    /// (`claude.py:73`/`codex.py:62`)。fs watch / mtime fallback / repair。
    fn capture_session_id(
        &self,
        agent_id: &str,
        spawn_cwd: &std::path::Path,
        timeout_s: u64,
    ) -> Result<Option<CapturedSession>, ProviderError>;

    /// Internal capture surface for same-team multi-agent attribution: enumerate every
    /// cwd-matching provider transcript candidate, then let the runtime allocate them
    /// once per tick/restart pass using per-agent context.
    fn capture_session_candidates(
        &self,
        context: &CaptureSessionContext,
        timeout_s: u64,
    ) -> Result<Vec<CapturedSessionCandidate>, ProviderError> {
        Ok(self
            .capture_session_id(&context.agent_id, &context.spawn_cwd, timeout_s)?
            .into_iter()
            .map(|captured| CapturedSessionCandidate {
                captured,
                embedded_agent_id: None,
                positive_agent_id_match: false,
                agent_path_match: false,
            })
            .collect())
    }


    /// 给定 session 是否可 resume(`claude.py:143`)。bug-085:compatible_api
    /// `session_id=None` → 不可 resume(`false`,不崩)。
    fn session_is_resumable(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
    ) -> Result<bool, ProviderError>;


    fn build_resume_command_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        dangerously_skip_permissions: bool,
    ) -> Result<Vec<String>, ProviderError>;

    fn build_resume_command_plan(
        &self,
        session_id: Option<&SessionId>,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        self.build_resume_command_with_context(
            session_id,
            ctx.auth_mode,
            ctx.mcp_config,
            ctx.system_prompt,
            ctx.model,
            ctx.dangerously_skip_permissions,
        )
        .map(CommandPlan::argv_only)
    }


    fn fork_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        dangerously_skip_permissions: bool,
    ) -> Result<Vec<String>, ProviderError>;

    fn fork_plan(
        &self,
        session_id: Option<&SessionId>,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        self.fork_with_context(
            session_id,
            ctx.auth_mode,
            ctx.mcp_config,
            ctx.system_prompt,
            ctx.model,
            ctx.dangerously_skip_permissions,
        )
        .map(CommandPlan::argv_only)
    }

    /// 计算本 provider 该用的 MCP server 配置(`adapter.py` mcp_config;claude
    /// compatible_api 走 `ensure_compatible_claude_mcp_config`)。
    fn mcp_config(&self, auth_mode: AuthMode) -> Result<McpConfig, ProviderError>;

    /// Eager stdio clients must publish their initial MCP handshake before input.
    /// Pi owns admission in its runtime registry hooks; Fake has no MCP client.
    /// The process-stamp receipt port is currently POSIX-only, not a Windows claim.
    fn requires_worker_mcp_handshake(&self) -> bool {
        cfg!(unix) && !matches!(self.provider(), Provider::Fake | Provider::Pi)
    }




    /// Provider-specific startup prompt handling. Codex and Claude delegate to
    /// provider-layer recognizers; providers without startup prompts return an
    /// empty list.
    fn handle_startup_prompts(
        &self,
        transport: &dyn crate::transport::Transport,
        target: &crate::transport::Target,
        checks: usize,
        sleep_s: f64,
    ) -> Vec<crate::provider::HandledPrompt> {
        self.handle_startup_prompts_outcome(transport, target, checks, sleep_s)
            .handled
    }

    /// swallow batch 2 ② (A1): the structured variant — `capture_error` surfaces a pane
    /// that could not even be captured, so callers can log the failure instead of
    /// silently treating it as "no prompts" (CLAUDE.md §5).
    fn handle_startup_prompts_outcome(
        &self,
        transport: &dyn crate::transport::Transport,
        target: &crate::transport::Target,
        checks: usize,
        sleep_s: f64,
    ) -> super::startup_prompt::StartupPromptOutcome {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match self.provider() {
            Provider::Codex => super::startup_prompt::codex_handle_startup_prompts(
                transport, target, checks, sleep_s,
            ),
            Provider::Claude | Provider::ClaudeCode => {
                super::startup_prompt::claude_handle_startup_prompts(
                    transport, target, checks, sleep_s,
                )
            }
            Provider::Copilot => super::startup_prompt::copilot_handle_startup_prompts(
                transport, target, checks, sleep_s,
            ),
            _ => super::startup_prompt::StartupPromptOutcome::default(),
        }))
        .unwrap_or_default()
    }
}

// ===========================================================================
// FACADE 自由函数 (doc §71 providers.get_adapter — body unimplemented)
// ===========================================================================

/// `providers.get_adapter(provider)`(`providers.py:47`)——拿某 provider 的命令构造器。
/// `claude`/`claude_code` 指向同一 adapter(`Provider::ClaudeCode` 单变体已归一);
/// copilot/opencode 返占位 adapter(调用即 `CapabilityUnsupported`)。
pub fn get_adapter(p: Provider) -> Box<dyn ProviderAdapter> {
    Box::new(BasicProviderAdapter { provider: p })
}

#[cfg(test)]
#[path = "tests/command_helpers.rs"]
mod command_helpers_tests;

#[derive(Debug, Clone, Copy)]
pub(crate) struct BasicProviderAdapter {
    provider: Provider,
}

impl ProviderAdapter for BasicProviderAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }

    fn caps(&self) -> ProviderCaps {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => ProviderCaps {
                resume: true,
                fork: true,
                native_mcp_config: true,
                writes_global_settings: false,
            },
            // Copilot resume 走 --resume <sid>;store fork 在隔离 COPILOT_HOME
            // 复制并重键 session backing 后以 --resume <new sid> 启动。
            // native_mcp_config=true(`--additional-mcp-config` 接 inline JSON 或 @file);
            // writes_global_settings=false(session 走 --session-id 预定 UUID,不污染
            // ~/.copilot/mcp-config.json,help 原文 "augments config for this session")。
            Provider::Copilot => ProviderCaps {
                resume: true,
                fork: true,
                native_mcp_config: true,
                writes_global_settings: false,
            },
            Provider::GeminiCli => ProviderCaps {
                resume: false,
                fork: false,
                native_mcp_config: false,
                writes_global_settings: true,
            },
            // Codex and Grok workers run only through their contract leaves. A
            // persisted legacy row is neither resumable nor capture-pending.
            Provider::Codex | Provider::Grok => ProviderCaps {
                resume: false,
                fork: false,
                native_mcp_config: false,
                writes_global_settings: false,
            },
            Provider::Pi => ProviderCaps {
                resume: true,
                fork: false,
                native_mcp_config: false,
                writes_global_settings: false,
            },
            Provider::Fake => ProviderCaps {
                resume: false,
                fork: false,
                native_mcp_config: false,
                writes_global_settings: false,
            },
        }
    }

    fn is_installed(&self) -> bool {
        if matches!(self.provider, Provider::Fake) {
            return true;
        }
        command_on_path(command_name(self.provider))
    }




    fn build_command_with_permissions(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        dangerously_skip_permissions: bool,
    ) -> Result<Vec<String>, ProviderError> {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => Ok(claude_launch_command(
                self,
                auth_mode,
                mcp_config,
                system_prompt,
                model,
                dangerously_skip_permissions,
            )?),
            // §C1 worker argv 形态 + C-1/C-5/C-6 cr verdict:
            //   copilot --no-color --no-auto-update [<dangerous|granular>] [--model]
            //          --additional-mcp-config <inline json> --session-id <expected_uuid> -C <cwd>
            // system_prompt 经 spawn env(COPILOT_CUSTOM_INSTRUCTIONS_DIRS)+ per-worker
            // AGENTS.md(launch 路径写文件,见 lifecycle/launch.rs)注入,**不入 argv**
            // (B2 灵魂件降级,C-1-2 禁 silent 写全局)。
            Provider::Copilot => Ok(copilot_base_command(
                auth_mode,
                mcp_config,
                system_prompt,
                model,
                dangerously_skip_permissions,
            )),
            Provider::GeminiCli => {
                let mut argv = vec!["gemini".to_string()];
                if let Some(model) = model {
                    argv.push("--model".to_string());
                    argv.push(model.to_string());
                }
                Ok(argv)
            }
            Provider::Codex | Provider::Grok => {
                Err(ProviderError::Command(retired_worker(self.provider)))
            }
            Provider::Pi => Err(ProviderError::Command(
                "Pi commands require the shared lifecycle materializer".to_string(),
            )),
            Provider::Fake => Ok(fake_worker_command()),
        }
    }

    fn build_command_plan(
        &self,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => {
                // 0.4.7 (B1 verified, restart partial resume): RESTORE
                // `--session-id <uuid>` on Claude fresh spawn. B1 confirmed
                // Claude ≥ 2.1.185 honours the framework-supplied --session-id
                // and creates a transcript at that id. The earlier 0.4.6
                // P0 removal (commit 9feafc31) is reverted; the actual issue
                // it tried to fix (leader-marker pollution) is addressed
                // separately in commit d39b5104 (leader session exclusion
                // in capture/repair).
                //
                // Fresh path: --session-id <expected> → Claude writes
                // transcript at that id → capture/restart restore use the
                // SAME id we predicted, so apply-time backing-store check
                // passes immediately (no cwd+spawned_at attribution race).
                //
                // Resume path unchanged — `build_resume_command_plan` uses
                // `--resume <existing_id>` on a session id that already has
                // a real transcript.
                let expected = next_session_token();
                let managed = ctx
                    .profile_launch
                    .is_some_and(|profile| profile.managed_mcp_config);
                let projects_root = ctx
                    .profile_launch
                    .and_then(|profile| profile.claude_projects_root.clone());
                let model = claude_context_model(ctx);
                let mut argv = claude_base_command(
                    self,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    model,
                    ctx.dangerously_skip_permissions,
                    managed,
                    ctx.effort,
                )?;
                argv.push("--session-id".to_string());
                argv.push(expected.clone());
                // Layer 1 self-healing (architect probe 2026-06-22, claude help
                // `-n, --name <name>`): pass `--name <agent_id>` so the
                // resume picker and on-disk `~/.claude/sessions/*.json name`
                // field carry our role label.
                if let Some(agent_id) = ctx.agent_id_hint {
                    if !agent_id.is_empty() {
                        argv.push("--name".to_string());
                        argv.push(agent_id.to_string());
                    }
                }
                Ok(CommandPlan {
                    argv,
                    expected_session_id: Some(SessionId::new(expected)),
                    provider_projects_root: projects_root,
                    managed_mcp_config: managed,
                })
            }
            // §C1 + §C4 cr verdict — copilot plan 端预定 UUID + workspace `-C` 双保险:
            //   * `--session-id <uuid>`(claude 同法,捕获免目录扫描,sqlite 仅校验)
            //   * `-C <workspace>`(双保险,即便 spawn cwd 漂移也能锚定)
            // mcp_config inline 形态由 build_command 写入,launch 路径会用
            // point_native_mcp_config_at_file 重写为 @<file> 形(§C1 note)。
            Provider::Copilot => {
                let expected = next_session_token();
                let mut argv = copilot_base_command(
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.dangerously_skip_permissions,
                );
                argv.push("--session-id".to_string());
                argv.push(expected.clone());
                Ok(CommandPlan {
                    argv,
                    expected_session_id: Some(SessionId::new(expected)),
                    provider_projects_root: None,
                    managed_mcp_config: false,
                })
            }
            Provider::Codex | Provider::Grok => {
                Err(ProviderError::Command(retired_worker(self.provider)))
            }
            Provider::Pi => Err(ProviderError::Command(
                "Pi commands require the shared lifecycle materializer".to_string(),
            )),
            _ => self
                .build_command_with_permissions(
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.dangerously_skip_permissions,
                )
                .map(CommandPlan::argv_only),
        }
    }

    fn capture_session_id(
        &self,
        agent_id: &str,
        spawn_cwd: &Path,
        timeout_s: u64,
    ) -> Result<Option<CapturedSession>, ProviderError> {
        let context = CaptureSessionContext {
            agent_id: agent_id.to_string(),
            spawn_cwd: spawn_cwd.to_path_buf(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: None,
        };
        Ok(self
            .capture_session_candidates(&context, timeout_s)?
            .into_iter()
            .next()
            .map(|candidate| candidate.captured))
    }

    fn capture_session_candidates(
        &self,
        context: &CaptureSessionContext,
        timeout_s: u64,
    ) -> Result<Vec<CapturedSessionCandidate>, ProviderError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
        loop {
            let out = crate::provider::session_scan::scan_session_candidates_once(
                self.provider,
                context,
            )?;
            if !out.is_empty() || timeout_s == 0 || std::time::Instant::now() >= deadline {
                return Ok(out);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }


    fn session_is_resumable(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
    ) -> Result<bool, ProviderError> {
        if session_id.is_none() || auth_mode == AuthMode::CompatibleApi {
            return Ok(false);
        }
        Ok(self.caps().resume)
    }


    fn build_resume_command_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        dangerously_skip_permissions: bool,
    ) -> Result<Vec<String>, ProviderError> {
        if !self.session_is_resumable(session_id, auth_mode)? {
            return Err(ProviderError::ResumeUnavailable(format!(
                "{} resume requires session_id",
                provider_wire(self.provider)
            )));
        }
        let Some(session_id) = session_id else {
            return Err(ProviderError::ResumeUnavailable(
                "resume requires session_id".to_string(),
            ));
        };
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => {
                let mut argv = claude_base_command(
                    self,
                    auth_mode,
                    mcp_config,
                    system_prompt,
                    model,
                    dangerously_skip_permissions,
                    false,
                    None,
                )?;
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            // §C1 cr verdict:resume 同 base + `--resume <sid>`(去 --session-id,
            // copilot --resume 接受 id|name)。
            Provider::Copilot => {
                let mut argv =
                    copilot_base_command(auth_mode, mcp_config, system_prompt, model, dangerously_skip_permissions);
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            Provider::Codex | Provider::Grok => {
                Err(ProviderError::ResumeUnavailable(retired_worker(self.provider)))
            }
            Provider::Pi => Err(ProviderError::ResumeUnavailable(
                "pi resume requires the captured exact session path".to_string(),
            )),
            Provider::GeminiCli | Provider::Fake => Err(ProviderError::ResumeUnavailable(format!(
                "{} resume requires session_id",
                provider_wire(self.provider)
            ))),
        }
    }

    fn build_resume_command_plan(
        &self,
        session_id: Option<&SessionId>,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => {
                let Some(session_id) = session_id else {
                    return Err(ProviderError::ResumeUnavailable(
                        "resume requires session_id".to_string(),
                    ));
                };
                let managed = ctx
                    .profile_launch
                    .is_some_and(|profile| profile.managed_mcp_config);
                let model = claude_context_model(ctx);
                let mut argv = claude_base_command(
                    self,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    model,
                    ctx.dangerously_skip_permissions,
                    managed,
                    ctx.effort,
                )?;
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                // Layer 1 self-healing: keep the role name on resume too —
                // makes resumed sessions findable in the picker by agent_id
                // when state.json drifts.
                if let Some(agent_id) = ctx.agent_id_hint {
                    if !agent_id.is_empty() {
                        argv.push("--name".to_string());
                        argv.push(agent_id.to_string());
                    }
                }
                let mut plan = CommandPlan::argv_only(argv);
                plan.provider_projects_root = ctx
                    .profile_launch
                    .and_then(|profile| profile.claude_projects_root.clone());
                plan.managed_mcp_config = managed;
                Ok(plan)
            }
            _ => self
                .build_resume_command_with_context(
                    session_id,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.dangerously_skip_permissions,
                )
                .map(CommandPlan::argv_only),
        }
    }


    fn fork_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        dangerously_skip_permissions: bool,
    ) -> Result<Vec<String>, ProviderError> {
        if !self.caps().fork || auth_mode == AuthMode::CompatibleApi {
            return Err(ProviderError::CapabilityUnsupported(format!(
                "{} does not support native session fork",
                provider_wire(self.provider)
            )));
        }
        let Some(session_id) = session_id else {
            return Err(ProviderError::ResumeUnavailable(
                "fork requires session_id".to_string(),
            ));
        };
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => {
                let mut argv = claude_base_command(
                    self,
                    auth_mode,
                    mcp_config,
                    system_prompt,
                    model,
                    dangerously_skip_permissions,
                    false,
                    None,
                )?;
                argv.push("--session-id".to_string());
                argv.push(next_session_token());
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                argv.push("--fork-session".to_string());
                Ok(argv)
            }
            // Context-free adapter calls cannot materialize the Copilot store fork.
            // The lifecycle path uses `fork_plan`, which receives the prepared isolated
            // backing; never degrade this context-free call to a fresh spawn.
            Provider::Copilot => Err(ProviderError::CapabilityUnsupported(
                "copilot CLI 无 fork 旗标,session-store 不支持 branched continuation".to_string(),
            )),
            Provider::Codex | Provider::Grok => {
                Err(ProviderError::CapabilityUnsupported(retired_worker(self.provider)))
            }
            Provider::Pi | Provider::GeminiCli | Provider::Fake => {
                Err(ProviderError::CapabilityUnsupported(format!(
                    "{} does not support native session fork",
                    provider_wire(self.provider)
                )))
            }
        }
    }

    fn fork_plan(
        &self,
        session_id: Option<&SessionId>,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => {
                if !self.caps().fork || ctx.auth_mode == AuthMode::CompatibleApi {
                    return Err(ProviderError::CapabilityUnsupported(format!(
                        "{} does not support native session fork",
                        provider_wire(self.provider)
                    )));
                }
                let Some(session_id) = session_id else {
                    return Err(ProviderError::ResumeUnavailable(
                        "fork requires session_id".to_string(),
                    ));
                };
                let expected = next_session_token();
                let managed = ctx
                    .profile_launch
                    .is_some_and(|profile| profile.managed_mcp_config);
                let projects_root = ctx
                    .profile_launch
                    .and_then(|profile| profile.claude_projects_root.clone());
                let model = claude_context_model(ctx);
                let mut argv = claude_base_command(
                    self,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    model,
                    ctx.dangerously_skip_permissions,
                    managed,
                    ctx.effort,
                )?;
                // Native new-seat fork: Claude copies the parent (only read)
                // into the preassigned child, so capture binds the child
                // exactly as a fresh spawn. `--session-id` with `--resume` is
                // admitted only together with `--fork-session`.
                argv.push("--session-id".to_string());
                argv.push(expected.clone());
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                argv.push("--fork-session".to_string());
                if let Some(agent_id) = ctx.agent_id_hint {
                    if !agent_id.is_empty() {
                        argv.push("--name".to_string());
                        argv.push(agent_id.to_string());
                    }
                }
                Ok(CommandPlan {
                    argv,
                    expected_session_id: Some(SessionId::new(expected)),
                    provider_projects_root: projects_root,
                    managed_mcp_config: managed,
                })
            }
            Provider::Copilot => {
                if ctx.auth_mode == AuthMode::CompatibleApi {
                    return Err(ProviderError::CapabilityUnsupported(
                        "copilot does not support session fork in compatible_api mode".to_string(),
                    ));
                }
                let profile = ctx.profile_launch.ok_or_else(|| {
                    ProviderError::Command(
                        "copilot fork requires a prepared provider profile".to_string(),
                    )
                })?;
                let expected = profile
                    .env_overlay
                    .get("TEAM_AGENT_INTERNAL_COPILOT_FORK_SESSION_ID")
                    .filter(|value| !value.is_empty())
                    .map(SessionId::new)
                    .ok_or_else(|| {
                        ProviderError::Command(
                            "copilot fork session backing was not materialized".to_string(),
                        )
                    })?;
                let root = profile
                    .env_overlay
                    .get("COPILOT_HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        ProviderError::Command("copilot fork root was not materialized".to_string())
                    })?;
                let mut argv = copilot_base_command(
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.dangerously_skip_permissions,
                );
                argv.push("--resume".to_string());
                argv.push(expected.as_str().to_string());
                Ok(CommandPlan {
                    argv,
                    expected_session_id: Some(expected),
                    provider_projects_root: Some(root),
                    managed_mcp_config: profile.managed_mcp_config,
                })
            }
            _ => self
                .fork_with_context(
                    session_id,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.dangerously_skip_permissions,
                )
                .map(CommandPlan::argv_only),
        }
    }

    fn mcp_config(&self, auth_mode: AuthMode) -> Result<McpConfig, ProviderError> {
        let server = mcp_server_config(auth_mode);
        Ok(McpConfig {
            raw: serde_json::json!({
                "team_orchestrator": server
            }),
        })
    }



}

use crate::provider::wire::{command_name, provider_wire};

/// Codex and Grok workers run only through their contract leaves
/// (`provider: codex|grok`); their legacy worker builders were excised. Only a
/// pre-upgrade state row can still name the legacy key.
fn retired_worker(provider: Provider) -> String {
    format!(
        "legacy {} worker runtime was excised; workers run only through the contract leaf",
        provider_wire(provider)
    )
}

fn auth_mode_wire(auth_mode: AuthMode) -> &'static str {
    match auth_mode {
        AuthMode::Subscription => "subscription",
        AuthMode::OfficialApi => "official_api",
        AuthMode::CompatibleApi => "compatible_api",
    }
}



fn claude_context_model(ctx: ProviderCommandContext<'_>) -> Option<&str> {
    ctx.profile_launch
        .and_then(|profile| profile.command_overrides.model.as_deref())
        .or(ctx.model)
}

fn command_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

pub(crate) fn prompt_needs_native_mcp(prompt: &str) -> bool {
    prompt.contains('\n') || prompt.contains('"')
}

// 0.4.x decoupling step 2: provider-local command builders moved to provider/adapters/.
// Only the entry points the trait impl actually calls are re-imported here;
// The per-provider command helpers and MCP renderers are called from within
// the extracted base-command functions, not directly by this file.
use super::adapters::claude::{claude_base_command, claude_launch_command};
use super::adapters::copilot::copilot_base_command;
use super::adapters::fake::fake_worker_command;

/// Contract C / MUST-8: the per-worker Team Agent MCP server config used by the
/// remaining legacy workers (Copilot `--additional-mcp-config`). Placeholders
/// `{workspace}` / `{agent_id}` / `{team_id}` are substituted at spawn time by
/// [`crate::lifecycle::launch::fill_spawn_placeholders`]; this template MUST NOT
/// contain hardcoded paths or agent/team ids — the probe5 RED probe burned that.
///
/// step3/rt finding (binary c5d22208): bare `command="team-agent"` lets the worker
/// process's PATH resolve to a stale Python CLI install (e.g. `~/.local/bin/team-agent`)
/// that lacks the `mcp-server` subcommand → handshake fails (`MCP startup failed:
/// connection closed: initialize response`). Pin to the absolute path of the CURRENT
/// running binary via `std::env::current_exe()` so the spawned MCP server is the same
/// build that produced this config.
fn mcp_server_config(auth_mode: AuthMode) -> serde_json::Value {
    serde_json::json!({
        "type": "stdio",
        "command": current_team_agent_command(),
        "args": ["mcp-server", "--workspace", "{workspace}"],
        "env": {
            "TEAM_AGENT_WORKSPACE": "{workspace}",
            "TEAM_AGENT_ID": "{agent_id}",
            "TEAM_AGENT_OWNER_TEAM_ID": "{team_id}",
            "TEAM_AGENT_AUTH_MODE": auth_mode_wire(auth_mode),
        }
    })
}

/// Absolute path of the running `team-agent` binary, suitable for an MCP `command`
/// field that must not be PATH-resolved. Uses `std::env::current_exe()`, canonicalizes
/// to drop `/proc/self/exe`-style indirection where supported. Falls back to a
/// well-known absolute install path only if both lookups fail (CI sandboxes); the
/// contract requires `Path::is_absolute(command) == true`, which the fallback honors.
fn current_team_agent_command() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(canon) = std::fs::canonicalize(&exe) {
            return canon.to_string_lossy().to_string();
        }
        return exe.to_string_lossy().to_string();
    }
    "/usr/local/bin/team-agent".to_string()
}

pub(crate) fn next_session_token() -> String {
    use sha2::Digest;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hasher = sha2::Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(counter.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[cfg(test)]
mod fork_materialization_source_guard {
    #[test]
    fn legacy_fork_neither_materializes_backing_nor_injects_into_the_source_pane() {
        let source = include_str!("../lifecycle/launch/fork_agent.rs");
        assert!(
            source.contains("is_retired_provider") && source.contains("unverified (未验证)"),
            "legacy fork must refuse retired rows and unverified providers"
        );
        for gone in [".materialize_fork_backing(", "materialize_codex_fork", "codex_fork", ".inject(", "send_keys"] {
            assert!(!source.contains(gone), "legacy fork must not use {gone}");
        }
    }
}
