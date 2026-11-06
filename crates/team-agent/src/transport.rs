//! ---
//! purpose: 终端控制面的 Transport trait、typed 载荷,以及 tmux argv 的纯构造函数
//! contract:
//!   provides:
//!     - name: Transport
//!       what: 后端无关的 spawn / inject / capture / query / liveness / lifecycle 接口;daemon 可达方法一律返回 Result<_, TransportError>
//!     - name: SetEnvOutcome
//!       what: set-env 的能力性拒绝走 typed 变体,不走 Err、不 panic
//!     - name: AttachOutcome
//!       what: attach 的能力性拒绝走 typed 变体,不走 Err、不 panic
//!     - name: tmux_spawn_argv
//!       what: spawn argv 的唯一构造点(first→new-session / 否则 new-window),纯函数可 golden 锁
//!     - name: tmux_inject_text_argv
//!       what: 文本注入三步 argv(装 buffer→paste→删 buffer)的唯一构造点
//!     - name: tmux_send_submit_argv
//!       what: 提交键 argv 的唯一构造点,Enter 保持逻辑 Return 而非 Ctrl-M
//!     - name: tmux_capture_argv
//!       what: capture-pane argv 的唯一构造点
//!     - name: tmux_query_argv
//!       what: display-message 单字段查询 argv 的唯一构造点
//!     - name: tmux_cancel_mode_argv
//!       what: 按 pane mode 分派退出键的 argv 构造点
//!     - name: InjectReport
//!       what: 阶段化注入报告(stage_reached / inject_verification / submit_verification / turn_verification / attempts / submit_diagnostics)
//!     - name: submit_verification_wire
//!       what: SubmitVerification 落 JSON 的唯一拼写来源
//!     - name: turn_verification_wire
//!       what: TurnVerification 落 JSON 的唯一拼写来源
//!   depends:
//!     - crate::model::enums::PaneLiveness
//!     - serde
//!     - thiserror
//! boundary:
//!   - 不做投递语义(发给谁 / 为何发 / 可见性启发式)——那在 transport 之上的 delivery 层
//!   - 本文件不执行 I/O;真正跑 argv 的是 tmux_backend
//!   - turn_verification 不是投递闸门(Gap42),not_yet_observed 也算成功
//!   - 不透传后端字面键名:抽象 Key 由各后端自行翻译
//!   - submit_diagnostics 不上 typed wire,只在进程内传递
//! maturity: wired
//! ---
//!
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

pub mod native_input;

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
    /// ---
    /// purpose: 把已知的 pane 标识字符串包成 PaneId newtype
    /// params:
    ///   s: 后端给出的 pane 标识原文;不做校验、不归一化
    /// returns: 与入参字节相同的 PaneId(serde transparent)
    /// boundary: 不校验格式、不区分 tmux/wezterm/conpty 方言;混入 window/session 名由类型系统在调用点挡
    /// ---
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// ---
    /// purpose: 借出 pane 标识的原始字符串,供 argv 拼装
    /// returns: 内部字符串的借用,与构造时字节一致
    /// boundary: 不做转义、不加引号;shell 引用由调用方负责
    /// ---
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
    /// ---
    /// purpose: 把 tmux session 名包成 SessionName newtype
    /// params:
    ///   s: session 名原文;不做校验、不归一化
    /// returns: 与入参字节相同的 SessionName(serde transparent)
    /// boundary: 不校验 session 是否存在(那是 Transport::has_session)
    /// ---
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// ---
    /// purpose: 借出 session 名原文,供 argv 拼装
    /// returns: 内部字符串的借用,与构造时字节一致
    /// boundary: 不做转义、不加引号
    /// ---
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
    /// ---
    /// purpose: 把 tmux window 名包成 WindowName newtype
    /// params:
    ///   s: window 名原文;不做校验、不归一化
    /// returns: 与入参字节相同的 WindowName(serde transparent)
    /// boundary: 不校验 window 是否存在(那是 Transport::list_windows)
    /// ---
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// ---
    /// purpose: 借出 window 名原文,供 argv 拼装
    /// returns: 内部字符串的借用,与构造时字节一致
    /// boundary: 不做转义、不加引号
    /// ---
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



