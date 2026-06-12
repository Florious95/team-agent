//! cli · types — 错误信封 / 退出码 / 命令结果载体 / leader passthrough 参数 /
//! 每子命令的 clap-style arg 结构 / 五行 summary 计数桶。

use super::*;
use crate::provider::Provider;
use crate::transport::PaneId;

// =============================================================================
// ERRORS / EXIT(helpers.py `_emit_cli_error` / `_cli_error_payload`)
// =============================================================================

/// CLI 顶层错误信封(`_cli_error_payload`,`helpers.py:137-155`)。`--json` 时序列化为
/// `{ok:false, error, action, log, reason?, session_name?, next_actions?}`(字节级保留)。
/// 人读时打 `error:`/`action:`/`log:` 三行到 stderr(`helpers.py:132-134`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliErrorPayload {
    /// 恒 `false`(错误信封)。
    pub ok: bool,
    pub error: String,
    pub action: String,
    /// `.team/logs/cli-error-<ts>.log` 路径(mkdir 失败 fallback cwd,`helpers.py:122-125`)。
    pub log: String,
    /// tmux session 冲突时富化 `tmux_session_name_conflict`(`helpers.py:146-154`)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_actions: Option<Vec<String>>,
}

/// CLI 层错误(lib 边界 thiserror,§12)。bin main 经 [`run`] 把它落盘 + 转
/// [`CliErrorPayload`] + 退出码 1(`main` `except → _emit_cli_error → SystemExit(1)`)。
#[derive(Debug, Error)]
pub enum CliError {
    /// 委派的 runtime/lifecycle/state 错误(对应 Python `TeamAgentError`)。message == `str(exc)`。
    #[error("{0}")]
    Runtime(String),
    /// argparse 风格用法错误 / 互斥违反(如 `--summary` + `--json`、`--fix` 缺 `--gate`、
    /// `peek` 缺 `--allow-raw-screen`)。对应 Python 抛 `TeamAgentError` 或 parser.error。
    #[error("usage error: {0}")]
    Usage(String),
    /// state 解析失败(歧义/未找到 team 等)。透传 step 5。
    #[error("{0}")]
    State(#[from] crate::state::StateError),
    /// messaging 委派失败(send/collect/stuck)。透传 step 11。
    #[error("{0}")]
    Messaging(crate::messaging::MessagingError),
    /// I/O(cli-error 落盘、inbox 游标读写)。bug-084:写路径降级,不裸 panic。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON 解析(`validate-result` 读 envelope)。
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<crate::messaging::MessagingError> for CliError {
    fn from(error: crate::messaging::MessagingError) -> Self {
        match error {
            crate::messaging::MessagingError::Validation(message)
                if message.starts_with("unknown task id:") =>
            {
                CliError::Runtime(message)
            }
            other => CliError::Messaging(other),
        }
    }
}

impl CliError {
    /// `_cli_error_payload`(`helpers.py:137-155`):落盘日志后构造稳定信封。tmux session
    /// 冲突时按 command 富化 reason/session_name/next_actions(`helpers.py:158-187`)。
    pub fn to_payload(&self, log_path: &Path, command: &str) -> CliErrorPayload {
        let error = self.to_string();
        let mut payload = CliErrorPayload {
            ok: false,
            error: error.clone(),
            action: "run `team-agent doctor` or inspect the log path shown here".to_string(),
            log: log_path.to_string_lossy().to_string(),
            reason: None,
            session_name: None,
            next_actions: None,
        };
        if let Some(session) = tmux_conflict_session(&error) {
            payload.reason = Some("tmux_session_name_conflict".to_string());
            payload.session_name = Some(session.clone());
            if command == "quick-start" {
                payload.action = format!(
                    "tmux session `{session}` already exists. It may be your own existing team. To resume it use `team-agent restart` (NOT --fresh, which discards context). Only if you want a separate team, change `name:` in TEAM.md and run quick-start again. Never terminate existing tmux sessions from quick-start."
                );
                payload.next_actions = Some(vec![
                    "If this is your existing team, resume it with `team-agent restart`.".to_string(),
                    "If you want a separate team, change `name:` in TEAM.md and run `team-agent quick-start` again.".to_string(),
                ]);
            } else {
                payload.action = format!(
                    "tmux session `{session}` already exists. It may be an active team. Do not terminate existing tmux sessions from startup; use a different team name or runtime.session_name and start again."
                );
                payload.next_actions = Some(vec![
                    "Use a different team name or runtime.session_name before starting again."
                        .to_string(),
                ]);
            }
        }
        payload
    }
}

/// CLI 进程退出码(`main`:`result.ok is False` 或异常 → `SystemExit(1)`,否则 0)。
/// `watch` 路径直接 `SystemExit(0)`(`cmd_watch`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Ok,
    Error,
    Usage,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        match self {
            ExitCode::Ok => 0,
            ExitCode::Error => 1,
            ExitCode::Usage => 2,
        }
    }
}

