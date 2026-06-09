//! ProviderAdapter trait(per-provider 命令构造 + 能力面)+ registry facade + 占位实现。

use std::path::{Path, PathBuf};
use std::process::Command;

use super::helpers::{find_session_id, parse_jsonl_records, patterns};
use super::types::{
    AuthHintStatus, CaptureVia, CapturedSession, CommandPlan, Confidence, McpConfig,
    ProviderCaps, ProviderCommandContext, ProviderError, RolloutPath, SessionId,
    StatusPatterns,
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
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match self.provider() {
            Provider::Codex => {
                super::startup_prompt::codex_handle_startup_prompts(transport, target, checks, sleep_s)
            }
            Provider::Claude | Provider::ClaudeCode => {
                super::startup_prompt::claude_handle_startup_prompts(
                    transport, target, checks, sleep_s,
                )
            }
            _ => Vec::new(),
        }))
        .unwrap_or_default()
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
struct BasicProviderAdapter {
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
            Provider::Codex => Ok(codex_base_command(None, auth_mode, mcp_config, system_prompt, model, tools)),
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
                Ok(CommandPlan {
                    argv,
                    expected_session_id: Some(SessionId::new(expected)),
                    provider_projects_root: projects_root,
                    managed_mcp_config: managed,
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
            Provider::GeminiCli | Provider::Fake => patterns(r">", r"working|processing", r"Error|Traceback"),
        }
    }

    fn validate_model(&self, _model: &str) -> Result<bool, ProviderError> {
        Ok(true)
    }
}

fn command_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        Provider::GeminiCli => "gemini",
        Provider::Fake => "team-agent",
    }
}

fn provider_wire(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Codex => "codex",
        Provider::GeminiCli => "gemini_cli",
        Provider::Fake => "fake",
    }
}

fn auth_mode_wire(auth_mode: AuthMode) -> &'static str {
    match auth_mode {
        AuthMode::Subscription => "subscription",
        AuthMode::OfficialApi => "official_api",
        AuthMode::CompatibleApi => "compatible_api",
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
    let candidates = candidate_session_files(provider, context)?;
    let mut out = Vec::new();
    for candidate in candidates {
        let path = candidate.path;
        let Ok(text) = std::fs::read_to_string(&path) else {
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
                collect_optional_candidate_files(&home.join(".claude").join("sessions"), &context.agent_id, &mut out)?;
                collect_optional_candidate_files(&home.join(".claude").join("projects"), &context.agent_id, &mut out)?;
            }
            Provider::GeminiCli | Provider::Fake => {}
        }
    }
    out.sort_by(|a, b| {
        a.requires_cwd_match
            .cmp(&b.requires_cwd_match)
            .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.requires_cwd_match == b.requires_cwd_match);
    Ok(out)
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

fn fake_worker_command() -> Vec<String> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "team-agent".to_string());
    vec![
        exe,
        "fake-worker".to_string(),
        "--workspace".to_string(),
        "{workspace}".to_string(),
        "--agent-id".to_string(),
        "{agent_id}".to_string(),
    ]
}

fn claude_launch_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
) -> Result<Vec<String>, ProviderError> {
    let mut argv = claude_base_command(adapter, auth_mode, mcp_config, system_prompt, model, tools, false)?;
    argv.push("--session-id".to_string());
    argv.push(next_session_token());
    Ok(argv)
}

