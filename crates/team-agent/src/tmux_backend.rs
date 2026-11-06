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
//!       what: 统一 prepare_pane_for_submit：Enter 前 cancel 一切非 0 pane_mode（按真实 mode 分派）；⛔ 不发 ESC 也不发其替代品（含字面 CSI 201~，用户裁定 2026-08-23）；A1 Empty / A3 skip-poll / A4 Phase2 / A6 send_keys(含 Enter) 都走这里
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
use crate::provider::wire::{parse_provider, provider_wire};
use crate::transport::{
    normalize_capture, tmux_capture_argv, tmux_query_argv, tmux_send_keys_argv, tmux_spawn_argv, BackendKind, CaptureRange, CapturedText, InjectStage, Key, PaneField, PaneId, PaneInfo, PaneMode, SessionName, SessionOwner, SpawnResult, Target, Transport, TransportError, WindowName,
};

mod native_input;

pub const PANE_BINDING_NONCE_METADATA_KEY: &str = "TEAM_AGENT_PANE_BINDING_NONCE";
pub(crate) const KEEPALIVE_WINDOW_NAME: &str = "[team-agent keepalive]";
const TMUX_PANE_BINDING_NONCE_OPTION: &str = "@team_agent_pane_binding_nonce";
const TMUX_SESSION_OWNER_WORKSPACE_OPTION: &str = "@team_agent_owner_workspace";
const TMUX_SESSION_OWNER_TEAM_OPTION: &str = "@team_agent_owner_team";
const TMUX_SESSION_OWNER_GENERATION_OPTION: &str = "@team_agent_owner_generation";

/// Shared existing pane nonce generator; transport owns the physical nonce.
/// Same material/algorithm as the original Leader claim producer, no new dependency.
pub(crate) fn next_pane_binding_nonce(workspace: &Path, endpoint: &str, pane: &PaneId) -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let material = format!(
        "{}\0{endpoint}\0{}\0{}\0{nanos}\0{counter}",
        canonical.display(),
        pane.as_str(),
        std::process::id(),
    );
    let digest = Sha256::digest(material.as_bytes());
    digest[..16].iter().map(|byte| format!("{byte:02x}")).collect()
}

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
        self.run_inner(argv, None, COMMAND_TIMEOUT)
    }

}

impl RealCommandRunner {
    /// Callers with an end-to-end deadline may shorten, never extend, the native budget.
    pub(crate) fn run_with_budget(&self, argv: &[String], budget: Duration) -> Result<CommandOutput, std::io::Error> {
        self.run_inner(argv, None, budget.min(COMMAND_TIMEOUT))
    }

    fn run_inner(
        &self,
        argv: &[String],
        stdin_text: Option<&str>,
        budget: Duration,
    ) -> Result<CommandOutput, std::io::Error> {
        let Some(program) = argv.first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty argv",
            ));
        };
        let mut command = std::process::Command::new(program);
        #[cfg(unix)] {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.args(argv.iter().skip(1))
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
        let stdout = pipe_reader(stdout);
        let stderr = pipe_reader(stderr);
        let deadline = Instant::now() + budget;
        let mut stdout_text = None;
        let mut stderr_text = None;
        // Observe short-lived commands promptly; retain the old cadence for long ones.
        let mut poll_interval = Duration::from_millis(1);
        let status = loop {
            if stdout_text.is_none() { stdout_text = poll_pipe(&stdout); }
            if stderr_text.is_none() { stderr_text = poll_pipe(&stderr); }
            // Do not reap the group leader while descendants still hold pipes:
            // keeping its PID reserved makes deadline cancellation ownership-safe.
            if stdout_text.is_some() && stderr_text.is_some() {
                if let Some(status) = child.try_wait()? { break status; }
            }
            let now = Instant::now();
            if now >= deadline {
                // The unreaped child owns a fresh process group. Cancel its
                // wrapper descendants too; inherited pipes must not defeat the deadline.
                #[cfg(unix)] unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
                #[cfg(not(unix))] child.kill()?;
                child.wait()?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{program} exceeded {budget:?} timeout"),
                ));
            }
            std::thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
            poll_interval = (poll_interval * 2).min(Duration::from_millis(25));
        };
        let stdout = stdout_text.ok_or_else(|| std::io::Error::other("stdout result missing"))??;
        let stderr = stderr_text.ok_or_else(|| std::io::Error::other("stderr result missing"))??;
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