// =============================================================================
// 命令结果(每个 subcommand handler 返回的 typed 富载体)
// =============================================================================

/// 单条 subcommand 的输出载体(`cmd_*` 的返回 + `emit` 的输入,`helpers.py:12-23`)。
/// Python `cmd_*` 返回 `dict | str | None`:
///   - `Json(Value)` ← 委派 runtime 的稳定 `{ok, ...}` dict(`--json` 序列化 sort_keys+indent=2)。
///   - `Human(String)` ← 人读渲染(`format_status`/`cmd_advanced`/五行 summary/comms boundary text)。
///   - `None` ← passthrough/watch 命令(`cmd_codex`/`cmd_claude`/`cmd_watch`,直接 SystemExit,无 emit)。
#[derive(Debug, Clone, PartialEq)]
pub enum CmdOutput {
    /// 机器可读稳定 dict(`--json` 路径或 `result.get("ok") is False` 路径)。
    Json(Value),
    /// 人读字符串(`emit` 非 dict 分支直接 print)。
    Human(String),
    /// 无输出(passthrough/watch;不经 `emit`)。
    None,
}

/// subcommand handler 的统一返回(card:"each returns a typed CmdResult + CliError")。
/// 携带:输出载体 + 派生退出码 + 命令后是否需吐 leader inbox 摘要。
/// `main` 据 `output` 走 `emit`,据 `exit` 决定 `SystemExit`(`parser.py:494-508`)。
#[derive(Debug, Clone, PartialEq)]
pub struct CmdResult {
    pub output: CmdOutput,
    /// `result.get("ok") is False` → `ExitCode::Error`(`parser.py:507-508`),否则 `Ok`。
    pub exit: ExitCode,
    /// `--json` 旗标(决定 `emit` 与 inbox 摘要落 stderr vs stdout,`parser.py:505-506`)。
    pub as_json: bool,
}

impl CmdResult {
    /// 委派 dict 结果 → CmdResult(从 `{ok:..}` 推 exit code)。
    pub fn from_json(value: Value, as_json: bool) -> Self {
        let exit = if value.get("ok").and_then(Value::as_bool) == Some(false) {
            ExitCode::Error
        } else {
            ExitCode::Ok
        };
        Self {
            output: CmdOutput::Json(value),
            exit,
            as_json,
        }
    }
    pub fn human(text: impl Into<String>) -> Self {
        Self {
            output: CmdOutput::Human(text.into()),
            exit: ExitCode::Ok,
            as_json: false,
        }
    }
    pub fn none() -> Self {
        Self {
            output: CmdOutput::None,
            exit: ExitCode::Ok,
            as_json: false,
        }
    }
}

// =============================================================================
// `codex`/`claude` passthrough 解析(helpers.py `_provider_args`/`_leader_launcher_args`)
// =============================================================================

/// `_leader_launcher_args`(`helpers.py:196-226`):leader passthrough 的 `--`/`--attach`/
/// `--attach-existing`/`--confirm`/`--attach-session[=]` 解析结果(`AttachLauncherArgs`)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LeaderLauncherArgs {
    /// `--` 之后或非旗标的透传给 provider 的 args。
    pub provider_args: Vec<String>,
    /// `--attach`/`--attach-existing`。
    pub attach_existing: bool,
    /// `--confirm`。
    pub confirm_attach: bool,
    /// `--attach-session <name>` / `--attach-session=<name>`。
    pub attach_session: Option<String>,
    /// 0.3.16 topology opt-out: keep the old external/current-pane leader launcher path.
    pub external_leader: bool,
}

// =============================================================================
// 五行 triage summary 计数桶(commands.py `_agent_summary_counts` / `_interaction_counts`)
// =============================================================================

/// 五行 triage 的 agent 分类桶(`_agent_summary_counts`,`commands.py:309-330`)。
/// **bug-071/077/085 铁律(§11)**:`blocked/awaiting_approval/interrupted/missing/stuck/uncertain`
/// 及任何无匹配态显式落 [`Unknown`](`else: unknown += 1`),**绝不 fallthrough 成 idle**。
/// 穷尽 match 在编译期防漏 idle 臂。
///
/// [`Unknown`]: SummaryBucket::Unknown
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SummaryBucket {
    Running,
    Busy,
    Idle,
    Stopped,
    Failed,
    Unknown,
}

/// 五行 summary 的 agent 计数(`_agent_summary_counts` 返回的 dict 的 typed 版)。
/// 渲染 line[2] 的 `running=.. busy=.. idle=.. stopped=.. failed=.. unknown=..`(Gap 18a 字节锁)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SummaryCounts {
    pub running: usize,
    pub busy: usize,
    pub idle: usize,
    pub stopped: usize,
    pub failed: usize,
    pub unknown: usize,
}

