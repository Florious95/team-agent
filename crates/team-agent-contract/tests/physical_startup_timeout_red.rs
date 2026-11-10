//! RED (Item 3): a preassigned-session startup that never reaches its composer
//! must leave blackbox evidence. Real tmux + owned native TTY fixture; the
//! fixture stays alive with a blank (or non-composer) screen, exactly like the
//! Cursor incident where the TUI's first render was blocked.
#![cfg(unix)]
mod physical_support;
mod support;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support::FakeIo;
use team_agent_contract::contract::{descriptor::*, plan::*, session::*, types::*};
use team_agent_contract::host::{clock::RealClock, command::*, process::*, tmux::*};
use team_agent_contract::orchestration::{lifecycle::*, physical::*, store::*};

const CAPTURE_FILE: &str = "startup-timeout.capture.txt";

#[test]
#[ignore = "internal owned TTY fixture; invoked only by the startup timeout tests"]
fn native_terminal_fixture() {
    physical_support::native_terminal::run();
}

fn settings(tmux: &std::path::Path) -> PhysicalSettings {
    PhysicalSettings {
        tmux: tmux.to_path_buf(),
        limits: HostLimits {
            command_output: OutputLimits { stdout: 256 * 1024, stderr: 65536, stdin: 2 * 1024 * 1024 },
            max_script_bytes: 128 * 1024,
            max_capture_bytes: 256 * 1024,
            max_payload_bytes: 1024 * 1024,
            max_executable_bytes: 512 * 1024 * 1024,
            poll_interval: Duration::from_millis(50),
            columns: 120,
            rows: 40,
        },
        // Short readiness budget: the stall is permanent, only the evidence matters.
        action_budget: Duration::from_secs(4),
        freshness: Duration::from_secs(2),
        journal_bytes: 2 * 1024 * 1024,
    }
}

/// Start one preassigned-session seat whose native never renders a composer.
/// Returns the final operation record and the seat's owned native directory.
fn stalled_startup(mode: &str) -> (OperationRecord, PathBuf) {
    let tmux = PathBuf::from(std::env::var_os("CONTRACT_TMUX").expect("prepared absolute tmux"));
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let parent = PathBuf::from("/tmp").canonicalize().unwrap()
        .join(format!("tac-s{}-{}-{nonce}", &mode[6..7], std::process::id()));
    std::fs::create_dir(&parent).unwrap();
    let root = parent.join("owned");
    let mut store = ContractStore::create(&root, ScopeId::new("scope").unwrap(), "fixture-coordinator").unwrap();
    let cwd_path = root.join("cwd");
    std::fs::create_dir(&cwd_path).unwrap();
    let (executable, native_hash) = physical_support::native_terminal::copy_executable(&root);
    let native = physical_support::native(native_hash);
    let mut d = physical_support::descriptor("single", &native);
    // The Cursor shape: an exact session ID is allocated before launch and the
    // seat is only adopted once its composer is observed.
    d.session.fresh = FreshSession::Preassigned;
    let h = physical_support::native_terminal::hooks();
    let clock = RealClock::new();
    let op = OperationId::new(format!("stall-{mode}")).unwrap();
    let mut runtime = PhysicalRuntime::new(
        Registration { descriptor: &d, hooks: &h, controls: &[], startup: None, session_panel: None },
        settings(&tmux), op.clone(), &clock,
    ).unwrap();
    let request = LaunchRequest {
        provider: "physical-fixture".into(),
        operation: Operation::Fresh,
        mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription,
        model: Some(mode.into()),
        role_effort: None,
        team_effort: None,
        bypass: false,
        prompt: Some("startup stall fixture".into()),
        identity: InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(),
            seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new(format!("stall-{mode}-1")).unwrap(),
            generation: Generation(1),
        },
        native,
        paths: LaunchPaths {
            launcher: physical_support::public_launcher(&executable, native_hash),
            executable,
            candidate: std::env::current_exe().unwrap(),
            cwd: resolve_cwd(&cwd_path).unwrap(),
            runtime_root: root.clone(),
        },
        channel: Channel::Tmux,
        input_profile: Some("physical-fixture".into()),
        evidence_kind: EvidenceKind::Fixture,
        preassigned_session: Some(NativeSessionId::new("11111111-2222-4333-8444-555555555555").unwrap()),
        resume: None,
        fork: None,
    };
    let mut io = FakeIo::new("unused");
    let started = Lifecycle { store: &mut store, host: &mut runtime, io: &mut io }
        .startup(
            &Adapter { descriptor: &d, hooks: &h, catalog: None },
            &request,
            Routing { pane: "unspawned-worker".into(), binding_key: "unbound-worker".into(), server_key: "fixture-server".into() },
            op,
        )
        .expect("a readiness timeout is compensated and recorded, not a lifecycle error");
    let directory = started.target.physical.as_ref()
        .unwrap_or_else(|| panic!("the spawned target receipt is retained after compensation: phase={:?} outcome={:?} failure={:?}", started.phase, started.outcome, started.failure))
        .directory.path.clone();
    (started, directory)
}

fn assert_blackbox(started: &OperationRecord, directory: &std::path::Path, surface: &str, screen: Option<&str>) {
    assert_eq!(started.outcome, Outcome::Compensated, "{started:?}");
    let failure = started.failure.as_deref().unwrap_or_default();
    assert!(failure.contains("explicit session startup not ready"), "failure keeps its cause: {failure}");
    assert!(failure.contains(&format!("last_surface={surface}")),
        "failure must name the last observed surface ({surface}): {failure}");
    let capture = directory.join(CAPTURE_FILE);
    assert!(failure.contains(&capture.display().to_string()),
        "failure must point at the persisted capture {}: {failure}", capture.display());
    let text = std::fs::read_to_string(&capture)
        .unwrap_or_else(|error| panic!("final pane capture must survive compensation at {}: {error}", capture.display()));
    match screen {
        Some(expected) => assert!(text.contains(expected), "capture holds the final screen: {text:?}"),
        None => assert!(text.trim().is_empty(), "a blank screen is persisted as blank: {text:?}"),
    }
}

#[test]
fn blank_startup_timeout_persists_capture_and_reports_last_surface_blank() {
    let (started, directory) = stalled_startup("stall-blank");
    assert_blackbox(&started, &directory, "blank", None);
}

#[test]
fn non_composer_startup_timeout_reports_the_specific_surface() {
    let (started, directory) = stalled_startup("stall-text");
    assert_blackbox(&started, &directory, "ShellOrUnknown", Some("BOOTING|-"));
}
