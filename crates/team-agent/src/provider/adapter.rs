//! ProviderAdapter trait(per-provider 命令构造 + 能力面)+ registry facade + 占位实现。

use std::path::{Path, PathBuf};
use std::process::Command;

use super::helpers::{find_session_id, parse_jsonl_records, patterns};
use super::types::{
    AuthHintStatus, CaptureVia, CapturedSession, CommandPlan, Confidence, McpConfig,
    ProviderCaps, ProviderCommandContext, ProviderError, RolloutPath,
    SessionId, StatusPatterns,
};
use super::{AuthMode, Provider};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSessionContext {
    pub agent_id: String,
    pub spawn_cwd: PathBuf,
    pub pane_id: Option<String>,
    pub pane_pid: Option<u32>,
    pub spawned_at: Option<String>,
    pub expected_session_id: Option<SessionId>,
    pub provider_projects_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSessionCandidate {
    pub captured: CapturedSession,
    pub positive_agent_id_match: bool,
    pub agent_path_match: bool,
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
            Provider::Codex => Ok(codex_base_command(None, auth_mode, mcp_config, system_prompt, model, tools, None)),
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
            // `scan_session_candidates_once` below.
            Provider::Codex => Ok(CommandPlan::argv_only(codex_base_command(
                None,
                ctx.auth_mode,
                ctx.mcp_config,
                ctx.system_prompt,
                ctx.model,
                ctx.tools,
                ctx.profile_launch.map(|profile| &profile.command_overrides),
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
            let out = scan_session_candidates_once(self.provider, context)?;
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
                );
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            Provider::Claude | Provider::ClaudeCode => {
                let mut argv =
                    claude_base_command(self, auth_mode, mcp_config, system_prompt, model, tools, false)?;
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
                );
                argv.push(session_id.as_str().to_string());
                Ok(argv)
            }
            Provider::Claude | Provider::ClaudeCode => {
                let mut argv =
                    claude_base_command(self, auth_mode, mcp_config, system_prompt, model, tools, false)?;
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

fn scan_session_candidates_once(
    provider: Provider,
    context: &CaptureSessionContext,
) -> Result<Vec<CapturedSessionCandidate>, ProviderError> {
    // §C4 + cr verdict: copilot session 真相源是 ~/.copilot/session-store.db(sqlite),
    // 不是 jsonl 流。点查 sessions(cwd==spawn_cwd)取最新行,**禁** 走目录扫描
    // (PERF P2 不放大;sqlite 点查天然有界)。decoy 文件不进 parse_session_records,
    // 不会"被毒文件炸"。
    if matches!(provider, Provider::Copilot) {
        return Ok(scan_copilot_session_store(context));
    }
    let candidates = candidate_session_files(provider, context)?;
    let mut out = Vec::new();
    for candidate in candidates {
        let path = candidate.path;
        // P2 (C-P2-1/4) / Python claude.py:432 — bounded HEAD read (session_meta / cwd /
        // sessionId live in the file head; Python stops at 200 lines). A poisoned
        // (invalid UTF-8) tail must not silently drop the candidate the way a
        // whole-file read_to_string did.
        let Ok(text) = read_head_text(&path, CAPTURE_HEAD_BYTES) else {
            continue;
        };
        let records = parse_session_records(&text);
        if records.is_empty() {
            continue;
        }
        if candidate.requires_cwd_match
            && !provider_home_records_match_spawn_cwd(&records, &context.spawn_cwd)
        {
            continue;
        }
        let session_id = records.iter().find_map(find_session_id);
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && session_id.is_some()
            && !records.iter().any(has_cwd_field)
        {
            continue;
        }
        let captured_via = if session_id.is_some() {
            CaptureVia::FsWatch
        } else {
            CaptureVia::FsMtimeFallback
        };
        let attribution_confidence = if session_id.is_some() {
            Confidence::High
        } else {
            Confidence::Low
        };
        let positive_agent_id_match = candidate_text_has_team_agent_id(&text, context);
        let agent_path_match = candidate_path_matches_agent_id(&path, context);
        // P0 (lane-046-capture-gap): Claude leader transcripts must NEVER be
        // returned as a worker capture candidate. The macmini repro showed a
        // 590MB leader transcript (sessionId=ea059b82, customTitle="claude
        // leader") being attributed to a fresh release-engineer worker via the
        // cwd+spawned_at time window. Filter by leader marker in the head
        // records — limited to Claude/ClaudeCode (codex/copilot transcripts
        // don't use customTitle/agentName the same way).
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && claude_records_have_leader_marker(&records)
        {
            continue;
        }
        out.push(CapturedSessionCandidate {
            captured: CapturedSession {
                session_id: session_id.map(SessionId::new),
                rollout_path: Some(RolloutPath::new(path)),
                captured_via,
                attribution_confidence,
                spawn_cwd: context.spawn_cwd.clone(),
            },
            positive_agent_id_match,
            agent_path_match,
        });
    }
    // 0.3.31 Codex capture correction: HARD (cwd, spawned_at) filter.
    // Codex CLI does NOT honor `--session-id` so we cannot use
    // expected_session_id semantics. The only safe identity boundary is:
    //   * session_meta.payload.cwd == spawn_cwd (already filtered above via
    //     requires_cwd_match), AND
    //   * session_meta.payload.timestamp >= spawned_at - small_grace, OR file
    //     mtime >= spawned_at - small_grace (the candidate must be POST-spawn).
    // Candidates older than the current spawn are pre-reset / pre-restart
    // remnants and MUST be dropped, not merely de-prioritized — otherwise the
    // single-candidate allocator path picks them, and the Stage 1 mismatch
    // guard later rejects them, producing the 0.4.4 attribution_ambiguous loop.
    if matches!(provider, Provider::Codex) {
        if let Some(spawned_at) = context.spawned_at.as_deref().and_then(parse_spawned_at) {
            // 5-second grace: allow for clock skew between Codex's session
            // timestamp and our spawned_at (Codex records LOCAL time before
            // RFC3339-encoding, framework records UTC; small skew possible
            // across midnight or DST boundary).
            let grace = std::time::Duration::from_secs(5);
            let cutoff = spawned_at.checked_sub(grace).unwrap_or(spawned_at);
            out.retain(|candidate| {
                let path = match candidate.captured.rollout_path.as_ref() {
                    Some(p) => p.as_path(),
                    None => return false,
                };
                std::fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .map(|mtime| mtime >= cutoff)
                    .unwrap_or(false)
            });
        }
    }

    // E6 层1·C(机会性兜底):若盘上真有 expected_session_id 命名的 transcript(claude 哪天
    // 真采用 --session-id,或别的 provider 本就采用),直接唯一命中,省去时间窗扫描。
    // 命不中(交互式 claude 现实:不落 <expected>.jsonl)→ 回落 B。
    if let Some(expected) = context.expected_session_id.as_ref() {
        if let Some(hit) = out.iter().find(|candidate| {
            candidate
                .captured
                .session_id
                .as_ref()
                .is_some_and(|session| session.as_str() == expected.as_str())
        }) {
            return Ok(vec![hit.clone()]);
        }
        // Stage 1 (identity-boundary unified plan, architect direction
        // 2026-06-23): for Claude/ClaudeCode with an expected session id,
        // an exact-id miss MUST NOT fall back to same-cwd latest. Pre-fix
        // the scanner returned every same-cwd candidate sorted with
        // expected-first; with no exact match, that handed `frontend`'s
        // transcript to `reviewer` in the AI-sync repro. Mirror Copilot's
        // stricter contract: keep only candidates with a positive worker
        // identity match (TEAM_AGENT_ID literal in the transcript head, or
        // path-encoded agent_id). If none, return empty — capture stays
        // pending/ambiguous, which is safer than misattribution.
        //
        // Bug 2 (0.4.2 P0) reinforcement: even when positive-identity
        // filtering returns no candidates, do NOT let downstream
        // time-window narrowing run for Claude when expected_session_id
        // misses — that's exactly the leader-session-leak symptom the
        // architect cataloged in .team/artifacts/bug-042-restart-scope-and-session.md.
        if matches!(provider, Provider::Claude | Provider::ClaudeCode) {
            let positive_only: Vec<CapturedSessionCandidate> = out
                .iter()
                .filter(|candidate| candidate.positive_agent_id_match || candidate.agent_path_match)
                .cloned()
                .collect();
            return Ok(positive_only);
        }
    }
    // E6 层1·B(主路径,交互式现实):cwd 匹配但盘上有多个 sibling transcript(claude 自生成,
    // 不采用预定 UUID)→ 用 spawn 时间窗唯一选:只留 mtime >= spawned_at 的候选,打破歧义。
    // spawned_at 缺/无法解析时不收窄(保守,维持既有行为)。
    if context.expected_session_id.is_none() || out.len() > 1 {
        if let Some(spawned_at) = context.spawned_at.as_deref().and_then(parse_spawned_at) {
            let within: Vec<CapturedSessionCandidate> = out
                .iter()
                .filter(|candidate| {
                    candidate
                        .captured
                        .rollout_path
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p.as_path()).and_then(|m| m.modified()).ok())
                        .is_some_and(|mtime| mtime >= spawned_at)
                })
                .cloned()
                .collect();
            // 只有当时间窗把候选收成唯一时才采用(收成 0 或仍多义则不强行,交给上层 ambiguous)。
            if within.len() == 1 {
                return Ok(within);
            }
        }
    }
    // Non-Claude / non-strict providers with an expected id but no exact
    // match: order expected-first so the allocator's `unique_available_candidate`
    // sees the deterministically preferred candidate. Claude/ClaudeCode took
    // the strict positive-only return above and never reaches here.
    if let Some(expected) = context.expected_session_id.as_ref() {
        out.sort_by_key(|candidate| {
            candidate
                .captured
                .session_id
                .as_ref()
                .is_none_or(|session| session.as_str() != expected.as_str())
        });
    }
    Ok(out)
}

/// 解析 state 里的 `spawned_at`(RFC3339)为 SystemTime,用于 spawn 时间窗候选筛选。
/// 解析失败 → None(调用方据此不收窄时间窗,保守维持既有行为)。
fn parse_spawned_at(raw: &str) -> Option<std::time::SystemTime> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| std::time::SystemTime::from(dt.with_timezone(&chrono::Utc)))
}

