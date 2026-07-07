//! step 9 · transport — 控制面 trait + typed payload(ROUND-0 SKELETON).
//!
//! 设计真相源:`docs/phase0/transport-backend-design.md` §1(Rust 草图:Target /
//! InjectPayload / Key / CaptureRange / PaneField / Liveness / SpawnResult /
//! PaneInfo / Transport trait §1.3 / BackendKind / SetEnvOutcome / TransportError)。
//! InjectReport 的阶段化字段(InjectStage / InjectVerification / SubmitVerification /
//! TurnVerification)来自子系统卡 `docs/phase0/subsystems/09-transport.md`(表 §38-42)。
//!
//! Python 真相源(`team-agent-public` @ v0.2.11):`messaging/tmux_io.py`、
//! `messaging/tmux_prompt.py`、`terminal.py`、`sessions/*`(capture/resume/inventory/drift)。
//!
//! 本文件**只有类型与方法签名**,无实现(§4 铁律:RED 契约要 name 的 TYPES 先编过)。
//!
//! §10:transport 被 coordinator/lifecycle/daemon 调用,所有 daemon 可达方法返
//! `Result<_, TransportError>`(I/O / 子进程错误);**能力性拒绝**(set-env 不支持事后
//! 补种、attach 无概念)用 typed `enum` variant 表达(SetEnvOutcome / AttachOutcome),
//! **不**用 `Result::Err` / `unwrap` / `expect` / `panic`。`#![deny(...)]` 由 leader
//! 在集成时统一加,本骨架不加。

// §10:daemon 可达方法实现层禁 unwrap/expect/panic(unimplemented! 未实现 stub 不算)。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
pub mod test_support;

// 裁决(transport-backend-design.md §1.2 / 本 lane ADJUDICATION):
// 文档草图的 `Liveness {Live/Dead/Unknown}` 就是既有的 model::enums::PaneLiveness
// (`state.py:336-341`,bug-085 穷尽三态)—— REUSE,不重定义。
pub use crate::model::enums::PaneLiveness;

// ─────────────────────────────────────────────────────────────────────────────
// id / name newtypes(透明 String 包装,字节 == 裸字符串,沿用 model::ids 风格)
// ─────────────────────────────────────────────────────────────────────────────

/// tmux `#{pane_id}`(如 `%7`)/ wezterm 整数 pane-id / conpty 内部 handle key。
/// 禁与 window/session 名混传(09-transport.md 表 §30)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub String);

impl PaneId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PaneId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// tmux session 名(`tmux_session_exists`,terminal.py:20-33)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionName(pub String);

impl SessionName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SessionName {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// tmux window 名(`tmux_window_exists`,terminal.py:20-33)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowName(pub String);

impl WindowName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WindowName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for WindowName {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 关键 typed payload(避免散字符串,§19/§22;transport-backend-design.md §1.2)
// ─────────────────────────────────────────────────────────────────────────────

/// 注入目标:两种合法寻址,类型上区分(禁混传)。tmux 用 SessionWindow 或 Pane;
/// WezTerm 只认整数 pane-id(SessionWindow 在后端内部先解析成 pane-id);ConPTY 只认
/// 自有 pane 的内部 key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// tmux `%7` / wezterm 整数 pane-id / conpty 内部 handle key。
    Pane(PaneId),
    SessionWindow {
        session: SessionName,
        window: WindowName,
    },
}

/// 注入载荷在类型上分流空文本(trust turn-integrity 契约 §3:空文本禁走 buffer,
/// 直发 Enter;tmux 拒空 buffer 会卡 trust prompt)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectPayload {
    /// → 纯 send submit-key。
    Empty,
    Text(String),
    /// Text payload for human-facing panes that do not consume provider turns.
    TextSkipConsumptionPoll(String),
}

impl InjectPayload {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) | Self::TextSkipConsumptionPoll(text) => Some(text),
            Self::Empty => None,
        }
    }

    pub fn skip_consumption_poll(&self) -> bool {
        matches!(self, Self::TextSkipConsumptionPoll(_))
    }
}

/// 抽象 Key 枚举(§gap-5):各后端翻译,不透传 tmux 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// tmux `Enter` / VT `\r`(CR,非 LF —— provider 提交触发的是 \r)。
    Enter,
    /// tmux 名 / VT `\x1b[A..D`(application-cursor-mode `\x1bO?` 风险)。
    Up,
    Down,
    Left,
    Right,
    /// 选项键/数字。
    Char(char),
    /// `\x03`。
    CtrlC,
    /// E46 (0.3.24 bug#5): pre-submit Escape to exit bracketed-paste mode on
    /// fresh provider TUIs (claude). When a bracketed paste lands on a TUI
    /// whose composer is still initialising, the framework's plain `Enter`
    /// gets interpreted as paste content, not submit. Sending `Escape` first
    /// closes the paste bracket so the subsequent `Enter` submits.
    /// Real-machine truth source: macmini demo-director repro.
    Escape,
    /// tmux `-X cancel` / `q` / `d`;非 tmux 后端无 copy-mode 概念 → no-op。
    CancelMode,
}