fn pipe_reader<R: Read + Send + 'static>(reader: R) -> std::sync::mpsc::Receiver<std::io::Result<String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = sender.send(read_pipe(reader)); });
    receiver
}

fn poll_pipe(receiver: &std::sync::mpsc::Receiver<std::io::Result<String>>) -> Option<std::io::Result<String>> {
    match receiver.try_recv() {
        Ok(result) => Some(result),
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(std::io::Error::other("native pipe reader disconnected"))),
    }
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

    fn set_session_owner_marker(
        &self,
        session: &SessionName,
        workspace: &Path,
        team: &str,
        generation: &str,
    ) -> Result<(), TransportError> {
        let workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf())
            .to_string_lossy()
            .into_owned();
        for (option, value) in [
            (TMUX_SESSION_OWNER_WORKSPACE_OPTION, workspace),
            (TMUX_SESSION_OWNER_TEAM_OPTION, team.to_string()),
            (TMUX_SESSION_OWNER_GENERATION_OPTION, generation.to_string()),
        ] {
            let argv = self.tmux_argv(&[
                "tmux".to_string(),
                "set-option".to_string(),
                "-t".to_string(),
                session.as_str().to_string(),
                option.to_string(),
                value,
            ]);
            let output = self.runner.run(&argv)?;
            if !output.success {
                return Err(subprocess_error(argv, output));
            }
        }
        Ok(())
    }

    fn read_session_owner_option(
        &self,
        session: &SessionName,
        option: &str,
    ) -> Result<Option<String>, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "show-options".to_string(),
            "-qv".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
            option.to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(None);
        }
        let value = output.stdout.trim();
        Ok((!value.is_empty()).then(|| value.to_string()))
    }

    fn read_session_owner_marker(
        &self,
        session: &SessionName,
    ) -> Result<Option<SessionOwner>, TransportError> {
        let Some(workspace) = self.read_session_owner_option(
            session,
            TMUX_SESSION_OWNER_WORKSPACE_OPTION,
        )? else {
            return Ok(None);
        };
        let Some(team) = self.read_session_owner_option(session, TMUX_SESSION_OWNER_TEAM_OPTION)?
        else {
            return Ok(None);
        };
        let Some(generation) = self.read_session_owner_option(
            session,
            TMUX_SESSION_OWNER_GENERATION_OPTION,
        )? else {
            return Ok(None);
        };
        Ok(Some(SessionOwner {
            workspace,
            team,
            generation,
        }))
    }

    /// Set the pane-instance nonce only when the pane option is unset. tmux
    /// serializes commands at the server, so concurrent first claims have one
    /// winner; losers read back that winner instead of persisting a stale nonce.
    pub(crate) fn set_pane_binding_nonce_if_unset(
        &self,
        pane: &PaneId,
        nonce: &str,
    ) -> Result<String, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "set-option".to_string(),
            "-p".to_string(),
            "-o".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            TMUX_PANE_BINDING_NONCE_OPTION.to_string(),
            nonce.to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if output.success {
            return Ok(nonce.to_string());
        }
        if output
            .stderr
            .to_ascii_lowercase()
            .contains("already set")
        {
            return self.read_pane_binding_nonce(pane);
        }
        Err(subprocess_error(argv, output))
    }

    fn read_pane_binding_nonce(&self, pane: &PaneId) -> Result<String, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "show-options".to_string(),
            "-p".to_string(),
            "-v".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            TMUX_PANE_BINDING_NONCE_OPTION.to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Err(subprocess_error(argv, output));
        }
        let nonce = output.stdout.trim();
        if nonce.is_empty() {
            return Err(TransportError::Subprocess {
                argv,
                code: output.code,
                stderr: "pane binding nonce remained unset after conditional write".to_string(),
            });
        }
        Ok(nonce.to_string())
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
    let (mut backend, source) =
        if let Some((endpoint, source)) = runtime_tmux_endpoint_from_state(state) {
            (TmuxBackend::for_tmux_endpoint(endpoint), source)
        } else {
            (
                TmuxBackend::for_workspace(workspace),
                RuntimeTmuxEndpointSource::WorkspaceFallback,
            )
        };
    // A persisted endpoint changes addressing, not the owning input-lock scope.
    backend.event_workspace = Some(workspace.to_path_buf());
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
    let (mut backend, source) =
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
    backend.event_workspace = Some(workspace.to_path_buf());
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
/// returns: Unix 上按 TMUX_TMPDIR（未设置时 /tmp）解析短名；Windows 上 None
/// cfg: unix 分支读 geteuid;not(unix) 分支恒 None
/// boundary: 不创建目录、不创建 socket;返回 Some 不代表文件存在
/// ---
pub(crate) fn socket_path_for_name(socket_name: &str) -> Option<PathBuf> {
    if socket_name.is_empty() || socket_name == "default" || Path::new(socket_name).is_absolute() {
        return None;
    }
    // A native `-L` label belongs to one configured root, not whichever
    // same-named socket happens to exist in the discovery roots.
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let root = std::env::var_os("TMUX_TMPDIR").filter(|value| !value.is_empty())
            .map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let root = root.canonicalize().unwrap_or(root);
        let candidate = root.join(format!("tmux-{uid}")).join(socket_name);
        Some(candidate.canonicalize().unwrap_or(candidate))
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
    socket_path_for_name(socket_name).filter(|candidate| candidate.exists())
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
/// returns: Unix 上为默认根、TMUX_TMPDIR 与旧 TMPDIR 根(排序去重);Windows 上为空
/// cfg: unix 读 geteuid 与环境；这些根仅用于发现，不能互相替代原生 -L 解析
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
        for variable in ["TMUX_TMPDIR", "TMPDIR"] {
            if let Some(tmpdir) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
                roots.push(PathBuf::from(tmpdir).join(format!("tmux-{uid}")));
            }
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

    fn pane_mode(&self, target: &Target) -> Option<PaneMode> {
        let raw = self.query(target, PaneField::PaneMode).ok().flatten()?;
        pane_mode_from_raw(Some(raw))
    }

    /// 发 Enter 前归零接收态。A1/A3/A4/A6 唯一入口。
    ///
    /// 1. 非 0 pane_mode → `tmux_cancel_mode_argv`（按真实 mode 分派，不是硬编码 Copy）
    /// cancel 失败不 fail 提交。E55：不送 Escape/C-c。
    ///
    /// 用户裁定 2026-08-23：**不发 ESC，也不发 ESC 的替代品**（含字面 CSI 201~）。
    /// 发 ESC/CSI 的唯一好处是让消息当场直接上屏；本项目只要求「下一次工具调用时能上屏」
    /// ⇒ 不存在需要它才能满足的场景，发它没有收益，只有污染 composer 输入的风险。
    fn prepare_pane_for_submit(&self, target: &Target, close_bracketed_paste: bool) {
        let _ = close_bracketed_paste;
        let pane = pane_from_target(target);
        if let Some(mode) = self.pane_mode(target) {
            let argv = crate::transport::tmux_cancel_mode_argv(&pane, mode);
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


thread_local! {
    static PASTE_TO_SUBMIT_FLOOR: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static CURSOR_SINGLE_ENTER: Cell<bool> = const { Cell::new(false) };
}











fn shell_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let caller_provider = caller_provider_for_invocation(env);
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
        if unset_set.contains(key.as_str())
            || key.starts_with(crate::layout::worker_env::CALLER_CONTEXT_PREFIX)
        {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    append_caller_context_assignments(&mut parts, caller_provider);
    parts.push("exec".to_string());
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn caller_provider_for_invocation(env: &BTreeMap<String, String>) -> Option<&'static str> {
    env.get(crate::layout::worker_env::CALLER_PROVIDER_ENV)
        .and_then(|value| parse_provider(value))
        .map(provider_wire)
}

fn append_caller_context_assignments(parts: &mut Vec<String>, provider: Option<&str>) {
    let Some(provider) = provider else {
        return;
    };
    parts.push(format!(
        "{}={}",
        crate::layout::worker_env::CALLER_PROVIDER_ENV,
        shell_quote(provider)
    ));
    // These are expanded by the shell created inside the target pane. They are
    // assignments on the provider command only, so the exit marker/inert tail
    // cannot retain a usable caller context.
    parts.push(format!(
        "{}=\"${{TMUX_PANE-}}\"",
        crate::layout::worker_env::CALLER_PANE_ENV
    ));
    parts.push(format!(
        "{}=\"${{TMUX%%,*}}\"",
        crate::layout::worker_env::CALLER_ENDPOINT_ENV
    ));
}

/// Public exit-marker text emitted by `leader_shell_wrapper_command`. Format:
/// `"[team-agent] {provider_label} exited with {rc}"`.
pub const LEADER_PROVIDER_EXIT_MARKER_PREFIX: &str = "[team-agent]";
pub const LEADER_PROVIDER_EXIT_MARKER_SUFFIX: &str = "exited with";
pub(crate) const PROVIDER_EXIT_STATUS_OPTION: &str = "@team-agent-provider-exit-status";

/// 0.4.x (CR R6): build the leader exit marker text for `provider_label`.
/// Used by the shell wrapper for human-readable output.
/// ---
/// purpose: 单一来源地拼出 leader pane 的 provider 退出标记文本
/// params:
///   provider_label: 人类可读的 provider 名,原样嵌入标记
/// returns: 前缀 + provider 名 + 后缀 拼成的标记(不含退出码,退出码由 shell 的 printf 补)
/// boundary:
///   - shell wrapper 统一使用此显示标记,进程状态另由监督 shell 收据证明
///   - 仅供显示;历史正文可包含相同文本,不得据此判定进程退出
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
    let caller_provider = caller_provider_for_invocation(env);
    let mut parts = vec![format!(
        "{{ tmux -S \"${{TMUX%%,*}}\" set-option -p -t \"$TMUX_PANE\" {PROVIDER_EXIT_STATUS_OPTION} running >/dev/null 2>&1 || :; }} &&"
    )];
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    for (key, value) in env {
        if unset_set.contains(key.as_str())
            || key.starts_with(crate::layout::worker_env::CALLER_CONTEXT_PREFIX)
        {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    append_caller_context_assignments(&mut parts, caller_provider);
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.push(";".to_string());
    parts.push("rc=$?;".to_string());
    parts.push(format!(
        "tmux -S \"${{TMUX%%,*}}\" set-option -p -t \"$TMUX_PANE\" {PROVIDER_EXIT_STATUS_OPTION} \"$rc\" >/dev/null 2>&1;"
    ));
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
    // Clear any prior receipt before invoking the provider. Terminal content,
    // including replayed sessions, must never be treated as an exit receipt.
    parts.push(format!(
        "{{ tmux -S \"${{TMUX%%,*}}\" set-option -p -t \"$TMUX_PANE\" {PROVIDER_EXIT_STATUS_OPTION} running >/dev/null 2>&1 || :; }} &&"
    ));
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
    parts.push(format!(
        "tmux -S \"${{TMUX%%,*}}\" set-option -p -t \"$TMUX_PANE\" {PROVIDER_EXIT_STATUS_OPTION} \"$rc\" >/dev/null 2>&1;"
    ));
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
    // `sh` is deliberate: exited-wrapper input classifiers recognize shell
    // basenames. `sh -c` does not read commands from
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

    fn set_session_owner(
        &self,
        session: &SessionName,
        workspace: &Path,
        team: &str,
    ) -> Result<(), TransportError> {
        self.set_session_owner_marker(session, workspace, team, session.as_str())
    }

    fn set_session_owner_with_generation(
        &self,
        session: &SessionName,
        workspace: &Path,
        team: &str,
        generation: &str,
    ) -> Result<(), TransportError> {
        self.set_session_owner_marker(session, workspace, team, generation)
    }

    fn session_owner(
        &self,
        session: &SessionName,
    ) -> Result<Option<SessionOwner>, TransportError> {
        self.read_session_owner_marker(session)
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



    fn observe_native_input(
        &self,
        target: &Target,
        provider: crate::model::enums::Provider,
    ) -> Result<Option<crate::transport::native_input::InputFrame>, TransportError> {
        self.observe_current_native_input(target, provider)
    }




    fn send_keys(&self, target: &Target, keys: &[Key]) -> Result<(), TransportError> {
        let pane = pane_from_target(target);
        let _pane_input = crate::lifecycle::pane_input_lock::acquire_for_write(
            crate::lifecycle::pane_input_lock::PaneInputLockRequest {
                workspace: self.event_workspace.as_deref(),
                target_key: pane.as_str(),
                operation: "send_keys",
            },
        ).map_err(|_| TransportError::NativeInputNotReady { reason: "required_guard_active".into() })?;
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

    fn provider_exit_status(&self, pane: &PaneId) -> Result<Option<u8>, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            format!("#{{{PROVIDER_EXIT_STATUS_OPTION}}}"),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(None);
        }
        Ok(output.stdout.trim().parse::<u8>().ok())
    }

    fn pane_is_dead(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "#{pane_dead}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(None);
        }
        Ok(match output.stdout.trim() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        })
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

    fn preserve_session_before_stop(
        &self,
        session: &SessionName,
        targets: &[Target],
    ) -> Result<(), TransportError> {
        let list_argv = self.tmux_argv(&[
            "tmux".to_string(),
            "list-panes".to_string(),
            "-s".to_string(),
            "-t".to_string(),
            format!("={}", session.as_str()),
            "-F".to_string(),
            "#{pane_id}\t#{window_name}".to_string(),
        ]);
        let output = self.runner.run(&list_argv)?;
        if !output.success {
            return Err(subprocess_error(list_argv, output));
        }
        for line in output.stdout.lines() {
            // Some tmux/runner combinations return the literal escape. Split
            // only the first column boundary; window names are not unescaped.
            let separator = line.find('\t').map(|at| (at, 1)).into_iter()
                .chain(line.find("\\t").map(|at| (at, 2)))
                .min_by_key(|(at, _)| *at);
            let Some((at, width)) = separator else {
                return Err(TransportError::Subprocess {
                    argv: list_argv,
                    code: output.code,
                    stderr: "tmux returned an invalid session pane listing".to_string(),
                });
            };
            let (pane, rest) = line.split_at(at);
            let window = &rest[width..];
            if !targets.iter().any(|target| match target {
                Target::Pane(id) => id.as_str() == pane,
                Target::SessionWindow {
                    session: owner,
                    window: name,
                } => owner == session && name.as_str() == window,
            }) {
                return Ok(());
            }
        }
        // An empty listing cannot prove that this is the last worker.
        if output.stdout.trim().is_empty() {
            return Err(TransportError::TargetNotFound {
                target: session.as_str().to_string(),
            });
        }
        // Use a separate, non-worker window: remain-on-exit does not protect a
        // pane explicitly deleted by kill-pane. Configure by the new pane ID,
        // not a window name that could resolve to another same-named window.
        let spawn_argv = tmux_spawn_argv(
            &SessionName::new(format!("={}", session.as_str())),
            &WindowName::new(KEEPALIVE_WINDOW_NAME),
            "exec sleep 86400",
            false,
        );
        let spawned = self.run_spawn(&spawn_argv)?;
        let pane = spawned.stdout.trim();
        if !pane.starts_with('%')
            || pane.len() == 1
            || !pane[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            return Err(TransportError::Subprocess {
                argv: self.tmux_argv(&spawn_argv),
                code: spawned.code,
                stderr: "tmux keepalive spawn returned no valid pane id".to_string(),
            });
        }
        let result = self
            .run_ok(&[
                "tmux".to_string(),
                "set-window-option".to_string(),
                "-t".to_string(),
                pane.to_string(),
                "remain-on-exit".to_string(),
                "on".to_string(),
            ])
            .and_then(|()| {
                self.run_ok(&[
                    "tmux".to_string(),
                    "respawn-pane".to_string(),
                    "-k".to_string(),
                    "-t".to_string(),
                    pane.to_string(),
                    "exit 0".to_string(),
                ])
            });
        if result.is_err() {
            // The worker has not been killed yet; remove only our new anchor.
            let _ = self.kill_pane(&PaneId::new(pane));
        }
        result
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

#[cfg(all(test, unix))]
mod command_runner_regressions {
    use super::{CommandRunner, RealCommandRunner, COMMAND_TIMEOUT};
    use std::io::ErrorKind;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn shell(command: &str) -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), command.into()]
    }

    #[test]
    fn nonzero_exit_preserves_both_outputs_and_lossy_utf8() {
        let output = RealCommandRunner
            .run(&shell("printf 'stdout\\377'; printf 'stderr\\376' >&2; exit 17"))
            .unwrap();
        assert!(!output.success);
        assert_eq!(output.code, Some(17));
        assert_eq!(output.stdout, "stdout\u{fffd}");
        assert_eq!(output.stderr, "stderr\u{fffd}");
    }


    #[test]
    fn signal_exit_retains_absent_exit_code() {
        let output = RealCommandRunner.run(&shell("kill -TERM $$")).unwrap();
        assert!(!output.success);
        assert_eq!(output.code, None);
    }

    #[test]
    fn drains_stdout_and_stderr_beyond_pipe_capacity() {
        let output = RealCommandRunner
            .run(&shell(
                "i=0; while [ \"$i\" -lt 20000 ]; do printf 'out\\n'; printf 'err\\n' >&2; i=$((i+1)); done",
            ))
            .unwrap();
        assert!(output.success);
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, "out\n".repeat(20000));
        assert_eq!(output.stderr, "err\n".repeat(20000));
    }

    #[test]
    fn timeout_keeps_five_second_budget_and_reaps_the_owned_child() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid_path = std::env::temp_dir().join(format!(
            "team-agent-runner-timeout-{}-{nonce}.pid",
            std::process::id()
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pid_path)
            .unwrap();
        drop(file);
        // exec keeps one owned PID and closes both pipes when the runner kills it.
        let mut argv = shell("printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 30");
        argv.extend([
            "runner-timeout-test".into(),
            pid_path.to_string_lossy().into_owned(),
        ]);
        let started = Instant::now();
        let error = RealCommandRunner.run(&argv).unwrap_err();
        let elapsed = started.elapsed();
        let pid: i32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
        std::fs::remove_file(pid_path).unwrap();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(elapsed >= COMMAND_TIMEOUT);
        assert!(elapsed < COMMAND_TIMEOUT + Duration::from_secs(2));
        assert!(pid > 1);
        // SAFETY: signal 0 only probes the exact child PID recorded by this fixture.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