impl SummaryCounts {
    pub fn total(self) -> usize {
        self.running + self.busy + self.idle + self.stopped + self.failed + self.unknown
    }
}

/// `interacted` marker 计数(`_interaction_counts`,`commands.py:292-306`)。源自 status 富化的
/// 每-agent `interacted` 字段(非空且 ≠ `"never"` 计 interacted,否则 never)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InteractionCounts {
    pub interacted: usize,
    pub never: usize,
}

// =============================================================================
// clap-style arg structs(每个子命令一个;字段名 == argparse dest)
// =============================================================================

/// `quick-start`(`parser.py:105`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickStartArgs {
    pub workspace: PathBuf,
    pub agents_dir: PathBuf,
    pub name: Option<String>,
    pub team_id: Option<String>,
    pub yes: bool,
    pub fresh: bool,
    pub no_display: bool,
    pub json: bool,
}

/// `init`(`parser.py` bootstrap verb)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitArgs {
    pub workspace: PathBuf,
    pub force: bool,
    pub json: bool,
}

/// `compile`(`parser.py:125`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileArgs {
    pub team: PathBuf,
    pub out: PathBuf,
    pub json: bool,
}

/// `send`(`parser.py:262`)。`target` xor `--to`(fanout);`message` 多 token join 空格。
#[derive(Debug, Clone, PartialEq)]
pub struct SendArgs {
    pub target: Option<String>,
    pub message: Vec<String>,
    /// `--to a,b,c`(comma-split fanout)。
    pub targets: Option<String>,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub task: Option<String>,
    pub sender: String,
    pub no_ack: bool,
    pub no_wait: bool,
    pub watch_result: bool,
    pub timeout: f64,
    pub confirm_human: bool,
    pub json: bool,
    /// `--message-id <id>` — caller-supplied idempotency key (CR-015/054).
    /// When set, the store insert uses this id verbatim; a repeat with the same
    /// id returns a `Duplicate` refusal instead of creating a second row.
    pub message_id: Option<String>,
}

/// E23 worker-side emergency fallback for `team_orchestrator.send_message`
/// transport failures. This is not a general control-plane send path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackSendLeaderArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub sender: String,
    pub task: Option<String>,
    pub message_id: String,
    pub content: String,
    pub primary_error: String,
    pub json: bool,
}

/// E23 worker-side emergency fallback for `team_orchestrator.report_result`
/// transport failures. It must still persist through the results DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackReportResultArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub agent_id: String,
    pub task_id: String,
    pub result_json: String,
    pub primary_error: String,
    pub json: bool,
}

/// `allow-peer-talk`(`parser.py`): allow direct peer communication between two agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowPeerTalkArgs {
    pub a: String,
    pub b: String,
    pub workspace: PathBuf,
    pub json: bool,
}

/// `status`(`parser.py:182`)。`--summary`/`--json`/`--detail` 三态(summary xor json,见 `cmd_status`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusArgs {
    pub agent: Option<String>,
    pub workspace: PathBuf,
    pub detail: bool,
    pub summary: bool,
    pub json: bool,
}

/// `watch`(`parser.py:190`)。无 `--json`(纯 stream 到 SystemExit)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
}

/// `approvals`(`parser.py:195`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalsArgs {
    pub agent: Option<String>,
    pub workspace: PathBuf,
    pub json: bool,
}

/// `inbox`(`parser.py:217`)。`--since` ISO8601(claim-leader inbox_hint 复用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub limit: usize,
    pub since: Option<String>,
    pub json: bool,
}

/// `takeover`(`parser.py:242`)。`--confirm` 必需(覆写 recorded team_owner)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoverArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub confirm: bool,
    pub json: bool,
}

/// `claim-leader`(`parser.py:249`)。`--confirm` 缺省 = dry-run 摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimLeaderArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub confirm: bool,
    pub json: bool,
}

/// `attach-leader` public CLI args. `cmd_attach_leader` consumes the typed pane/provider
/// fields and returns/writes a `leader_receiver` binding through the leader lease port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachLeaderArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub pane: Option<PaneId>,
    pub provider: Provider,
    pub confirm: bool,
    pub json: bool,
}

/// `identity`(`parser.py:256`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub json: bool,
}

/// `shutdown`(`parser.py:355`)。`--keep-logs` 默认 true。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub keep_logs: bool,
    pub json: bool,
}

/// `restart`(`parser.py:362`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartArgs {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub allow_fresh: bool,
    pub session_converge_deadline_ms: Option<u64>,
    pub json: bool,
}