/// capture 范围(tmux `-S -<N>` / `-S -`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRange {
    Tail(u32),
    Head(u32),
    Full,
}

/// 单字段查询(display-message -p -F);非 tmux 后端无对应概念的字段返回 typed
/// 「不适用」(trait `query` 返回 `Option<String>`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneField {
    PaneId,
    PaneMode,
    PaneWidth,
    PaneCurrentCommand,
    PaneCurrentPath,
    SessionName,
    /// pane's controlling tty (e.g. `/dev/ttys015`). Consumers use it to
    /// flip terminal driver bits like `stty -f <tty> -echo` from outside
    /// tmux's control channel — the query itself still goes through the
    /// backend so N16/CP-1 (single tmux entry point) stays intact.
    PaneTty,
}

/// 后端种类(诊断/事件用)。
///
/// 裁决(本 lane ADJUDICATION):既有 `model::enums::Backend` 是 `{Tmux, Pty}`
/// (spec.py:255 的两值 spec 字段),**不等于** `BackendKind {Tmux, WezTerm, ConPty}`
/// (transport-backend-design.md §1.3 的三后端运行时种类)—— 故此处**新定义**
/// `BackendKind`,不复用 `Backend`。两者语义不同:`Backend` 是 spec 声明面的粗粒度
/// 选择(tmux vs pty),`BackendKind` 是 transport 实现面的具体后端(pty 进一步分
/// WezTerm 外部 mux 与 ConPTY 单机)。NOTE 给 leader 集成时复核映射关系
/// (Backend::Pty → {WezTerm | ConPty} 的解析在 §5 probe-first 逻辑里)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Tmux,
    WezTerm,
    ConPty,
}

/// spawn 返回:后端把它创建的终端的稳定 id 交回框架,供后续 RIE 寻址 + 身份正向
/// 登记(§4a)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResult {
    pub pane_id: PaneId,
    pub session: SessionName,
    pub window: WindowName,
    /// ConPTY 自有;tmux/wezterm 视 list 是否给 pid([真机])。
    pub child_pid: Option<u32>,
}

/// 全局枚举的一行(身份地基)。`leader_env` 在 tmux 后端靠反向读进程 env,在
/// WezTerm/ConPTY 后端靠正向登记表投影(§4a)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub pane_id: PaneId,
    pub session: SessionName,
    pub window_index: Option<u32>,
    pub window_name: Option<WindowName>,
    pub pane_index: Option<u32>,
    pub tty: Option<String>,
    /// WezTerm [真机]:list json 未必给前台命令 → GAP-3。
    pub current_command: Option<String>,
    pub current_path: Option<PathBuf>,
    pub active: bool,
    /// WezTerm [真机]:list json 未必给 OS pid → GAP-2。
    pub pane_pid: Option<u32>,
    /// tmux=反向读;wezterm/conpty=正向登记表的投影。
    pub leader_env: BTreeMap<String, String>,
}

/// 抓取到的屏幕文本(recognizer 的唯一输入;backend-agnostic,§4b)。
/// transport 出口处统一规范化(rstrip / `\xa0` 归一),recognizer 不为后端开分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedText {
    /// 规范化后的明文(各后端 capture 管线差异在此收口)。
    pub text: String,
    /// 抓取范围(诊断/审计)。
    pub range: CaptureRange,
}

// ─────────────────────────────────────────────────────────────────────────────
// inject 流水线阶段化报告(09-transport.md InjectOutcome → InjectReport,表 §38-42)
// ─────────────────────────────────────────────────────────────────────────────

/// 注入流水线各阶段(失败定位;构成 `TransportError::Inject` 的定位字段)。
/// 字符串值取自 09-transport.md 表 §41(kebab-case,显式 `rename`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InjectStage {
    #[serde(rename = "send-keys")]
    SendKeys,
    #[serde(rename = "pre-paste-capture")]
    PrePasteCapture,
    #[serde(rename = "set-buffer")]
    SetBuffer,
    #[serde(rename = "load-buffer")]
    LoadBuffer,
    #[serde(rename = "paste-buffer")]
    PasteBuffer,
    #[serde(rename = "delete-buffer")]
    DeleteBuffer,
    #[serde(rename = "pre-paste-pane-state")]
    PrePastePaneState,
    #[serde(rename = "pane-mode-check")]
    PaneModeCheck,
    #[serde(rename = "submit")]
    Submit,
    #[serde(rename = "visible-check")]
    VisibleCheck,
}

