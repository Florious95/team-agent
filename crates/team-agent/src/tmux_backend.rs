//! ---
//! purpose: 通过 tmux argv 执行 spawn/inject/capture
//! contract:
//!   provides:
//!     - name: TmuxBackend
//!       what: Transport 的 tmux 实现，inject/send_keys 取 pane 输入锁
//!     - name: submit-consumption-verdict
//!       what: token 见过再消失且本次占位符身份（#N / grok 行数或 KB）已离开 composer ⇒ 已消费；Gone 时占位符仍在 ⇒ 未消费；无身份时只按一次回车；consumed=None（capture 失败/读数拿不到）⇒ SubmitConsumptionUnverified，不得与 Some(true) 同值
//!     - name: token-sighting
//!       what: Visible / Gone / NeverSeen 三态接到判定（不只进报告）
//!     - name: pre-submit-copy-mode-cancel
//!       what: 统一 prepare_pane_for_submit：Enter 前 cancel 一切非 0 pane_mode（按真实 mode 分派）并字面闭合 CSI 201~（不是 Escape）；A1 Empty / A3 skip-poll / A4 Phase2 / A6 send_keys(含 Enter) 都走这里
//!     - name: turn-inbox-vs-run
//!       what: busy ⇒ Verified(开跑)；composer 仍有本次粘贴且无 busy ⇒ Missing(没开跑)；其余 NotYetObserved(不知道)
//!     - name: capture-fail-retry
//!       what: post-Enter capture 失败只重读 capture，计入 attempts，不重粘、不加 Enter
//!     - name: cursor-single-enter
//!       what: TLS 打开时，busy 或 token 不在输入区（底部 5 非空行）则不重按 Enter；transcript 里的 pasted #N 不算输入框
//!     - name: unverified-composer-resend
//!       what: SubmitConsumptionUnverified 且本次身份仍在 composer 且 A 的 should_resubmit_enter 为假（折行拼接缺口）⇒ 只补一颗 C-m；A 因 latch/底15 单行 token 已能重试（含打满 cap）时 B 不介入；consumed=None 不补；达上限 1 或身份消失或已消费或 busy 则停。不认提示符皮肤。
//! boundary:
//!   - 不把 fire-and-forget 报成 delivered
//!   - 不把「粘贴命中」(any_attempt_matched) 当成已提交
//!   - 不把「没能判断」(consumed=None) 落到成功值
//!   - turn_verification 不是投递闸门；分辨不出不得报开跑
//!   - 补发只重按回车，不重粘、不用 Escape/C-c；闸是折行缺口（A 看不到、拼接身份还在），不是提示符字符串、也不是 pane 忙闲；A 已覆盖的 latch 滞留不走 B
//! maturity: wired
//! ---
//!
//! Concrete tmux `Transport` backend (SKELETON) — the real executor that runs `tmux <argv>`.
//!
//! step 9 shipped the [`crate::transport::Transport`] trait + the pure tmux argv-builders
//! (`tmux_spawn_argv`, `tmux_capture_argv`, `tmux_send_keys_argv`, `tmux_inject_text_argv`,
//! `tmux_query_argv`, `tmux_cancel_mode_argv`) but NO concrete backend that actually executes them.
//! This is that backend: each `Transport` method builds its argv via those builders, runs it through
//! a [`CommandRunner`] seam, and parses the tmux output into the trait's typed return.
//!
//! THE SEAM: [`CommandRunner`] is the single OS edge. [`RealCommandRunner`] runs
//! `std::process::Command::new("tmux") …`; tests inject a recording/canned runner so the argv
//! construction + output parsing are unit-testable in-process, while the real subprocess execution
//! stays the `#[ignore]` real-machine boundary (acceptance framework).
//!
//! §10: the implementation must be panic-free (porter adds the deny + bodies; this skeleton is
//! `unimplemented!()`). MUST-NOT-13: a transport backend has no provider-client dependency.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
// 0.5.x Windows portability Batch 1: gate the sole Unix API surface
// in this concrete tmux backend (FileTypeExt::is_socket + libc::geteuid
// tmux-socket-root derivation). The rest of the file is Command::new
// "tmux" shellouts that compile on Windows but return runtime errors
// honestly (tmux binary absent → typed subprocess error).
// Truth source: `.team/artifacts/0.5.x-windows-portability-survey-design.md` §Batch 1.
use std::cell::Cell;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::model::enums::PaneLiveness;
use crate::transport::{
    normalize_capture, tmux_capture_argv, tmux_empty_inject_argv, tmux_inject_text_argv,
    tmux_query_argv, tmux_send_keys_argv, tmux_send_submit_argv, tmux_spawn_argv, AttachOutcome,
    BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, InjectStage,
    InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneMode, SessionName, SetEnvOutcome,
    SpawnResult, SubmitAttemptObservation, SubmitObserver, SubmitVerification, Target, Transport,
    TransportError, TurnVerification, WindowName,
};

pub const PANE_BINDING_NONCE_METADATA_KEY: &str = "TEAM_AGENT_PANE_BINDING_NONCE";
const TMUX_PANE_BINDING_NONCE_OPTION: &str = "@team_agent_pane_binding_nonce";

/// Result of running an external command — the typed output of the OS edge.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// process exit status was success (code 0).
    pub success: bool,
    /// exit code if the process exited normally.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The single OS-edge seam: run an argv vector and return its output.
/// Real impl spawns `std::process::Command`; tests inject canned/recording output so the
/// argv-construction + output-parsing of [`TmuxBackend`] is testable without a live tmux server.
pub trait CommandRunner: Send + Sync {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error>;

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        let _ = stdin;
        self.run(argv)
    }
}

/// Production runner: `std::process::Command::new(argv[0]).args(argv[1..]).output()`.
pub struct RealCommandRunner;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const SPAWN_IDENTITY_TIMEOUT: Duration = Duration::from_millis(500);
const SPAWN_IDENTITY_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Keep the final tmux argv below the ~16 KiB command envelope observed on macOS.
/// This is checked after shell quoting and socket arguments are applied, before
/// the external tmux process is called.
pub const TMUX_SPAWN_COMMAND_LIMIT_BYTES: usize = 16_000;

impl CommandRunner for RealCommandRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.run_inner(argv, None)
    }

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run_inner(argv, Some(stdin))
    }
}

impl RealCommandRunner {
    fn run_inner(
        &self,
        argv: &[String],
        stdin_text: Option<&str>,
    ) -> Result<CommandOutput, std::io::Error> {
        let Some(program) = argv.first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty argv",
            ));
        };
        let mut child = std::process::Command::new(program)
            .args(argv.iter().skip(1))
            .stdin(if stdin_text.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(text) = stdin_text {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("stdin pipe missing"))?;
            stdin.write_all(text.as_bytes())?;
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout pipe missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("stderr pipe missing"))?;
        let stdout_thread = std::thread::spawn(move || read_pipe(stdout));
        let stderr_thread = std::thread::spawn(move || read_pipe(stderr));
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                let _ = join_pipe_reader(stdout_thread)?;
                let _ = join_pipe_reader(stderr_thread)?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{program} exceeded 5s timeout"),
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let stdout = join_pipe_reader(stdout_thread)?;
        let stderr = join_pipe_reader(stderr_thread)?;
        Ok(CommandOutput {
            success: status.success(),
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

fn read_pipe<R: Read>(mut reader: R) -> Result<String, std::io::Error> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn join_pipe_reader(
    handle: std::thread::JoinHandle<Result<String, std::io::Error>>,
) -> Result<String, std::io::Error> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("pipe reader thread panicked"))?
}

/// Concrete tmux backend: builds argv via the `transport::tmux_*_argv` builders, runs them through
/// the [`CommandRunner`], and parses tmux output into the [`Transport`] typed returns.
///
/// CP-1: a workspace-bound backend carries a PER-TEAM tmux socket (`socket = Some("ta-<hash>")`) so a
/// dying shared `default` server can no longer tear the team down. The socket is injected at the RUN
/// CHOKEPOINT ([`TmuxBackend::tmux_argv`]) — the `transport::tmux_*_argv` builders stay socket-free.
pub struct TmuxBackend {
    runner: Box<dyn CommandRunner>,
    /// `Some(name)` for a per-team socket -> every `tmux` argv gets `-L <name>` injected after the
    /// leading "tmux" token; `None` (default) -> bare `tmux` on the shared default socket.
    socket: Option<TmuxSocketEndpoint>,
    /// swallow batch 2: workspace for failure-observability events (`tmux.*_failed`);
    /// `None` for non-workspace-bound backends (no event log to write to).
    event_workspace: Option<PathBuf>,
}

enum TmuxSocketEndpoint {
    Name(String),
    Path(String),
}

impl TmuxSocketEndpoint {
    fn as_endpoint(&self) -> &str {
        match self {
            TmuxSocketEndpoint::Name(socket) | TmuxSocketEndpoint::Path(socket) => socket,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeTmuxEndpointSource {
    StateTmuxEndpoint,
    StateTmuxSocket,
    WorkspaceFallback,
}

impl RuntimeTmuxEndpointSource {
    /// ---
    /// purpose: endpoint 来源的稳定诊断拼写,让日志能分清 endpoint 是从 state 读的还是 workspace 兜底算的
    /// returns: state.tmux_endpoint / state.tmux_socket / workspace_fallback 三者之一
    /// boundary: 只标来源,不表示该 endpoint 上真的有 tmux server
    /// ---
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::StateTmuxEndpoint => "state.tmux_endpoint",
            Self::StateTmuxSocket => "state.tmux_socket",
            Self::WorkspaceFallback => "workspace_fallback",
        }
    }
}

pub(crate) struct RuntimeTmuxBackendSelection {
    pub(crate) backend: TmuxBackend,
    pub(crate) tmux_endpoint_used: Option<String>,
    pub(crate) tmux_endpoint_source: RuntimeTmuxEndpointSource,
}

impl TmuxBackend {
    /// Backend bound to the real `tmux` subprocess on the SHARED default socket (no `-L`).
    /// Non-team callers + existing argv/unit tests stay unaffected.
    /// ---
    /// purpose: 构造绑在共享默认 socket 上的真实 tmux 后端(argv 不带 -L / -S)
    /// returns: 用 RealCommandRunner 执行、无 socket、无事件 workspace 的后端
    /// boundary: 不属于任何 team,故 kill_server 对它是 no-op;不写 events.jsonl(没有 workspace)
    /// ---
    pub fn new() -> Self {
        Self {
            runner: Box::new(RealCommandRunner),
            socket: None,
            event_workspace: None,
        }
    }

    /// CP-1 team backend: bound to the real `tmux` subprocess on a PER-WORKSPACE socket, derived
    /// deterministically from the canonicalized workspace path so the leader CLI, the daemon, and
    /// every later op (spawn / inject / has_session / kill) hit the SAME `tmux -L <socket>` server.
    /// ---
    /// purpose: 构造绑在「按 workspace 派生的专属 socket」上的真实 tmux 后端
    /// params:
    ///   workspace: 工作区路径;socket 名由它经 socket_name_for_workspace 确定性派生
    /// returns: socket 为 Name(ta-xxxxxxxxxxxx)、事件写入该 workspace 的后端
    /// boundary: 只决定「连哪台 server」,不创建 server、不验证 socket 是否存在(探测见 socket_probe_missing_for_workspace)
    /// ---
    pub fn for_workspace(workspace: &Path) -> Self {
        Self {
            runner: Box::new(RealCommandRunner),
            socket: Some(TmuxSocketEndpoint::Name(socket_name_for_workspace(
                workspace,
            ))),
            event_workspace: Some(workspace.to_path_buf()),
        }
    }

    /// ---
    /// purpose: 按给定短 socket 名构造真实 tmux 后端
    /// params:
    ///   socket: 短 socket 名;空串或 "default" 视为「用共享默认 socket」
    /// returns: socket 名非空且非 default 时绑 Name(socket),否则等价于 new()
    /// boundary: 不识别绝对路径(会被原样当短名走 -L);要按路径寻址请用 for_tmux_endpoint;不设事件 workspace
    /// ---
    pub(crate) fn for_socket_name(socket: &str) -> Self {
        if socket.is_empty() || socket == "default" {
            Self::new()
        } else {
            Self {
                runner: Box::new(RealCommandRunner),
                socket: Some(TmuxSocketEndpoint::Name(socket.to_string())),
                event_workspace: None,
            }
        }
    }

    /// ---
    /// purpose: 按 endpoint 字符串构造真实 tmux 后端,自动分辨绝对路径与短名
    /// params:
    ///   endpoint: 绝对路径 ⇒ 直接当 socket 路径;短名 ⇒ 先解析成已存在或默认根下的路径;空串 / "default" ⇒ 共享默认 socket
    /// returns: 解析成功时绑 Path(...);短名解析不出路径时退回 new()
    /// boundary: 只解析寻址,不探活、不创建 server;不设事件 workspace
    /// ---
    pub(crate) fn for_tmux_endpoint(endpoint: &str) -> Self {
        if endpoint.is_empty() || endpoint == "default" {
            Self::new()
        } else if Path::new(endpoint).is_absolute() {
            Self {
                runner: Box::new(RealCommandRunner),
                socket: Some(TmuxSocketEndpoint::Path(endpoint.to_string())),
                event_workspace: None,
            }
        } else if let Some(path) = socket_path_for_name(endpoint) {
            Self {
                runner: Box::new(RealCommandRunner),
                socket: Some(TmuxSocketEndpoint::Path(
                    path.to_string_lossy().into_owned(),
                )),
                event_workspace: None,
            }
        } else {
            Self::new()
        }
    }

    /// ---
    /// purpose: 把 leader 认领的绑定 nonce 写进 pane 级 tmux 用户选项,给「这个 pane 实例是不是我绑的那个」留可核凭据
    /// params:
    ///   pane: 目标 pane 标识
    ///   nonce: 本次绑定的凭据串,原样写入,不做格式校验
    /// returns: 写入命令成功即 Ok
    /// errors: tmux set-option 非 0 或子进程失败时返回 TransportError
    /// boundary: 只写不读、不比对;写的是 pane 级(-p)选项,随 pane 消亡,不跨 socket;不判断该 nonce 是否已被别人占用
    /// ---
    pub(crate) fn set_pane_binding_nonce(
        &self,
        pane: &PaneId,
        nonce: &str,
    ) -> Result<(), TransportError> {
        let argv = [
            "tmux".to_string(),
            "set-option".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            TMUX_PANE_BINDING_NONCE_OPTION.to_string(),
            nonce.to_string(),
        ];
        self.run_ok(&argv)
    }

    /// Backend with an injected runner (tests: canned/recording tmux output). Shared default socket.
    /// ---
    /// purpose: 用注入的 runner 构造后端(测试用录制/罐装 tmux 输出),绑共享默认 socket
    /// params:
    ///   runner: 替代 RealCommandRunner 的执行 seam
    /// returns: 无 socket、无事件 workspace 的后端
    /// boundary: runner 注入是本文件唯一的 OS 边界替换 seam(with_runner_for_workspace / with_runner_for_tmux_endpoint 同用);不改变任何 argv 构造逻辑
    /// ---
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self {
            runner,
            socket: None,
            event_workspace: None,
        }
    }

    /// Backend with an injected runner bound to a per-workspace socket (tests: assert the `-L` is in
    /// the recorded argv for a workspace-bound backend).
    /// ---
    /// purpose: 用注入的 runner 构造绑 workspace 专属 socket 的后端(测试断言 -L 确实进了 argv)
    /// params:
    ///   runner: 执行 seam
    ///   workspace: socket 名与事件落盘位置的来源
    /// returns: socket 为 Name(派生名)、事件写入该 workspace 的后端
    /// boundary: socket 名派生与 for_workspace 完全同源,不允许出现第二套派生规则
    /// ---
    pub fn with_runner_for_workspace(runner: Box<dyn CommandRunner>, workspace: &Path) -> Self {
        Self {
            runner,
            socket: Some(TmuxSocketEndpoint::Name(socket_name_for_workspace(
                workspace,
            ))),
            event_workspace: Some(workspace.to_path_buf()),
        }
    }