/// E6 层1·B:把 spawn cwd 映射成 claude transcript 子目录 `~/.claude/projects/<encoded>`。
/// claude 用 **canonical(realpath)** cwd 且把每个 `/` 替换成 `-`(实证:cwd
/// `/private/tmp/x` → `-private-tmp-x`;macOS `/tmp`→`/private/tmp` 必须先 canonical)。
/// canonical 失败(目录已不在)退回原始路径,仍尽力编码。dir 不存在也返回(调用方
/// `collect_optional_candidate_files` 对不存在目录是 no-op)。
fn claude_projects_dir_for_cwd(home: &Path, spawn_cwd: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(spawn_cwd).unwrap_or_else(|_| spawn_cwd.to_path_buf());
    let encoded = encode_claude_projects_dir(&canonical.to_string_lossy());
    if encoded.is_empty() {
        return None;
    }
    Some(home.join(".claude").join("projects").join(encoded))
}

/// 0.4.6 Stage 4: Claude CLI's project-dir encoding rule. Collapse every
/// non-ASCII-alphanumeric character (slashes, dots, spaces, punctuation,
/// non-ASCII codepoints like Chinese) into a single `-`. The pre-fix
/// implementation only replaced `/` → `-` which silently produced wrong
/// directory names for any cwd containing Chinese / spaces / dots
/// (`/Users/alauda/.../agent前沿探索/多agent协作` produced raw UTF-8 while
/// Claude actually creates `-Users-alauda-...-agent------agent--`).
///
/// Note: each non-alnum CHARACTER produces one `-`. A 2-char Chinese word
/// like "协作" becomes 2 dashes. Adjacent non-alnums each contribute one
/// dash (Claude does NOT collapse runs).
fn encode_claude_projects_dir(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    out
}

