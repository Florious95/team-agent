//! daemon 主循环面(`__main__.py`)—— 退避序列 + tick 间隔解析 + 子进程入口。

use thiserror::Error;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::model::enums::Provider;
use crate::provider::ProviderAdapter;

use super::health::{coordinator_pid_path, write_coordinator_metadata};
use super::tick::{write_coordinator_heartbeat, TickError, TickReport, HEARTBEAT_STATUS_PANIC};
use super::types::{
    ErrorLists, MetadataSource, Pid, ProviderRegistry, WorkspacePath, BACKOFF_MAX_SEC,
    DEFAULT_TICK_INTERVAL_SEC,
};
use super::Coordinator;

// ===========================================================================
// daemon 主循环(__main__.py:25)—— 退避 + 孤儿自检
// ===========================================================================

/// daemon 主循环参数(`main` argv,`__main__.py:26-30`)。Rust 侧 `team-agent coordinator --workspace ..`。
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonArgs {
    pub workspace: WorkspacePath,
    /// `--once`:跑一 tick 即退(`__main__.py:28`)。
    pub once: bool,
    /// `--tick-interval`(`__main__.py:29`)。`None` → 读 spec `runtime.tick_interval_sec`。
    pub tick_interval_sec: Option<f64>,
    /// 0.5.x Windows portability Batch 9 F8 (leader msg_2a4cc1fa54c0):
    /// team_key passed via `--team` CLI so the coordinator daemon
    /// doesn't have to derive from state at boot time. That derivation
    /// (Batch 7 F5) got trapped by Batch 8's seed-state pattern
    /// (F8 root cause). Preserving the derivation as a fallback when
    /// `team_key` is None keeps Unix daemons and pre-existing test
    /// harnesses byte-preserving.
    pub team_key: Option<String>,
}