fn claude_base_command(
    adapter: &BasicProviderAdapter,
    auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
    managed_mcp_config: bool,
) -> Result<Vec<String>, ProviderError> {
    let mut argv = vec!["claude".to_string()];
    if claude_dangerous_auto_approve(tools) {
        argv.push("--dangerously-skip-permissions".to_string());
    } else {
        argv.push("--permission-mode".to_string());
        argv.push("default".to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(prompt) = system_prompt {
        argv.push("--append-system-prompt".to_string());
        argv.push(prompt.to_string());
    }
    if !managed_mcp_config
        && (mcp_config.is_some()
            || auth_mode == AuthMode::CompatibleApi
            || system_prompt.is_some_and(prompt_needs_native_mcp))
    {
        let raw = if let Some(config) = mcp_config {
            serde_json::json!({"mcpServers": config.raw.clone()})
        } else {
            serde_json::json!({"mcpServers": adapter.mcp_config(auth_mode)?.raw})
        };
        argv.push("--mcp-config".to_string());
        argv.push(raw.to_string());
        argv.push("--strict-mcp-config".to_string());
    }
    for tool in claude_disallowed_tools(tools) {
        argv.push("--disallowedTools".to_string());
        argv.push(tool.to_string());
    }
    Ok(argv)
}

fn prompt_needs_native_mcp(prompt: &str) -> bool {
    prompt.contains('\n') || prompt.contains('"')
}

fn codex_base_command(
    subcommand: Option<&str>,
    _auth_mode: AuthMode,
    mcp_config: Option<&McpConfig>,
    system_prompt: Option<&str>,
    model: Option<&str>,
    tools: &[&str],
) -> Vec<String> {
    let mut argv = vec![
        "codex".to_string(),
    ];
    if let Some(subcommand) = subcommand {
        argv.push(subcommand.to_string());
    }
    argv.extend([
        "--no-alt-screen".to_string(),
        "--disable".to_string(),
        "shell_snapshot".to_string(),
        "--disable".to_string(),
        "apps".to_string(),
    ]);
    if codex_dangerous_auto_approve(tools) {
        argv.push("--dangerously-bypass-approvals-and-sandbox".to_string());
    } else {
        argv.push("--sandbox".to_string());
        argv.push(codex_sandbox_mode(tools).to_string());
        argv.push("--ask-for-approval".to_string());
        argv.push("on-request".to_string());
    }
    if let Some(model) = model {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    if let Some(prompt) = system_prompt {
        argv.push("-c".to_string());
        argv.push(format!("developer_instructions=\"{}\"", prompt.replace('"', "\\\"")));
    }
    // Contract C / MUST-8: Codex CLI (2026-06) does NOT take Claude's `--mcp-config <json>` flag;
    // instead it uses `-c mcp_servers.<name>.<field>=...` overrides, the same pattern used by
    // the live Team Agent workers in this very session (attach-leader codex panes spawn with
    // `-c mcp_servers.team_orchestrator.command="..."`, ` ...args=[...]`, `...env.TEAM_AGENT_ID=...`).
    // Inject the resolved MCP config that way so the Codex worker has a real callable
    // `team_orchestrator` server (not prompt-only metadata).
    if let Some(config) = mcp_config {
        append_codex_mcp_overrides(&mut argv, &config.raw);
    }
    argv
}

/// Render an `McpConfig::raw` ({ name: { type, command, args, env: {...} } }) into Codex
/// `-c mcp_servers.<name>.<field>=...` overrides. JSON values are stringified with serde
/// so arrays/objects survive (Codex parses the right-hand side as JSON; this is what the
/// Python golden + the live attached Codex panes do).
fn append_codex_mcp_overrides(argv: &mut Vec<String>, raw: &serde_json::Value) {
    let Some(servers) = raw.as_object() else {
        return;
    };
    for (name, server) in servers {
        let Some(obj) = server.as_object() else {
            continue;
        };
        for (key, value) in obj {
            if key == "env" {
                if let Some(env) = value.as_object() {
                    for (env_key, env_value) in env {
                        argv.push("-c".to_string());
                        argv.push(format!(
                            "mcp_servers.{name}.env.{env_key}={}",
                            json_inline(env_value)
                        ));
                    }
                }
                continue;
            }
            argv.push("-c".to_string());
            argv.push(format!("mcp_servers.{name}.{key}={}", json_inline(value)));
        }
    }
}

fn json_inline(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        other => other.to_string(),
    }
}

fn codex_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}

fn claude_dangerous_auto_approve(tools: &[&str]) -> bool {
    tools.contains(&"dangerous_auto_approve")
}

fn claude_disallowed_tools(tools: &[&str]) -> Vec<&'static str> {
    let mut disallowed = Vec::new();
    if !tools.contains(&"execute_bash") {
        disallowed.push("Bash");
    }
    if !tools.contains(&"fs_read") {
        disallowed.push("Read");
    }
    if !tools.contains(&"fs_write") {
        disallowed.extend(["Edit", "Write", "MultiEdit", "NotebookEdit"]);
    }
    if !tools.contains(&"fs_list") {
        disallowed.extend(["Glob", "Grep"]);
    }
    disallowed
}

fn codex_sandbox_mode(tools: &[&str]) -> &'static str {
    if tools.iter().any(|tool| matches!(*tool, "fs_write" | "execute_bash")) {
        "workspace-write"
    } else {
        "read-only"
    }
}

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

fn has_cwd_field(record: &serde_json::Value) -> bool {
    record_cwd(record).is_some()
}

fn next_session_token() -> String {
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