/// §C4 cr verdict — copilot session 真相源 sqlite 点查。
///
/// 路径:`<HOME>/.copilot/session-store.db`,sessions 表(id/cwd/created_at/updated_at)
/// where `cwd == context.spawn_cwd` 取 updated_at 最新行。**绝不**全文件扫描、**绝不**
/// 走 `parse_session_records`(jsonl)路径 → decoy 毒文件不会触碰任何解析器。
///
/// 失败(HOME 缺、db 缺、表缺、sqlite 错)统一返回空 candidate 列表,与既有
/// `collect_optional_candidate_files` 同精神(absent → empty)。
fn scan_copilot_session_store(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let db_path = home.join(".copilot").join("session-store.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    // E11 层1(本机实锤):copilot honor `--session-id`(sessions.id == 注入的 expected_session_id),
    // 故 worker 权威 id 可靠在 db。**expected-id 优先点查**:命中即返(High,直接根治 leader/worker
    // 同 cwd 共享 db 时 latest-wins 误抓 leader 的 bug)。expected 查无 → **不 promote**(E6 铁律:
    // 不硬写不在盘的假 session),回落 cwd-latest 让收敛重试。
    if let Some(expected) = context.expected_session_id.as_ref() {
        let hit: Option<String> = conn
            .prepare("select id from sessions where id = ?1 limit 1")
            .ok()
            .and_then(|mut stmt| {
                stmt.query_row([expected.as_str()], |row| row.get::<_, String>(0)).ok()
            });
        if let Some(session_id) = hit {
            return vec![copilot_candidate(session_id, &db_path, context)];
        }
        // expected 设了但 db 无该 id → 返空(收敛重试),绝不回落抓别人(尤其 leader)的 latest。
        return Vec::new();
    }
    // 无 expected → **返空**(保守,不 cwd-latest 猜)。
    // E11 层1 兜底洞(architect 核实):leader+worker 同 cwd 共享 db 时,cwd-latest 可能抓 leader
    // 的 session;而 allocator 的 claimed 去重只扫 state.agents,**leader 在 state.leader/team_owner
    // 不在 agents → 兜不住**;且 leader 的 copilot session_id 运行期不入 state(team_owner 只存
    // leader_session_uuid,非 copilot db id),故无从显式排除。所幸 copilot build_command_plan **总**
    // 注入 --session-id(expected_session_id 恒 Some)→ 真实 copilot worker 永走上面点查路径,
    // 此 expected=None 分支对真实 worker 不可达。故直接返空最干净:不猜、绝不把 leader session 分给
    // worker。db 留 _。
    let _ = (&db_path, &conn);
    Vec::new()
}

fn copilot_candidate(
    session_id: String,
    db_path: &Path,
    context: &CaptureSessionContext,
) -> CapturedSessionCandidate {
    CapturedSessionCandidate {
        captured: CapturedSession {
            session_id: Some(SessionId::new(session_id)),
            rollout_path: Some(RolloutPath::new(db_path.to_path_buf())),
            captured_via: CaptureVia::FsWatch,
            attribution_confidence: Confidence::High,
            spawn_cwd: context.spawn_cwd.clone(),
        },
        positive_agent_id_match: false,
        agent_path_match: false,
    }
}

fn command_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

struct SessionCandidate {
    path: PathBuf,
    requires_cwd_match: bool,
}

fn candidate_session_files(
    provider: Provider,
    context: &CaptureSessionContext,
) -> Result<Vec<SessionCandidate>, ProviderError> {
    let mut out = Vec::new();
    if let Some(root) = context.provider_projects_root.as_ref() {
        collect_optional_candidate_files(root, &context.agent_id, &mut out)?;
    }
    collect_candidate_files(&context.spawn_cwd, &context.agent_id, 0, false, &mut out)?;
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        match provider {
            Provider::Codex => {
                collect_optional_candidate_files(&home.join(".codex").join("sessions"), &context.agent_id, &mut out)?;
            }
            Provider::Claude | Provider::ClaudeCode => {
                // Blocker-2 Layer-1 (prerelease 0.4.0): the activity rollout_path
                // MUST be the real transcript under .claude/projects/<encoded cwd>/
                // <session_id>.jsonl. ~/.claude/sessions/<pid>.json is a small
                // session-metadata file (~300-400 bytes) that contains a
                // sessionId but no lifecycle records; if it wins capture, the
                // activity classifier reads it forever and never sees the real
                // transcript. Architect verdict (bugs-prerelease-blockers.md §138):
                // do not let .claude/sessions/<pid>.json become rollout_path.
                // Skip that directory entirely; resume backing probe still scans
                // it via its own path checks in restart/common.rs.
                // E6 层1·B:优先锚到 ~/.claude/projects/<canonical spawn_cwd 编码> 子目录
                // (claude 把 cwd 的 '/' 编码成 '-';交互式 worker 的真实 transcript 落在此),
                // 而非全 projects 树盲扫(交互式 claude 自生成 UUID,锚 cwd 子目录 + 时间窗才能唯一选)。
                if let Some(dir) = claude_projects_dir_for_cwd(&home, &context.spawn_cwd) {
                    collect_optional_candidate_files(&dir, &context.agent_id, &mut out)?;
                }
                collect_optional_candidate_files(&home.join(".claude").join("projects"), &context.agent_id, &mut out)?;
            }
            // §C4 cr verdict + 设计 §C: copilot session 真相源是 ~/.copilot/session-store.db
            // (sqlite 点查 sessions.cwd==spawn_cwd 最新行)和 ~/.copilot/session-state/<uuid>/
            // workspace.yaml — **不走全文件扫描**(PERF P2 禁不放大)。主路径是
            // build_command_plan 预定 UUID(--session-id <expected>)→ pending_session_id
            // 直接命中,这里只补 sqlite 查询的二期入口。一期返空,resume 走 caps 校验。
            Provider::Copilot | Provider::GeminiCli | Provider::Fake => {}
        }
    }
    out.sort_by(|a, b| {
        a.requires_cwd_match
            .cmp(&b.requires_cwd_match)
            .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.requires_cwd_match == b.requires_cwd_match);
    cap_candidates_by_mtime(&mut out, CAPTURE_CANDIDATE_CAP);
    Ok(out)
}

/// P2 (C-P2-2/3) / Python claude.py:300 — candidates are capped to the newest `cap`
/// by mtime (descending priority: old candidates must not crowd out new ones; the cap
/// may be raised above Python's 300 but never lowered). The existing selection
/// ordering of the survivors is preserved.
const CAPTURE_CANDIDATE_CAP: usize = 300;

/// P2 (C-P2-1): head window ≥ Python's 200-line read (meta fields live in the head).
const CAPTURE_HEAD_BYTES: u64 = 65_536;

fn cap_candidates_by_mtime(out: &mut Vec<SessionCandidate>, cap: usize) {
    if out.len() <= cap {
        return;
    }
    let mut ranked: Vec<(std::time::SystemTime, usize)> = out
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let mtime = std::fs::metadata(&candidate.path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (mtime, index)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    let keep: std::collections::BTreeSet<usize> =
        ranked.into_iter().take(cap).map(|(_, index)| index).collect();
    let mut index = 0;
    out.retain(|_| {
        let kept = keep.contains(&index);
        index += 1;
        kept
    });
}

/// P2: bounded head read, truncated to the last complete line (a cut record must not
/// reach the JSONL parser); lossy UTF-8 so a mid-codepoint boundary stays safe.
fn read_head_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes)?;
    let complete = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(last_newline) => &bytes[..=last_newline],
        None => &bytes[..],
    };
    Ok(String::from_utf8_lossy(complete).into_owned())
}