    /// ---
    /// purpose: 用注入的 runner 构造按 endpoint 寻址的后端
    /// params:
    ///   runner: 执行 seam
    ///   endpoint: 绝对路径 ⇒ Path(...);空串 / "default" ⇒ 无 socket;短名 ⇒ 解析成路径,解析不出则无 socket
    /// returns: 按上述分支绑定 socket 的后端;不设事件 workspace
    /// boundary: 分支与 for_tmux_endpoint 语义一致但顺序不同(此处先判绝对路径),两者都不探活
    /// ---
    pub fn with_runner_for_tmux_endpoint(runner: Box<dyn CommandRunner>, endpoint: &str) -> Self {
        if Path::new(endpoint).is_absolute() {
            Self {
                runner,
                socket: Some(TmuxSocketEndpoint::Path(endpoint.to_string())),
                event_workspace: None,
            }
        } else if endpoint.is_empty() || endpoint == "default" {
            Self {
                runner,
                socket: None,
                event_workspace: None,
            }
        } else if let Some(path) = socket_path_for_name(endpoint) {
            Self {
                runner,
                socket: Some(TmuxSocketEndpoint::Path(
                    path.to_string_lossy().into_owned(),
                )),
                event_workspace: None,
            }
        } else {
            Self {
                runner,
                socket: None,
                event_workspace: None,
            }
        }
    }

    /// Build the exact argv that a workspace-bound tmux backend will execute.
    /// ---
    /// purpose: 算出「workspace 绑定后端实际会执行的那条 argv」,供诊断与提示文案复用
    /// params:
    ///   workspace: 决定注入哪个 -L socket
    ///   argv: 原始 argv;首元素为 "tmux" 时才会被插入 socket 参数
    /// returns: 插好 socket 参数的 argv;非 tmux argv 原样返回
    /// boundary: 只构造不执行;结果必须与真实执行走的 tmux_argv 完全一致,否则提示会骗人
    /// ---
    pub fn argv_for_workspace(workspace: &Path, argv: &[String]) -> Vec<String> {
        Self::for_workspace(workspace).tmux_argv(argv)
    }

    /// THE RUN CHOKEPOINT: every executed `tmux` argv is funneled through here. When a per-team
    /// socket is set, inject `-L <socket>` right after the leading "tmux" token; otherwise pass argv
    /// through unchanged. Non-`tmux` argv (e.g. the spawned provider command) is never rewritten.
    fn tmux_argv(&self, argv: &[String]) -> Vec<String> {
        match &self.socket {
            Some(endpoint) if argv.first().map(String::as_str) == Some("tmux") => {
                let mut out = Vec::with_capacity(argv.len() + 2);
                out.push("tmux".to_string());
                match endpoint {
                    TmuxSocketEndpoint::Name(socket) => {
                        out.push("-L".to_string());
                        out.push(socket.clone());
                    }
                    TmuxSocketEndpoint::Path(socket) => {
                        out.push("-S".to_string());
                        out.push(socket.clone());
                    }
                }
                out.extend(argv.iter().skip(1).cloned());
                out
            }
            _ => argv.to_vec(),
        }
    }

    /// `tmux -L <socket> kill-server` (CP-1 cleanup): best-effort teardown of the per-team server on
    /// shutdown so per-team sockets do not orphan. No-op (and never errors) for a default-socket
    /// backend, and a "no server" failure is ignored.
    /// ---
    /// purpose: 收尾时尽力拆掉本 team 专属的 tmux server,避免 socket 变孤儿
    /// boundary:
    ///   - 默认 socket 后端直接返回,绝不拆共享 server
    ///   - 只在 list_targets 证明 server 为空时才拆;列举失败按「未证明所有权」处理,倒向不拆;有事件 workspace 时才写 tmux.kill_server_skipped_nonempty_or_unknown 事件(按 endpoint / 短名构造的后端没有,静默跳过)
    ///   - 尽力而为:执行结果被丢弃,不返回错误、不重试
    /// ---
    pub fn kill_server(&self) {
        if self.socket.is_none() {
            return;
        }
        // A server-level kill is only safe after the caller has removed every
        // resource it owns.  A non-empty server may carry a pre-existing team
        // (or a leader owned by another launcher), so fail closed instead of
        // turning an endpoint cleanup into a shared-server teardown.  An
        // inconclusive probe is also not proof of ownership.
        let server_is_empty = match self.list_targets() {
            Ok(targets) => targets.is_empty(),
            Err(_) => false,
        };
        if !server_is_empty {
            if let Some(workspace) = &self.event_workspace {
                let _ = crate::event_log::EventLog::new(workspace).write(
                    "tmux.kill_server_skipped_nonempty_or_unknown",
                    serde_json::json!({
                        "reason": "server_ownership_not_proven",
                        "endpoint": self.tmux_endpoint(),
                    }),
                );
            }
            return;
        }
        let argv = self.tmux_argv(&["tmux".to_string(), "kill-server".to_string()]);
        let _ = self.runner.run(&argv);
    }
}

/// ---
/// purpose: 选出运行期该用的 tmux 后端——优先跟随 state 里持久化的 endpoint,没有才按 workspace 派生
/// params:
///   workspace: 兜底派生 socket 的来源
///   state: 运行期状态 JSON;从中读 tmux_endpoint,其次 tmux_socket
/// returns: 后端 + 实际使用的 endpoint + 来源标记(三者一并返回,便于日志说清为什么连这台)
/// boundary: 只做选择,不探活、不创建 server;state 里的 endpoint 即便已失效也照选,失败留给后续命令暴露
/// ---
pub(crate) fn tmux_backend_for_runtime_state_or_workspace(
    workspace: &Path,
    state: Option<&serde_json::Value>,
) -> RuntimeTmuxBackendSelection {
    let (backend, source) =
        if let Some((endpoint, source)) = runtime_tmux_endpoint_from_state(state) {
            (TmuxBackend::for_tmux_endpoint(endpoint), source)
        } else {
            (
                TmuxBackend::for_workspace(workspace),
                RuntimeTmuxEndpointSource::WorkspaceFallback,
            )
        };
    RuntimeTmuxBackendSelection {
        tmux_endpoint_used: backend.tmux_endpoint(),
        backend,
        tmux_endpoint_source: source,
    }
}

/// ---
/// purpose: 同 tmux_backend_for_runtime_state_or_workspace,但用注入的 runner,给测试断言选择逻辑
/// params:
///   runner: 执行 seam
///   workspace: 兜底派生 socket 的来源
///   state: 运行期状态 JSON
/// returns: 后端 + 实际使用的 endpoint + 来源标记
/// cfg: test
/// boundary: 仅测试编译;选择分支必须与生产版本同构,否则测的不是同一件事
/// ---
#[cfg(test)]
pub(crate) fn tmux_backend_with_runner_for_runtime_state_or_workspace(
    runner: Box<dyn CommandRunner>,
    workspace: &Path,
    state: Option<&serde_json::Value>,
) -> RuntimeTmuxBackendSelection {
    let (backend, source) =
        if let Some((endpoint, source)) = runtime_tmux_endpoint_from_state(state) {
            (
                TmuxBackend::with_runner_for_tmux_endpoint(runner, endpoint),
                source,
            )
        } else {
            (
                TmuxBackend::with_runner_for_workspace(runner, workspace),
                RuntimeTmuxEndpointSource::WorkspaceFallback,
            )
        };
    RuntimeTmuxBackendSelection {
        tmux_endpoint_used: backend.tmux_endpoint(),
        backend,
        tmux_endpoint_source: source,
    }
}

fn runtime_tmux_endpoint_from_state(
    state: Option<&serde_json::Value>,
) -> Option<(&str, RuntimeTmuxEndpointSource)> {
    state.and_then(|state| {
        state
            .get("tmux_endpoint")
            .and_then(|v| v.as_str())
            .filter(|endpoint| !endpoint.is_empty())
            .map(|endpoint| (endpoint, RuntimeTmuxEndpointSource::StateTmuxEndpoint))
            .or_else(|| {
                state
                    .get("tmux_socket")
                    .and_then(|v| v.as_str())
                    .filter(|endpoint| !endpoint.is_empty())
                    .map(|endpoint| (endpoint, RuntimeTmuxEndpointSource::StateTmuxSocket))
            })
    })
}

/// CP-1 socket name: SHORT + DETERMINISTIC per canonical workspace path. `ta-` + 12 hex chars of a
/// stable FNV-1a hash over the canonicalized path. AF_UNIX `sun_path` is ~104 chars and the socket
/// lives at `/tmp/tmux-<uid>/<name>`, so we must NOT use the (~88-char) session name. §10: a
/// canonicalize failure falls back to the raw path (never panics).
/// Public re-export of the crate-private canonical workspace hash used
/// by the tmux short-socket derivation. `transport_factory` uses this
/// same hash for the ConPTY `workspace_hash` so both backends see
/// identical workspace identity. Adding a wrapper (not calling
/// `socket_name_for_workspace` directly outside this file) keeps the
/// existing internal API stable.
/// ---
/// purpose: 对外暴露「workspace 身份短哈希」,让 tmux 与 ConPTY 两个后端看到同一个 workspace 身份
/// params:
///   workspace: 工作区路径;先 canonicalize 再哈希
/// returns: 12 位十六进制串,即 socket 名去掉 ta- 前缀的部分
/// boundary: 与 socket_name_for_workspace 同源,禁止另起一套哈希;canonicalize 失败时退回原始路径参与哈希,故未落盘的路径可能与落盘后不同值
/// ---
pub fn workspace_short_hash_pub(workspace: &Path) -> String {
    // The tmux short-socket name is `ta-<12 hex>`; the workspace-hash
    // used by the ConPTY workspace identity is the raw 12-hex portion
    // (no `ta-` prefix), which is what the shim/pipe name derivation
    // expects. Reuse the exact same hash so drift is impossible.
    let name = socket_name_for_workspace(workspace);
    name.strip_prefix("ta-").unwrap_or(&name).to_string()
}

/// Public re-export of the crate-private `runtime_tmux_endpoint_from_state`
/// probe. `transport_factory` uses this to spot the legacy tmux endpoint
/// in `state` without duplicating the field-precedence logic.
/// ---
/// purpose: 对外暴露 state 里 tmux endpoint 的读取与字段优先级,避免调用方各写一份
/// params:
///   state: 运行期状态 JSON
/// returns: Some((endpoint, 来源标记)) 当 tmux_endpoint 或 tmux_socket 非空;两者都缺或为空则 None
/// boundary: 只读字段不探活;返回值里不会出现 workspace_fallback(那是选后端时才产生的来源)
/// ---
pub fn runtime_tmux_endpoint_from_state_pub(
    state: Option<&serde_json::Value>,
) -> Option<(String, &'static str)> {
    runtime_tmux_endpoint_from_state(state).map(|(endpoint, source)| {
        let src = match source {
            RuntimeTmuxEndpointSource::StateTmuxEndpoint => "state.tmux_endpoint",
            RuntimeTmuxEndpointSource::StateTmuxSocket => "state.tmux_socket",
            RuntimeTmuxEndpointSource::WorkspaceFallback => "workspace_fallback",
        };
        (endpoint.to_string(), src)
    })
}

