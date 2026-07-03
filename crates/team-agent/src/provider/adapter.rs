//! ProviderAdapter trait(per-provider 命令构造 + 能力面)+ registry facade + 占位实现。

use std::path::{Path, PathBuf};
use std::process::Command;

use super::helpers::patterns;
use super::types::{
    AuthHintStatus, CapturedSession, CommandPlan, McpConfig, ProviderCaps,
    ProviderCommandContext, ProviderError, SessionId, StatusPatterns,
};
use super::{AuthMode, Provider};

pub use crate::provider::session_scan::{CapturedSessionCandidate, CaptureSessionContext};

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

    /// CLI 版本字符串(doctor;`adapter.py` version)。I/O → Result。
    fn version(&self) -> Result<String, ProviderError>;

    /// auth 状态提示(doctor;`adapter.py:38` auth_hint)。
    fn auth_hint(&self, auth_mode: AuthMode) -> AuthHintStatus;

    /// 构造 launch 命令(含 MCP 注入 / 权限模式 / system prompt / model)。
    /// 返回可 exec 的 argv(`adapter.py` build_command;`providers.py:54`)。
    fn build_command(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
    ) -> Result<Vec<String>, ProviderError>;

    /// Same as [`ProviderAdapter::build_command`], with the agent tool list supplied
    /// so provider-specific sandbox flags can be computed without guessing.
    fn build_command_with_tools(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        tools: &[&str],
    ) -> Result<Vec<String>, ProviderError>;

    fn build_command_plan(
        &self,
        ctx: ProviderCommandContext<'_>,
    ) -> Result<CommandPlan, ProviderError> {
        self.build_command_with_tools(
            ctx.auth_mode,
            ctx.mcp_config,
            ctx.system_prompt,
            ctx.model,
            ctx.tools,
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
                positive_agent_id_match: false,
                agent_path_match: false,
            })
            .collect())
    }

    /// restart/reset 路径:从已存 transcript/rollout 回收 session_id
    /// (`claude.py:115`)。`None` 合法(找不到)。
    fn recover_session_id(
        &self,
        agent_id: &str,
        spawn_cwd: &std::path::Path,
    ) -> Result<Option<SessionId>, ProviderError>;

    /// 给定 session 是否可 resume(`claude.py:143`)。bug-085:compatible_api
    /// `session_id=None` → 不可 resume(`false`,不崩)。
    fn session_is_resumable(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
    ) -> Result<bool, ProviderError>;

    /// 构造 resume 命令(`providers.py:74`)。不可 resume → `Err(ResumeUnavailable)`。
    fn build_resume_command(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
    ) -> Result<Vec<String>, ProviderError>;

    fn build_resume_command_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        tools: &[&str],
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
            ctx.tools,
        )
        .map(CommandPlan::argv_only)
    }

    /// 构造 fork 命令(`providers.py:99`)。fork 需 caps.fork ∧ auth_mode!=compatible_api;
    /// 不支持 → `Err`。
    fn fork(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
    ) -> Result<Vec<String>, ProviderError>;

    fn fork_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        tools: &[&str],
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
            ctx.tools,
        )
            .map(CommandPlan::argv_only)
    }

    /// 计算本 provider 该用的 MCP server 配置(`adapter.py` mcp_config;claude
    /// compatible_api 走 `ensure_compatible_claude_mcp_config`)。
    fn mcp_config(&self, auth_mode: AuthMode) -> Result<McpConfig, ProviderError>;

    /// 安装 MCP server。gemini 写全局 `~/.gemini/settings.json` 并备份/还原
    /// (`gemini.py:40-78`)——有副作用 I/O,失败可还原 → Result。
    fn install_mcp(&self, config: &McpConfig) -> Result<(), ProviderError>;

    /// idle/processing/trust 识别正则集(`claude.py`/`codex.py` status_patterns)。
    /// pane→status 检测用。返编译失败可能 → Result(`re.error` 容错)。
    fn status_patterns(&self) -> Result<StatusPatterns, ProviderError>;

    /// 校验 model 名对本 provider 合法(`codex debug models` 等;doctor)。
    fn validate_model(&self, model: &str) -> Result<bool, ProviderError>;

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
            Provider::Codex => {
                super::startup_prompt::codex_handle_startup_prompts(transport, target, checks, sleep_s)
            }
            Provider::Claude | Provider::ClaudeCode => {
                super::startup_prompt::claude_handle_startup_prompts(
                    transport, target, checks, sleep_s,
                )
            }
            Provider::Copilot => {
                super::startup_prompt::copilot_handle_startup_prompts(
                    transport, target, checks, sleep_s,
                )
            }
            _ => super::startup_prompt::StartupPromptOutcome::default(),
        }))
        .unwrap_or_default()
    }

    /// Python launch/core.py:235-237 + tmux_prompt.py:124-129 — `runtime.fast` toggles
    /// codex fast mode by sending `/fast` + Enter to the worker pane after spawn.
    /// Providers without a fast-mode toggle are a no-op so upper layers stay
    /// provider-agnostic (F032). Returns whether a toggle was sent.
    fn enable_fast_mode(
        &self,
        transport: &dyn crate::transport::Transport,
        target: &crate::transport::Target,
    ) -> bool {
        match self.provider() {
            Provider::Codex => {
                let keys: Vec<crate::transport::Key> = "/fast"
                    .chars()
                    .map(crate::transport::Key::Char)
                    .chain([crate::transport::Key::Enter])
                    .collect();
                transport.send_keys(target, &keys).is_ok()
            }
            _ => false,
        }
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
            Provider::Codex => ProviderCaps {
                resume: true,
                fork: true,
                native_mcp_config: false,
                writes_global_settings: false,
            },
            // Copilot(C-4-1 cr verdict):resume 走 --resume <sid>;**无 fork** 旗标,
            // session-store 不支持 branched continuation → caps.fork=false 显式拒。
            // native_mcp_config=true(`--additional-mcp-config` 接 inline JSON 或 @file);
            // writes_global_settings=false(session 走 --session-id 预定 UUID,不污染
            // ~/.copilot/mcp-config.json,help 原文 "augments config for this session")。
            Provider::Copilot => ProviderCaps {
                resume: true,
                fork: false,
                native_mcp_config: true,
                writes_global_settings: false,
            },
            Provider::GeminiCli => ProviderCaps {
                resume: false,
                fork: false,
                native_mcp_config: false,
                writes_global_settings: true,
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

    fn version(&self) -> Result<String, ProviderError> {
        if matches!(self.provider, Provider::Fake) {
            return Ok("fake".to_string());
        }
        let output = Command::new(command_name(self.provider))
            .arg("--version")
            .output()
            .map_err(|e| ProviderError::Io(format!("{} --version: {e}", command_name(self.provider))))?;
        if !output.status.success() {
            return Ok("unknown".to_string());
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            Ok("unknown".to_string())
        } else {
            Ok(text)
        }
    }

    fn auth_hint(&self, auth_mode: AuthMode) -> AuthHintStatus {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => claude_auth_hint(auth_mode),
            // C-A-5 cr verdict v2(诚实 MUST-NOT-13) — copilot 无 auth status 子命令
            // (main-help Commands 节仅 completion/help/init/login/mcp/plugin/update/
            // version)。framework 只能弱检测(命令在 PATH + ~/.copilot/config.json
            // 存在),不能假报强 Present;Subscription 档返 PresentWeak,doctor 文案
            // 标"weak / no auth-status command available";Compatible/Official 走 BYOK
            // 路径,有 COPILOT_PROVIDER_BASE_URL 时已脱离 GitHub 登录通道。
            Provider::Copilot => copilot_auth_hint(auth_mode),
            _ => match auth_mode {
                AuthMode::Subscription => AuthHintStatus::Present,
                AuthMode::OfficialApi | AuthMode::CompatibleApi => AuthHintStatus::MissingOrUnknown,
            },
        }
    }

    fn build_command(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
    ) -> Result<Vec<String>, ProviderError> {
        self.build_command_with_tools(auth_mode, mcp_config, system_prompt, model, &[])
    }

    fn build_command_with_tools(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        tools: &[&str],
    ) -> Result<Vec<String>, ProviderError> {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => {
                Ok(claude_launch_command(self, auth_mode, mcp_config, system_prompt, model, tools)?)
            }
            Provider::Codex => Ok(codex_base_command(None, auth_mode, mcp_config, system_prompt, model, tools, None, None)),
            // §C1 worker argv 形态 + C-1/C-5/C-6 cr verdict:
            //   copilot --no-color --no-auto-update [<dangerous|granular>] [--model]
            //          --additional-mcp-config <inline json> --session-id <expected_uuid> -C <cwd>
            // system_prompt 经 spawn env(COPILOT_CUSTOM_INSTRUCTIONS_DIRS)+ per-worker
            // AGENTS.md(launch 路径写文件,见 lifecycle/launch.rs)注入,**不入 argv**
            // (B2 灵魂件降级,C-1-2 禁 silent 写全局)。
            Provider::Copilot => Ok(copilot_base_command(
                auth_mode, mcp_config, system_prompt, model, tools,
            )),
            Provider::GeminiCli => {
                let mut argv = vec!["gemini".to_string()];
                if let Some(model) = model {
                    argv.push("--model".to_string());
                    argv.push(model.to_string());
                }
                Ok(argv)
            }
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
                let managed = ctx.profile_launch.is_some_and(|profile| profile.managed_mcp_config);
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
                    ctx.tools,
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
            // codex.py:105-118 — the profile command overrides (codex_profile / codex_config)
            // ride on `agent["_provider_profile"]`, which only the plan path carries.
            //
            // 0.3.31 Codex capture correction (reverts ad518f8): Codex CLI does
            // NOT accept `--session-id`, so a framework-generated UUID is never
            // matched against Codex's own session_meta.payload.id. Setting
            // expected_session_id caused the apply-time Stage 1 guard to
            // permanently reject the real Codex transcript. Codex capture must
            // anchor on (cwd, spawned_at) instead — handled in
            // `provider/session_scan/codex.rs`.
            Provider::Codex => Ok(CommandPlan::argv_only(codex_base_command(
                None,
                ctx.auth_mode,
                ctx.mcp_config,
                ctx.system_prompt,
                ctx.model,
                ctx.tools,
                ctx.profile_launch.map(|profile| &profile.command_overrides),
                ctx.effort,
            ))),
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
                    ctx.tools,
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
            _ => self
                .build_command_with_tools(
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.tools,
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

    fn recover_session_id(
        &self,
        agent_id: &str,
        spawn_cwd: &Path,
    ) -> Result<Option<SessionId>, ProviderError> {
        Ok(self
            .capture_session_id(agent_id, spawn_cwd, 0)?
            .and_then(|captured| captured.session_id))
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

    fn build_resume_command(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
    ) -> Result<Vec<String>, ProviderError> {
        self.build_resume_command_with_context(session_id, auth_mode, mcp_config, None, None, &[])
    }

    fn build_resume_command_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        tools: &[&str],
    ) -> Result<Vec<String>, ProviderError> {
        if !self.session_is_resumable(session_id, auth_mode)? {
            return Err(ProviderError::ResumeUnavailable(format!(
                "{} resume requires session_id",
                provider_wire(self.provider)
            )));
        }
        let Some(session_id) = session_id else {
            return Err(ProviderError::ResumeUnavailable("resume requires session_id".to_string()));
        };
        match self.provider {
            Provider::Codex => {
                let mut argv = codex_base_command(
                    Some("resume"),
                    auth_mode,
                    mcp_config,
                    system_prompt,
                    model,
                    tools,
                    None,
                    None,
                );
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            Provider::Claude | Provider::ClaudeCode => {
                let mut argv =
                    claude_base_command(self, auth_mode, mcp_config, system_prompt, model, tools, false, None)?;
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            // §C1 cr verdict:resume 同 base + `--resume <sid>`(去 --session-id,
            // copilot --resume 接受 id|name)。
            Provider::Copilot => {
                let mut argv = copilot_base_command_resume(
                    auth_mode, mcp_config, system_prompt, model, tools,
                );
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
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
                    return Err(ProviderError::ResumeUnavailable("resume requires session_id".to_string()));
                };
                let managed = ctx.profile_launch.is_some_and(|profile| profile.managed_mcp_config);
                let model = claude_context_model(ctx);
                let mut argv = claude_base_command(
                    self,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    model,
                    ctx.tools,
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
            Provider::Codex => {
                if !self.session_is_resumable(session_id, ctx.auth_mode)? {
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
                let mut argv = codex_base_command(
                    Some("resume"),
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.tools,
                    ctx.profile_launch.map(|profile| &profile.command_overrides),
                    ctx.effort,
                );
                argv.push(session_id.as_str().to_string());
                Ok(CommandPlan::argv_only(argv))
            }
            _ => self
                .build_resume_command_with_context(
                    session_id,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.tools,
                )
                .map(CommandPlan::argv_only),
        }
    }

    fn fork(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
    ) -> Result<Vec<String>, ProviderError> {
        self.fork_with_context(session_id, auth_mode, mcp_config, None, None, &[])
    }

    fn fork_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        tools: &[&str],
    ) -> Result<Vec<String>, ProviderError> {
        if !self.caps().fork || auth_mode == AuthMode::CompatibleApi {
            return Err(ProviderError::CapabilityUnsupported(format!(
                "{} does not support native session fork",
                provider_wire(self.provider)
            )));
        }
        let Some(session_id) = session_id else {
            return Err(ProviderError::ResumeUnavailable("fork requires session_id".to_string()));
        };
        match self.provider {
            Provider::Codex => {
                let mut argv = codex_base_command(
                    Some("fork"),
                    auth_mode,
                    mcp_config,
                    system_prompt,
                    model,
                    tools,
                    None,
                    None,
                );
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            Provider::Claude | Provider::ClaudeCode => {
                let mut argv =
                    claude_base_command(self, auth_mode, mcp_config, system_prompt, model, tools, false, None)?;
                argv.push("--session-id".to_string());
                argv.push(next_session_token());
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                argv.push("--fork-session".to_string());
                Ok(argv)
            }
            // C-4-2 cr verdict: copilot 无 fork 旗标 + session-store 不支持 branched
            // continuation → 显式 CapabilityUnsupported,**绝不** silent fallback 到
            // restart-from-scratch(MUST-NOT-13 诚实)。本分支理论上不可达(caps.fork=false
            // 已在 fork_with_context 入口拦截,line 582),保留作 totality 守护。
            Provider::Copilot => Err(ProviderError::CapabilityUnsupported(
                "copilot CLI 无 fork 旗标,session-store 不支持 branched continuation".to_string(),
            )),
            Provider::GeminiCli | Provider::Fake => Err(ProviderError::CapabilityUnsupported(format!(
                "{} does not support native session fork",
                provider_wire(self.provider)
            ))),
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
                    return Err(ProviderError::ResumeUnavailable("fork requires session_id".to_string()));
                };
                let expected = next_session_token();
                let managed = ctx.profile_launch.is_some_and(|profile| profile.managed_mcp_config);
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
                    ctx.tools,
                    managed,
                    ctx.effort,
                )?;
                argv.push("--session-id".to_string());
                argv.push(expected.clone());
                argv.push("--resume".to_string());
                argv.push(session_id.as_str().to_string());
                argv.push("--fork-session".to_string());
                Ok(CommandPlan {
                    argv,
                    expected_session_id: Some(SessionId::new(expected)),
                    provider_projects_root: projects_root,
                    managed_mcp_config: managed,
                })
            }
            Provider::Codex => {
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
                let mut argv = codex_base_command(
                    Some("fork"),
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.tools,
                    ctx.profile_launch.map(|profile| &profile.command_overrides),
                    ctx.effort,
                );
                argv.push(session_id.as_str().to_string());
                Ok(CommandPlan::argv_only(argv))
            }
            _ => self
                .fork_with_context(
                    session_id,
                    ctx.auth_mode,
                    ctx.mcp_config,
                    ctx.system_prompt,
                    ctx.model,
                    ctx.tools,
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

    fn install_mcp(&self, config: &McpConfig) -> Result<(), ProviderError> {
        if !matches!(self.provider, Provider::GeminiCli) {
            return Ok(());
        }
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Err(ProviderError::Io("HOME is not set".to_string()));
        };
        let dir = home.join(".gemini");
        std::fs::create_dir_all(&dir)
            .map_err(|e| ProviderError::Io(format!("{}: {e}", dir.display())))?;
        let path = dir.join("settings.json");
        let text = serde_json::to_string_pretty(&config.raw)
            .map_err(|e| ProviderError::Command(format!("serialize mcp config: {e}")))?;
        std::fs::write(&path, text)
            .map_err(|e| ProviderError::Io(format!("{}: {e}", path.display())))?;
        Ok(())
    }

    fn status_patterns(&self) -> Result<StatusPatterns, ProviderError> {
        match self.provider {
            Provider::Claude | Provider::ClaudeCode => patterns(r"[>❯]\s", r"[✶✢✽✻✳·].*…", r"Error|Traceback"),
            Provider::Codex => patterns(r"(›|❯|codex>)", r"•.*esc to interrupt", r"Error|Traceback|panic"),
            Provider::Copilot => patterns(r"(?m)^\s*❯\s*$| / commands · \? help", r"working|processing", r"Error|panic"),
            Provider::GeminiCli | Provider::Fake => patterns(r">", r"working|processing", r"Error|Traceback"),
        }
    }

    fn validate_model(&self, _model: &str) -> Result<bool, ProviderError> {
        Ok(true)
    }
}

use crate::provider::wire::{command_name, provider_wire};

fn auth_mode_wire(auth_mode: AuthMode) -> &'static str {
    match auth_mode {
        AuthMode::Subscription => "subscription",
        AuthMode::OfficialApi => "official_api",
        AuthMode::CompatibleApi => "compatible_api",
    }
}

/// C-A-5 cr verdict v2 — copilot 弱检测(无 auth status 子命令)。
/// 当 copilot 命令在 PATH 且 `~/.copilot/config.json` 存在 → PresentWeak;否则 Missing
/// (PATH 缺)或 MissingOrUnknown(无 config 文件)。Compatible/Official 走 BYOK,
/// 由 profile_launch 端校验(COPILOT_PROVIDER_BASE_URL 等),hint 层报 Unknown。
fn copilot_auth_hint(auth_mode: AuthMode) -> AuthHintStatus {
    if !matches!(auth_mode, AuthMode::Subscription) {
        return AuthHintStatus::Unknown;
    }
    if !command_on_path("copilot") {
        return AuthHintStatus::Missing;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return AuthHintStatus::MissingOrUnknown;
    };
    if home.join(".copilot").join("config.json").exists() {
        AuthHintStatus::PresentWeak
    } else {
        AuthHintStatus::MissingOrUnknown
    }
}

fn claude_auth_hint(auth_mode: AuthMode) -> AuthHintStatus {
    if auth_mode != AuthMode::Subscription {
        return AuthHintStatus::MissingOrUnknown;
    }
    if !command_on_path("claude") {
        return AuthHintStatus::Missing;
    }
    let output = match Command::new("claude").args(["auth", "status"]).output() {
        Ok(output) => output,
        Err(_) => return AuthHintStatus::MissingOrUnknown,
    };
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    let status = serde_json::from_str::<serde_json::Value>(text.trim()).unwrap_or_default();
    if status.get("loggedIn").and_then(serde_json::Value::as_bool) == Some(true)
        || output.status.success()
    {
        AuthHintStatus::Present
    } else {
        AuthHintStatus::Missing
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

/// Shared JSON renderer used by the codex `-c mcp_servers.*=...` overrides
/// and any other adapter that wants inline JSON. String values are quoted
/// (with quote escape); other values use serde's default Display.
pub(crate) fn json_inline(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        other => other.to_string(),
    }
}

// 0.4.x decoupling step 2: provider-local command builders moved to provider/adapters/.
// Only the entry points the trait impl actually calls are re-imported here;
// the per-provider helper fns (dangerous_auto_approve, permission flags,
// disallowed_tools, sandbox_mode, mcp_overrides) are called from within the
// extracted base_command fns, not directly by this file.
use super::adapters::claude::{claude_base_command, claude_launch_command};
use super::adapters::codex::codex_base_command;
use super::adapters::copilot::{copilot_base_command, copilot_base_command_resume};
use super::adapters::fake::fake_worker_command;

/// Contract C / MUST-8: the per-worker Team Agent MCP server config used by Claude
/// (`--mcp-config`) and Codex (`-c mcp_servers.*` injection). Placeholders
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