/// 注入可见性验证(审计语义;tmux_io.py + tmux_prompt.py 散布字符串穷尽枚举,
/// 09-transport.md 表 §38)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectVerification {
    CaptureContainsToken,
    CaptureContainsMessageFragment,
    CaptureContainsNewPastedContentPrompt,
    NoToken,
    CaptureMissingToken,
    /// 空文本走纯 send-keys(InjectPayload::Empty)的验证。
    EmptyTextSendKeys,
}

/// 提交键验证(tmux_io.py:214-221, tmux_prompt.py:281-322,09-transport.md 表 §39)。
/// `{key}_sent_after_visible_token` 是模板 → 用携带 Key 的 variant 表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitVerification {
    /// `enter_sent_without_placeholder_check`。MUST-10:此 variant 代表 paste+Enter
    /// 完成且无需 placeholder probe(纯 peer 消息路径)。**保留** ⇒ delivered 语义;
    /// E46 后只有 post-Enter consumption 确认通过(input 清空 / Working 信号)才返回
    /// 此 variant — fresh TUI 未消费走 [`Self::SubmitConsumptionUnverified`]。
    EnterSentWithoutPlaceholderCheck,
    /// `pasted_content_prompt_absent_after_submit`。
    PastedContentPromptAbsentAfterSubmit,
    /// `pasted_content_prompt_still_present_after_submit`。
    PastedContentPromptStillPresentAfterSubmit,
    /// `{key}_sent_after_visible_token`(key 由 variant 携带)。
    KeySentAfterVisibleToken { key: Key },
    /// `send_keys_failed`。
    SendKeysFailed,
    /// E46 (0.3.24 bug#5, demo-director 卡 bracketed paste): Enter 已发但
    /// post-Enter 接收侧消费信号(input 行清空 / provider 进 Working)在 bounded
    /// resend 上限内未观察到。**delivery 不当作 delivered**;走 submitted_unverified /
    /// failed 路径。区别于
    /// [`Self::PastedContentPromptStillPresentAfterSubmit`](claude paste-prompt
    /// 折叠场景)— 本 variant 是结构化 input-empty 检测,适配 demo-director
    /// 直接渲染文本路径。
    SubmitConsumptionUnverified,
}

/// turn-boundary 观测(tmux_io.py:224-260,09-transport.md 表 §40)。
/// **Gap42**:turn 观测仅 metadata,`not_yet_observed` 也算成功,绝非投递闸门。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnVerification {
    LeaderNewTurnBoundaryVerified,
    LeaderNewTurnBoundaryMissing,
    NotYetObserved,
    NotRequired,
}

/// 注入流水线阶段化报告(09-transport.md InjectReport;daemon 路径 typed 返回值)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectReport {
    pub stage_reached: InjectStage,
    pub inject_verification: InjectVerification,
    pub submit_verification: SubmitVerification,
    /// Gap42:仅 metadata,`not_yet_observed` 也算成功。
    pub turn_verification: TurnVerification,
    pub attempts: u32,
    /// E50 PR-1 (0.3.24 P0, pasted-prompt 假阴诊断): per-attempt observations
    /// emitted by the paste-prompt + Enter loop in `tmux_backend.rs::inject`.
    /// `None` when the inject path did not exercise the diagnostic
    /// instrumentation (peer payloads, empty payloads, non-bracketed Text,
    /// or any path that bypasses `capture_has_pasted_content_prompt`). When
    /// present, downstream `send.unverified` / `send.failed` events surface
    /// it as `submit_attempts_detail[]` so operators see live pane state
    /// per attempt (matched literal, where-in-tail offset, scrubbed pane
    /// excerpt, elapsed ms) — the missing forensic data the user has been
    /// asking for for many rounds.
    ///
    /// Tests / mocks should default this to `None` via `Default`. Wire
    /// byte-lock (`transport::tests::wire`) is untouched: this field is NOT
    /// serialised on the typed wire — it lives only in the in-process
    /// `InjectReport` and is rendered into JSON manually by the delivery
    /// layer when emitting forensic events.
    pub submit_diagnostics: Option<SubmitDiagnostics>,
}

/// E50 PR-1 diagnostic payload — one entry per submit attempt in the
/// pasted-prompt branch + (informational) for the appear-gate poll.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmitDiagnostics {
    /// Time spent in the appear-gate (poll for the pasted-content placeholder
    /// before Enter). When `saw_pasted_prompt == false` this is the time we
    /// spent polling before falling through to the E46 token path.
    pub appear_gate_elapsed_ms: u64,
    /// Did the appear-gate ever match the `pasted content` / `pasted text`
    /// literal? When `false`, the inject took the E46 token path; the
    /// `attempts_detail` may still capture observations the operator wants.
    pub appear_gate_matched: bool,
    /// Total elapsed across the whole submit gate (appear-gate + Enter
    /// loop). Useful to triage real-machine slow-paste (codex large-paste
    /// collapse can take >100 s while the framework's loop is sub-second).
    pub total_elapsed_ms: u64,
    /// Per-attempt observations of the post-Enter capture in the pasted-
    /// prompt loop. Empty when `saw_pasted_prompt == false` and the inject
    /// went through the E46 token path.
    pub attempts_detail: Vec<SubmitAttemptObservation>,
}