/// ---
/// purpose: 由 workspace 路径确定性派生短 socket 名,保证 CLI、daemon 与后续每条命令都落到同一台 tmux server
/// params:
///   workspace: 工作区路径;先 canonicalize,失败则退回原始路径
/// returns: ta- 加 12 位十六进制(FNV-1a 低 48 位)
/// boundary:
///   - 必须短:AF_UNIX 路径长度有限,不能拿 session 名当 socket 名
///   - 用固定 FNV-1a 而非 std 默认哈希(后者跨版本不稳定)
///   - 只算名字,不判断该 socket 是否存在
/// ---
pub(crate) fn socket_name_for_workspace(workspace: &Path) -> String {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = Fnv1a::default();
    canonical.as_os_str().hash(&mut hasher);
    format!("ta-{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

/// ---
/// purpose: 给出 workspace 专属 socket 的文件路径
/// params:
///   workspace: 工作区路径
/// returns: 已存在的 socket 路径优先;否则给默认根下的预期路径;Windows 上恒为 None
/// boundary: 返回 Some 不代表 socket 存在(可能只是「按约定应该在这」);要判存在用 socket_probe_missing_for_workspace
/// ---
pub(crate) fn socket_path_for_workspace(workspace: &Path) -> Option<PathBuf> {
    socket_path_for_name(&socket_name_for_workspace(workspace))
}

/// ---
/// purpose: 探测 workspace 专属 socket 当前是否不存在
/// params:
///   workspace: 工作区路径
/// returns: 在已知 socket 根下都找不到该名字时为 true
/// boundary: 只看文件是否存在,不连不握手——socket 文件在但 server 已死也会报 false
/// ---
pub(crate) fn socket_probe_missing_for_workspace(workspace: &Path) -> bool {
    existing_socket_path_for_workspace(workspace).is_none()
}

fn existing_socket_path_for_workspace(workspace: &Path) -> Option<PathBuf> {
    existing_socket_path_for_name(&socket_name_for_workspace(workspace))
}

/// ---
/// purpose: 把短 socket 名解析成文件路径
/// params:
///   socket_name: 短名;空串、"default"、绝对路径三种输入都视为「无短名可解析」
/// returns: 已存在则返回实际路径;否则 Unix 上给 /tmp/tmux-<uid>/<name> 预期路径,Windows 上 None
/// cfg: unix 分支读 geteuid;not(unix) 分支恒 None
/// boundary: 不创建目录、不创建 socket;返回 Some 不代表文件存在
/// ---
pub(crate) fn socket_path_for_name(socket_name: &str) -> Option<PathBuf> {
    if socket_name.is_empty() || socket_name == "default" || Path::new(socket_name).is_absolute() {
        return None;
    }
    if let Some(existing) = existing_socket_path_for_name(socket_name) {
        return Some(existing);
    }
    // Batch 1: tmux uses `/tmp/tmux-<uid>/` on Unix; on Windows this
    // helper is dead code (Command::new "tmux" fails at spawn time
    // before this path is dereferenced). N38 typed unsupported honored
    // at the shellout boundary.
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let default_root = PathBuf::from(format!("/tmp/tmux-{uid}"));
        let default_root = default_root.canonicalize().unwrap_or(default_root);
        Some(default_root.join(socket_name))
    }
    #[cfg(not(unix))]
    {
        // Windows: tmux socket-root derivation is meaningless. Return
        // None so the caller sees the honest "no such socket" branch.
        let _ = socket_name;
        None
    }
}

fn existing_socket_path_for_name(socket_name: &str) -> Option<PathBuf> {
    let roots = tmux_socket_roots();
    for root in &roots {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let candidate = root.join(socket_name);
        if candidate.exists() {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }
    None
}

/// ---
/// purpose: socket 找不到时给操作者的可执行提示,把期望的 socket 名与找过的目录都写出来
/// params:
///   workspace: 工作区路径,用于派生 socket 名
/// returns: 含 socket 名、已搜索根目录列表与下一步命令的单行提示
/// boundary: 只生成文案,不重试、不修复、不创建 socket
/// ---
pub(crate) fn socket_missing_hint_for_workspace(workspace: &Path) -> String {
    let socket_name = socket_name_for_workspace(workspace);
    let roots = tmux_socket_roots()
        .into_iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "tmux socket {socket_name} not found under [{roots}]; run `team-agent attach-leader` or restart the team before attaching"
    )
}

/// ---
/// purpose: 生成让操作者手工 attach 到指定窗口的 tmux 命令行(按 workspace 派生 socket)
/// params:
///   workspace: 决定 -S 后面的 socket 路径
///   session_name: attach 目标 session
///   window_name: attach 目标 window
/// returns: 完整命令字符串;socket 路径解析不出时 None
/// boundary: 只生成文案、不执行、不验证 session/window 是否存在;endpoint 已持久化在 state 里时应改用 attach_command_for_runtime_state_or_workspace,否则会指到错误的 socket
/// ---
pub(crate) fn attach_command_for_workspace(
    workspace: &Path,
    session_name: &SessionName,
    window_name: &str,
) -> Option<String> {
    let socket_path = socket_path_for_workspace(workspace)?;
    Some(format!(
        "tmux -S {} attach -t {}:{}",
        socket_path.display(),
        session_name.as_str(),
        window_name
    ))
}

/// ---
/// purpose: 同 attach_command_for_workspace,但只到 session 一级(不指定 window)
/// params:
///   workspace: 决定 -S 后面的 socket 路径
///   session_name: attach 目标 session
/// returns: 完整命令字符串;socket 路径解析不出时 None
/// boundary: 只生成文案、不执行、不验证 session 是否存在
/// ---
pub(crate) fn attach_command_for_session(
    workspace: &Path,
    session_name: &SessionName,
) -> Option<String> {
    let socket_path = socket_path_for_workspace(workspace)?;
    Some(format!(
        "tmux -S {} attach -t {}",
        socket_path.display(),
        session_name.as_str()
    ))
}

/// ---
/// purpose: 按 transport 自报的 endpoint 生成 attach 命令,保证提示指向它真正在用的那台 server
/// params:
///   transport: 提供 tmux_endpoint 的传输后端
///   session_name: attach 目标 session
/// returns: 完整命令字符串;后端报不出 endpoint(非 tmux / 测试替身)时 None
/// boundary: 只生成文案、不执行;endpoint 是绝对路径用 -S、短名用 -L,由 attach_command_for_endpoint_session 分辨
/// ---
pub(crate) fn attach_command_for_transport_session(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
) -> Option<String> {
    let endpoint = transport.tmux_endpoint()?;
    Some(attach_command_for_endpoint_session(&endpoint, session_name))
}

/// Bug #7 (prerelease 0.4.0 gate review §6): when the runtime state carries a
/// persisted `tmux_endpoint` / `tmux_socket` (e.g. `/private/tmp/tmux-501/default`),
/// the attach command MUST point at THAT endpoint, not the workspace-hash
/// socket — otherwise operators are told to attach to a socket where the
/// session does not exist. Falls back to workspace-hash when state has no
/// persisted endpoint.
/// ---
/// purpose: 生成 attach 到指定窗口的命令,优先跟随 state 里持久化的 endpoint,没有才回落 workspace 派生 socket
/// params:
///   workspace: 回落路径的来源
///   state: 运行期状态 JSON;从中读 tmux_endpoint / tmux_socket
///   session_name: attach 目标 session
///   window_name: attach 目标 window
/// returns: 完整命令字符串;两条路都算不出 socket 时 None
/// boundary: 只生成文案、不执行、不验证目标存在;endpoint 是绝对路径用 -S、短名用 -L
/// ---
pub(crate) fn attach_command_for_runtime_state_or_workspace(
    workspace: &Path,
    state: Option<&serde_json::Value>,
    session_name: &SessionName,
    window_name: &str,
) -> Option<String> {
    if let Some((endpoint, _source)) = runtime_tmux_endpoint_from_state(state) {
        let display = endpoint.to_string();
        // Distinguish absolute path (`-S <path>`) from short socket name (`-L <name>`).
        let flag = if Path::new(endpoint).is_absolute() {
            "-S"
        } else {
            "-L"
        };
        return Some(format!(
            "tmux {flag} {display} attach -t {}:{}",
            session_name.as_str(),
            window_name
        ));
    }
    attach_command_for_workspace(workspace, session_name, window_name)
}

/// ---
/// purpose: 同 attach_command_for_runtime_state_or_workspace,但只到 session 一级
/// params:
///   workspace: 回落路径的来源
///   state: 运行期状态 JSON
///   session_name: attach 目标 session
/// returns: 完整命令字符串;两条路都算不出 socket 时 None
/// boundary: 只生成文案、不执行、不验证 session 存在
/// ---
pub(crate) fn attach_command_for_runtime_state_session_or_workspace(
    workspace: &Path,
    state: Option<&serde_json::Value>,
    session_name: &SessionName,
) -> Option<String> {
    if let Some((endpoint, _source)) = runtime_tmux_endpoint_from_state(state) {
        return Some(attach_command_for_endpoint_session(endpoint, session_name));
    }
    attach_command_for_session(workspace, session_name)
}

fn attach_command_for_endpoint_session(endpoint: &str, session_name: &SessionName) -> String {
    let flag = if Path::new(endpoint).is_absolute() {
        "-S"
    } else {
        "-L"
    };
    format!("tmux {flag} {endpoint} attach -t {}", session_name.as_str())
}

/// ---
/// purpose: 为一组窗口批量生成 attach 命令
/// params:
///   workspace: 决定 socket 路径
///   session_name: attach 目标 session
///   window_names: 待生成的窗口名序列
/// returns: 生成成功的命令列表;算不出 socket 的条目被静默丢弃,故长度可能短于入参
/// boundary: 只生成文案、不执行;丢弃发生在整体层面(socket 算不出就一条都没有),调用方不能假定下标与入参对齐
/// ---
pub(crate) fn attach_commands_for_windows<'a>(
    workspace: &Path,
    session_name: &SessionName,
    window_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    window_names
        .into_iter()
        .filter_map(|window_name| {
            attach_command_for_workspace(workspace, session_name, window_name)
        })
        .collect()
}

/// ---
/// purpose: 列出本机可能放 tmux socket 的根目录
/// returns: Unix 上为 /tmp/tmux-<uid> 加 TMPDIR/tmux-<uid>(排序去重);Windows 上为空
/// cfg: unix 读 geteuid 与 TMPDIR;not(unix) 返回空表示「本平台无此约定」
/// boundary: 只列约定目录,不验证目录存在、不列举里面的 socket(那是 tmux_socket_endpoints)
/// ---
pub(crate) fn tmux_socket_roots() -> Vec<PathBuf> {
    // Batch 1: `/tmp/tmux-<uid>` root enumeration is Unix-only tmux
    // convention. On Windows return empty so the caller loops zero
    // times (honest "no tmux socket roots" — tmux is not deployed on
    // native Windows without WSL, and WSL is out of scope).
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let mut roots = vec![PathBuf::from(format!("/tmp/tmux-{uid}"))];
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            roots.push(PathBuf::from(tmpdir).join(format!("tmux-{uid}")));
        }
        roots.sort();
        roots.dedup();
        roots
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// ---
/// purpose: 枚举本机所有已存在的 tmux socket 路径
/// returns: 各 socket 根下真正是 socket 文件的绝对路径,排序去重;Windows 上为空
/// cfg: unix 才用 is_socket 过滤;not(unix) 直接跳过(该平台 socket 根为空)
/// boundary: 只看文件类型,不连不握手——列出的 socket 可能属于已死的 server;不做任何删除
/// ---
pub(crate) fn tmux_socket_endpoints() -> Vec<String> {
    let mut endpoints = Vec::new();
    for root in tmux_socket_roots() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // Batch 1: `FileTypeExt::is_socket` is Unix-only. Dead code
            // on Windows because `tmux_socket_roots()` returns empty
            // there (no /tmp/tmux-<uid> convention). We still cfg the
            // method call so `cargo check --target x86_64-pc-windows-msvc`
            // sees no `std::os::unix::fs::FileTypeExt` reference.
            #[cfg(unix)]
            {
                if !file_type.is_socket() {
                    continue;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = file_type;
                continue;
            }
            let path = entry.path();
            let path = path.canonicalize().unwrap_or(path);
            endpoints.push(path.to_string_lossy().to_string());
        }
    }
    endpoints.sort();
    endpoints.dedup();
    endpoints
}

/// ---
/// purpose: 从当前进程所在的 tmux 环境($TMUX)反推它连的是哪个 socket
/// returns: $TMUX 首段是绝对路径时返回该路径;$TMUX 未设 / 为空 / 首段非绝对路径时 None
/// boundary: 读进程环境,只在自己就跑在 tmux 里时有意义;不校验该 socket 仍可用
/// ---
pub(crate) fn socket_name_from_tmux_env() -> Option<String> {
    let tmux = std::env::var("TMUX")
        .ok()
        .filter(|value| !value.is_empty())?;
    let socket_path = tmux.split(',').next().unwrap_or("").trim();
    if socket_path.is_empty() || !Path::new(socket_path).is_absolute() {
        return None;
    }
    Some(socket_path.to_string())
}

