use super::*;

// ═════════════════════════════════════════════════════════════════════════
// (B) coordinator daemon — health gate + idempotent start_coordinator + --once daemon boot.
// coordinator_health / start_coordinator are unimplemented!() skeletons (panic today = RED). Golden
// coordinator/lifecycle.py:28-121. HARD: no in-process test spawns a real daemon — the spawn /
// multi-tick / real-Coordinator paths are #[ignore] real-machine.
// ═════════════════════════════════════════════════════════════════════════

/// A unique workspace with the db schema created (so coordinator_health's schema_ok can be true) and
/// the runtime dir present (so coordinator.pid / coordinator.json writes land).
fn daemon_ws() -> (WorkspacePath, std::path::PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("ta-rs-daemon-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(crate::model::paths::runtime_dir(&dir)).unwrap();
    let _ = crate::message_store::MessageStore::open(&dir).unwrap(); // create the schema (schema_ok)
    (WorkspacePath::new(dir.clone()), dir)
}

// coordinator_health (lifecycle.py:28-46): pid_path missing -> ok:false / status missing.
#[test]
fn coordinator_health_missing_pid_is_not_ok() {
    let (wp, _dir) = daemon_ws();
    let h = coordinator_health(&wp);
    assert!(!h.ok, "no coordinator.pid -> not healthy");
    assert_eq!(h.status, CoordinatorHealthStatus::Missing);
}

// coordinator_health: pid running (this process) + metadata pid/protocol/schema all match -> healthy.
#[test]
fn coordinator_health_running_with_matching_metadata_is_ok() {
    let (wp, _dir) = daemon_ws();
    let me = Pid(std::process::id());
    write_coordinator_metadata(&wp, me, MetadataSource::Boot).unwrap();
    std::fs::write(coordinator_pid_path(&wp), me.0.to_string()).unwrap();
    let h = coordinator_health(&wp);
    assert_eq!(h.status, CoordinatorHealthStatus::Running, "a live pid -> status running");
    assert!(h.metadata_ok, "pid+protocol+schema all match -> metadata_ok");
    assert!(h.ok, "running ∧ metadata_ok ∧ schema_ok -> healthy");
}

// coordinator_health: a pid that is NOT running -> ok:false / status stale (stale != missing).
#[test]
fn coordinator_health_dead_pid_is_stale_not_ok() {
    let (wp, _dir) = daemon_ws();
    let dead = Pid(4_000_000); // far above the macOS/Linux pid ceiling -> kill(pid,0)=ESRCH -> not running
    write_coordinator_metadata(&wp, dead, MetadataSource::Boot).unwrap();
    std::fs::write(coordinator_pid_path(&wp), dead.0.to_string()).unwrap();
    let h = coordinator_health(&wp);
    assert_eq!(h.status, CoordinatorHealthStatus::Stale, "a dead pid -> status stale");
    assert!(!h.ok, "a stale daemon is not healthy");
}

// start_coordinator (lifecycle.py:49-54) IDEMPOTENT: already-healthy -> AlreadyRunning no-op, NO spawn.
#[test]
fn start_coordinator_when_healthy_is_already_running_no_spawn() {
    let (wp, _dir) = daemon_ws();
    let me = Pid(std::process::id());
    write_coordinator_metadata(&wp, me, MetadataSource::Boot).unwrap();
    std::fs::write(coordinator_pid_path(&wp), me.0.to_string()).unwrap();
    let report = start_coordinator(&wp).expect("start_coordinator");
    assert_eq!(report.status, StartOutcome::AlreadyRunning, "a healthy coordinator -> AlreadyRunning (no spawn)");
    assert!(report.ok);
    assert_eq!(report.pid, Some(me));
}

// start_coordinator: a fresh workspace DECIDES Started. The actual `team-agent coordinator` daemon
// subprocess spawn is the real-machine boundary (#[ignore]).
#[test]
#[ignore = "real-machine: start_coordinator spawns the `team-agent coordinator` daemon subprocess"]
fn start_coordinator_fresh_workspace_decides_started() {
    let (wp, _dir) = daemon_ws();
    let report = start_coordinator(&wp).expect("start_coordinator");
    assert_eq!(report.status, StartOutcome::Started);
    assert!(report.ok && report.pid.is_some());
}

// run_daemon --once: writes the boot pid/metadata + runs exactly one tick + returns Ok. run_daemon
// constructs a real Coordinator (TmuxBackend) internally with NO injection seam, so a single tick
// would touch real tmux — #[ignore] real-machine until a run_daemon_with_coordinator(args, coord) seam
// (mirroring lifecycle::launch_with_transport) exists. SURFACED to the leader.
#[test]
#[ignore = "real-machine: run_daemon builds a real Coordinator (TmuxBackend); needs a \
            run_daemon_with_coordinator(args, coord) seam (mirror launch_with_transport) for OS-safe \
            single-tick testing"]
fn run_daemon_once_writes_boot_metadata_and_returns_ok() {
    let (wp, dir) = daemon_ws();
    crate::state::persist::save_runtime_state(&dir, &serde_json::json!({"session_name": "team-x", "agents": {}})).unwrap();
    let r = run_daemon(DaemonArgs { workspace: wp.clone(), once: true, tick_interval_sec: None });
    assert!(r.is_ok(), "run_daemon --once must return Ok; got {r:?}");
    assert!(coordinator_meta_path(&wp).exists(), "run_daemon must write the coordinator boot metadata");
}

// ═════════════════════════════════════════════════════════════════════════
// HOST-B P1 — coordinator transient-session race (timeout-tolerated vs definitive-stop fork).
//
// GOLDEN (truth source, settle by it):
//   - terminal.py:12-13   run_cmd(args, timeout=timeout, check=False)
//   - runtime.py:1010-14  _tmux_session_exists -> run_cmd(["tmux","has-session","-t",s], timeout=5);
//                         return proc.returncode == 0
//   - lifecycle.py:276-9  if session_name and not _tmux_session_exists(name):
//                             emit coordinator.session_missing; return {ok:False, stop:True,
//                             reason:"tmux_session_missing"}        # stops on the FIRST definitive miss
//   - __main__.py:60-97   a tick that RAISES (`except Exception`) -> exponential backoff + retry +
//                         (on the next clean tick) coordinator.tick_recovered  [TOLERATED];
//                         a tick that returns stop -> break (then coordinator.exit).
//
// THE CRUX: golden's ONLY tolerance for a transient session-missing is the 5s subprocess timeout.
//   - SLOW/HUNG has-session (>5s) -> subprocess.TimeoutExpired -> daemon `except` -> backoff + retry
//     (server recovers -> next tick fine). In Rust the timeout surfaces from RealCommandRunner as
//     io::ErrorKind::TimedOut -> TmuxBackend::has_session maps it to a TransportError (NOT Ok(false))
//     -> Coordinator::tick() returns Err (a TOLERATED error the daemon backs off on).
//   - FAST DEFINITIVE non-zero (session genuinely gone) -> returncode != 0 -> has_session=false ->
//     tick() returns Ok{stop:true, reason:tmux_session_missing}. A genuine miss MUST still stop.
//   NO grace-window, NO K-consecutive counting: slow=tolerated(retry), genuine-fast-miss=stop.
//
// These three are REGRESSION-LOCKS, not REDs: the tick/backend mapping (`?` propagation) and the
// daemon backoff/recover loop are ALREADY correct today. The actual gap is purely in
// RealCommandRunner::run lacking the 5s timeout (the #[ignore] real-machine RED lives in
// tmux_backend.rs::real_command_runner_enforces_golden_5s_timeout_on_hang). These locks guard the
// tick/daemon semantics from regressing when the porter adds that timeout seam.
// ═════════════════════════════════════════════════════════════════════════

/// A staged tmux `CommandRunner` for the transient-session-race fork. Each `run` pops the next
/// staged step (then repeats `last`): `Timeout` models a >5s hung has-session that the golden 5s
/// subprocess timeout converts into `io::ErrorKind::TimedOut`; `Exit(success)` models a definitive
/// tmux exit (`success=false` => session genuinely gone). Records every argv it is asked to run so a
/// test can assert the probe was exactly `tmux has-session -t <s>`.
#[derive(Clone)]
enum RunnerStep {
    /// >5s hang -> RealCommandRunner returns Err(TimedOut) (golden subprocess.TimeoutExpired).
    Timeout,
    /// fast definitive tmux exit; `false` => returncode!=0 => session genuinely gone.
    Exit(bool),
}

struct StagedTmuxRunner {
    steps: std::sync::Mutex<std::collections::VecDeque<RunnerStep>>,
    last: RunnerStep,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

impl crate::tmux_backend::CommandRunner for StagedTmuxRunner {
    fn run(&self, argv: &[String]) -> Result<crate::tmux_backend::CommandOutput, std::io::Error> {
        self.seen.lock().unwrap().push(argv.to_vec());
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.last.clone());
        match step {
            RunnerStep::Timeout => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "tmux has-session exceeded the golden 5s timeout (subprocess.TimeoutExpired analog)",
            )),
            RunnerStep::Exit(success) => Ok(crate::tmux_backend::CommandOutput {
                success,
                code: Some(if success { 0 } else { 1 }),
                stdout: String::new(),
                stderr: if success { String::new() } else { "can't find session".to_string() },
            }),
        }
    }
}