/// E50 PR-1 single attempt observation (one Enter + one capture). All fields
/// are forensic — they exist to make `send.unverified` / `send.failed` events
/// self-describing rather than requiring the operator to guess.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmitAttemptObservation {
    /// 1-based attempt index (mirrors the `attempts` counter that already
    /// ships in `InjectReport.attempts`).
    pub attempt_index: u32,
    /// Did this attempt's post-Enter capture STILL match the pasted-prompt
    /// literal? `true` means the inject saw the placeholder; `false` means
    /// the placeholder cleared (= submit succeeded by the legacy criterion).
    pub matched: bool,
    /// The matched literal substring (`pasted content` / `pasted text`)
    /// when `matched == true`. `None` otherwise.
    pub matched_literal: Option<String>,
    /// Distance from the bottom of the tail where the match occurred:
    /// `Some(0)` = bottom-most line (composer), `Some(N)` = N lines above
    /// bottom (likely scrollback), `None` = no match. Critical for the
    /// false-negative root cause: a `pasted content` literal in scrollback
    /// is NOT the live composer placeholder.
    pub where_in_tail: Option<u32>,
    /// Scrubbed last 20 / 80 lines of the captured tail, ANSI-stripped,
    /// capped ~1200 bytes. Secrets scrubbed (sk-/ghp_/AKIA/Bearer/hex32+).
    pub pane_tail_excerpt: String,
    /// Number of non-empty lines in `pane_tail_excerpt`.
    pub pane_tail_lines: u32,
    /// Time elapsed for THIS attempt (Enter + capture + match).
    pub elapsed_ms: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// 能力性拒绝的 typed 结果(非 Err —— 已知能力差,审计用;§10)
// ─────────────────────────────────────────────────────────────────────────────

/// `set_session_env` 的 typed 结果(transport-backend-design.md §1.3 / §4c)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetEnvOutcome {
    /// tmux:写进 session env。
    Applied,
    /// wezterm/conpty:worker env 已在 spawn 时注入,无需事后补种。
    InternalizedAtSpawn,
    /// 外部 leader pane 无法事后补种 → leader 必须启动时自带 env(§4c)。
    UnsupportedForExternalPane { reason: String },
}