/// Deterministic FNV-1a (64-bit) — std `DefaultHasher` is NOT stable across releases, so a fixed
/// FNV keeps the socket identical for the CLI, the daemon, and every later op on the same workspace.
struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TmuxBackend {
    fn spawn(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        first: bool,
    ) -> Result<SpawnResult, TransportError> {
        let command = shell_command(argv, cwd, env, env_unset);
        self.spawn_with_command(session, window, &command, first)
    }

    /// 0.4.x (CR C-2): spawn variant that takes a pre-built shell command
    /// (used by `spawn_first_with_leader_shell_wrapper` /
    /// `spawn_into_with_leader_shell_wrapper` to inject the leader wrapper
    /// shape without going through `shell_command`'s `exec`-only template).
    fn spawn_with_command(
        &self,
        session: &SessionName,
        window: &WindowName,
        command: &str,
        first: bool,
    ) -> Result<SpawnResult, TransportError> {
        let spawn_argv = tmux_spawn_argv(session, window, command, first);
        self.validate_spawn_command(
            &spawn_argv,
            if first {
                "tmux.new-session"
            } else {
                "tmux.new-window"
            },
            session,
            window,
        )?;
        let output = self.run_spawn(&spawn_argv)?;
        let pane = output.stdout.trim();
        if pane.is_empty() {
            return Err(TransportError::Subprocess {
                argv: spawn_argv,
                code: output.code,
                stderr: format!(
                    "tmux spawn returned no pane id for {}:{}",
                    session.as_str(),
                    window.as_str()
                ),
            });
        }
        let pane_id = PaneId::new(pane);
        let deadline = Instant::now() + SPAWN_IDENTITY_TIMEOUT;
        let observed = loop {
            match self.list_targets() {
                Ok(targets) => {
                    if let Some(target) = targets.iter().find(|target| {
                        target.pane_id == pane_id
                            && target.session.as_str() == session.as_str()
                            && target
                                .window_name
                                .as_ref()
                                .is_some_and(|name| name.as_str() == window.as_str())
                    }) {
                        return Ok(SpawnResult {
                            pane_id,
                            session: session.clone(),
                            window: window.clone(),
                            child_pid: target.pane_pid,
                        });
                    }
                    if let Some(target) = targets.iter().find(|target| target.pane_id == pane_id) {
                        break format!(
                            "{}:{}",
                            target.session.as_str(),
                            target
                                .window_name
                                .as_ref()
                                .map(WindowName::as_str)
                                .unwrap_or("<unknown>")
                        );
                    }
                    if Instant::now() >= deadline {
                        break "<missing-from-list-targets>".to_string();
                    }
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        break format!("<list-targets-error:{error}>");
                    }
                }
            }
            std::thread::sleep(SPAWN_IDENTITY_POLL_INTERVAL);
        };
        let rollback_argv = vec![
            "tmux".to_string(),
            "kill-pane".to_string(),
            "-t".to_string(),
            pane_id.as_str().to_string(),
        ];
        let rollback_error = self.run_ok(&rollback_argv).err();
        let rollback_suffix = rollback_error
            .map(|error| format!("; failed to roll back spawned pane: {error}"))
            .unwrap_or_default();
        Err(TransportError::Subprocess {
            argv: spawn_argv,
            code: output.code,
            stderr: format!(
                "tmux spawn pane identity mismatch: requested={}:{} observed_pane={} observed={}{}",
                session.as_str(),
                window.as_str(),
                pane_id.as_str(),
                observed,
                rollback_suffix
            ),
        })
    }

    fn spawn_split(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let command = shell_command(argv, cwd, env, env_unset);
        let target = format!("{}:{}", session.as_str(), window.as_str());
        // E53 (0.3.26, adaptive layout same-session tabs): `-d` prevents the
        // new split pane from stealing focus from the leader's active pane.
        // Same rationale as the `-d` on `new-window` in transport.rs; for
        // adaptive layout the leader and all workers share the same tmux
        // session, and every focus-stealing spawn is a disruption.
        let split_argv = vec![
            "tmux".to_string(),
            "split-window".to_string(),
            "-d".to_string(),
            "-t".to_string(),
            target.clone(),
            "-h".to_string(),
            "-P".to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
            "sh".to_string(),
            "-lc".to_string(),
            command,
        ];
        self.validate_spawn_command(&split_argv, "tmux.split-window", session, window)?;
        let output = self.run_spawn(&split_argv)?;
        let pane = output.stdout.trim();
        if pane.is_empty() {
            return Err(TransportError::Subprocess {
                argv: split_argv,
                code: output.code,
                stderr: format!("tmux split-window returned no pane id for {target}"),
            });
        }
        let layout_argv = vec![
            "tmux".to_string(),
            "select-layout".to_string(),
            "-t".to_string(),
            target,
            "even-horizontal".to_string(),
        ];
        self.run_ok(&layout_argv)?;
        Ok(SpawnResult {
            pane_id: PaneId::new(pane),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn run_ok(&self, argv: &[String]) -> Result<(), TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self.runner.run(&argv)?;
        if output.success {
            Ok(())
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    fn validate_spawn_command(
        &self,
        argv: &[String],
        segment: &str,
        session: &SessionName,
        window: &WindowName,
    ) -> Result<(), TransportError> {
        let final_argv = self.tmux_argv(argv);
        let actual_bytes = final_argv
            .iter()
            .map(|arg| arg.len().saturating_add(1))
            .sum::<usize>();
        if actual_bytes > TMUX_SPAWN_COMMAND_LIMIT_BYTES {
            return Err(TransportError::CommandTooLong {
                backend: BackendKind::Tmux,
                segment: segment.to_string(),
                session: session.as_str().to_string(),
                window: window.as_str().to_string(),
                actual_bytes,
                limit_bytes: TMUX_SPAWN_COMMAND_LIMIT_BYTES,
            });
        }
        Ok(())
    }

    fn run_spawn(&self, argv: &[String]) -> Result<CommandOutput, TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self
            .runner
            .run(&argv)
            .map_err(|source| TransportError::Spawn {
                backend: BackendKind::Tmux,
                source,
            })?;
        if output.success {
            Ok(output)
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    fn run_inject_stage(&self, argv: &[String], stage: InjectStage) -> Result<(), TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self
            .runner
            .run(&argv)
            .map_err(|source| TransportError::Inject { stage, source })?;
        if output.success {
            Ok(())
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    fn run_inject_stage_with_stdin(
        &self,
        argv: &[String],
        stage: InjectStage,
        stdin: &str,
    ) -> Result<(), TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self
            .runner
            .run_with_stdin(&argv, stdin)
            .map_err(|source| TransportError::Inject { stage, source })?;
        if output.success {
            Ok(())
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    /// 任意非 0 tmux mode。best-effort，查询失败 → None。
    ///
    /// Cherry-pick 866939b1 冲突裁定：本线没有 `inject_journal` 模块（modify/delete），
    /// 不复活整份 journal。`pane_mode_from_raw` 识别 copy/tree/view/client；
    /// 一切 Some(mode) 都算在 mode（头注释「非 0 先 cancel」）。
    fn pane_in_mode(&self, target: &Target) -> Option<bool> {
        let raw = self.query(target, PaneField::PaneMode).ok().flatten()?;
        Some(pane_mode_from_raw(Some(raw)).is_some())
    }

    fn pane_mode(&self, target: &Target) -> Option<PaneMode> {
        let raw = self.query(target, PaneField::PaneMode).ok().flatten()?;
        pane_mode_from_raw(Some(raw))
    }

    /// 发 Enter 前归零接收态。A1/A3/A4/A6 唯一入口。
    ///
    /// 1. 非 0 pane_mode → `tmux_cancel_mode_argv`（按真实 mode 分派，不是硬编码 Copy）
    /// 2. 字面 CSI 201~ 闭合未完成的 bracketed paste（不是 Escape 键）
    /// cancel/闭合失败不 fail 提交。E55：不送 Escape/C-c。
    fn prepare_pane_for_submit(&self, target: &Target, close_bracketed_paste: bool) {
        let pane = pane_from_target(target);
        if let Some(mode) = self.pane_mode(target) {
            let argv = crate::transport::tmux_cancel_mode_argv(&pane, mode);
            let _ = self.run_inject_stage(&argv, InjectStage::Submit);
        }
        if close_bracketed_paste {
            let argv = crate::transport::tmux_close_bracketed_paste_argv(&pane);
            let _ = self.run_inject_stage(&argv, InjectStage::Submit);
        }
    }
}

fn subprocess_error(argv: Vec<String>, output: CommandOutput) -> TransportError {
    TransportError::Subprocess {
        argv,
        code: output.code,
        stderr: output.stderr,
    }
}

fn pane_from_target(target: &Target) -> PaneId {
    match target {
        Target::Pane(pane) => pane.clone(),
        Target::SessionWindow { session, window } => {
            PaneId::new(format!("{}:{}", session.as_str(), window.as_str()))
        }
    }
}

fn target_name(target: &Target) -> String {
    match target {
        Target::Pane(pane) => pane.as_str().to_string(),
        Target::SessionWindow { session, window } => {
            format!("{}:{}", session.as_str(), window.as_str())
        }
    }
}

fn inject_stage_for_argv(argv: &[String]) -> InjectStage {
    match argv.get(1).map(String::as_str) {
        Some("set-buffer") => InjectStage::SetBuffer,
        Some("load-buffer") => InjectStage::LoadBuffer,
        Some("paste-buffer") => InjectStage::PasteBuffer,
        Some("delete-buffer") => InjectStage::DeleteBuffer,
        Some("send-keys") => InjectStage::Submit,
        _ => InjectStage::Submit,
    }
}

fn pane_mode_from_raw(raw: Option<String>) -> Option<PaneMode> {
    match raw.as_deref().map(str::trim) {
        Some("") | Some("0") => None,
        Some("copy-mode") => Some(PaneMode::Copy),
        Some("tree-mode") => Some(PaneMode::Tree),
        Some("view-mode") => Some(PaneMode::View),
        Some("client-mode") => Some(PaneMode::Client),
        _ => Some(PaneMode::Unknown),
    }
}

fn buffer_name_for_text(text: &str) -> String {
    const PREFIX: &str = "[team-agent-token:";
    match text.find(PREFIX) {
        Some(prefix_start) => {
            let token_start = prefix_start.saturating_add(PREFIX.len());
            let Some(rest) = text.get(token_start..) else {
                return "team-agent-buf".to_string();
            };
            let Some(token_end) = rest.find(']') else {
                return "team-agent-buf".to_string();
            };
            let Some(token) = rest.get(..token_end).filter(|s| !s.is_empty()) else {
                return "team-agent-buf".to_string();
            };
            format!("team-agent-send-{token}")
        }
        None => "team-agent-buf".to_string(),
    }
}

fn inject_verification_for_payload(payload: &InjectPayload) -> InjectVerification {
    match payload {
        InjectPayload::Empty => InjectVerification::EmptyTextSendKeys,
        InjectPayload::Text(text) | InjectPayload::TextSkipConsumptionPoll(text)
            if text.contains("[team-agent-token:") =>
        {
            InjectVerification::CaptureContainsToken
        }
        InjectPayload::Text(_) | InjectPayload::TextSkipConsumptionPoll(_) => {
            InjectVerification::NoToken
        }
    }
}

/// U1 #7: the exact delivery-token marker a token payload carries
/// (`[team-agent-token:<id>]`). Use the full marker, not only the prefix, so an old
/// scrollback token cannot verify a new message.
/// Measured cursor-agent 2026.08.11 floor: text and first Enter ≥1s apart
/// (phase0 M2-5/M2-6; `.team/scripts/cursor_send.sh`).
/// Grok uses the same 1s floor (悬案B：零地板时 Enter 落在折叠窗口内被吞).
pub const CURSOR_PASTE_TO_SUBMIT_FLOOR: Duration = Duration::from_secs(1);

thread_local! {
    static PASTE_TO_SUBMIT_FLOOR: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static CURSOR_SINGLE_ENTER: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with a paste→Enter floor. Delivery sets 1s for CursorAgent and Grok.
/// Tests pass a small non-zero Duration to cover the sleep branch.
/// Default is ZERO — claude/codex and unset callers pay nothing.
/// This is not TEAM_AGENT_TEST_TMP (that var is a path, always set in seats).
/// ---
/// purpose: 在 f 执行期间设定「粘贴到回车之间至少等多久」的地板,退出时恢复原值
/// params:
///   floor: 本次生效的最小间隔;ZERO 表示不等
///   f: 在该地板下执行的闭包
/// returns: 原样返回 f 的返回值
/// boundary: thread_local 作用域,不跨线程;只设阈值,真正的等待发生在 sleep_remaining_paste_to_submit_floor;默认 ZERO,只有 delivery 对 cursor 才设 1s
/// ---
pub fn with_paste_to_submit_floor<R>(floor: Duration, f: impl FnOnce() -> R) -> R {
    PASTE_TO_SUBMIT_FLOOR.with(|cell| {
        let previous = cell.replace(floor);
        let result = f();
        cell.set(previous);
        result
    })
}

/// ---
/// purpose: 读当前线程生效的粘贴到回车地板
/// returns: 当前地板;未设过则为 ZERO
/// boundary: 只读不改;ZERO 表示「不等」,不是「未配置」——两者在此不做区分
/// ---
pub(crate) fn current_paste_to_submit_floor() -> Duration {
    PASTE_TO_SUBMIT_FLOOR.with(Cell::get)
}

/// ---
/// purpose: cursor 注入单回车闸（delivery 仅对 CursorAgent 打开）
/// contract: 默认关；打开后重试 Enter 仅当 token 仍在输入区且无 busy
/// boundary: 不改 claude/grok/codex 默认重试；测试隔离下默认关
/// ---
pub fn with_cursor_single_enter<R>(enabled: bool, f: impl FnOnce() -> R) -> R {
    CURSOR_SINGLE_ENTER.with(|cell| {
        let previous = cell.replace(enabled);
        let result = f();
        cell.set(previous);
        result
    })
}

/// ---
/// purpose: 读当前线程的 cursor 单回车闸开关
/// returns: 打开为 true;默认关
/// boundary: 只读不改;开关只影响重按 Enter 的判据来源,不改变首次 Enter
/// ---
pub(crate) fn cursor_single_enter_enabled() -> bool {
    CURSOR_SINGLE_ENTER.with(Cell::get)
}

/// ---
/// purpose: 补齐粘贴到回车之间还差的等待时间
/// params:
///   pasted_at: 粘贴完成的时刻
///   floor: 要求的最小间隔
/// returns: 无返回值;已过地板则立即返回,否则阻塞剩余时长
/// boundary: 阻塞当前线程;只保证「不早于」,不保证 TUI 真的处理完粘贴
/// ---
pub(crate) fn sleep_remaining_paste_to_submit_floor(pasted_at: Instant, floor: Duration) {
    let remain = floor.saturating_sub(pasted_at.elapsed());
    if !remain.is_zero() {
        std::thread::sleep(remain);
    }
}

fn payload_token_marker(payload: &InjectPayload) -> Option<&str> {
    let text = payload.text()?;
    let start = text.find("[team-agent-token:")?;
    let marker = &text[start..];
    let end = marker.find(']')?;
    Some(&marker[..=end])
}

fn token_visible_in_capture(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    match payload_token_marker(payload) {
        None => Ok(None),
        Some(marker) => {
            let captured = backend.capture(target, CaptureRange::Tail(80))?;
            Ok(Some(captured.text.contains(marker)))
        }
    }
}

/// U1 #7 / E31: wait briefly for the just-pasted token marker before submitting.
/// `Ok(None)` for non-token payloads (nothing to check). `Ok(Some(false))` means the
/// paste did not become visible before the Python-parity fallback delay.
fn pre_submit_token_visible(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    if payload_token_marker(payload).is_none() {
        return Ok(None);
    }
    for attempt in 0..PASTED_CONTENT_APPEAR_POLLS {
        if let Some(true) = token_visible_in_capture(backend, target, payload)? {
            return Ok(Some(true));
        }
        if attempt + 1 < PASTED_CONTENT_APPEAR_POLLS {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    // Python waits 250ms between paste-buffer and Enter to let bracketed paste settle.
    std::thread::sleep(Duration::from_millis(250));
    Ok(Some(false))
}

const TOKEN_POST_SUBMIT_READBACK_POLLS: u32 = 5;

/// Some non-echo panes, including the integration harness' `stty -echo; cat`, only
/// render the injected line after the submit key. If pre-submit readback missed the
/// token, do a bounded post-submit check before reporting `CaptureMissingToken`.
fn post_submit_token_visible(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    if payload_token_marker(payload).is_none() {
        return Ok(None);
    }
    for attempt in 0..TOKEN_POST_SUBMIT_READBACK_POLLS {
        if let Some(true) = token_visible_in_capture(backend, target, payload)? {
            return Ok(Some(true));
        }
        if attempt + 1 < TOKEN_POST_SUBMIT_READBACK_POLLS {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(Some(false))
}

/// U1 #7: downgrade the static token verification to `CaptureMissingToken` when the
/// pre-submit readback did not see the token in the pane. A `None` readback (non-token
/// payload, or capture unavailable) falls back to the static verification.
fn inject_verification_after_readback(
    payload: &InjectPayload,
    token_visible_before_submit: Option<bool>,
) -> InjectVerification {
    match (payload, token_visible_before_submit) {
        (_, Some(visible)) if payload_token_marker(payload).is_some() => {
            if visible {
                InjectVerification::CaptureContainsToken
            } else {
                InjectVerification::CaptureMissingToken
            }
        }
        _ => inject_verification_for_payload(payload),
    }
}

fn submit_verification_for_key(key: Key) -> SubmitVerification {
    match key {
        Key::Enter => SubmitVerification::EnterSentWithoutPlaceholderCheck,
        other => SubmitVerification::KeySentAfterVisibleToken { key: other },
    }
}

fn capture_has_pasted_content_prompt(text: &str) -> bool {
    pasted_prompt_match(text).is_some()
}

/// 0.3.27: check if a token marker is present in the bottom N non-empty lines.
/// Used by the unified submit_and_verify to detect whether the provider consumed
/// the pasted message (token scrolls out of composer region on successful submit).
fn token_in_bottom_n(text: &str, marker: &str, n: usize) -> bool {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .any(|line| line.contains(marker))
}

/// purpose: token 在本轮注入中的三态观测
/// contract:
///   - Visible: 当前 capture 可见
///   - Gone: 本轮见过，当前不可见（「见过又消失」才是 token 正信号）
///   - NeverSeen: 本轮从未出现 — 不得把缺席写成 consumed
/// boundary: 观测枚举，不是 SubmitVerification 裁定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenSighting {
    Visible,
    Gone,
    NeverSeen,
}

/// purpose: composer 区折叠占位符（含可选 #N 或 grok 行数）
/// contract: 只看底部 n 非空行；id 来自 `pasted text #N` / `pasted content #N`；
///   grok `[Pasted: N lines]` 无 #N，用 line_count 当身份，不得把「有占位符」当成同一次
/// boundary: scrollback 里的旧占位符不算
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ComposerPastedPrompt {
    pub literal: &'static str,
    pub id: Option<u32>,
    pub line_count: Option<u32>,
    pub from_bottom: u32,
}

/// purpose: 本次粘贴在 composer 上的可锁定身份
/// contract: HashId = claude/codex `#N`；GrokLineCount = grok `[Pasted: N lines|N KB]` 的 N
/// boundary: 无 #N 时禁止用「任意占位符」冒充 HashId
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasteLatch {
    HashId(u32),
    GrokLineCount(u32),
}

fn parse_pasted_hash_id(lower_line: &str) -> Option<u32> {
    for prefix in ["pasted text #", "pasted content #"] {
        if let Some(idx) = lower_line.find(prefix) {
            let rest = &lower_line[idx + prefix.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            if let Ok(id) = digits.parse::<u32>() {
                return Some(id);
            }
        }
    }
    None
}

fn parse_grok_pasted_line_count(lower_line: &str) -> Option<u32> {
    // grok: `[pasted: 42 lines]` or `[pasted: 13 kb]` — 冒号紧跟 pasted，没有 `#N`
    let idx = lower_line.find("pasted:")?;
    let rest = lower_line.get(idx.saturating_add("pasted:".len())..)?;
    let rest = rest.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = rest.get(digits.len()..)?.trim_start();
    let unit_ok = after.starts_with("lines")
        || after.starts_with("line")
        || after.starts_with("kb")
        || after.starts_with("mb")
        || after.starts_with('b');
    if unit_ok {
        digits.parse().ok()
    } else {
        None
    }
}

fn pasted_literal_in_line(lower_line: &str) -> Option<&'static str> {
    if lower_line.contains("pasted content") {
        Some("pasted content")
    } else if lower_line.contains("pasted text") {
        Some("pasted text")
    } else if parse_grok_pasted_line_count(lower_line).is_some() {
        Some("pasted:")
    } else {
        None
    }
}

/// ---
/// purpose: 只在 composer（底部 n 非空行）认折叠占位符
/// params:
///   text: 已规范化的 pane 尾文本
///   n: 只看倒数 n 个非空行；超出这一窗口的占位符视为 transcript 残留，不算
/// returns: 命中则给出字面量、可选编号、可选 grok 行数、距底行数；否则 None
/// contract: 返回字面量 + 可选编号；没有占位符则 None
/// boundary: 不替代 token 路径；编号缺失时调用方必须保守，不得报已消费
/// ---
pub(crate) fn pasted_prompt_in_composer(text: &str, n: usize) -> Option<ComposerPastedPrompt> {
    let mut from_bottom = 0u32;
    for line in text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
    {
        let lower = line.to_ascii_lowercase();
        if let Some(literal) = pasted_literal_in_line(&lower) {
            return Some(ComposerPastedPrompt {
                literal,
                id: parse_pasted_hash_id(&lower),
                line_count: parse_grok_pasted_line_count(&lower),
                from_bottom,
            });
        }
        from_bottom = from_bottom.saturating_add(1);
    }
    None
}

fn latch_paste(text: &str, current: Option<PasteLatch>) -> Option<PasteLatch> {
    if current.is_some() {
        return current;
    }
    let prompt = pasted_prompt_in_composer(text, 15)?;
    if let Some(id) = prompt.id {
        return Some(PasteLatch::HashId(id));
    }
    prompt.line_count.map(PasteLatch::GrokLineCount)
}

fn token_sighting(token_now: bool, token_ever_visible: bool) -> TokenSighting {
    if token_now {
        TokenSighting::Visible
    } else if token_ever_visible {
        TokenSighting::Gone
    } else {
        TokenSighting::NeverSeen
    }
}

fn consumption_from_placeholder(text: &str, tracked: Option<PasteLatch>) -> Option<bool> {
    let prompt = pasted_prompt_in_composer(text, 15);
    match (tracked, prompt) {
        (Some(PasteLatch::HashId(id)), Some(p)) if p.id == Some(id) => Some(false),
        (Some(PasteLatch::HashId(_)), Some(p)) if p.id.is_some() => Some(true),
        (Some(PasteLatch::HashId(_)), Some(_)) => Some(false),
        (Some(PasteLatch::HashId(_)), None) => Some(true),
        (Some(PasteLatch::GrokLineCount(n)), Some(p)) if p.line_count == Some(n) => Some(false),
        // 不同 N：不能把「上一贴走了」写成已消费（无 #N，换行数 ≠ 同一次）
        (Some(PasteLatch::GrokLineCount(_)), Some(_)) => Some(false),
        (Some(PasteLatch::GrokLineCount(_)), None) => Some(true),
        (None, _) => Some(false),
    }
}

/// ---
/// purpose: 无身份时禁止把「未证实」升级成连按回车
/// params:
///   text: 最近一次 pane 尾文本
///   marker: 本条消息的投递 token 完整标记
///   tracked: 本次粘贴已锁定的占位符身份（编号或 grok 行数）；None 表示没锁到身份
/// returns: true 才允许再按一次 Enter
/// contract: 只有 token 仍在 composer 底 15 非空行，或锁定的占位符身份仍在，才重按
/// boundary: 不重粘文本；不把不同 grok 行数当成可重按的同一次；tracked 为 None 时占位符路径一律不重按（token 仍在底 15 非空行的重按与 tracked 无关）
/// ---
pub(crate) fn should_resubmit_enter(text: &str, marker: &str, tracked: Option<PasteLatch>) -> bool {
    if token_in_bottom_n(text, marker, 15) {
        return true;
    }
    let Some(prompt) = pasted_prompt_in_composer(text, 15) else {
        return false;
    };
    match tracked {
        Some(PasteLatch::HashId(id)) => prompt.id == Some(id),
        Some(PasteLatch::GrokLineCount(n)) => prompt.line_count == Some(n),
        None => false,
    }
}

fn composer_joined(text: &str, n: usize) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .collect();
    lines.reverse();
    lines.concat()
}

/// ---
/// purpose: 本次注入身份是否还在 composer（复用 token / #N / grok 行数，不认提示符皮肤）
/// contract: 单行 token、折行拼接 token、或锁定的 PasteLatch 仍在底部 ⇒ true
/// boundary: payload 里的 ❯/`composer>`/`>` 不算身份；无身份不得据此连按回车
/// ---
pub(crate) fn this_paste_identity_in_composer(
    text: &str,
    marker: &str,
    tracked: Option<PasteLatch>,
) -> bool {
    should_resubmit_enter(text, marker, tracked) || composer_joined(text, 15).contains(marker)
}

/// ---
/// purpose: Unverified 之后要不要补一颗 C-m（本次身份仍在，不是提示符长什么样）
/// contract: 无 busy 且本次身份仍在 composer ⇒ true；达上限由调用方停
/// boundary: consumed=None 不在这里决定（调用方不补）；不重粘；不把 payload 提示符字符当守卫
/// ---
pub(crate) fn should_resend_enter_after_unverified(
    text: &str,
    marker: &str,
    tracked: Option<PasteLatch>,
) -> bool {
    if provider_busy_signal_in_tail(text) {
        return false;
    }
    this_paste_identity_in_composer(text, marker, tracked)
}

/// ---
/// purpose: Unverified 补发 B 是否该介入——只补 A 的 should_resubmit_enter 看不到的折行缺口
/// params:
///   text: 最近一次 pane 尾文本
///   marker: 本条消息的投递 token 完整标记
///   tracked: 本次粘贴已锁定的占位符身份（编号或 grok 行数）；None 表示没锁到身份
/// returns: true 才允许 B 再按一次 Enter
/// contract: !busy 且 this_paste_identity_in_composer 且 !should_resubmit_enter ⇒ true
/// boundary: A 已因 latch / 底15 单行 token 为真时不介入（含 A 已打满 cap）；不重粘；consumed=None 不在这里决定
/// ---
pub(crate) fn should_resend_unverified_wrap_gap(
    text: &str,
    marker: &str,
    tracked: Option<PasteLatch>,
) -> bool {
    if should_resubmit_enter(text, marker, tracked) {
        return false;
    }
    should_resend_enter_after_unverified(text, marker, tracked)
}

/// ---
/// purpose: cursor 重按 Enter 的核实：token 仍在输入框，且回合未在进行
/// contract: busy ⇒ 不重按；token 不在底部 5 非空行 ⇒ 不重按
/// boundary: 不把 transcript 里的 pasted #N 当输入框；不替代 claude/grok 的 should_resubmit_enter
/// ---
pub(crate) fn should_resubmit_enter_cursor(text: &str, marker: &str) -> bool {
    if provider_busy_signal_in_tail(text) {
        return false;
    }
    token_in_bottom_n(text, marker, 5)
}

/// ---
/// purpose: Phase 1 判定 TUI 侧注入已完成、可以按回车
/// params:
///   text: 最近一次 pane 尾文本
///   marker: 本条消息的投递 token 完整标记
/// returns: true 表示可以进入提交阶段
/// contract: composer 已有折叠占位符 ⇒ 完成；短贴 token 可见且不是待折叠高块 ⇒ 完成
/// boundary: token 出现在未折叠的高块里不算完成（按早了）；只判「能不能按」，不判「按了有没有成」
/// ---
pub(crate) fn paste_ready_for_enter(text: &str, marker: &str) -> bool {
    if pasted_prompt_in_composer(text, 15).is_some() {
        return true;
    }
    text.contains(marker) && !raw_multiline_still_unfolded(text, marker)
}

fn raw_multiline_still_unfolded(text: &str, marker: &str) -> bool {
    if pasted_prompt_in_composer(text, 15).is_some() {
        return false;
    }
    if !text.contains(marker) {
        return false;
    }
    text.lines().filter(|line| !line.trim().is_empty()).count() > 15
}

/// ---
/// purpose: 把 token 三态 + 本次 #N / grok 占位符合成消费判定
/// params:
///   text: 最近一次 pane 尾文本
///   marker: 本条消息的投递 token 完整标记
///   token_ever_visible: 本次注入期间是否曾在 pane 上见过该 token
///   tracked: 本次粘贴锁定的占位符身份；None 表示没锁到
/// returns: Some(true) 已消费；Some(false) 未消费；本函数不产生 None
/// contract:
///   Some(true)=已消费；Some(false)=未消费（含 NeverSeen 且无正信号 → 未证实走 false）
///   Gone 必须先复核 PasteLatch 身份：占位符仍在 composer ⇒ 未消费
///   None 不由本函数产生；调用方在 capture 失败时保持 None，必须落到 Unverified
/// boundary: 不修 BUSY 入队；Working 信号由调用方在 Some(false) 之后看；None 不得当成功
/// ---
pub(crate) fn consumption_from_capture(
    text: &str,
    marker: &str,
    token_ever_visible: bool,
    tracked: Option<PasteLatch>,
) -> Option<bool> {
    let token_now = token_in_bottom_n(text, marker, 15);
    match token_sighting(token_now, token_ever_visible) {
        TokenSighting::Visible => Some(false),
        TokenSighting::Gone => gone_consumption(text, tracked),
        TokenSighting::NeverSeen => consumption_from_placeholder(text, tracked),
    }
}

/// ---
/// purpose: Gone 分支在判已消费前复核 composer 占位符（PasteLatch 身份）
/// contract: 锁定身份仍在，或未锁定但 grok `[Pasted: …]` 仍在 ⇒ Some(false)
/// boundary: 空 composer + 无 grok 占位符保持 Some(true)（claude 短贴路径不变）
/// ---
fn gone_consumption(text: &str, tracked: Option<PasteLatch>) -> Option<bool> {
    if tracked.is_some() {
        return match consumption_from_placeholder(text, tracked) {
            Some(false) => Some(false),
            _ => Some(true),
        };
    }
    match pasted_prompt_in_composer(text, 15) {
        Some(p) if p.literal == "pasted:" => Some(false),
        _ => Some(true),
    }
}

fn marker_position_from_bottom(text: &str, marker: &str) -> Option<u32> {
    let mut from_bottom = 0u32;
    for line in text.lines().rev().filter(|line| !line.trim().is_empty()) {
        if line.contains(marker) {
            return Some(from_bottom);
        }
        from_bottom = from_bottom.saturating_add(1);
    }
    None
}

fn provider_busy_signal_in_tail(text: &str) -> bool {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(15)
        .any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("working")
                || lower.contains("thinking")
                || lower.contains("processing")
                || lower.contains("esc to interrupt")
                || line.contains('●')
                || line.contains('⏳')
                || line.contains('⠋')
                || line.contains('⠙')
                || line.contains('⠹')
                || line.contains('⠸')
                || line.contains('⠼')
                || line.contains('⠴')
                || line.contains('⠦')
                || line.contains('⠧')
                || line.contains('⠇')
                || line.contains('⠏')
                || line.contains('✶')
                || line.contains('✢')
                || line.contains('✻')
                || line.contains('✽')
                || line.contains('✳')
        })
}

/// ---
/// purpose: 入箱 vs 开跑（G4）。给定最近一次 pane 尾，回答席位是否因此开跑。
/// params:
///   text: 最近一次 pane 尾文本
///   marker: 本条消息的投递 token 完整标记；None 表示本次载荷没有 token 可查
/// returns: 三态观测值，绝不返回 NotRequired（那是空载荷路径由调用方直接给的）
/// contract:
///   - LeaderNewTurnBoundaryVerified = 底部 busy 信号
///   - LeaderNewTurnBoundaryMissing = composer 仍有本次粘贴（占位符或 token）且无 busy
///   - NotYetObserved = 其余（含 composer 已空但无 busy）→ 不知道，不得报开跑
/// boundary:
///   - 不是 SubmitVerification / 不是投递闸门；不许用过了 N 秒来判
///   - busy 判据是底 15 非空行的宽子串匹配，正文或 transcript 残留可让它假阳，判定强度到此为止
/// ---
pub(crate) fn observe_turn_from_capture(text: &str, marker: Option<&str>) -> TurnVerification {
    if provider_busy_signal_in_tail(text) {
        return TurnVerification::LeaderNewTurnBoundaryVerified;
    }
    let paste_still = pasted_prompt_in_composer(text, 15).is_some();
    let token_still = marker
        .map(|m| token_in_bottom_n(text, m, 15))
        .unwrap_or(false);
    if paste_still || token_still {
        return TurnVerification::LeaderNewTurnBoundaryMissing;
    }
    TurnVerification::NotYetObserved
}

fn turn_verification_for_payload(
    payload: &InjectPayload,
    last_text: Option<&str>,
) -> TurnVerification {
    match payload {
        InjectPayload::Empty => TurnVerification::NotRequired,
        InjectPayload::Text(_) | InjectPayload::TextSkipConsumptionPoll(_) => {
            observe_turn_from_capture(last_text.unwrap_or(""), payload_token_marker(payload))
        }
    }
}

fn submit_attempt_observation(
    attempt_index: u32,
    captured: &CapturedText,
    marker: Option<&str>,
    elapsed_ms: u64,
) -> SubmitAttemptObservation {
    let marker_position = marker.and_then(|m| marker_position_from_bottom(&captured.text, m));
    // marker 在时也走 pasted_prompt_match：折叠占位符是编排长粘贴的正信号，
    // 不能只在无 token 载荷上才认。matched 仍表示「composer 里还有待提交信号」。
    let (matched, matched_literal, where_in_tail) = if let Some(marker) = marker {
        if token_in_bottom_n(&captured.text, marker, 15) {
            (
                true,
                marker_position.map(|_| marker.to_string()),
                marker_position,
            )
        } else if let Some((literal, where_in_tail)) = pasted_prompt_match(&captured.text) {
            (
                pasted_prompt_in_composer(&captured.text, 15).is_some(),
                Some(literal.to_string()),
                Some(where_in_tail),
            )
        } else {
            (false, None, None)
        }
    } else if let Some((literal, where_in_tail)) = pasted_prompt_match(&captured.text) {
        (true, Some(literal.to_string()), Some(where_in_tail))
    } else {
        (false, None, None)
    };
    let (pane_tail_excerpt, pane_tail_lines) = scrub_pane_excerpt(&captured.text, 20);
    SubmitAttemptObservation {
        attempt_index,
        matched,
        matched_literal,
        where_in_tail,
        pane_tail_excerpt,
        pane_tail_lines,
        elapsed_ms,
    }
}

/// 0.3.27: check if a pasted-content prompt literal (`pasted content` / `pasted text`)
/// appears in the bottom N non-empty lines. Narrower than the full-Tail(80) check
/// that caused scrollback ghost matches (E50 defect B).
fn pasted_prompt_in_bottom(text: &str, n: usize) -> bool {
    pasted_prompt_in_composer(text, n).is_some()
}

/// E50 PR-1 (0.3.24 P0, pasted-prompt 假阴诊断): factor `capture_has_pasted_content_prompt`
/// so the diagnostic layer can recover the MATCHED LITERAL and its position
/// in the tail. Returns `(literal, line_index_from_bottom)` on a match.
/// Byte-identical match semantics — `capture_has_pasted_content_prompt`'s
/// `bool` wrapper preserves the legacy `true/false` contract for the three
/// existing callers (the appear-gate poll at :1122-1128 + the legacy submit
/// loop matcher at :1138 + post-flip clearer at :1138).
///
/// **Where-in-tail rationale**: a `pasted content` literal that lives in
/// the SCROLLBACK (line 6+ from bottom) is NOT the live composer
/// placeholder — codex's successful submit scrolls the block into history
/// where it remains in the last 80 lines. The current matcher cannot
/// distinguish that from a live placeholder; this fn surfaces the data
/// the operator needs to see (PR-2 will USE it to fix the criterion).
/// ---
/// purpose: 在整段 pane 尾里找折叠占位符字面量,并给出它距底部多少非空行
/// params:
///   text: pane 尾文本;不限窗口,整段都找
/// returns: Some((命中的字面量, 距底非空行数));无命中则 None;字面量只出现在被裁掉的空白行时保守记为 0
/// boundary:
///   - 只做取证不做判定:距底行数大说明多半在 transcript 而非 composer,但本函数不替调用方下结论
///   - 与 pasted_prompt_in_composer 不同——后者限定底部窗口,是判定用;本函数不限窗口,是诊断用
/// ---
pub(crate) fn pasted_prompt_match(text: &str) -> Option<(&'static str, u32)> {
    let lower = text.to_ascii_lowercase();
    let lit = if lower.contains("pasted content") {
        "pasted content"
    } else if lower.contains("pasted text") {
        "pasted text"
    } else if text
        .lines()
        .any(|line| parse_grok_pasted_line_count(&line.to_ascii_lowercase()).is_some())
    {
        "pasted:"
    } else {
        return None;
    };
    // Distance from the bottom of the tail in non-empty lines.
    let non_empty: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut from_bottom: u32 = 0;
    for line in non_empty.iter().rev() {
        let line_lower = line.to_ascii_lowercase();
        let hit = if lit == "pasted:" {
            parse_grok_pasted_line_count(&line_lower).is_some()
        } else {
            line_lower.contains(lit)
        };
        if hit {
            return Some((lit, from_bottom));
        }
        from_bottom = from_bottom.saturating_add(1);
    }
    // Literal appeared only in trimmed-away whitespace lines — treat as
    // bottom (defensive; rare).
    Some((lit, 0))
}

/// E50 PR-1 (0.3.24 P0): scrub a pane capture for safe inclusion in
/// `events.jsonl`. Steps:
///   1. Strip CSI / OSC ANSI escapes.
///   2. Take the bottom `tail_lines` of non-empty lines.
///   3. Redact common secret shapes (sk-, ghp_, AKIA, Bearer ..., 32+ hex).
///   4. Cap at ~1200 bytes (UTF-8 safe truncation).
/// Returns `(excerpt, line_count)`. Designed to be CHEAP — no regex crate
/// dependency, simple byte scanning.
/// ---
/// purpose: 把 pane 抓屏加工成可安全写进 events.jsonl 的摘录
/// params:
///   raw: 原始抓屏文本
///   tail_lines: 只保留倒数这么多非空行
/// returns: (脱敏并截断后的摘录, 实际保留的行数)
/// boundary:
///   - 只做四件事:剥 ANSI、取尾部、脱敏常见密钥形状、按 1200 字节在字符边界截断
///   - 脱敏是形状匹配,不是完备保证;非常见形状的凭据不会被识别
///   - 输出仍是消息正文的一部分,只落本地事件文件,不外传
/// ---
pub(crate) fn scrub_pane_excerpt(raw: &str, tail_lines: usize) -> (String, u32) {
    let stripped = strip_ansi_escapes_inplace(raw);
    let lines: Vec<&str> = stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = if lines.len() > tail_lines {
        &lines[lines.len() - tail_lines..]
    } else {
        &lines[..]
    };
    let mut out = tail
        .iter()
        .map(|line| scrub_secrets(line))
        .collect::<Vec<_>>()
        .join("\n");
    if out.len() > 1200 {
        // Truncate at UTF-8 char boundary.
        let mut cut = 1200;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("…[truncated]");
    }
    (out, tail.len() as u32)
}

fn strip_ansi_escapes_inplace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            // CSI: ESC [ ... <final byte 0x40-0x7e>
            if bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                i = j.saturating_add(1).min(bytes.len());
                continue;
            }
            // OSC: ESC ] ... BEL or ESC \
            if bytes[i + 1] == b']' {
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j.min(bytes.len());
                continue;
            }
            // Other single-char ESC sequence — skip ESC + next byte.
            i += 2;
            continue;
        }
        let ch = bytes[i];
        out.push(ch as char);
        i += 1;
    }
    // Re-decode from bytes to recover UTF-8 (since we pushed bytes as chars,
    // multi-byte UTF-8 is preserved correctly because we only skip on the
    // single-byte ESC start). For pane text this is good enough; pathological
    // UTF-8 inside CSI parameter bytes is invalid anyway.
    out
}

fn scrub_secrets(line: &str) -> String {
    let line = crate::redaction::redact_external_text(line);
    // Five shapes: sk-XXXX, ghp_XXXX, AKIAXXXX (16-char uppercase id), Bearer XXXX,
    // 32+ hex (token).
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // sk- / ghp_ / AKIA prefixes: detect and redact through end-of-token.
        if matches_prefix(bytes, i, b"sk-") || matches_prefix(bytes, i, b"ghp_") {
            let prefix_len = if bytes[i] == b's' { 3 } else { 4 };
            let token_end = scan_token_end(bytes, i + prefix_len);
            out.push_str(&line[i..i + prefix_len]);
            out.push_str("REDACTED");
            i = token_end;
            continue;
        }
        if matches_prefix(bytes, i, b"AKIA") {
            let token_end = scan_token_end(bytes, i + 4);
            out.push_str("AKIA");
            out.push_str("REDACTED");
            i = token_end;
            continue;
        }
        if matches_prefix_case_insensitive(bytes, i, b"Bearer ") {
            let token_end = scan_token_end(bytes, i + 7);
            out.push_str(&line[i..i + 7]);
            out.push_str("REDACTED");
            i = token_end;
            continue;
        }
        // 32+ hex run.
        if is_hex_byte(bytes[i]) {
            let mut j = i;
            while j < bytes.len() && is_hex_byte(bytes[j]) {
                j += 1;
            }
            if j - i >= 32 {
                out.push_str("REDACTED_HEX");
                i = j;
                continue;
            }
        }
        // Default: passthrough byte.
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn matches_prefix(bytes: &[u8], i: usize, prefix: &[u8]) -> bool {
    bytes.get(i..i + prefix.len()).is_some_and(|s| s == prefix)
}

fn matches_prefix_case_insensitive(bytes: &[u8], i: usize, prefix: &[u8]) -> bool {
    bytes
        .get(i..i + prefix.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
}

fn scan_token_end(bytes: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < bytes.len() && is_token_byte(bytes[j]) {
        j += 1;
    }
    j
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn is_hex_byte(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

const PASTED_CONTENT_APPEAR_POLLS: u32 = 5;
const PASTED_CONTENT_SUBMIT_ATTEMPTS: u32 = 3;

/// E46 (0.3.24 bug#5): bounded resend cap for post-Enter consumption probe.
/// Mirrors PASTED_CONTENT_SUBMIT_ATTEMPTS shape: try Enter then bounded
/// re-checks of the pane's input region. Each iteration first re-checks that
/// the input still has content before resending Enter — guards against double
/// submission when the first Enter was consumed but our readback was slow.
const POST_SUBMIT_CONSUMPTION_ATTEMPTS: u32 = 3;
const POST_SUBMIT_CONSUMPTION_POLL_MS: u64 = 60;
/// post-Enter `capture()` 失败时只重读、不重按。计入 `InjectReport.attempts`。
const POST_SUBMIT_CAPTURE_RETRY: u32 = 4;
const POST_SUBMIT_CAPTURE_RETRY_MS: u64 = 20;
/// Unverified 之后、本次身份仍在 composer 时，只补这一颗回车。
const UNVERIFIED_COMPOSER_RESEND_MAX: u32 = 1;

/// E46 (0.3.24 bug#5, C5 provider-agnostic detector): the pane's input region
/// is "consumed" when the token text that was just visible BEFORE the Enter
/// is no longer present in the captured tail. Structural signal — no
/// provider-specific UI string. Works across claude / codex / copilot because
/// every provider's composer clears the input area after a successful submit
/// (the content scrolls into history, leaving the prompt empty).
///
/// Returns:
///   * `Some(true)`  — token was visible BEFORE submit and is GONE from
///     the visible input area now → consumption confirmed.
///   * `Some(false)` — token still visible (or other reason to think not yet
///     consumed).
///   * `None` — payload has no token marker (peer message without token,
///     empty payload) so we can't structurally check; caller treats this as
///     non-blocking (the pre-existing `EnterSentWithoutPlaceholderCheck`
///     path).
fn post_submit_input_consumed(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    let Some(marker) = payload_token_marker(payload) else {
        return Ok(None);
    };
    let captured = backend.capture(target, CaptureRange::Tail(40))?;
    // The token may legitimately appear in scrollback (a successful submit
    // pushes it into history). We only treat the BOTTOM-of-pane region (last
    // few lines, where the input area lives) as the consumption signal. Tail
    // 30 lines is small enough that the input area still dominates if the
    // submit didn't go through, while a successful submit has pushed the
    // token marker out of the bottom 15 lines by the time the response
    // composer redraws.
    let tail_lines: Vec<&str> = captured.text.lines().rev().take(15).collect();
    let token_in_tail = tail_lines.iter().any(|line| line.contains(marker));
    Ok(Some(!token_in_tail))
}

fn shell_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let mut parts = Vec::new();
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    // D9 (#264) / Python providers.py:142-145 + provider_env.py:86 — profile env_unset keys
    // must be unset in the shell itself: the `sh -lc` line inherits the tmux SERVER's stale
    // environment, which exec-prefix assignments cannot clear.
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    // 0.4.x ordering fix (env-leak symptom #3): KEY=val exports must NOT
    // re-introduce any key that was just unset. Filter env entries whose key
    // appears in env_unset so the unset wins on the final shell line. This
    // matters when inherited env (worker_spawn_env / apply_profile_launch_env)
    // contains the very keys we want to scrub (e.g. CLAUDE_EFFORT carried
    // forward from the launching shell into the env map).
    for (key, value) in env {
        if unset_set.contains(key.as_str()) {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    parts.push("exec".to_string());
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

/// 0.4.x (CR R6): single-source marker prefix. The exit marker emitted by
/// `leader_shell_wrapper_command` and the substring detected by
/// `leader_provider_health` MUST share this prefix exactly. Format:
/// `"[team-agent] {provider_label} exited with {rc}"`.
pub const LEADER_PROVIDER_EXIT_MARKER_PREFIX: &str = "[team-agent]";
pub const LEADER_PROVIDER_EXIT_MARKER_SUFFIX: &str = "exited with";

/// 0.4.x (CR R6): build the leader exit marker text for `provider_label`.
/// Used by both the shell wrapper (printf source) and the health check
/// (capture substring) so they cannot drift.
/// ---
/// purpose: 单一来源地拼出 leader pane 的 provider 退出标记文本
/// params:
///   provider_label: 人类可读的 provider 名,原样嵌入标记
/// returns: 前缀 + provider 名 + 后缀 拼成的标记(不含退出码,退出码由 shell 的 printf 补)
/// boundary:
///   - 写标记的 shell wrapper 与读标记的健康检查必须都用这一个函数,禁止各写一份字面量
///   - 这是内容信号:pane 里出现同样文本(如 cat 日志)就会误触发,判定强度到此为止
/// ---
pub fn leader_provider_exit_marker(provider_label: &str) -> String {
    format!(
        "{LEADER_PROVIDER_EXIT_MARKER_PREFIX} {provider_label} {LEADER_PROVIDER_EXIT_MARKER_SUFFIX}"
    )
}

/// 0.5.39 Slice 1 (tmux-server-death-locate §7 Slice 1): ambient-tmux
/// leader-pane probe. Kept inside `tmux_backend` because it is
/// definitionally ambient — its job is to discover *which* session/pane
/// the leader process is currently inside via $TMUX/$TMUX_PANE +
/// `tmux display-message`. Everywhere else in the codebase, tmux ops
/// must go through a socket-scoped `TmuxBackend` (that constraint is
/// enforced by `n16_tmux_socket_invariant_red.rs` +
/// `tmux_server_death_0539_contract.rs::display_cleanup_...`); this
/// helper is the single controlled exception.
///
/// Returns `(session_name, Some(pane_id))` when the ambient tmux
/// responds, or `None` if `$TMUX` is unset / `display-message` fails.
/// ---
/// purpose: 探出当前 leader 进程正待在哪个环境 tmux 的哪个 session / pane 里
/// returns: Some((session 名, 可选 pane 标识));TMUX 与 TMUX_PANE 都未设,或所有探测命令都失败或解析不出时 None(仅 TMUX_PANE 在设时仍会带 -t 探测)
/// boundary:
///   - 本文件唯一允许绕过 socket 绑定后端、直接 Command::new("tmux") 的例外,因为它的任务就是发现「我在哪」
///   - 依次试 TMUX_PANE 定位与无 -t 的当前 pane,取第一个能解析出的结果;失败静默跳到下一条,不报错
///   - 只读不写,不 attach、不改任何 tmux 状态
/// ---
pub fn probe_ambient_leader_pane_info() -> Option<(String, Option<String>)> {
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.is_empty());
    let mut commands: Vec<Vec<String>> = Vec::new();
    if let Some(pane) = pane.as_deref() {
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "-F".to_string(),
            "#{session_name}\t#{pane_id}".to_string(),
        ]);
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "-F".to_string(),
            "#{session_name}".to_string(),
        ]);
    }
    if std::env::var("TMUX").is_ok_and(|value| !value.is_empty()) {
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-F".to_string(),
            "#{session_name}\t#{pane_id}".to_string(),
        ]);
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-F".to_string(),
            "#{session_name}".to_string(),
        ]);
    }
    for command in commands {
        let output = match std::process::Command::new("tmux").args(&command).output() {
            Ok(output) if output.status.success() => output,
            _ => continue,
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) else {
            continue;
        };
        let line = line.trim();
        let parts: Vec<&str> = line.split('\t').collect();
        let parsed = match parts.as_slice() {
            [session, pane_str, ..] if !session.is_empty() && !session.starts_with('%') => Some((
                (*session).to_string(),
                (!pane_str.is_empty()).then(|| (*pane_str).to_string()),
            )),
            [pane_str, session, ..] if pane_str.starts_with('%') && !session.is_empty() => {
                Some(((*session).to_string(), Some((*pane_str).to_string())))
            }
            [session] if !session.is_empty() && !session.starts_with('%') => {
                Some(((*session).to_string(), None))
            }
            _ => None,
        };
        if parsed.is_some() {
            return parsed;
        }
    }
    None
}