/// 抽象 Key 枚举(§gap-5):各后端翻译,不透传 tmux 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// tmux `Enter` / VT carriage return.
    Enter,
    Up,
    Down,
    /// Leave copy mode; backends without copy mode perform no operation.
    CancelMode,
}

/// capture 范围(tmux `-S -<N>` / `-S -`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRange {
    Tail(u32),
    Full,
}

/// 单字段查询(display-message -p -F);非 tmux 后端无对应概念的字段返回 typed
/// 「不适用」(trait `query` 返回 `Option<String>`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneField {
    PaneMode,
    PaneCurrentCommand,
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
pub struct SessionOwner {
    pub workspace: String,
    pub team: String,
    pub generation: String,
}

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

















// ─────────────────────────────────────────────────────────────────────────────
// 能力性拒绝的 typed 结果(非 Err —— 已知能力差,审计用;§10)
// ─────────────────────────────────────────────────────────────────────────────



// ─────────────────────────────────────────────────────────────────────────────
// TransportError(thiserror,lib 边界;transport-backend-design.md §1.3)
// ─────────────────────────────────────────────────────────────────────────────

/// transport 的 I/O / 子进程错误。能力性拒绝**不**走这里(用上面的 typed outcome)。
#[derive(Debug, Error)]
pub enum TransportError {
    /// Guard refused before a paste/key; the owned evidence carries any later effect floor.
    #[error("native input not ready: {reason}")]
    NativeInputNotReady { reason: String },
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
    /// The final tmux spawn argv is too large for the transport command envelope.
    /// Keep this separate from `Subprocess` so callers do not have to parse a
    /// provider-sized argv dump to learn the actionable limit.
    #[error(
        "command_too_long: backend={backend:?}; segment={segment}; session={session}; window={window}; actual_bytes={actual_bytes}; limit_bytes={limit_bytes}"
    )]
    CommandTooLong {
        backend: BackendKind,
        segment: String,
        session: String,
        window: String,
        actual_bytes: usize,
        limit_bytes: usize,
    },
    /// wezterm mux 连不上 / tmux server 不在。
    #[error("mux unavailable on {backend:?}: {detail}")]
    MuxUnavailable {
        backend: BackendKind,
        detail: String,
    },
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

    /// 0.5.39 Slice 2 (tmux-server-death-locate §7 Slice 2): worker spawn
    /// variant that reuses the leader-wrapper mechanism so worker provider
    /// exit does not collapse the pane into `[exited]` (which under
    /// upstream tmux 3.6a private-server bugs can cascade into server
    /// death). When the provider exits, the worker pane keeps an explicit
    /// worker exit marker and runs the inert `/bin/sh -c` tail: it ignores
    /// INT/QUIT and does not read pane stdin. Default falls back to plain
    /// `spawn_first_with_env_unset` for backends that have no shell layer
    /// (test-only `OfflineTransport`).
    fn spawn_first_with_worker_shell_wrapper(
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

    /// 同 [`Transport::spawn_first_with_worker_shell_wrapper`],对应 `spawn_into`。
    fn spawn_into_with_worker_shell_wrapper(
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

    /// 0.4.x (CR C-2): leader-specific spawn variant. Instead of `exec <cmd>`
    /// (which makes the provider the pane's primary process and turns the
    /// pane into `[exited]` when the provider exits), build a shell line
    /// that runs the provider as a CHILD of a long-lived shell. When the
    /// provider exits, the wrapper records the exit code and marker, then
    /// execs an inert `/bin/sh -c` tail that ignores INT/QUIT and does not
    /// read pane stdin. Default falls back to plain
    /// `spawn_first_with_env_unset` for backends that have no shell layer
    /// (test-only `OfflineTransport`).
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



    /// Observation only; None means this backend/provider has no native parser.
    /// An implemented parser with an incomplete frame returns an error, not absence.
    fn observe_native_input(
        &self,
        _target: &Target,
        _provider: crate::model::enums::Provider,
    ) -> Result<Option<native_input::InputFrame>, TransportError> {
        Ok(None)
    }




    fn send_keys(&self, target: &Target, keys: &[Key]) -> Result<(), TransportError>;

    fn capture(&self, target: &Target, range: CaptureRange)
        -> Result<CapturedText, TransportError>;

    /// 非 tmux 后端无对应概念的字段返回 `Ok(None)`(typed「不适用」)。
    fn query(&self, target: &Target, field: PaneField) -> Result<Option<String>, TransportError>;

    /// pane 存活三态(`PaneLiveness`,bug-085 穷尽 match;unknown ≠ dead ≠ live)。
    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError>;

    /// Exit status recorded by the provider's supervising wrapper, never inferred
    /// from terminal text. None means running or no authoritative receipt.
    fn provider_exit_status(&self, _pane: &PaneId) -> Result<Option<u8>, TransportError> {
        Ok(None)
    }

    /// Native dead-pane observation. Only Some(true) proves no live pane
    /// process; a missing/unsupported sample must remain unknown.
    fn pane_is_dead(&self, _pane: &PaneId) -> Result<Option<bool>, TransportError> {
        Ok(None)
    }

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

    /// Write an owner marker at the session scope after a new session is created.
    /// Backends without a session-local metadata primitive may no-op, but then
    /// shutdown must treat the missing marker as unknown and refuse destructive work.
    fn set_session_owner(
        &self,
        _session: &SessionName,
        _workspace: &Path,
        _team: &str,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    /// Write owner metadata including the launch generation when the backend
    /// can persist one.  The default preserves compatibility for non-tmux
    /// transports, which remain Unknown on reads.
    fn set_session_owner_with_generation(
        &self,
        session: &SessionName,
        workspace: &Path,
        team: &str,
        _generation: &str,
    ) -> Result<(), TransportError> {
        self.set_session_owner(session, workspace, team)
    }

    /// Read the session-local owner marker. `None` is an unknown/legacy session,
    /// never proof that the caller owns it.
    fn session_owner(
        &self,
        _session: &SessionName,
    ) -> Result<Option<SessionOwner>, TransportError> {
        Ok(None)
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError>;

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError>;

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


    /// A-55 (window rename race): set a tmux per-window option (e.g.
    /// `allow-rename off`, `automatic-rename off`) on an existing window.
    /// tmux=`set-window-option -t SESSION:WINDOW`; non-tmux backends refuse
    /// via the default (typed unsupported).
    fn set_window_option(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _option: &str,
        _value: &str,
    ) -> Result<(), TransportError> {
        Err(TransportError::MuxUnavailable {
            backend: self.kind(),
            detail: "set-window-option is tmux-only".to_string(),
        })
    }

    // —— LIFECYCLE(SL)——

    /// Before stopping a seat, preserve its session if removing these targets
    /// would otherwise destroy it. This is not called by team shutdown/restart.
    /// Backends whose sessions outlive their panes need no special handling.
    fn preserve_session_before_stop(
        &self,
        _session: &SessionName,
        _targets: &[Target],
    ) -> Result<(), TransportError> {
        Ok(())
    }


    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError>;

    fn kill_window(&self, target: &Target) -> Result<(), TransportError>;

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        self.kill_window(&Target::Pane(pane.clone()))
    }

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
/// 菜单/方向键序列继续用这里的 `Enter`。投递提交键走 [`tmux_submit_key_name`]。
/// ---
/// purpose: 抽象 Key 到 tmux send-keys 字面键名的翻译表
/// params:
///   key: 抽象按键;CancelMode 与非数字 Char 无单键拼写
/// returns: tmux 键名;CancelMode 与未覆盖的 Char 返回空串,由调用方过滤掉
/// boundary: 不构 argv、不发键;CancelMode 的真正出口是 tmux_cancel_mode_argv;提交键拼写不在这里(见 tmux_submit_key_name)
/// ---
pub fn tmux_key_name(key: Key) -> &'static str {
    match key {
        Key::Enter => "Enter",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::CancelMode => "",
    }
}

/// `send-keys -t <target> <k1> <k2> ...`(键名经 `tmux_key_name` 翻译)。
/// golden:`[Down, Enter]` → `["tmux","send-keys","-t","%7","Down","Enter"]`(codex.py:266)。
/// ---
/// purpose: 构造按键序列的 send-keys argv(菜单/方向键路径)
/// params:
///   pane: 目标 pane 标识,直接进 -t
///   keys: 按顺序翻译成键名;翻译为空串的键被丢弃,不占位
/// returns: 完整 argv;keys 全为空串时退化成只有前缀的 send-keys(不会报错)
/// boundary: 纯构造,不执行;不加 socket 参数(-L/-S 由 tmux_backend 的 run chokepoint 补);此处的 Enter 是菜单键名,不是提交拼写
/// ---
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
/// ---
/// purpose: 按 pane 当前 mode 分派退出键,构造离开特殊 mode 的 argv
/// params:
///   pane: 目标 pane 标识
///   mode: 已探到的 pane mode;Unknown 保守走 -X cancel
/// returns: 单条 send-keys argv,尾部按 mode 取 -X cancel / q / d
/// boundary: 纯构造,不执行、不探测 mode(探测在 tmux_backend);不保证 mode 真的退出了,调用方需复查
/// ---
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
/// ---
/// purpose: 把 CaptureRange 翻成 capture-pane 的起止参数
/// params:
///   pane: 目标 pane 标识
///   range: Tail(n) 取倒数 n 行 -S -n;Head(n) 取 -S 0 并追加 -E n-1;Full 取 -S -
/// returns: 完整 capture-pane argv;Head(0) 时 -E 饱和到 0(不会下溢)
/// boundary: 纯构造,不执行、不解析输出;抓回来的文本要经 normalize_capture 才交给识别层
/// ---
pub fn tmux_capture_argv(pane: &PaneId, range: CaptureRange) -> Vec<String> {
    let spec = match range {
        CaptureRange::Tail(lines) => format!("-{lines}"),
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
    argv
}

/// PaneField → `display-message -p -t <target> [-F] <fmt>`。
/// golden:PaneWidth → `-F '#{pane_width}'`(delivery.py:34);
///         PaneMode → `'#{pane_mode}'`(tmux_io.py:403);PaneId → `'#{pane_id}'`(state.py:346)。
/// ---
/// purpose: 把 PaneField 翻成 display-message 的单字段查询 argv
/// params:
///   pane: 目标 pane 标识
///   field: 待查字段;PaneMode 走不带 -F 的形式,其余字段加 -F
/// returns: 完整 display-message argv,末位是 tmux 格式串
/// boundary: 纯构造,不执行、不解析;查询失败与「字段不适用」的区分在 Transport::query 的 Option 返回值上
/// ---
pub fn tmux_query_argv(pane: &PaneId, field: PaneField) -> Vec<String> {
    let fmt = match field {
        PaneField::PaneMode => "#{pane_mode}",
        PaneField::PaneCurrentCommand => "#{pane_current_command}",
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
///   first → `new-session -d -P -F #{pane_id} -s <s> -n <w> sh -lc <cmd>`
///   into  → `new-window -d -P -F #{pane_id} -t <s> -n <w> sh -lc <cmd>`
/// `argv` 被组装成单条 `sh -lc` 命令字符串(provider 启动行)。
/// ---
/// purpose: 构造 spawn argv:首个席位用 new-session,后续席位用 new-window
/// params:
///   session: 目标 session 名
///   window: 新窗口名
///   command: 已拼好的单条 shell 命令行,原样进 sh -lc(本函数不做引用/转义)
///   first: true 走 new-session,false 走 new-window
/// returns: 完整 argv;两支都带 -d(后台创建,不抢客户端焦点)与 -P -F 使 tmux 回打 pane_id
/// boundary: 纯构造,不执行、不校验 session 是否已存在;command 的 shell 引用由调用方(tmux_backend 的 shell wrapper)负责
/// ---
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
            "-P".to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
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
            "-P".to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
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

/// capture 出口规范化(§4b,design line 399-400):逐行 rstrip 行尾空白 +
/// `\xa0`(NBSP)→`\x20`,保留 box-drawing。recognizer 不为后端开分支。
/// golden:`"line one  \nbusy\xa0marker   \n  \n"` → `"line one\nbusy marker\n\n"`。
/// ---
/// purpose: capture 出口的统一规范化,让识别层不必为后端开分支
/// params:
///   raw: 后端抓回的原始屏幕文本
/// returns: 逐行去掉行尾空白、NBSP 归一为普通空格的文本;换行位置与行数不变
/// boundary: 只做这两件事——不剥 ANSI 转义、不裁剪行数、不脱敏、不改 box-drawing 字符
/// ---
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