/// `attach_session` 的 typed 结果(transport-backend-design.md §1.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachOutcome {
    Attached,
    /// wezterm:GUI 启动即 attach,无独立动作。
    GuiAttachIsImplicit,
    /// conpty:无 attach 概念。
    Unsupported { reason: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// TransportError(thiserror,lib 边界;transport-backend-design.md §1.3)
// ─────────────────────────────────────────────────────────────────────────────

/// transport 的 I/O / 子进程错误。能力性拒绝**不**走这里(用上面的 typed outcome)。
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("spawn failed on {backend:?}: {source}")]
    Spawn {
        backend: BackendKind,
        #[source]
        source: std::io::Error,
    },
    #[error("inject failed at stage {stage:?}: {source}")]
    Inject {
        stage: InjectStage,
        #[source]
        source: std::io::Error,
    },
    #[error("capture failed: {source}")]
    Capture {
        #[source]
        source: std::io::Error,
    },
    /// tmux/wezterm cli 非 0 退出。
    #[error("subprocess {argv:?} exited with {code:?}: {stderr}")]
    Subprocess {
        argv: Vec<String>,
        code: Option<i32>,
        stderr: String,
    },
    /// wezterm mux 连不上 / tmux server 不在。
    #[error("mux unavailable on {backend:?}: {detail}")]
    MuxUnavailable { backend: BackendKind, detail: String },
    #[error("target not found: {target}")]
    TargetNotFound { target: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// ─────────────────────────────────────────────────────────────────────────────
// Transport trait(控制面;transport-backend-design.md §1.3)—— 仅签名,无 body
// ─────────────────────────────────────────────────────────────────────────────

/// 控制面:把字节送进按稳定 id 寻址的终端 + 枚举/探活/生命周期。
///
/// 全部 daemon 可达方法返 `Result<_, TransportError>`(§10:I/O/子进程错误用 `Err`,
/// 能力性拒绝用 typed outcome variant)。投递语义(发给谁/为何发/可见性验证启发式)
/// 在 transport **之上**(step 10/11),不进 trait(§gap-recognizer)。
pub trait Transport: Send + Sync {
    /// 后端种类(诊断/事件用)。
    fn kind(&self) -> BackendKind;

    /// Only the concrete tmux backend should scan real tmux socket roots.
    /// Test doubles stay hermetic and use their injected probe results.
    fn probes_real_tmux_socket_roots(&self) -> bool {
        false
    }

    /// Physical tmux endpoint used by this transport when known. For tmux this is
    /// either a full `-S` socket path or a `-L` socket name; non-tmux/test
    /// transports can leave it unknown.
    fn tmux_endpoint(&self) -> Option<String> {
        None
    }

    // —— SPAWN(ST):所有后端天然满足;cwd/env 是 spawn 参数,无独立动词(§gap-setenv)——

    /// tmux=`new-session -d` / wezterm=`spawn --new-window` / conpty=`openpty`+spawn。
    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError>;

    /// tmux=`new-window` / wezterm=`spawn --pane-id 锚` / conpty=再 `openpty` 进内存表。
    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError>;

    /// D9 (#264): spawn + profile `env_unset` keys that must be REALLY unset in the worker
    /// shell line (Python providers.py:142-145 sources an env file whose first lines are
    /// `unset <KEY>`) — the tmux server environment can carry stale values that plain
    /// env-map removal cannot clear. Default forwards to the plain spawn: backends without
    /// an inherited-shell layer have nothing stale to unset.
    fn spawn_first_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let _ = env_unset;
        self.spawn_first(session, window, argv, cwd, env)
    }

    /// 同 [`Transport::spawn_first_with_env_unset`],对应 `spawn_into`。
    fn spawn_into_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let _ = env_unset;
        self.spawn_into(session, window, argv, cwd, env)
    }

    /// Spawn a worker as an additional pane in an existing session/window.
    /// Backends without a split-pane primitive may conservatively fall back to
    /// `spawn_into`; tmux overrides this with `split-window`.
    fn spawn_split_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let _ = env_unset;
        self.spawn_into(session, window, argv, cwd, env)
    }

    /// 0.4.x (CR C-2): leader-specific spawn variant. Instead of `exec <cmd>`
    /// (which makes the provider the pane's primary process and turns the
    /// pane into `[exited]` when the provider exits), build a shell line
    /// that runs the provider as a CHILD of a long-lived shell:
    /// `cd ... && unset ... && KEY=val ... <cmd>; rc=$?; printf '\n[team-agent] <provider> exited with %s\n' "$rc"; exec "${SHELL:-/bin/zsh}" -l`.
    /// When the provider exits, the pane returns to an interactive shell with
    /// an explicit exit marker — matching manual `tmux new-session` then
    /// `claude` behaviour. Default falls back to plain `spawn_first_with_env_unset`
    /// for backends that have no shell layer (test-only `OfflineTransport`).
    fn spawn_first_with_leader_shell_wrapper(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        provider_label: &str,
    ) -> Result<SpawnResult, TransportError> {
        let _ = provider_label;
        self.spawn_first_with_env_unset(session, window, argv, cwd, env, env_unset)
    }

    /// 同 [`Transport::spawn_first_with_leader_shell_wrapper`],对应 `spawn_into`。
    fn spawn_into_with_leader_shell_wrapper(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        provider_label: &str,
    ) -> Result<SpawnResult, TransportError> {
        let _ = provider_label;
        self.spawn_into_with_env_unset(session, window, argv, cwd, env, env_unset)
    }

    // —— INJECT / CAPTURE / QUERY(RIE):按稳定 Target 寻址 ——

    /// 归并 set/load-buffer + paste-buffer + send submit;空文本走纯 send-keys
    /// (`InjectPayload::Empty`)。仅保证「字节进去了 + 提交键发了」+ typed 阶段化报告;
    /// 可见性验证启发式在 transport 之上(§gap-recognizer)。
    fn inject(
        &self,
        target: &Target,
        payload: &InjectPayload,
        submit: Key,
        bracketed: bool,
    ) -> Result<InjectReport, TransportError>;

    fn send_keys(&self, target: &Target, keys: &[Key]) -> Result<(), TransportError>;

    fn capture(
        &self,
        target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError>;

    /// 非 tmux 后端无对应概念的字段返回 `Ok(None)`(typed「不适用」)。
    fn query(&self, target: &Target, field: PaneField)
        -> Result<Option<String>, TransportError>;

    /// pane 存活三态(`PaneLiveness`,bug-085 穷尽 match;unknown ≠ dead ≠ live)。
    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError>;

    /// Cheap direct pane existence check when a backend can prove it. `Ok(None)`
    /// preserves the existing Unknown boundary.
    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let _ = pane;
        Ok(None)
    }

    // —— ENUMERATE / IDENTITY(SL + 进程探测):身份/rebind 地基 ——

    /// 全局枚举所有 pane + 每 pane 的 leader_env。tmux=`list-panes -a` + 读进程 env;
    /// wezterm=`cli list --format json` + 正向登记表;conpty=daemon 内存表(仅自有,§4a)。
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError>;

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError>;

    fn list_windows(
        &self,
        session: &SessionName,
    ) -> Result<Vec<WindowName>, TransportError>;

    fn configure_adaptive_pane_title(
        &self,
        session: &SessionName,
        window: &WindowName,
        pane: &PaneId,
        title: &str,
    ) -> Result<(), TransportError> {
        let _ = (session, window, pane, title);
        Ok(())
    }

    /// tmux=`set-environment`;无 server-env 的后端(WezTerm/ConPTY)对 worker 内化为
    /// 「spawn 时注入」(`InternalizedAtSpawn`),对外部 leader pane 返回 typed 不支持
    /// (`UnsupportedForExternalPane`,§4c)。
    fn set_session_env(
        &self,
        session: &SessionName,
        key: &str,
        value: &str,
    ) -> Result<SetEnvOutcome, TransportError>;

    // —— LIFECYCLE(SL)——

    fn kill_server(&self) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError>;

    fn kill_window(&self, target: &Target) -> Result<(), TransportError>;

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        self.kill_window(&Target::Pane(pane.clone()))
    }

    /// 交互前台 attach(leader 用)。tmux=`attach-session`;wezterm=GUI 即 attach
    /// (`GuiAttachIsImplicit`);conpty=typed 不支持(`Unsupported`)。
    fn attach_session(
        &self,
        session: &SessionName,
    ) -> Result<AttachOutcome, TransportError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// 命令构造 seam(STEP-9 头号目标:exact tmux command construction).