/// 0.5.39 Slice 2 (tmux-server-death-locate §11.2): single-source worker
/// exit marker prefix. Same envelope shape as `LEADER_PROVIDER_EXIT_MARKER_*`
/// but distinct so status/classifier code can tell "leader pane fell back
/// to shell" from "worker pane fell back to shell". Format:
/// `"[team-agent worker] {provider_label} exited with {rc}"`.
pub const WORKER_PROVIDER_EXIT_MARKER_PREFIX: &str = "[team-agent worker]";
pub const WORKER_PROVIDER_EXIT_MARKER_SUFFIX: &str = "exited with";

/// 0.5.39 Slice 2: build the worker exit marker text for `provider_label`.
/// Used by both the worker shell wrapper (printf source) and future
/// status/classifier code (capture substring) so they cannot drift.
/// ---
/// purpose: 单一来源地拼出 worker pane 的 provider 退出标记文本
/// params:
///   provider_label: 人类可读的 provider 名,原样嵌入标记
/// returns: worker 专用前缀 + provider 名 + 后缀;与 leader 标记刻意不同,便于分清是哪种 pane 掉回 shell
/// boundary:
///   - 写标记的 worker shell wrapper 与读标记的分类代码必须都用这一个函数
///   - 同为内容信号,pane 里出现同样文本即误触发
/// ---
pub fn worker_provider_exit_marker(provider_label: &str) -> String {
    format!(
        "{WORKER_PROVIDER_EXIT_MARKER_PREFIX} {provider_label} {WORKER_PROVIDER_EXIT_MARKER_SUFFIX}"
    )
}