fn collect_optional_candidate_files(
    dir: &Path,
    agent_id: &str,
    out: &mut Vec<SessionCandidate>,
) -> Result<(), ProviderError> {
    if dir.exists() {
        let _ = collect_candidate_files(dir, agent_id, 0, true, out);
    }
    Ok(())
}

fn collect_candidate_files(
    dir: &Path,
    agent_id: &str,
    depth: usize,
    requires_cwd_match: bool,
    out: &mut Vec<SessionCandidate>,
) -> Result<(), ProviderError> {
    if depth > 4 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if depth == 0 => return Err(ProviderError::Io(format!("{}: {e}", dir.display()))),
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            collect_candidate_files(&path, agent_id, depth.saturating_add(1), requires_cwd_match, out)?;
        } else if looks_like_session_file(&path, agent_id) {
            out.push(SessionCandidate {
                path,
                requires_cwd_match,
            });
        }
    }
    Ok(())
}

fn looks_like_session_file(path: &Path, agent_id: &str) -> bool {
    // F5/N11/Contract D: Team Agent's own runtime/log JSONL must never be picked up
    // as a provider transcript. `.team/logs/events.jsonl` and the rest of the
    // `.team/runtime/` tree are framework files, not Codex/Claude rollout. Reject
    // anything under `.team/` BEFORE the filename-shape match, so honest "no valid
    // rollout" yields `rollout_path=None` instead of fake idle from our own logs.
    if path_is_under_team_runtime(path) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.ends_with(".jsonl")
        || name.ends_with(".json")
        || name.contains("session")
        || name.contains("rollout")
        || (!agent_id.is_empty() && name.contains(agent_id))
}