//
// trait 只回高层 typed 值(SpawnResult/InjectReport/CapturedText/Option<String>),
// 永远拿不到它构的 argv —— 因此「set/load/paste-buffer -p、capture-pane -S -<N>、
// display-message -F #{pane_width}、send-keys 键名翻译、CancelMode→-X cancel/q/d」
// 这些 golden 在 trait 层**无法断言**。这里引入纯函数构造 seam(无 I/O,可单测):
// tmux 后端的实现必须用这些 fn 构 argv,契约则 golden-lock 它们的输出。
// 真相源:tmux_io.py / tmux_prompt.py / runtime.py / delivery.py / terminal.py /
// provider_cli/codex.py(argv 已 golden-probe via /tmp/transport_cmd_golden.py)。
// ─────────────────────────────────────────────────────────────────────────────

/// tmux 进入的特殊 mode(CancelMode 退出键分派的输入;tmux_io.py:416-427)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMode {
    /// `copy-mode` → `-X cancel`。
    Copy,
    /// `tree-mode` → `q`。
    Tree,
    /// `view-mode` → `q`。
    View,
    /// `client-mode` → `d`。
    Client,
    /// 其它/未知 → `-X cancel` + warn(`pane_mode_unknown_cancel_attempted`)。
    Unknown,
}

/// 把抽象 `Key` 翻译成 tmux send-keys 字面键名(各后端翻译,不透传字面量;§gap-5)。
/// tmux:Enter/Up/Down/Left/Right/数字字符/C-c;CancelMode 不是单键(走 cancel_mode_argv)。
/// 真相源:codex.py:266 `send-keys -t %7 Down Enter`、tmux send-keys 键名约定。
pub fn tmux_key_name(key: Key) -> &'static str {
    match key {
        Key::Enter => "Enter",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::Left => "Left",
        Key::Right => "Right",
        Key::Char('0') => "0",
        Key::Char('1') => "1",
        Key::Char('2') => "2",
        Key::Char('3') => "3",
        Key::Char('4') => "4",
        Key::Char('5') => "5",
        Key::Char('6') => "6",
        Key::Char('7') => "7",
        Key::Char('8') => "8",
        Key::Char('9') => "9",
        Key::CtrlC => "C-c",
        // E46 (0.3.24 bug#5): tmux supports `Escape` as a key name; sending
        // it as a send-keys arg emits `\x1b` to the pane (closes bracketed
        // paste mode on a stuck TUI composer).
        Key::Escape => "Escape",
        Key::CancelMode | Key::Char(_) => "",
    }
}

/// `send-keys -t <target> <k1> <k2> ...`(键名经 `tmux_key_name` 翻译)。
/// golden:`[Down, Enter]` → `["tmux","send-keys","-t","%7","Down","Enter"]`(codex.py:266)。
pub fn tmux_send_keys_argv(pane: &PaneId, keys: &[Key]) -> Vec<String> {
    let mut argv = vec![
        "tmux".to_string(),
        "send-keys".to_string(),
        "-t".to_string(),
        pane.as_str().to_string(),
    ];
    argv.extend(
        keys.iter()
            .copied()
            .map(tmux_key_name)
            .filter(|k| !k.is_empty())
            .map(str::to_string),
    );
    argv
}