/// daemon 主循环(`main`,`__main__.py:25-98`)。写 pid/meta(source=boot)、装信号→STOP、孤儿自检、
/// catch-all + 指数退避 + tick_error 去重/抑制、tick_recovered 重置、`result.stop || once` → break。
/// §10:返 `Result`(顶层 bin 用 anyhow 收;§12 边界)。
pub fn run_daemon(args: DaemonArgs) -> Result<(), DaemonError> {
    // CP-1: the daemon's whole tick surface (has_session / capture / inject / list_windows / kill)
    // runs through this backend. Prefer the persisted runtime endpoint so attached explicit-socket
    // teams are checked on the same socket as lifecycle worker operations.
    //
    // 0.5.x Phase 1d Batch 3: coordinator boot now routes through
    // `transport_factory::resolve_transport` so `state.transport.kind =
    // conpty` boots the ConPTY backend, not a tmux backend that would
    // fake `has_session()`. Tmux behavior is byte-equivalent (Layer 3
    // legacy `tmux_endpoint` → same `tmux_backend_for_runtime_state_or_workspace`
    // shape inside the factory's `build_tmux`).
    let state = crate::state::persist::load_runtime_state(args.workspace.as_path()).ok();
    // 0.5.x Windows portability Batch 9 F8 (leader msg_2a4cc1fa54c0):
    // team_key priority is now `args.team_key` (from `--team` CLI)
    // > `state.active_team_key` fallback. The Batch 7 F5 derivation
    // stays as the fallback so pre-Batch-9 tests + Unix daemons
    // continue byte-preserving. Callers that CAN pass `--team`
    // (Batch 9 quick-start on Windows) do — that avoids the F8
    // seed-state trap where writing active_team_key to state ahead
    // of coord spawn made downstream launch code think "existing
    // runtime, use restart" and skip spec compile.
    let team_key_from_state = args.team_key.clone().filter(|s| !s.is_empty()).or_else(|| {
        state
            .as_ref()
            .and_then(|s| s.get("active_team_key"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let mut factory_input = crate::transport_factory::TransportFactoryInput::new(
        args.workspace.as_path(),
        crate::transport_factory::TransportPurpose::Coordinator,
    )
    .with_state(state.as_ref());
    if let Some(ref team_key) = team_key_from_state {
        factory_input = factory_input.with_team_key(Some(team_key));
    }
    let resolved = match crate::transport_factory::resolve_transport(factory_input) {
        Ok(resolved) => resolved,
        Err(e) => {
            // Fail-closed honest degradation: coordinator boots with a
            // tmux-workspace backend so it doesn't crash, but records
            // the assembly refusal in the boot metadata source so the
            // event log shows the reason. This preserves the daemon
            // liveness path while making the failure explicit.
            eprintln!(
                "[coordinator] transport_factory refused ({e}); falling back to tmux workspace for daemon liveness"
            );
            let sel = crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
                args.workspace.as_path(),
                state.as_ref(),
            );
            let metadata = DaemonTmuxEndpointMetadata {
                tmux_endpoint_used: sel.tmux_endpoint_used.clone(),
                tmux_endpoint_source: "factory_refused_fallback",
            };
            let coord = Coordinator::new(
                args.workspace.clone(),
                Box::new(RealProviderRegistry),
                Box::new(sel.backend),
            )
            .with_team_key(team_key_from_state.clone());
            return run_daemon_with_coordinator_and_boot_tmux(&args, &coord, Some(metadata));
        }
    };
    // Preserve tmux boot metadata byte-equivalent for tmux teams.
    // ConPTY teams get their source string but no tmux endpoint (design
    // §Behavior Equivalence: same `tmux_endpoint_used` for tmux; None
    // + honest source for conpty).
    let tmux_metadata = DaemonTmuxEndpointMetadata {
        tmux_endpoint_used: resolved.tmux_endpoint_used.clone(),
        tmux_endpoint_source: resolved.source,
    };
    let coordinator = Coordinator::new(
        args.workspace.clone(),
        Box::new(RealProviderRegistry),
        resolved.backend,
    )
    .with_team_key(team_key_from_state.clone());
    run_daemon_with_coordinator_and_boot_tmux(&args, &coordinator, Some(tmux_metadata))
}

pub fn run_daemon_with_coordinator(
    args: &DaemonArgs,
    coordinator: &Coordinator,
) -> Result<(), DaemonError> {
    run_daemon_with_coordinator_and_boot_tmux(args, coordinator, None)
}

#[derive(Debug, Clone)]
struct DaemonTmuxEndpointMetadata {
    tmux_endpoint_used: Option<String>,
    tmux_endpoint_source: &'static str,
}

fn run_daemon_with_coordinator_and_boot_tmux(
    args: &DaemonArgs,
    coordinator: &Coordinator,
    tmux_metadata: Option<DaemonTmuxEndpointMetadata>,
) -> Result<(), DaemonError> {
    let pid = Pid::new(std::process::id());
    let boot_id = daemon_boot_id(pid);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_daemon_body_with_panic_marker(args, coordinator, tmux_metadata, pid, &boot_id)
    })) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            let reason = if matches!(error, DaemonError::Tick(_)) {
                "fatal_error"
            } else {
                "startup_error"
            };
            let _ = write_coordinator_heartbeat(
                &args.workspace,
                pid,
                Some(&boot_id),
                reason,
                Some("error"),
                Some(&error.to_string()),
                false,
            );
            let _ = write_daemon_exit(
                &EventLog::new(args.workspace.as_path()),
                &args.workspace,
                pid,
                &boot_id,
                reason,
                serde_json::json!({"error": error.to_string()}),
            );
            Err(error)
        }
        Err(payload) => {
            let panic_payload = panic_payload_message(payload.as_ref());
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();
            let _ = write_coordinator_heartbeat(
                &args.workspace,
                pid,
                Some(&boot_id),
                "panic",
                Some(HEARTBEAT_STATUS_PANIC),
                Some(&panic_payload),
                false,
            );
            let _ = write_daemon_exit(
                &EventLog::new(args.workspace.as_path()),
                &args.workspace,
                pid,
                &boot_id,
                "panic",
                serde_json::json!({
                    "panic_payload": panic_payload.clone(),
                    "backtrace": backtrace,
                }),
            );
            Err(DaemonError::Panic(panic_payload))
        }
    }
}