/// Build a real `Coordinator` over a real `TmuxBackend` whose OS edge is the staged runner above,
/// seeding a TRUTHY `session_name` so the tmux-session gate actually runs. Returns
/// `(coord, workspace_dir, recorded_argv)`. The workspace + schema mirror `daemon_ws`.
fn coord_over_staged_tmux(
    session_name: &str,
    steps: Vec<RunnerStep>,
    last: RunnerStep,
) -> (
    Coordinator,
    std::path::PathBuf,
    std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-coord-session-race-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(crate::model::paths::runtime_dir(&dir)).unwrap();
    let _ = crate::message_store::MessageStore::open(&dir).unwrap(); // create the schema
    crate::state::persist::save_runtime_state(
        &dir,
        &serde_json::json!({ "session_name": session_name }),
    )
    .unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = StagedTmuxRunner {
        steps: std::sync::Mutex::new(steps.into_iter().collect()),
        last,
        seen: std::sync::Arc::clone(&seen),
    };
    let backend = crate::tmux_backend::TmuxBackend::with_runner(Box::new(runner));
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let coord = Coordinator::for_test(
        WorkspacePath::new(dir.clone()),
        reg,
        Box::new(backend),
        None,
        None,
    );
    (coord, dir, seen)
}

fn coord_over_runtime_state_tmux_endpoint(
    session_name: &str,
    endpoint: &str,
    steps: Vec<RunnerStep>,
    last: RunnerStep,
) -> (
    Coordinator,
    std::path::PathBuf,
    std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    Option<String>,
    &'static str,
) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-e27-coord-explicit-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(crate::model::paths::runtime_dir(&dir)).unwrap();
    let _ = crate::message_store::MessageStore::open(&dir).unwrap();
    let state = serde_json::json!({
        "session_name": session_name,
        "tmux_endpoint": endpoint,
        "tmux_socket": endpoint,
        "agents": {},
    });
    crate::state::persist::save_runtime_state(&dir, &state).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = StagedTmuxRunner {
        steps: std::sync::Mutex::new(steps.into_iter().collect()),
        last,
        seen: std::sync::Arc::clone(&seen),
    };
    let selection = crate::tmux_backend::tmux_backend_with_runner_for_runtime_state_or_workspace(
        Box::new(runner),
        &dir,
        Some(&state),
    );
    let endpoint_used = selection.tmux_endpoint_used.clone();
    let endpoint_source = selection.tmux_endpoint_source.as_str();
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let coord = Coordinator::for_test(
        WorkspacePath::new(dir.clone()),
        reg,
        Box::new(selection.backend),
        None,
        None,
    );
    (coord, dir, seen, endpoint_used, endpoint_source)
}