/// CancelMode 在 tmux 上按 pane mode 分派退出键(tmux_io.py:419-426)。
/// golden:Copy→`-X cancel`,Tree/View→`q`,Client→`d`,Unknown→`-X cancel`(+warn)。
pub fn tmux_cancel_mode_argv(pane: &PaneId, mode: PaneMode) -> Vec<String> {
    let mut argv = vec![
        "tmux".to_string(),
        "send-keys".to_string(),
        "-t".to_string(),
        pane.as_str().to_string(),
    ];
    match mode {
        PaneMode::Copy | PaneMode::Unknown => {
            argv.push("-X".to_string());
            argv.push("cancel".to_string());
        }
        PaneMode::Tree | PaneMode::View => argv.push("q".to_string()),
        PaneMode::Client => argv.push("d".to_string()),
    }
    argv
}

/// CaptureRange → `capture-pane -p -S <spec> -t <target>`。
/// golden:`Tail(40)` → `-S -40`(tmux_prompt.py:149/tmux_io.py:410);
///         `Full` → `-S -`(runtime.py:519)。
pub fn tmux_capture_argv(pane: &PaneId, range: CaptureRange) -> Vec<String> {
    let spec = match range {
        CaptureRange::Tail(lines) => format!("-{lines}"),
        CaptureRange::Head(_) => "0".to_string(),
        CaptureRange::Full => "-".to_string(),
    };
    let mut argv = vec![
        "tmux".to_string(),
        "capture-pane".to_string(),
        "-p".to_string(),
        "-S".to_string(),
        spec,
        "-t".to_string(),
        pane.as_str().to_string(),
    ];
    if let CaptureRange::Head(lines) = range {
        argv.extend(["-E".to_string(), lines.saturating_sub(1).to_string()]);
    }
    argv
}

/// PaneField → `display-message -p -t <target> [-F] <fmt>`。
/// golden:PaneWidth → `-F '#{pane_width}'`(delivery.py:34);
///         PaneMode → `'#{pane_mode}'`(tmux_io.py:403);PaneId → `'#{pane_id}'`(state.py:346)。
pub fn tmux_query_argv(pane: &PaneId, field: PaneField) -> Vec<String> {
    let fmt = match field {
        PaneField::PaneId => "#{pane_id}",
        PaneField::PaneMode => "#{pane_mode}",
        PaneField::PaneWidth => "#{pane_width}",
        PaneField::PaneCurrentCommand => "#{pane_current_command}",
        PaneField::PaneCurrentPath => "#{pane_current_path}",
        PaneField::SessionName => "#{session_name}",
        PaneField::PaneTty => "#{pane_tty}",
    };
    let mut argv = vec![
        "tmux".to_string(),
        "display-message".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        pane.as_str().to_string(),
    ];
    if !matches!(field, PaneField::PaneMode) {
        argv.push("-F".to_string());
    }
    argv.push(fmt.to_string());
    argv
}

/// spawn argv:首个 session 用 new-session,后续 worker 用 new-window。
/// golden(terminal.py:44-45 / runtime.py:1019-1020):
///   first → `new-session -d -s <s> -n <w> sh -lc <cmd>`
///   into  → `new-window -t <s> -n <w> sh -lc <cmd>`
/// `argv` 被组装成单条 `sh -lc` 命令字符串(provider 启动行)。
pub fn tmux_spawn_argv(
    session: &SessionName,
    window: &WindowName,
    command: &str,
    first: bool,
) -> Vec<String> {
    if first {
        vec![
            "tmux".to_string(),
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            session.as_str().to_string(),
            "-n".to_string(),
            window.as_str().to_string(),
            "sh".to_string(),
            "-lc".to_string(),
            command.to_string(),
        ]
    } else {
        // E53 (0.3.26, adaptive layout same-session tabs): `-d` makes the new
        // window start without switching the client's active window to it. The
        // leader stays on its own window; the worker opens as a background tab.
        // Without `-d` every `new-window` call yanks the leader's terminal to
        // the freshly spawned worker window, disrupting whatever the leader is
        // doing. `-d` matches the Python golden: the managed leader and all
        // workers share the same tmux session (= same terminal window tabs).
        vec![
            "tmux".to_string(),
            "new-window".to_string(),
            "-d".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
            "-n".to_string(),
            window.as_str().to_string(),
            "sh".to_string(),
            "-lc".to_string(),
            command.to_string(),
        ]
    }
}