/// `start-agent`(`parser.py:369`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAgentArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub force: bool,
    pub allow_fresh: bool,
    pub no_display: bool,
    pub json: bool,
}

/// `stop-agent`(`parser.py:379`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAgentArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub json: bool,
}

/// `reset-agent`(`parser.py:386`)。`--discard-session` 必需。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetAgentArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub discard_session: bool,
    pub no_display: bool,
    pub json: bool,
}

/// `add-agent`(`parser.py:395`)。`--role-file` 必需。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddAgentArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub role_file: String,
    pub no_display: bool,
    pub json: bool,
}

/// `fork-agent`(`parser.py:404`)。`--as` 必需。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkAgentArgs {
    pub source_agent: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub as_agent: String,
    pub label: Option<String>,
    pub no_display: bool,
    pub json: bool,
}

/// `remove-agent`(`parser.py:414`)。`--from-spec` 须配 `--confirm`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveAgentArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub from_spec: bool,
    pub confirm: bool,
    pub force: bool,
    pub json: bool,
}

/// `stuck-list`(`parser.py:424`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckListArgs {
    pub workspace: PathBuf,
    pub json: bool,
}

/// `stuck-cancel`(`parser.py:429`)。`--alert-type` ∈ {stuck, idle_fallback, all},默认 stuck。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckCancelArgs {
    pub agent: String,
    pub workspace: PathBuf,
    /// `None` 表 `all`(展开全集);`Some(AlertType)` 表单类型。
    pub alert_type: Option<AlertType>,
    pub json: bool,
}

/// `acknowledge-idle`(`parser.py:436`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgeIdleArgs {
    pub team: Option<String>,
    pub workspace: PathBuf,
    pub json: bool,
}

/// `doctor`(`parser.py:318`)。`--gate` ∈ {orphans, comms};`--fix` 须配 `--gate`(`cmd_doctor` 校验)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorArgs {
    pub spec: Option<PathBuf>,
    pub workspace: PathBuf,
    /// `None` / `Some(Orphans)` / `Some(Comms)`。
    pub gate: Option<DoctorGate>,
    pub comms: bool,
    pub team: Option<String>,
    pub fix: bool,
    pub fix_schema: bool,
    pub cleanup_orphans: bool,
    pub confirm: bool,
    pub json: bool,
}

/// `doctor --gate` 选择(`commands.py:218-236`;clap choices)。
/// swallow batch 3: an unrecognized gate is carried verbatim so the doctor exit can
/// refuse with `unknown_gate` (Python commands.py:234-235 raises) instead of silently
/// falling through to the default doctor (empty green).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorGate {
    Orphans,
    Comms,
    Unknown(String),
}

/// `sessions`(`parser.py:230`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsArgs {
    pub workspace: PathBuf,
    pub json: bool,
}

/// `validate [spec=team.spec.yaml] --json`(`parser.py:120`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateArgs {
    pub spec: PathBuf,
    pub json: bool,
}

/// `profile {init,doctor,show}`(`parser.py:131`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileArgs {
    pub command: String,
    pub name: String,
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub auth_mode: Option<String>,
    pub json: bool,
}

/// `validate-result`(`parser.py:312`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateResultArgs {
    pub envelope: Option<String>,
    pub file: Option<PathBuf>,
    pub result: Option<String>,
    pub json: bool,
}

/// `collect`(`parser.py:292`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectArgs {
    pub workspace: PathBuf,
    pub result_file: Option<PathBuf>,
    pub json: bool,
}

/// `settle`(`parser.py:177`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleArgs {
    pub workspace: PathBuf,
    pub json: bool,
}

/// `repair-state`(`parser.py:303`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairStateArgs {
    pub workspace: PathBuf,
    pub task_id: String,
    pub assignee: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub json: bool,
}

/// `diagnose`(`parser.py:298`) runtime health report, distinct from `doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnoseArgs {
    pub workspace: PathBuf,
    pub json: bool,
}

/// `preflight`(`parser.py:160`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightArgs {
    pub team: PathBuf,
    pub json: bool,
}

/// `wait-ready`(`parser.py:171`)。
#[derive(Debug, Clone, PartialEq)]
pub struct WaitReadyArgs {
    pub workspace: PathBuf,
    pub timeout: f64,
    pub json: bool,
}

/// `e2e`(`parser.py:449`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E2eArgs {
    pub workspace: PathBuf,
    pub providers: Vec<String>,
    pub real: bool,
    pub json: bool,
}

/// `peek`(`parser.py:201`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeekArgs {
    pub agent: String,
    pub workspace: PathBuf,
    pub tail: usize,
    pub head: Option<usize>,
    pub search: Option<String>,
    pub allow_raw_screen: bool,
    pub json: bool,
}