fn parse_session_records(text: &str) -> Vec<serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Array(items)) => items,
        Ok(value) => vec![value],
        Err(_) => parse_jsonl_records(text),
    }
}

fn provider_home_records_match_spawn_cwd(records: &[serde_json::Value], spawn_cwd: &Path) -> bool {
    let cwd_values: Vec<String> = records.iter().filter_map(record_cwd).collect();
    !cwd_values.is_empty()
        && cwd_values
            .iter()
            .any(|cwd| paths_equivalent(Path::new(cwd), spawn_cwd))
}

fn candidate_text_has_team_agent_id(text: &str, context: &CaptureSessionContext) -> bool {
    let id = context.agent_id.as_str();
    if id.is_empty() {
        return false;
    }
    [
        format!("\"TEAM_AGENT_ID\":\"{id}\""),
        format!("\"TEAM_AGENT_ID\": \"{id}\""),
        format!("TEAM_AGENT_ID={id}"),
        format!("env.TEAM_AGENT_ID=\"{id}\""),
        format!("env.TEAM_AGENT_ID=\\\"{id}\\\""),
        format!("\"TEAM_AGENT_AGENT_ID\":\"{id}\""),
        format!("\"TEAM_AGENT_AGENT_ID\": \"{id}\""),
        format!("TEAM_AGENT_AGENT_ID={id}"),
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn candidate_path_matches_agent_id(path: &Path, context: &CaptureSessionContext) -> bool {
    let id = context.agent_id.as_str();
    if id.is_empty() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let dashed = id.replace('_', "-");
    name.contains(id) || name.contains(&dashed)
}

fn record_cwd(record: &serde_json::Value) -> Option<String> {
    record
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            record
                .get("session_meta")
                .and_then(|v| v.get("payload"))
                .or_else(|| record.get("payload"))
                .and_then(|v| v.get("cwd"))
                .and_then(serde_json::Value::as_str)
        })
        .map(ToString::to_string)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right || left.parent().is_some_and(|parent| parent == right)
}

/// `true` iff any path component is `.team` (the Team Agent runtime/logs root) — used
/// to gate session-file detection so `<workspace>/.team/logs/events.jsonl`,
/// `.team/runtime/team.db`, etc. are NEVER mistaken for a provider transcript.
fn path_is_under_team_runtime(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new(".team"))
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