#[test]
fn e27_coordinator_tick_uses_runtime_explicit_endpoint_for_session_gate() {
    let session = "team-e27-explicit-restart";
    let endpoint = "/tmp/e27-explicit-restart-test.sock";
    let (coord, dir, seen, endpoint_used, endpoint_source) = coord_over_runtime_state_tmux_endpoint(
        session,
        endpoint,
        vec![RunnerStep::Exit(true)],
        RunnerStep::Exit(true),
    );
    assert_eq!(endpoint_used.as_deref(), Some(endpoint));
    assert_eq!(endpoint_source, "state.tmux_endpoint");

    let report = coord
        .tick()
        .expect("explicit endpoint with present session should tick");
    assert!(
        report.ok,
        "present explicit endpoint session should keep tick ok"
    );
    assert!(
        !report.stop,
        "present explicit endpoint session must not trip the session-missing stop gate"
    );
    let calls = seen.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .all(|argv| !argv.iter().any(|part| part == "-L")),
        "explicit endpoint coordinator must not fall back to workspace -L socket; got {calls:?}"
    );
    assert_eq!(
        calls.first(),
        Some(&vec![
            "tmux".to_string(),
            "-S".to_string(),
            endpoint.to_string(),
            "has-session".to_string(),
            "-t".to_string(),
            session.to_string(),
        ]),
        "session gate must probe the persisted explicit endpoint"
    );
    let events = read_event_log_dir(&dir);
    assert!(
        events
            .iter()
            .all(|event| event.get("event").and_then(|v| v.as_str())
                != Some("coordinator.session_missing")),
        "present explicit endpoint session must not emit coordinator.session_missing; got {events:?}"
    );
}