/// 0.5.39 Slice 2 (tmux-server-death-locate §7 Slice 2): worker shell
/// wrapper. Same shape as `leader_shell_wrapper_command` — provider runs
/// as a CHILD of a long-lived shell so provider exit does NOT collapse the
/// worker pane (which under upstream tmux 3.6a private-server bugs can
/// cascade into whole-server death). When the provider exits, the worker
/// pane remains alive with an explicit worker exit marker, then runs an
/// inert `sh` tail that does not read terminal input.
/// ---
/// purpose: 拼出 worker pane 的 shell 启动行,让 provider 作为子进程运行,退出后 pane 不塌成 [exited]
/// params:
///   argv: provider 启动命令,逐项做 shell 引用
///   cwd: 进程工作目录
///   env: 要导出的环境变量;键若出现在 env_unset 中则跳过,保证 unset 赢
///   env_unset: 必须真正 unset 的键;先 unset 再导出
///   provider_label: 嵌入退出标记的 provider 名
/// returns: 单条 shell 命令行,依次为 cd、unset、env 导出与 provider(不带 exec)、printf 退出标记、exec 惰性 sh 尾巴
/// boundary:
///   - 只拼字符串,不执行、不 spawn
///   - 与 leader 版的唯一实质差异是退出标记前缀;结构必须保持一致
///   - 惰性尾巴不读 pane 标准输入,故该 pane 之后收到的注入不会被任何进程消费
/// ---
pub fn worker_shell_wrapper_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
    provider_label: &str,
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let mut parts = Vec::new();
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    for (key, value) in env {
        if unset_set.contains(key.as_str()) {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.push(";".to_string());
    parts.push("rc=$?;".to_string());
    parts.push("printf".to_string());
    parts.push(shell_quote(&format!(
        "\n{} %s\n",
        worker_provider_exit_marker(provider_label)
    )));
    parts.push("\"$rc\";".to_string());
    parts.push(inert_pane_tail_command());
    parts.join(" ")
}

/// 0.4.x (CR C-2): leader shell wrapper — provider runs as a CHILD of a
/// long-lived shell, not as the pane's primary process. When the provider
/// exits, the pane remains alive with an explicit exit marker, then runs an
/// inert `sh` tail that does not read terminal input.
///
/// Four required envelope sections (CR C-2):
///   1. cd <cwd>                    — same as `shell_command`
///   2. unset <KEY> ...             — provider env_unset block
///   3. KEY=val ... <provider>      — env exports + provider invocation
///                                    (NO `exec` — runs as child)
///   4. printf exit marker; exec inert sh tail
///
/// `provider_label` is a human-readable provider name (e.g. "claude",
/// "codex") embedded in the exit marker for diagnostics.
/// ---
/// purpose: 拼出 leader pane 的 shell 启动行,让 provider 作为子进程运行,退出后 pane 保留并显示退出标记
/// params:
///   argv: provider 启动命令,逐项做 shell 引用
///   cwd: 进程工作目录
///   env: 要导出的环境变量;键若出现在 env_unset 中则跳过,保证 unset 赢
///   env_unset: 必须真正 unset 的键;先 unset 再导出
///   provider_label: 嵌入退出标记的 provider 名
/// returns: 单条 shell 命令行,四段依次为 cd、unset、env 导出与 provider(刻意不带 exec)、printf 退出标记加 exec 惰性 sh 尾巴
/// boundary:
///   - 只拼字符串,不执行、不 spawn
///   - 刻意不用 exec:用了 provider 就成了 pane 主进程,退出即塌成 [exited]
///   - 退出标记文本必须来自 leader_provider_exit_marker,禁止在此写字面量
/// ---
pub fn leader_shell_wrapper_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
    provider_label: &str,
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let mut parts = Vec::new();
    // 1. cd
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    // 2. unset
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    // 3. env exports + provider (NO `exec` so the provider is a child).
    // 0.4.x ordering fix: skip keys present in env_unset so KEY=val does not
    // re-introduce a just-unset variable from the inherited env map.
    for (key, value) in env {
        if unset_set.contains(key.as_str()) {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.push(";".to_string());
    // 4. exit marker + inert shell tail
    parts.push("rc=$?;".to_string());
    parts.push("printf".to_string());
    // CR R6: marker text comes from single-source `leader_provider_exit_marker`.
    parts.push(shell_quote(&format!(
        "\n{} %s\n",
        leader_provider_exit_marker(provider_label)
    )));
    parts.push("\"$rc\";".to_string());
    parts.push(inert_pane_tail_command());
    parts.join(" ")
}

fn inert_pane_tail_command() -> String {
    // `sh` is deliberate: provider-exit health checks use the shell basename
    // to reach the exit-marker branch. `sh -c` does not read commands from
    // stdin; disabling echo also prevents input from appearing accepted.
    let script = r#"trap '' INT QUIT; stty -echo 2>/dev/null; printf "%s\n" "[team-agent] Provider exited; this pane no longer accepts input. Restart from another pane with the appropriate team-agent start command."; while :; do sleep 3600 & wait "$!"; done"#;
    format!("exec /bin/sh -c {}", shell_quote(script))
}

fn shell_quote(raw: &str) -> String {
    if raw.is_empty() {
        return "''".to_string();
    }
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        return raw.to_string();
    }
    let mut quoted = String::from("'");
    for ch in raw.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn notify_submit_observer(observer: Option<&dyn SubmitObserver>) {
    if let Some(observer) = observer {
        observer.after_physical_submit();
    }
}

impl Transport for TmuxBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn probes_real_tmux_socket_roots(&self) -> bool {
        true
    }

    fn tmux_endpoint(&self) -> Option<String> {
        self.socket
            .as_ref()
            .map(|endpoint| endpoint.as_endpoint().to_string())
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn(session, window, argv, cwd, env, &[], true)
    }

    fn spawn_first_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.spawn(session, window, argv, cwd, env, env_unset, true)
    }

    fn spawn_into_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.spawn(session, window, argv, cwd, env, env_unset, false)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn(session, window, argv, cwd, env, &[], false)
    }

    fn spawn_split_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_split(session, window, argv, cwd, env, env_unset)
    }

    /// 0.5.39 Slice 2: TmuxBackend override of the worker-shell-wrapper
    /// variant. Same mechanism as the leader wrapper (child provider under
    /// long-lived shell), but the marker text is distinct so downstream
    /// classifiers can tell leader vs worker provider exit apart.
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
        let command = worker_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, true)
    }

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
        let command = worker_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, false)
    }

    /// 0.4.x (CR C-2): TmuxBackend override of the leader-shell-wrapper
    /// variant. Builds the wrapper shell line via
    /// `leader_shell_wrapper_command` and runs it through
    /// `spawn_with_command` (bypassing the default `exec <cmd>` shape).
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
        let command = leader_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, true)
    }

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
        let command = leader_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, false)
    }

    fn inject(
        &self,
        target: &Target,
        payload: &InjectPayload,
        submit: Key,
        bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        // Trait entry delegates to inject_with_submit_observer with no
        // observer. Populated InjectReport fields: stage_reached,
        // inject_verification, submit_verification, turn_verification,
        // attempts, submit_diagnostics — the 0.5.43 debt-sweep
        // governance guard (debt_sweep_0543_contract.rs::
        // coordinator_debug_eprintlns_are_deleted_but_inject_report_shape_remains)
        // scans THIS function body for those field names so debug-
        // print cleanup can't silently drop InjectReport surface area.
        self.inject_with_submit_observer(target, payload, submit, bracketed, None)
    }

    fn inject_with_submit_observer(
        &self,
        target: &Target,
        payload: &InjectPayload,
        submit: Key,
        bracketed: bool,
        observer: Option<&dyn SubmitObserver>,
    ) -> Result<InjectReport, TransportError> {
        let pane = pane_from_target(target);
        let _pane_input = crate::lifecycle::pane_input_lock::acquire_or_proceed(
            crate::lifecycle::pane_input_lock::PaneInputLockRequest {
                workspace: self.event_workspace.as_deref(),
                target_key: pane.as_str(),
                operation: "inject",
            },
        );
        // U1 #7: pane readback signal for the non-pasted-prompt text path.
        let mut token_visible_for_report: Option<bool> = None;
        match payload {
            InjectPayload::Empty => {
                self.prepare_pane_for_submit(target, matches!(submit, Key::Enter));
                let argv = tmux_empty_inject_argv(&pane, submit);
                self.run_ok(&argv)?;
                notify_submit_observer(observer);
            }
            InjectPayload::Text(text) | InjectPayload::TextSkipConsumptionPoll(text) => {
                let buffer = buffer_name_for_text(text);
                for argv in tmux_inject_text_argv(&pane, &buffer, text, bracketed) {
                    let stage = inject_stage_for_argv(&argv);
                    if stage == InjectStage::LoadBuffer {
                        self.run_inject_stage_with_stdin(&argv, stage, text)?;
                    } else {
                        self.run_inject_stage(&argv, stage)?;
                    }
                }
                // ═══════════════════════════════════════════════════════════
                // 0.3.27 UNIFIED submit_and_verify
                //
                // Replaces the dual-branch split (saw_pasted_prompt weak loop
                // + E46 token consumption gate) with a single pipeline:
                //
                //   Phase 1 — token visibility poll (dynamic timeout based on
                //     payload size, 50ms interval, replaces the fixed 125ms
                //     appear_gate)
                //   Phase 2 — Escape (if bracketed+Text+Enter) + Enter + poll
                //     token disappeared from bottom 3 lines. On failure:
                //     re-check → Escape+Enter → poll. Up to 3 attempts.
                //
                // Design truth source: .team/artifacts/E55-delivery-architecture-design.html
                // Python parity: dynamic timeout max(2s, bytes/25000), poll 50ms.
                // Cursor Ink：文本与 Enter 必须分开发且间隔 ≥1s，否则第一次
                // Enter 被吞。token 可见性轮询的耗时算进这 1s；测试隔离下地板为 0。
                // ═══════════════════════════════════════════════════════════
                let inject_start = std::time::Instant::now();
                let pasted_at = inject_start;
                let submit_argv = tmux_send_submit_argv(&pane, submit);

                // Phase 1: token visibility poll — wait for the pasted text to
                // become visible in the pane before submitting. Dynamic timeout
                // based on payload size (large codex pastes can take seconds to
                // render the bracketed-paste block).
                let token_poll_timeout_ms = {
                    let size_based = (text.len() as u64) / 25;
                    size_based.max(2000)
                };
                let poll_start = std::time::Instant::now();
                let mut token_ever_visible = false;
                let mut tracked_paste: Option<PasteLatch> = None;
                token_visible_for_report = if let Some(m) = payload_token_marker(payload) {
                    let mut visible = false;
                    while poll_start.elapsed().as_millis() < token_poll_timeout_ms as u128 {
                        match self.capture(target, CaptureRange::Tail(80)) {
                            Ok(cap) => {
                                tracked_paste = latch_paste(&cap.text, tracked_paste);
                                if cap.text.contains(m) {
                                    visible = true;
                                    token_ever_visible = true;
                                }
                                // 折叠占位符出现 = TUI 侧注入完成。token 出现在未折叠高块里不算完成。
                                if paste_ready_for_enter(&cap.text, m) {
                                    break;
                                }
                            }
                            Err(_) => break, // tmux unavailable, skip poll
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Some(visible)
                } else {
                    None
                };

                // Phase 2: submit_and_verify — unified Escape+Enter+poll loop.
                let use_escape =
                    bracketed && payload.text().is_some() && matches!(submit, Key::Enter);
                let escape_argv = if use_escape {
                    Some(tmux_send_keys_argv(&pane, &[Key::Escape]))
                } else {
                    None
                };
                // Non-Enter submit keys (Key::Down for codex menu, etc.) skip
                // the entire submit_and_verify loop — single send, no consumption
                // check, KeySentAfterVisibleToken verification.
                if !matches!(submit, Key::Enter) {
                    self.prepare_pane_for_submit(target, false);
                    self.run_inject_stage(&submit_argv, InjectStage::Submit)?;
                    notify_submit_observer(observer);
                    let total_elapsed_ms = inject_start.elapsed().as_millis() as u64;
                    return Ok(InjectReport {
                        stage_reached: InjectStage::Submit,
                        inject_verification: inject_verification_after_readback(
                            payload,
                            token_visible_for_report,
                        ),
                        submit_verification: submit_verification_for_key(submit),
                        turn_verification: TurnVerification::NotYetObserved,
                        attempts: 1,
                        submit_diagnostics: Some(crate::transport::SubmitDiagnostics {
                            appear_gate_elapsed_ms: 0,
                            appear_gate_matched: false,
                            total_elapsed_ms,
                            attempts_detail: Vec::new(),
                        }),
                    });
                }

                sleep_remaining_paste_to_submit_floor(pasted_at, current_paste_to_submit_floor());

                let marker = payload_token_marker(payload);
                let max_submit_attempts: u32 = 3;
                let mut consumption_attempts: u32 = 0;
                let mut capture_retries: u32 = 0;
                let mut consumed: Option<bool> = None;
                let mut attempts_detail: Vec<SubmitAttemptObservation> = Vec::new();
                let mut any_attempt_matched = false;

                let poll_consumption = !payload.skip_consumption_poll();
                if !poll_consumption {
                    self.prepare_pane_for_submit(target, true);
                    if self
                        .run_inject_stage(&submit_argv, InjectStage::Submit)
                        .is_ok()
                    {
                        notify_submit_observer(observer);
                        consumption_attempts = 1;
                        // 跳过消费门（leader skip）不是「没能判断」；保持既有 EnterSent。
                        consumed = Some(true);
                    }
                }
                let submit_attempt_limit = if poll_consumption {
                    max_submit_attempts
                } else {
                    0
                };

                for attempt in 0..submit_attempt_limit {
                    let attempt_index = attempt + 1;
                    let attempt_start = std::time::Instant::now();
                    // Before resending (attempt > 0), re-check if the token
                    // already disappeared — guards against double-submit (C3).
                    // Capture failures are non-fatal (tmux may not be running
                    // in MCP sim / test env).
                    if attempt > 0 {
                        if let Some(m) = marker {
                            if let Ok(cap) = self.capture(target, CaptureRange::Tail(40)) {
                                if cap.text.contains(m) {
                                    token_ever_visible = true;
                                }
                                tracked_paste = latch_paste(&cap.text, tracked_paste);
                                let obs = submit_attempt_observation(
                                    attempt_index,
                                    &cap,
                                    marker,
                                    attempt_start.elapsed().as_millis() as u64,
                                );
                                if obs.matched {
                                    any_attempt_matched = true;
                                }
                                attempts_detail.push(obs);
                                // NeverSeen 不得把 token 缺席写成 consumed；
                                // 见过再消失，或本次占位符身份离开 composer，才停手。
                                if consumption_from_capture(
                                    &cap.text,
                                    m,
                                    token_ever_visible,
                                    tracked_paste,
                                ) == Some(true)
                                {
                                    consumed = Some(true);
                                    break;
                                }
                                if cursor_single_enter_enabled() {
                                    if provider_busy_signal_in_tail(&cap.text) {
                                        consumed = Some(true);
                                        break;
                                    }
                                    if !should_resubmit_enter_cursor(&cap.text, m) {
                                        consumed = Some(false);
                                        break;
                                    }
                                } else if !should_resubmit_enter(&cap.text, m, tracked_paste) {
                                    consumed = Some(false);
                                    break;
                                }
                            }
                        }
                    }

                    // 0.3.28-final (E55 false-positive truth source):
                    // Escape retry is DELETED. The researcher established
                    // Escape on Claude TUI with [Pasted content] visible
                    // may CLEAR the composer content rather than exit paste
                    // mode — sending Escape+Enter on retry would submit an
                    // empty message and hide the genuine consumption failure
                    // under a fake-success path. Python parity: ONLY ever
                    // send Enter, never Escape.
                    let _ = escape_argv;

                    // 发前归零接收态（tmux mode + 未闭合 bracketed paste）。
                    // 所有 Enter 路径的收敛点：prepare_pane_for_submit。
                    // mode 不是忙闲；cancel 失败不得让注入失败。E55: 不送 Escape/C-c。
                    self.prepare_pane_for_submit(target, true);

                    // Enter — send-keys failure is degraded (tmux may not have
                    // the pane in sim/test env). Break to consumed=None → Unverified.
                    if self
                        .run_inject_stage(&submit_argv, InjectStage::Submit)
                        .is_err()
                    {
                        consumed = None;
                        break;
                    }
                    notify_submit_observer(observer);
                    consumption_attempts = attempt + 1;

                    // Post-submit token readback (U1 #7 parity: check token
                    // visible after Enter for no-echo panes).
                    if attempt == 0 && matches!(token_visible_for_report, Some(false)) {
                        token_visible_for_report =
                            post_submit_token_visible(self, target, payload).unwrap_or(Some(false));
                        if token_visible_for_report == Some(true) {
                            token_ever_visible = true;
                        }
                    }

                    // Poll: token 三态 + 本次 #N 占位符。
                    // Capture failures → 只重读 capture，绝不重粘、不加 Enter；读不到则 None。
                    if let Some(m) = marker {
                        let mut found_consumed = false;
                        let mut saw_capture = false;
                        let mut consecutive_capture_failures: u32 = 0;
                        for _ in 0..12 {
                            std::thread::sleep(Duration::from_millis(100));
                            match self.capture(target, CaptureRange::Tail(40)) {
                                Ok(cap) => {
                                    consecutive_capture_failures = 0;
                                    saw_capture = true;
                                    if cap.text.contains(m) {
                                        token_ever_visible = true;
                                    }
                                    tracked_paste = latch_paste(&cap.text, tracked_paste);
                                    let obs = submit_attempt_observation(
                                        attempt_index,
                                        &cap,
                                        marker,
                                        attempt_start.elapsed().as_millis() as u64,
                                    );
                                    if obs.matched {
                                        any_attempt_matched = true;
                                    }
                                    attempts_detail.push(obs);
                                    if consumption_from_capture(
                                        &cap.text,
                                        m,
                                        token_ever_visible,
                                        tracked_paste,
                                    ) == Some(true)
                                    {
                                        found_consumed = true;
                                        break;
                                    }
                                    if cursor_single_enter_enabled()
                                        && provider_busy_signal_in_tail(&cap.text)
                                    {
                                        found_consumed = true;
                                        break;
                                    }
                                }
                                Err(_) => {
                                    capture_retries = capture_retries.saturating_add(1);
                                    consecutive_capture_failures =
                                        consecutive_capture_failures.saturating_add(1);
                                    if consecutive_capture_failures >= POST_SUBMIT_CAPTURE_RETRY {
                                        break;
                                    }
                                    std::thread::sleep(Duration::from_millis(
                                        POST_SUBMIT_CAPTURE_RETRY_MS,
                                    ));
                                }
                            }
                        }
                        if !saw_capture {
                            consumed = None;
                            break;
                        }
                        consumed = Some(found_consumed);
                        if found_consumed {
                            break;
                        }
                    } else {
                        // Non-token payload: single Enter, no consumption check
                        // (0.3.27). Not 「没能判断」— 本门不覆盖无 token 路径。
                        consumed = Some(true);
                        break;
                    }
                }

                // 0.5.43 debt-sweep (§6.2): three unconditional
                // `eprintln!` submit-consumption debug lines removed.
                // The decision they narrated is already captured in
                // `InjectReport.submit_diagnostics` /
                // `submit_verification`; the prints only spammed
                // coordinator.log without additive signal. Behavior
                // is byte-identical.
                // consumed=false: token still in composer. Paste landing
                // (`any_attempt_matched`) is A, not submit. Only a Working
                // signal counts as consumption. Else say unverified.
                let _ = any_attempt_matched;
                let mut last_turn_text = attempts_detail
                    .last()
                    .map(|obs| obs.pane_tail_excerpt.clone());
                let mut submit_verification = match consumed {
                    Some(true) => SubmitVerification::EnterSentWithoutPlaceholderCheck,
                    Some(false) => match self.capture(target, CaptureRange::Tail(15)) {
                        Ok(cap) => {
                            last_turn_text = Some(cap.text.clone());
                            attempts_detail.push(submit_attempt_observation(
                                consumption_attempts.max(1),
                                &cap,
                                marker,
                                inject_start.elapsed().as_millis() as u64,
                            ));
                            if provider_busy_signal_in_tail(&cap.text) {
                                SubmitVerification::EnterSentWithoutPlaceholderCheck
                            } else {
                                SubmitVerification::SubmitConsumptionUnverified
                            }
                        }
                        Err(_) => SubmitVerification::SubmitConsumptionUnverified,
                    },
                    None => SubmitVerification::SubmitConsumptionUnverified,
                };
                // Unverified + 折行缺口：只补一颗 C-m。A 的 should_resubmit_enter
                // 已为真（latch/底15 token，含打满 cap）时 B 不介入。身份拿不到
                // （None）不补——重复不可逆，滞留可人工救。cursor 单回车不走这里。
                // 终止：已消费 / 身份消失 / A 已覆盖 / 达上限 / busy。
                if matches!(
                    submit_verification,
                    SubmitVerification::SubmitConsumptionUnverified
                ) && consumed == Some(false)
                    && !cursor_single_enter_enabled()
                {
                    if let Some(m) = marker {
                        let mut leftover_resends = UNVERIFIED_COMPOSER_RESEND_MAX;
                        while leftover_resends > 0 {
                            leftover_resends -= 1;
                            let Ok(cap) = self.capture(target, CaptureRange::Tail(15)) else {
                                break;
                            };
                            last_turn_text = Some(cap.text.clone());
                            if !should_resend_unverified_wrap_gap(&cap.text, m, tracked_paste) {
                                break;
                            }
                            self.prepare_pane_for_submit(target, true);
                            if self
                                .run_inject_stage(&submit_argv, InjectStage::Submit)
                                .is_err()
                            {
                                break;
                            }
                            notify_submit_observer(observer);
                            consumption_attempts = consumption_attempts.saturating_add(1);
                            let attempt_start = std::time::Instant::now();
                            let mut found_consumed = false;
                            for _ in 0..12 {
                                std::thread::sleep(Duration::from_millis(100));
                                match self.capture(target, CaptureRange::Tail(40)) {
                                    Ok(cap) => {
                                        if cap.text.contains(m) {
                                            token_ever_visible = true;
                                        }
                                        tracked_paste = latch_paste(&cap.text, tracked_paste);
                                        let obs = submit_attempt_observation(
                                            consumption_attempts.max(1),
                                            &cap,
                                            marker,
                                            attempt_start.elapsed().as_millis() as u64,
                                        );
                                        attempts_detail.push(obs);
                                        last_turn_text = Some(cap.text.clone());
                                        if consumption_from_capture(
                                            &cap.text,
                                            m,
                                            token_ever_visible,
                                            tracked_paste,
                                        ) == Some(true)
                                            || provider_busy_signal_in_tail(&cap.text)
                                        {
                                            // Gone+无 latch 会把折行 token 写成已消费；身份仍在则不算。
                                            if !this_paste_identity_in_composer(
                                                &cap.text,
                                                m,
                                                tracked_paste,
                                            ) {
                                                found_consumed = true;
                                                break;
                                            }
                                        }
                                        if !this_paste_identity_in_composer(
                                            &cap.text,
                                            m,
                                            tracked_paste,
                                        ) {
                                            found_consumed = true;
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            if found_consumed {
                                submit_verification =
                                    SubmitVerification::EnterSentWithoutPlaceholderCheck;
                                break;
                            }
                        }
                    }
                }
                let total_elapsed_ms = inject_start.elapsed().as_millis() as u64;
                return Ok(InjectReport {
                    stage_reached: InjectStage::Submit,
                    inject_verification: inject_verification_after_readback(
                        payload,
                        token_visible_for_report,
                    ),
                    submit_verification,
                    turn_verification: turn_verification_for_payload(
                        payload,
                        last_turn_text.as_deref(),
                    ),
                    attempts: consumption_attempts.saturating_add(capture_retries),
                    submit_diagnostics: Some(crate::transport::SubmitDiagnostics {
                        appear_gate_elapsed_ms: 0,
                        appear_gate_matched: false,
                        total_elapsed_ms,
                        attempts_detail,
                    }),
                });
            }
        }
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: inject_verification_after_readback(
                payload,
                token_visible_for_report,
            ),
            submit_verification: submit_verification_for_key(submit),
            turn_verification: turn_verification_for_payload(payload, None),
            attempts: 1,
            // E50 PR-1: Empty payload / non-Text fallthrough path — no submit
            // diagnostics applicable.
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, target: &Target, keys: &[Key]) -> Result<(), TransportError> {
        let pane = pane_from_target(target);
        let _pane_input = crate::lifecycle::pane_input_lock::acquire_or_proceed(
            crate::lifecycle::pane_input_lock::PaneInputLockRequest {
                workspace: self.event_workspace.as_deref(),
                target_key: pane.as_str(),
                operation: "send_keys",
            },
        );
        if keys.contains(&Key::CancelMode) {
            if let Some(mode) = pane_mode_from_raw(self.query(target, PaneField::PaneMode)?) {
                let argv = crate::transport::tmux_cancel_mode_argv(&pane, mode);
                return self.run_ok(&argv);
            }
            return Ok(());
        }
        if keys.iter().any(|k| matches!(k, Key::Enter)) {
            self.prepare_pane_for_submit(target, true);
        }
        let argv = tmux_send_keys_argv(&pane, keys);
        self.run_ok(&argv)
    }

    fn capture(
        &self,
        target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let pane = pane_from_target(target);
        let argv = self.tmux_argv(&tmux_capture_argv(&pane, range));
        let output = self
            .runner
            .run(&argv)
            .map_err(|source| TransportError::Capture { source })?;
        if !output.success {
            return Err(subprocess_error(argv, output));
        }
        Ok(CapturedText {
            text: normalize_capture(&output.stdout),
            range,
        })
    }

    fn query(&self, target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        let pane = pane_from_target(target);
        let argv = self.tmux_argv(&tmux_query_argv(&pane, field));
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(None);
        }
        Ok(Some(output.stdout.trim().to_string()))
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "#{pane_id}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if output.success {
            return Ok(PaneLiveness::Live);
        }
        if output
            .stderr
            .to_ascii_lowercase()
            .contains("can't find pane")
        {
            Ok(PaneLiveness::Dead)
        } else {
            Ok(PaneLiveness::Unknown)
        }
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "#{pane_id}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if output.success {
            let pane_id = output.stdout.trim();
            if pane_id.is_empty() {
                return Ok(Some(false));
            }
            if pane_id == pane.as_str()
                && pane_id.starts_with('%')
                && pane_id[1..].chars().all(|ch| ch.is_ascii_digit())
            {
                return Ok(Some(true));
            }
            return Ok(None);
        }
        let stderr = output.stderr.to_ascii_lowercase();
        if stderr.contains("can't find pane")
            || stderr.contains("no such pane")
            || (stderr.contains("can't find") && stderr.contains("pane"))
        {
            Ok(Some(false))
        } else {
            Ok(None)
        }
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        // P5 (C-P5-3): `#{pane_pid}` rides the single list-panes call (field index 11),
        // killing the per-pane display-message N+1 fallback.
        // Use a printable sentinel so the 12-field frame stays explicit in argv/log evidence;
        // `parse_pane_info_line` retains compatibility with legacy tab-delimited output.
        const TMUX_PANE_FORMAT: &str = "#{pane_id}__TA_FIELD__#{session_name}__TA_FIELD__#{window_index}__TA_FIELD__#{window_name}__TA_FIELD__#{pane_index}__TA_FIELD__#{pane_tty}__TA_FIELD__#{pane_current_command}__TA_FIELD__#{pane_active}__TA_FIELD__#{pane_current_path}__TA_FIELD__#{session_attached}__TA_FIELD__#{pane_in_mode}__TA_FIELD__#{pane_pid}__TA_FIELD__#{@team_agent_pane_binding_nonce}";
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "list-panes".to_string(),
            "-a".to_string(),
            "-F".to_string(),
            TMUX_PANE_FORMAT.to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(Vec::new());
        }
        let mut panes = Vec::new();
        for line in output.stdout.lines().filter(|line| !line.is_empty()) {
            if let Some(mut pane) = parse_pane_info_line(line) {
                // 0.3.5 integration union: P5 (C-P5-3) makes `#{pane_pid}` ride the
                // single list-panes call — on real tmux the fallback below never fires.
                // swallow batch 2 ① keeps it as a RESILIENT degrade for panes whose pid
                // field came back empty (e.g. older tmux without #{pane_pid}): a single
                // pane's probe failure must not fail the WHOLE list — the pane degrades
                // to pane_pid=None and the failure is observable.
                if pane.pane_pid.is_none() {
                    match query_pane_pid(self, &pane.pane_id) {
                        Ok(pid) => pane.pane_pid = pid,
                        Err(error) => {
                            if let Some(workspace) = &self.event_workspace {
                                let _ = crate::event_log::EventLog::new(workspace).write(
                                    "tmux.pane_pid_query_failed",
                                    serde_json::json!({
                                        "pane_id": pane.pane_id.as_str(),
                                        "session": pane.session.as_str(),
                                        "error": error.to_string(),
                                    }),
                                );
                            }
                        }
                    }
                }
                panes.push(pane);
            }
        }
        Ok(panes)
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "has-session".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        Ok(output.success)
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        // golden runtime.py:1023-1029 `_tmux_window_exists`: `tmux list-windows -t <s> -F #{window_name}`;
        // returncode != 0 -> false (here: an empty window set), else the window names by line.
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "list-windows".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
            "-F".to_string(),
            "#{window_name}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(Vec::new());
        }
        Ok(output
            .stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(WindowName::new)
            .collect())
    }

    fn configure_adaptive_pane_title(
        &self,
        session: &SessionName,
        window: &WindowName,
        pane: &PaneId,
        title: &str,
    ) -> Result<(), TransportError> {
        let target = format!("{}:{}", session.as_str(), window.as_str());
        self.run_ok(&[
            "tmux".to_string(),
            "set-window-option".to_string(),
            "-t".to_string(),
            target.clone(),
            "pane-border-status".to_string(),
            "bottom".to_string(),
        ])?;
        self.run_ok(&[
            "tmux".to_string(),
            "set-window-option".to_string(),
            "-t".to_string(),
            target,
            "pane-border-format".to_string(),
            " #{pane_title} ".to_string(),
        ])?;
        self.run_ok(&[
            "tmux".to_string(),
            "select-pane".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "-T".to_string(),
            title.to_string(),
        ])
    }

    fn set_session_env(
        &self,
        session: &SessionName,
        key: &str,
        value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        let argv = vec![
            "tmux".to_string(),
            "set-environment".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
            key.to_string(),
            value.to_string(),
        ];
        self.run_ok(&argv)?;
        Ok(SetEnvOutcome::Applied)
    }

    fn set_window_option(
        &self,
        session: &SessionName,
        window: &WindowName,
        option: &str,
        value: &str,
    ) -> Result<(), TransportError> {
        let target = format!("{}:{}", session.as_str(), window.as_str());
        let argv = vec![
            "tmux".to_string(),
            "set-window-option".to_string(),
            "-t".to_string(),
            target,
            option.to_string(),
            value.to_string(),
        ];
        self.run_ok(&argv)
    }

    fn kill_server(&self) -> Result<(), TransportError> {
        TmuxBackend::kill_server(self);
        Ok(())
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "kill-session".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
        ]);
        self.run_ok(&argv)
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "kill-window".to_string(),
            "-t".to_string(),
            target_name(target),
        ]);
        self.run_ok(&argv)
    }

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "kill-pane".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
        ]);
        self.run_ok(&argv)
    }

    fn attach_session(&self, session: &SessionName) -> Result<AttachOutcome, TransportError> {
        let argv = [
            "tmux".to_string(),
            "attach-session".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
        ];
        self.run_ok(&argv)?;
        Ok(AttachOutcome::Attached)
    }
}