/// inject 文本路径的 buffer 构造序列(空文本不走这里,走 tmux_empty_inject_argv)。
/// 返回有序 argv 列表:set-buffer(小)或 load-buffer -(大,>=阈值)→ paste-buffer -p →
/// delete-buffer。golden(tmux_io.py:119/303/314/324):
///   set-buffer  → `set-buffer -b <buf> <text>`
///   paste-buffer→ `paste-buffer -t <target> -b <buf> -p`(-p = bracketed)
///   delete-buffer→`delete-buffer -b <buf>`
pub fn tmux_inject_text_argv(
    pane: &PaneId,
    buffer_name: &str,
    text: &str,
    bracketed: bool,
) -> Vec<Vec<String>> {
    const TMUX_STDIN_BUFFER_THRESHOLD: usize = 16 * 1024;
    let mut paste = vec![
        "tmux".to_string(),
        "paste-buffer".to_string(),
        "-t".to_string(),
        pane.as_str().to_string(),
        "-b".to_string(),
        buffer_name.to_string(),
    ];
    if bracketed {
        paste.push("-p".to_string());
    }
    let load = if text.len() >= TMUX_STDIN_BUFFER_THRESHOLD {
        vec![
            "tmux".to_string(),
            "load-buffer".to_string(),
            "-b".to_string(),
            buffer_name.to_string(),
            "-".to_string(),
        ]
    } else {
        vec![
            "tmux".to_string(),
            "set-buffer".to_string(),
            "-b".to_string(),
            buffer_name.to_string(),
            text.to_string(),
        ]
    };
    vec![
        load,
        paste,
        vec![
            "tmux".to_string(),
            "delete-buffer".to_string(),
            "-b".to_string(),
            buffer_name.to_string(),
        ],
    ]
}

/// 空文本 inject:纯 `send-keys -t <target> <submit_key>`,**禁** buffer 路径
/// (tmux 拒空 buffer 会卡 trust prompt;tmux_io.py:42)。
pub fn tmux_empty_inject_argv(pane: &PaneId, submit: Key) -> Vec<String> {
    tmux_send_keys_argv(pane, &[submit])
}

/// capture 出口规范化(§4b,design line 399-400):逐行 rstrip 行尾空白 +
/// `\xa0`(NBSP)→`\x20`,保留 box-drawing。recognizer 不为后端开分支。
/// golden:`"line one  \nbusy\xa0marker   \n  \n"` → `"line one\nbusy marker\n\n"`。
pub fn normalize_capture(raw: &str) -> String {
    raw.replace('\u{a0}', " ")
        .split_inclusive('\n')
        .map(|line| {
            if let Some(stripped) = line.strip_suffix('\n') {
                let mut s = stripped.trim_end().to_string();
                s.push('\n');
                s
            } else {
                line.trim_end().to_string()
            }
        })
        .collect()
}

/// `SubmitVerification` → wire 字符串(tmux_io.py:64/215-221, tmux_prompt.py:304/313)。
/// 模板:`{key}_sent_after_visible_token`(key 由 variant 携带,经 tmux_key_name 取名)。
/// 真相源:`enter_sent_without_placeholder_check` /
///        `pasted_content_prompt_absent_after_submit` / `send_keys_failed` /
///        `Enter_sent_after_visible_token`(submit_key 字面)。
pub fn submit_verification_wire(v: SubmitVerification) -> String {
    match v {
        SubmitVerification::EnterSentWithoutPlaceholderCheck => {
            "enter_sent_without_placeholder_check".to_string()
        }
        SubmitVerification::PastedContentPromptAbsentAfterSubmit => {
            "pasted_content_prompt_absent_after_submit".to_string()
        }
        SubmitVerification::PastedContentPromptStillPresentAfterSubmit => {
            "pasted_content_prompt_still_present_after_submit".to_string()
        }
        SubmitVerification::KeySentAfterVisibleToken { key } => {
            format!("{}_sent_after_visible_token", tmux_key_name(key))
        }
        SubmitVerification::SendKeysFailed => "send_keys_failed".to_string(),
        SubmitVerification::SubmitConsumptionUnverified => {
            "submit_consumption_unverified".to_string()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RED contracts — step 9 transport(ROUND-0).
//
// 真相源:Python team-agent-public @ v0.2.11(`terminal.py` / `messaging/tmux_io.py`
// / `runtime.py`)golden-locked via /tmp/transport_golden.py;rust-native parity
// 来自 `docs/phase0/transport-backend-design.md` §1/§3 + `contracts-rust-native.yaml`。
//
// 这些测试通过一个 **每个方法都 `unimplemented!()`** 的 `StubBackend` 驱动 `Transport`
// trait(skeleton 无任何 impl),因此跑起来即 panic == 真 RED;porter 填实现后转 GREEN。
// 另有一批 **纯 typed-payload / serde 字节锁** 断言(命令面字符串、verification 阶梯、
// 寻址稳定性)—— 它们锁住契约形状,porter 改坏类型/serde rename 立即失败。
//
// [真机] 标记的契约(WezTerm `cli list` / B1 等)`#[ignore]` —— 无法在此完整断言,
// 仅锁形状,实现期真机校验。
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests;