#[test]
fn e27_coordinator_tick_still_stops_when_explicit_endpoint_session_is_missing() {
    let session = "team-e27-explicit-restart";
    let endpoint = "/tmp/e27-explicit-restart-test.sock";
    let (coord, dir, seen, endpoint_used, endpoint_source) = coord_over_runtime_state_tmux_endpoint(
        session,
        endpoint,
        vec![RunnerStep::Exit(false)],
        RunnerStep::Exit(false),
    );
    assert_eq!(endpoint_used.as_deref(), Some(endpoint));
    assert_eq!(endpoint_source, "state.tmux_endpoint");

    let report = coord
        .tick()
        .expect("definitive missing session is a typed stop report");
    assert!(!report.ok, "missing explicit endpoint session => ok=false");
    assert!(
        report.stop,
        "genuine missing session on the selected explicit endpoint must still stop"
    );
    assert_eq!(report.reason, Some(TickStopReason::TmuxSessionMissing));
    let calls = seen.lock().unwrap().clone();
    assert_eq!(
        calls.first(),
        Some(&vec![
            "tmux".to_string(),
            "-S".to_string(),
            endpoint.to_string(),
            "has-session".to_string(),
            "-t".to_string(),
            session.to_string(),
        ]),
        "negative control must also probe the persisted explicit endpoint"
    );
    let events = read_event_log_dir(&dir);
    assert!(
        events
            .iter()
            .any(|event| event.get("event").and_then(|v| v.as_str())
                == Some("coordinator.session_missing")),
        "genuine explicit endpoint miss must still emit coordinator.session_missing; got {events:?}"
    );
}

// ── 2(a) tick TOLERATES a has-session TIMEOUT as Err (NOT a definitive miss) — LOCK ───────────────
#[test]
fn tick_tolerates_has_session_timeout_as_transport_err_not_session_missing() {
    // A has-session that times out (>5s) surfaces as io::ErrorKind::TimedOut from RealCommandRunner.
    // TmuxBackend::has_session maps the runner io::Error to a TransportError (NOT Ok(false)), and
    // Coordinator::tick() propagates it via `?` as TickError::Transport — a TOLERATED error the
    // daemon backs off on. It must NEVER be read as Ok{stop:true, reason:tmux_session_missing}.
    // LOCK (already GREEN via `?` propagation): guards this from regressing when the 5s timeout seam
    // is added to RealCommandRunner.
    let (coord, _dir, seen) =
        coord_over_staged_tmux("team-spine", vec![RunnerStep::Timeout], RunnerStep::Timeout);
    let err = coord.tick().expect_err(
        "a has-session TIMEOUT is a tolerated transport Err (daemon backs off), NOT a definitive \
         session-missing stop",
    );
    assert!(
        matches!(err, TickError::Transport(_)),
        "a transient has-session timeout must surface as TickError::Transport (tolerated/backoff); got {err:?}"
    );
    let calls = seen.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        1,
        "tick must short-circuit at the gate on a has-session error (exactly one probe); got {calls:?}"
    );
    assert_eq!(
        calls[0],
        vec![
            "tmux".to_string(),
            "has-session".to_string(),
            "-t".to_string(),
            "team-spine".to_string(),
        ],
        "the tolerated error must come from the golden `tmux has-session -t <s>` probe"
    );
}