/// swallow batch 2 ① fallback probe (only fires when `#{pane_pid}` came back empty —
/// see the P5 union note in `list_targets`).
fn query_pane_pid(backend: &TmuxBackend, pane: &PaneId) -> Result<Option<u32>, TransportError> {
    let argv = backend.tmux_argv(&[
        "tmux".to_string(),
        "display-message".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        pane.as_str().to_string(),
        "#{pane_pid}".to_string(),
    ]);
    let output = backend.runner.run(&argv)?;
    if !output.success {
        return Ok(None);
    }
    Ok(parse_optional_u32(output.stdout.trim()))
}

fn parse_pane_info_line(line: &str) -> Option<PaneInfo> {
    let fields = if line.contains("__TA_FIELD__") {
        line.split("__TA_FIELD__").collect::<Vec<_>>()
    } else {
        line.split('\t').collect::<Vec<_>>()
    };
    if fields.len() < 11 {
        return None;
    }
    let mut leader_env = BTreeMap::new();
    if let Some(nonce) = fields.get(12).and_then(|raw| non_empty(raw)) {
        leader_env.insert(
            PANE_BINDING_NONCE_METADATA_KEY.to_string(),
            nonce.to_string(),
        );
    }
    Some(PaneInfo {
        pane_id: PaneId::new(fields[0]),
        session: SessionName::new(fields[1]),
        window_index: parse_optional_u32(fields[2]),
        window_name: non_empty(fields[3]).map(WindowName::new),
        pane_index: parse_optional_u32(fields[4]),
        tty: non_empty(fields[5]).map(str::to_string),
        current_command: non_empty(fields[6]).map(str::to_string),
        active: fields[7] == "1",
        current_path: non_empty(fields[8]).map(PathBuf::from),
        pane_pid: fields.get(11).and_then(|raw| parse_optional_u32(raw)),
        leader_env,
    })
}

fn parse_optional_u32(raw: &str) -> Option<u32> {
    if raw.is_empty() {
        return None;
    }
    raw.parse::<u32>().ok()
}

fn non_empty(raw: &str) -> Option<&str> {
    if raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

// 0.5.x Windows portability Batch 5: the `tmux_backend/tests.rs`
// module uses `std::os::unix::net::UnixListener` for its mock
// runner + verifies Unix-specific socket-root derivation. Since the
// tmux backend itself only functions on Unix (design § Route B —
// tmux is a Unix concept), the test module stays Unix-only.
#[cfg(all(test, unix))]
mod tests;