fn run_daemon_body_with_panic_marker(
    args: &DaemonArgs,
    coordinator: &Coordinator,
    tmux_metadata: Option<DaemonTmuxEndpointMetadata>,
    pid: Pid,
    boot_id: &str,
) -> Result<(), DaemonError> {
    let runtime_dir = crate::model::paths::runtime_dir(args.workspace.as_path());
    std::fs::create_dir_all(&runtime_dir)?;
    std::fs::write(coordinator_pid_path(&args.workspace), pid.to_string())?;
    write_coordinator_metadata(&args.workspace, pid, MetadataSource::Boot)?;
    let binary_identity = crate::coordinator::current_coordinator_binary_identity();
    let _ = write_coordinator_heartbeat(
        &args.workspace,
        pid,
        Some(boot_id),
        "boot",
        None,
        None,
        false,
    );

    let event_log = EventLog::new(args.workspace.as_path());
    let mut boot_event = serde_json::json!({
        "workspace": args.workspace.as_path().to_string_lossy(),
        "once": args.once,
        "binary_path": binary_identity.binary_path,
        "binary_version": binary_identity.binary_version,
    });
    if let Some(metadata) = tmux_metadata {
        if let Some(object) = boot_event.as_object_mut() {
            object.insert(
                "tmux_endpoint_source".to_string(),
                serde_json::Value::String(metadata.tmux_endpoint_source.to_string()),
            );
            object.insert(
                "tmux_endpoint_used".to_string(),
                metadata
                    .tmux_endpoint_used
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
    }
    event_log.write("coordinator.boot", boot_event)?;

    // 0.5.x Windows portability Batch 8 F7 (leader msg_590b4dce0f68):
    // coordinator takes ownership of shim lifecycle. Idempotent:
    // spawns a fresh shim if none is recorded, reconnects if
    // `state.transport.shim.pid` names a living shim (post-crash
    // restart path). Failures are non-fatal for the coord daemon —
    // the tick loop's transport calls will surface honest
    // MuxUnavailable errors and `mark_transport_unavailable`
    // emits the C-3 stale-family event.
    //
    // On non-Windows this call is cfg'd out (there's no shim
    // concept on Unix).
    #[cfg(windows)]
    {
        let state_for_team_key =
            crate::state::persist::load_runtime_state(args.workspace.as_path()).ok();
        let team_key_opt = state_for_team_key
            .as_ref()
            .and_then(|s| s.get("active_team_key"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(team_key) = team_key_opt {
            let workspace_hash =
                crate::tmux_backend::workspace_short_hash_pub(args.workspace.as_path());
            match crate::coordinator::conpty_shim::ensure_shim_running(
                args.workspace.as_path(),
                &team_key,
                &workspace_hash,
            ) {
                Ok(handle) => {
                    // Detach so the shim survives beyond this
                    // coordinator instance (F7 core invariant).
                    let _shim_pid = handle.detach();
                    eprintln!("[coordinator] shim ensured (pid={_shim_pid})");
                }
                Err(e) => {
                    // Honest degradation: emit the stale-family
                    // event, coord continues without a shim. Tick
                    // loop will surface `mux_unavailable` on any
                    // transport call.
                    eprintln!("[coordinator] shim ensure failed: {e}");
                    let _ = crate::coordinator::conpty_shim::mark_transport_unavailable(
                        args.workspace.as_path(),
                        &format!("boot_ensure_failed: {e}"),
                    );
                }
            }
        }
    }

    let tick_interval = match args.tick_interval_sec {
        Some(v) if v > 0.0 => v,
        _ => resolve_tick_interval(&args.workspace)?,
    };
    // P7 (Gap 37b, Python __main__.py:44-59): capture the original parent BEFORE the
    // loop; the orphan predicate fires only on the literal triple condition
    // (ppid changed ∧ reparented to pid 1 ∧ workspace gone) — never wider.
    let initial_ppid = current_ppid();
    let mut consecutive_failures = 0_u32;
    let mut last_failure_signature: Option<String> = None;
    install_signal_handlers();
    loop {
        if let Some(signal) = take_signal_stop_request() {
            let _ = write_coordinator_heartbeat(
                &args.workspace,
                pid,
                Some(boot_id),
                "signal",
                Some("stop_requested"),
                Some(signal),
                false,
            );
            write_daemon_exit(
                &event_log,
                &args.workspace,
                pid,
                boot_id,
                "signal",
                serde_json::json!({"signal": signal}),
            )?;
            return Ok(());
        }
        let ppid_now = current_ppid();
        if super::should_orphan_self_terminate(initial_ppid, ppid_now, &args.workspace) {
            let _ = event_log.write(
                "coordinator.orphan_self_terminate",
                serde_json::json!({
                    "initial_ppid": initial_ppid,
                    "current_ppid": ppid_now,
                    "workspace": args.workspace.as_path().to_string_lossy(),
                }),
            );
            break;
        }
        let _ = write_coordinator_heartbeat(
            &args.workspace,
            pid,
            Some(boot_id),
            "tick_running",
            Some("running"),
            None,
            false,
        );
        match run_tick_with_panic_marker(&event_log, || coordinator.tick()) {
            Ok(report) => {
                let status = if report.stop {
                    "stop_requested"
                } else {
                    "ok"
                };
                let _ = write_coordinator_heartbeat(
                    &args.workspace,
                    pid,
                    Some(boot_id),
                    "tick_finished",
                    Some(status),
                    None,
                    false,
                );
                if consecutive_failures > 0 {
                    event_log.write(
                        "coordinator.tick_recovered",
                        serde_json::json!({"consecutive_failures": consecutive_failures}),
                    )?;
                    consecutive_failures = 0;
                    last_failure_signature = None;
                }
                if report.stop || args.once {
                    break;
                }
                sleep_seconds(tick_interval);
            }
            Err(err) => {
                let status = if matches!(err, TickError::Panic(_)) {
                    HEARTBEAT_STATUS_PANIC
                } else {
                    "error"
                };
                let _ = write_coordinator_heartbeat(
                    &args.workspace,
                    pid,
                    Some(boot_id),
                    "tick_finished",
                    Some(status),
                    Some(&err.to_string()),
                    false,
                );
                consecutive_failures = consecutive_failures.saturating_add(1);
                let next_sleep_sec = backoff_sleep_sec(tick_interval, consecutive_failures);
                // P7-F2 (Python __main__.py:66-89): identical-signature failures emit
                // ONE full tick_error; repeats only write `.suppressed` companions,
                // except the Python periodic re-emit tiers (failure #1, every 12th
                // failure, or the 40s/60s backoff steps).
                let signature: String = err.to_string().chars().take(200).collect();
                let signature_changed =
                    last_failure_signature.as_deref() != Some(signature.as_str());
                if signature_changed {
                    last_failure_signature = Some(signature);
                }
                if signature_changed
                    || consecutive_failures == 1
                    || consecutive_failures % 12 == 0
                    || next_sleep_sec == 40.0
                    || next_sleep_sec == 60.0
                {
                    event_log.write(
                        "coordinator.tick_error",
                        serde_json::json!({
                            "error": err.to_string(),
                            "exc_type": "TickError",
                            "consecutive_failures": consecutive_failures,
                            "next_sleep_sec": next_sleep_sec,
                        }),
                    )?;
                } else {
                    event_log.write(
                        "coordinator.tick_error.suppressed",
                        serde_json::json!({
                            "consecutive_failures": consecutive_failures,
                            "next_sleep_sec": next_sleep_sec,
                        }),
                    )?;
                }
                if args.once {
                    return Err(DaemonError::Tick(err));
                }
                sleep_seconds(next_sleep_sec);
            }
        }
    }
    let _ = write_coordinator_heartbeat(
        &args.workspace,
        pid,
        Some(boot_id),
        "exit",
        Some("stop_requested"),
        None,
        false,
    );
    write_daemon_exit(
        &event_log,
        &args.workspace,
        pid,
        boot_id,
        "stop",
        serde_json::json!({"stop": true}),
    )?;
    Ok(())
}

fn write_daemon_exit(
    event_log: &EventLog,
    workspace: &WorkspacePath,
    pid: Pid,
    boot_id: &str,
    reason: &str,
    mut extra: serde_json::Value,
) -> Result<(), crate::event_log::EventLogError> {
    let mut event = serde_json::json!({
        "reason": reason,
        "pid": pid.get(),
        "boot_id": boot_id,
        "workspace": workspace.as_path().to_string_lossy(),
    });
    if let (Some(target), Some(extra_obj)) = (event.as_object_mut(), extra.as_object_mut()) {
        target.extend(std::mem::take(extra_obj));
    }
    event_log.write("coordinator.exit", event).map(|_| ())
}

fn daemon_boot_id(pid: Pid) -> String {
    format!(
        "coord_{}_{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        pid.get()
    )
}

fn run_tick_with_panic_marker<F>(event_log: &EventLog, tick: F) -> Result<TickReport, TickError>
where
    F: FnOnce() -> Result<TickReport, TickError>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(tick)) {
        Ok(result) => result,
        Err(payload) => {
            let panic_message = panic_payload_message(payload.as_ref());
            event_log.write(
                "coordinator.tick_panic",
                serde_json::json!({
                    "panic": panic_message,
                    "backtrace": std::backtrace::Backtrace::force_capture().to_string(),
                }),
            )?;
            Err(TickError::Panic(panic_message))
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// 当前 ppid(`os.getppid()`,孤儿自检输入)。
///
/// 0.5.x Windows portability Batch 3: uses `platform::process::current_parent_pid`
/// so Windows sees a real Toolhelp32-derived ppid instead of `0`.
fn current_ppid() -> u32 {
    crate::platform::process::current_parent_pid().unwrap_or(0)
}

/// 计算 tick 间隔(`_tick_interval`,`__main__.py:104-115`)。读 spec `runtime.tick_interval_sec`,
/// 缺失/出错 → `DEFAULT_TICK_INTERVAL_SEC`;并确保 schema 存在(`MessageStore(workspace)`)。
pub fn resolve_tick_interval(workspace: &WorkspacePath) -> Result<f64, TickError> {
    let _ = MessageStore::open(workspace.as_path())?;
    Ok(DEFAULT_TICK_INTERVAL_SEC)
}

/// 退避序列(`__main__.py:65`):`min(interval * 2^min(failures-1, 5), 60.0)` → 5→10→20→40→60→60s。
/// unit test 锁死本序列(card §85)。**纯函数,无 I/O,可直接 impl 钉死**(但 ROUND-0 仍占位)。
pub fn backoff_sleep_sec(interval: f64, consecutive_failures: u32) -> f64 {
    let failures = consecutive_failures.saturating_sub(1).min(5);
    let exp = i32::try_from(failures).unwrap_or(5);
    (interval * 2f64.powi(exp)).min(BACKOFF_MAX_SEC)
}

struct RealProviderRegistry;

impl ProviderRegistry for RealProviderRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        crate::provider::get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists::default()
    }
}

fn sleep_seconds(seconds: f64) {
    if seconds <= 0.0 {
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    loop {
        #[cfg(unix)]
        if SIGNAL_STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline.saturating_duration_since(now);
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(100)));
    }
}

/// 子进程退出错误(daemon bin 顶层用 anyhow,但 lib 入口仍给 typed)。
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    #[error("tick: {0}")]
    Tick(#[from] TickError),
    #[error("panic: {0}")]
    Panic(String),
}

#[cfg(unix)]
static SIGNAL_STOP_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(unix)]
static SIGNAL_STOP_NUMBER: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn coordinator_signal_handler(signal: libc::c_int) {
    SIGNAL_STOP_NUMBER.store(signal, std::sync::atomic::Ordering::SeqCst);
    SIGNAL_STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
fn install_signal_handlers() {
    unsafe {
        let handler = coordinator_signal_handler as *const () as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
        libc::signal(libc::SIGINT, handler);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

#[cfg(unix)]
fn take_signal_stop_request() -> Option<&'static str> {
    if !SIGNAL_STOP_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    match SIGNAL_STOP_NUMBER.load(std::sync::atomic::Ordering::SeqCst) {
        libc::SIGTERM => Some("SIGTERM"),
        libc::SIGHUP => Some("SIGHUP"),
        libc::SIGINT => Some("SIGINT"),
        _ => Some("UNKNOWN"),
    }
}

#[cfg(not(unix))]
fn take_signal_stop_request() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ws(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ta-rs-coord-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn coordinator_tick_panic_writes_durable_marker() {
        let workspace = tmp_ws("tick-panic");
        let event_log = EventLog::new(&workspace);

        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = run_tick_with_panic_marker(&event_log, || -> Result<TickReport, TickError> {
            panic!("synthetic tick panic")
        });
        std::panic::set_hook(old_hook);

        assert!(
            matches!(result, Err(TickError::Panic(message)) if message == "synthetic tick panic")
        );
        let events = event_log.tail(20).unwrap();
        let panic_event = events
            .iter()
            .find(|event| {
                event.get("event").and_then(serde_json::Value::as_str)
                    == Some("coordinator.tick_panic")
            })
            .expect("coordinator.tick_panic event");
        assert_eq!(
            panic_event.get("panic").and_then(serde_json::Value::as_str),
            Some("synthetic tick panic")
        );
        assert!(
            panic_event
                .get("backtrace")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|backtrace| !backtrace.is_empty()),
            "panic marker must carry a backtrace; event={panic_event}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn signal_stop_request_interrupts_daemon_sleep_without_consuming_signal() {
        SIGNAL_STOP_NUMBER.store(libc::SIGTERM, std::sync::atomic::Ordering::SeqCst);
        SIGNAL_STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);

        let started = std::time::Instant::now();
        sleep_seconds(1.0);
        let elapsed = started.elapsed();
        let signal = take_signal_stop_request();

        assert_eq!(signal, Some("SIGTERM"));
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "daemon sleep must wake promptly after SIGTERM stop flag; elapsed={elapsed:?}"
        );
    }
}