// ── 2(b) a FAST DEFINITIVE has-session miss STILL stops — LOCK (byte-parity) ──────────────────────
#[test]
fn tick_genuine_fast_session_miss_still_stops() {
    // lifecycle.py:277-279 — a FAST definitive non-zero has-session (returncode != 0 => session
    // genuinely gone) => {ok:false, stop:true, reason:tmux_session_missing}. The OTHER side of the
    // fork from the timeout case: a definitive miss MUST still stop the daemon. LOCK (already GREEN):
    // guards the genuine-miss stop from being swallowed when the timeout tolerance is added.
    let (coord, _dir, _seen) =
        coord_over_staged_tmux("team-spine", vec![RunnerStep::Exit(false)], RunnerStep::Exit(false));
    let report = coord
        .tick()
        .expect("a definitive miss is a typed stop report, not an Err");
    assert!(!report.ok, "a definitive session miss => ok=false");
    assert!(
        report.stop,
        "a FAST definitive has-session miss still stops the daemon (byte-parity, lifecycle.py:279)"
    );
    assert_eq!(
        report.reason,
        Some(TickStopReason::TmuxSessionMissing),
        "reason=tmux_session_missing"
    );
}

// ── 3. daemon TOLERATES a transient tick Err: backoff + recover, NO exit on the error — LOCK ──────
#[test]
fn run_daemon_backs_off_on_transient_tick_err_then_recovers_without_exiting() {
    // __main__.py:60-97 — a tick that RAISES is caught, logged as coordinator.tick_error, and the
    // loop BACKS OFF + retries (it does NOT break/exit on the error); the next clean tick logs
    // coordinator.tick_recovered. Here: tick #1's has-session TIMES OUT (TimedOut -> tick Err ->
    // tolerated), tick #2's has-session is a definitive miss (-> Ok{stop:true} -> the loop breaks on
    // the GENUINE stop, after recovering). The healthy daemon must NOT be torn down by the transient
    // timeout. LOCK (the daemon backoff loop is already wired): guards the tolerate+recover path.
    let (coord, dir, _seen) = coord_over_staged_tmux(
        "team-spine",
        vec![RunnerStep::Timeout, RunnerStep::Exit(false)],
        RunnerStep::Exit(false),
    );
    let args = DaemonArgs {
        workspace: WorkspacePath::new(dir.clone()),
        once: false,
        tick_interval_sec: Some(0.01), // tiny backoff so the test is fast
    };
    let result = run_daemon_with_coordinator(&args, &coord);
    assert!(
        result.is_ok(),
        "a single transient has-session timeout must NOT abort the daemon; got {result:?}"
    );
    let events = read_event_log_dir(&dir);
    let tags: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(|v| v.as_str()))
        .collect();
    let err_idx = tags
        .iter()
        .position(|t| *t == "coordinator.tick_error")
        .unwrap_or_else(|| panic!("a transient tick Err must log coordinator.tick_error; got {tags:?}"));
    let rec_idx = tags
        .iter()
        .position(|t| *t == "coordinator.tick_recovered")
        .unwrap_or_else(|| panic!("the recovering Ok tick must log coordinator.tick_recovered; got {tags:?}"));
    let exit_idx = tags
        .iter()
        .position(|t| *t == "coordinator.exit")
        .unwrap_or_else(|| panic!("the daemon must log coordinator.exit once it stops; got {tags:?}"));
    assert!(
        err_idx < rec_idx,
        "tick_recovered must FOLLOW tick_error (backoff then recover); got {tags:?}"
    );
    assert!(
        rec_idx < exit_idx,
        "the daemon must NOT exit on the transient error — coordinator.exit appears only AFTER \
         recovery + the genuine stop; got {tags:?}"
    );
}