/// P0 (lane-046-capture-gap): detect Claude leader transcripts by their head
/// marker records. A leader transcript head contains records of the form
/// `{"type":"custom-title","customTitle":"claude leader",...}` or
/// `{"type":"agent-name","agentName":"claude leader",...}` written when the
/// leader pane starts. Workers must never be attributed to such transcripts.
/// E57 (lane-046-capture-gap postflight): expose the Claude leader-marker
/// scanner for the event-log repair path. `recover_resume_session_from_events`
/// must apply the SAME marker filter the capture allocator applies, otherwise
/// a stale `session.captured` event from a pre-fix run still pulls the leader
/// transcript onto a worker on the next restart.
///
/// Returns `true` ONLY for Claude/ClaudeCode providers when the rollout file's
/// head records carry `customTitle == "claude leader"` or `agentName ==
/// "claude leader"` (case-insensitive). Other providers always return `false`
/// — codex/copilot transcripts don't use those fields the same way.
pub(crate) fn rollout_path_has_claude_leader_marker(
    provider: crate::provider::Provider,
    rollout_path: &Path,
) -> bool {
    if !matches!(
        provider,
        crate::provider::Provider::Claude | crate::provider::Provider::ClaudeCode
    ) {
        return false;
    }
    let Ok(text) = read_head_text(rollout_path, CAPTURE_HEAD_BYTES) else {
        return false;
    };
    let records = parse_session_records(&text);
    claude_records_have_leader_marker(&records)
}

fn claude_records_have_leader_marker(records: &[serde_json::Value]) -> bool {
    records.iter().any(|record| {
        let custom_title = record
            .get("customTitle")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        let agent_name = record
            .get("agentName")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        matches!(custom_title.as_deref(), Some("claude leader"))
            || matches!(agent_name.as_deref(), Some("claude leader"))
    })
}

fn has_cwd_field(record: &serde_json::Value) -> bool {
    record_cwd(record).is_some()
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
mod lane_046_leader_marker_tests {
    use super::*;

    #[test]
    fn claude_leader_marker_in_custom_title_is_detected() {
        let records = vec![serde_json::json!({
            "type": "custom-title",
            "customTitle": "claude leader",
            "sessionId": "ea059b82",
        })];
        assert!(
            claude_records_have_leader_marker(&records),
            "customTitle=='claude leader' must be detected as leader marker"
        );
    }

    #[test]
    fn claude_leader_marker_in_agent_name_is_detected() {
        let records = vec![serde_json::json!({
            "type": "agent-name",
            "agentName": "claude leader",
            "sessionId": "ea059b82",
        })];
        assert!(
            claude_records_have_leader_marker(&records),
            "agentName=='claude leader' must be detected as leader marker"
        );
    }

    #[test]
    fn claude_worker_records_have_no_leader_marker() {
        let records = vec![
            serde_json::json!({
                "type": "custom-title",
                "customTitle": "claude release-engineer",
                "sessionId": "abc12345",
            }),
            serde_json::json!({
                "type": "user",
                "content": "Team Agent message from leader: do X",
                "sessionId": "abc12345",
            }),
        ];
        assert!(
            !claude_records_have_leader_marker(&records),
            "worker transcripts must NOT be flagged as leader marker (the \
             literal 'claude leader' in user content does not count — only \
             customTitle/agentName fields)"
        );
    }

    #[test]
    fn claude_leader_marker_is_case_insensitive() {
        let records = vec![serde_json::json!({
            "customTitle": "Claude Leader",
        })];
        assert!(
            claude_records_have_leader_marker(&records),
            "leader marker detection must be case-insensitive"
        );
    }
}

#[cfg(test)]
mod e6_session_attribution_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp_root(tag: &str) -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-e6-attr-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_transcript(dir: &Path, uuid: &str, cwd: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{uuid}.jsonl"));
        let line = serde_json::json!({
            "sessionId": uuid,
            "cwd": cwd.to_string_lossy(),
        });
        std::fs::write(&path, format!("{line}\n")).unwrap();
        path
    }

    #[test]
    fn claude_projects_dir_for_cwd_encodes_slashes_to_dashes() {
        let home = Path::new("/home/u");
        // 用一个真实存在的 cwd 让 canonicalize 成功(否则退回原始);用 tmp。
        let cwd = tmp_root("encode");
        let got = claude_projects_dir_for_cwd(home, &cwd).unwrap();
        let canon = std::fs::canonicalize(&cwd).unwrap();
        let expected_leaf = encode_claude_projects_dir(&canon.to_string_lossy());
        assert_eq!(got, home.join(".claude").join("projects").join(expected_leaf));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// **0.4.6 Stage 4 RED**: encode every non-[A-Za-z0-9] character to `-`,
    /// matching Claude CLI's actual project-dir naming rule. Pre-fix code
    /// only replaced `/` → `-` and silently produced wrong directories for
    /// non-ASCII / dotted / spaced cwds — invisible to the scanner.
    #[test]
    fn encode_claude_projects_dir_parity_with_real_claude_naming() {
        // Pure-ASCII parity (the pre-fix happy path).
        assert_eq!(
            encode_claude_projects_dir("/Users/alauda/code"),
            "-Users-alauda-code"
        );
        // The user's actual project root (Chinese):
        //   /Users/alauda/Documents/code/agent前沿探索/多agent协作
        // Each Chinese character → one `-`. "前沿探索"=4 chars → 4 dashes,
        // "多agent协作" = "多"+"agent"+"协作" = 1 + (alphanumeric kept) + 2.
        assert_eq!(
            encode_claude_projects_dir(
                "/Users/alauda/Documents/code/agent前沿探索/多agent协作"
            ),
            "-Users-alauda-Documents-code-agent------agent--"
        );
        // Dots and spaces also collapse.
        assert_eq!(
            encode_claude_projects_dir("/Users/foo bar.baz/v1.2"),
            "-Users-foo-bar-baz-v1-2"
        );
        // Hidden directory ".team" → "-team".
        assert_eq!(
            encode_claude_projects_dir("/proj/.team/runtime"),
            "-proj--team-runtime"
        );
    }

    #[test]
    fn parse_spawned_at_rfc3339_roundtrips_and_rejects_junk() {
        assert!(parse_spawned_at("2026-06-10T21:40:00+00:00").is_some());
        assert!(parse_spawned_at("not-a-date").is_none());
        assert!(parse_spawned_at("").is_none());
    }

    #[test]
    fn scan_expected_session_id_hit_returns_only_that_candidate() {
        // C 兜底:盘上恰有 <expected>.jsonl(假设 claude 哪天真采用)→ 唯一命中,忽略 sibling。
        let base = tmp_root("c-hit");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        write_transcript(&proj, "11111111-1111-4111-8111-111111111111", &cwd);
        write_transcript(&proj, "22222222-2222-4222-8222-222222222222", &cwd);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new("22222222-2222-4222-8222-222222222222")),
            provider_projects_root: Some(proj.clone()),
        };
        let out = scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert_eq!(out.len(), 1, "expected-id hit must collapse to the single match");
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            "22222222-2222-4222-8222-222222222222"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_spawn_time_window_disambiguates_two_siblings() {
        // B 主路径:claude 不采用预定 UUID,盘上两个自生成 sibling 都匹配 cwd。
        // 只有一个在 spawn 时间窗内(mtime >= spawned_at)→ 时间窗唯一选出它。
        let base = tmp_root("b-window");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        let old = write_transcript(&proj, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &cwd);
        let new = write_transcript(&proj, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", &cwd);
        // 把 old 的 mtime 设到很久以前,new 保持现在;spawned_at = 两者之间。
        let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        filetime_set(&old, long_ago);
        // spawned_at 取一个介于 old 与 new 之间、肯定早于 new 真实 mtime 的时刻(2020 年)。
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            expected_session_id: None,
            provider_projects_root: Some(proj.clone()),
        };
        let out = scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert_eq!(out.len(), 1, "time window must collapse two siblings to one");
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "the in-window (recent) transcript must win"
        );
        let _ = new;
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_no_spawned_at_keeps_both_siblings_ambiguous() {
        // 保守:spawned_at 缺 → 不收窄时间窗,两 sibling 仍并存(交上层 ambiguous 处理)。
        let base = tmp_root("b-noamb");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        write_transcript(&proj, "cccccccc-cccc-4ccc-8ccc-cccccccccccc", &cwd);
        write_transcript(&proj, "dddddddd-dddd-4ddd-8ddd-dddddddddddd", &cwd);
        let ctx = CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: Some(proj.clone()),
        };
        let out = scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert!(out.len() >= 2, "no spawned_at → no time-window narrowing");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        // 用 utimensat 经 std:无直接 set_mtime,借 filetime-free 方式:写后用 File::set_modified。
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }

    /// Bug 2 (0.4.2 P0): when expected_session_id is set (restart --allow-fresh
    /// pre-allocates a UUID via --session-id) but the Claude CLI didn't adopt
    /// it (no <expected>.jsonl on disk), capture must NOT fall back to
    /// time-window narrowing over leader transcripts sharing the same cwd.
    /// Pre-fix: leader transcript with later mtime would be silently picked
    /// up as the worker session. Post-fix: empty list returned (caller treats
    /// as session-not-yet-captured and retries on next tick).
    #[test]
    fn scan_expected_session_id_miss_refuses_to_pick_leader_sibling() {
        let base = tmp_root("strict-no-leader-fallback");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let proj = base.join("projects");
        // Simulate the leader's transcript and a stale earlier transcript in
        // the same cwd. NEITHER matches expected_session_id.
        let leader = write_transcript(&proj, "11111111-1111-4111-8111-111111111111", &cwd);
        let stale = write_transcript(&proj, "22222222-2222-4222-8222-222222222222", &cwd);
        let _ = (leader, stale);
        let ctx = CaptureSessionContext {
            agent_id: "claude-worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            // Worker was spawned with --session-id <expected> but Claude
            // didn't write <expected>.jsonl.
            expected_session_id: Some(SessionId::new(
                "99999999-9999-4999-8999-999999999999",
            )),
            provider_projects_root: Some(proj.clone()),
        };
        let out = scan_session_candidates_once(Provider::ClaudeCode, &ctx).unwrap();
        assert!(
            out.is_empty(),
            "expected_session_id set + no exact match → must NOT fall back to \
             time-window narrowing (would grab leader's transcript). \
             got={:?}",
            out.iter()
                .filter_map(|c| c.captured.session_id.as_ref().map(|s| s.as_str().to_string()))
                .collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── E11 层1:copilot session 归因(expected-id 优先,不抓 leader latest)──
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
    }
    impl HomeGuard {
        fn set(home: &Path) -> Self {
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", home);
            Self { prev }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// 造一个 copilot session-store.db,写 (id, cwd, updated_at) 行。
    fn seed_copilot_db(home: &Path, rows: &[(&str, &str, i64)]) {
        let dir = home.join(".copilot");
        std::fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("session-store.db")).unwrap();
        conn.execute(
            "create table sessions (id text primary key, cwd text, updated_at integer)",
            [],
        )
        .unwrap();
        for (id, cwd, updated) in rows {
            conn.execute(
                "insert into sessions (id, cwd, updated_at) values (?1, ?2, ?3)",
                rusqlite::params![id, cwd, updated],
            )
            .unwrap();
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_expected_id_wins_over_leader_latest_same_cwd() {
        // 真机复现的确定性 fixture:leader row updated 晚(latest),worker row id == expected。
        // capture 必返 worker 自己的 id,不返 leader 的 latest。
        let base = tmp_root("e11-copilot");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let worker_id = "1142c4c2-0000-4000-8000-000000000001";
        let leader_id = "f9c5485d-0000-4000-8000-00000000beef";
        // leader updated_at 更大(latest-wins 会抓它);worker 更早。
        seed_copilot_db(
            &home,
            &[
                (worker_id, &cwd.to_string_lossy(), 100),
                (leader_id, &cwd.to_string_lossy(), 999),
            ],
        );
        let _h = HomeGuard::set(&home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new(worker_id)),
            provider_projects_root: None,
        };
        let out = scan_copilot_session_store(&ctx);
        assert_eq!(out.len(), 1, "expected-id point query → single authoritative candidate");
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            worker_id,
            "must return worker's own (expected) session, NOT leader's latest"
        );
        drop(_h);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_expected_id_absent_in_db_returns_empty_not_leader() {
        // expected 设了但 db 无该 id(会话还没落)→ 返空(收敛重试),绝不回落抓 leader latest。
        let base = tmp_root("e11-copilot-absent");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let leader_id = "f9c5485d-0000-4000-8000-00000000beef";
        seed_copilot_db(&home, &[(leader_id, &cwd.to_string_lossy(), 999)]);
        let _h = HomeGuard::set(&home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new("1142c4c2-0000-4000-8000-000000000001")),
            provider_projects_root: None,
        };
        let out = scan_copilot_session_store(&ctx);
        assert!(
            out.is_empty(),
            "expected id absent in db → empty (no promote, no leader latest); got {out:?}"
        );
        drop(_h);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_no_expected_same_cwd_only_leader_row_returns_empty_not_leader() {
        // E11 层1 兜底洞(architect):无 expected + 同 cwd 仅 leader row → 必返空,绝不返 leader。
        // (真实 copilot worker 恒有 expected,此分支不可达;保守返空堵住 allocator 不排除 leader 的洞。)
        let base = tmp_root("e11-copilot-noexp");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let leader_id = "f9c5485d-0000-4000-8000-00000000beef";
        seed_copilot_db(&home, &[(leader_id, &cwd.to_string_lossy(), 999)]);
        let _h = HomeGuard::set(&home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None, // 无 expected → 兜底路径
            provider_projects_root: None,
        };
        let out = scan_copilot_session_store(&ctx);
        assert!(
            out.is_empty(),
            "no expected + only leader row in same cwd → must return empty, NOT leader; got {out:?}"
        );
        drop(_h);
        let _ = std::fs::remove_dir_all(&base);
    }
}
