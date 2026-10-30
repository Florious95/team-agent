//! Real Lifecycle + real owned tmux/process, with a controlled terminal.
//! No model, provider subscription, fixture-seam seat, or forged process liveness.
#![cfg(unix)]
#[path = "../../team-agent-contract/tests/physical_support/mod.rs"]
mod physical_support;
use std::{path::PathBuf, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use serde_json::{json, Value};
use team_agent::contract_runtime::backend::{self, Binding};
use team_agent_contract::{
    contract::{delivery::StepOutcome, descriptor::{AuthMode, FreshSession}, hooks::PlanHook, plan::*, types::*},
    host::{clock::RealClock, command::{OutputLimits, RealCommandRunner}, materialize::ScopedMaterializer,
        process::{fingerprint_file, resolve_cwd}, tmux::{HostLimits, TmuxHost}, transport::TargetReceipt},
    orchestration::{lifecycle::*, physical::*, store::*},
};

#[test]
#[ignore = "internal owned terminal child, not a skipped acceptance case"]
fn native_terminal_fixture() { physical_support::native_terminal::run(); }

fn limits() -> HostLimits {
    HostLimits { command_output: OutputLimits { stdout: 256 * 1024, stderr: 65536, stdin: 1024 * 1024 },
        max_script_bytes: 128 * 1024, max_capture_bytes: 256 * 1024, max_payload_bytes: 1024 * 1024,
        max_executable_bytes: 1024 * 1024 * 1024, poll_interval: Duration::from_millis(10), columns: 120, rows: 40 }
}
struct OwnedFixture { root: PathBuf, tmux: PathBuf, target: Option<TargetReceipt>, remove: bool }
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        if let Some(target) = self.target.take() {
            let clock = RealClock::new();
            if let Ok(mut host) = TmuxHost::restore_owned(target.clone(), self.tmux.clone(), limits(),
                RealCommandRunner::default(), &clock, Duration::from_secs(3)) {
                let receipt = host.close_owned_pane(&target, &clock, Duration::from_secs(3));
                self.remove = receipt.pane_close.outcome == StepOutcome::Confirmed
                    && receipt.native == team_agent_contract::host::process::ProcessState::Exited;
            }
        }
        if self.remove { let _ = std::fs::remove_dir_all(&self.root); }
        else { eprintln!("retained controlled fixture evidence: {}", self.root.display()); }
    }
}

#[test]
fn committed_starting_seat_is_located_and_receives_managed_send() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::io::Write;
    let selected = std::process::Command::new("sh").args(["-c", "command -v tmux"]).output().unwrap();
    assert!(selected.status.success(), "public tmux command is required for this real-host gate");
    let tmux = PathBuf::from(String::from_utf8(selected.stdout).unwrap().trim()).canonicalize().unwrap();
    let root = std::fs::canonicalize("/tmp").unwrap().join(format!("m4d10-{}-{:x}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos()));
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let mut owned = OwnedFixture { root: root.clone(), tmux: tmux.clone(), target: None, remove: false };
    let cwd = resolve_cwd(&root).unwrap();
    let scope = backend::scope_for(&cwd, "d10").unwrap();
    let endpoint = format!("framework:{}", scope.as_str());
    let transport_root = root.join("store");
    let mut store = ProjectionStore::create(&transport_root, scope.clone(), &endpoint).unwrap();
    let identity = store.propose_identity(&SeatId::new("worker").unwrap()).unwrap();
    let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
    let candidate_sha256 = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    let binding = Binding { version: 1, workspace: cwd.clone(), team: "d10".into(), scope: scope.clone(),
        endpoint, candidate: candidate.clone(), candidate_sha256, tmux: tmux.clone(), transport_root: transport_root.clone() };
    let binding_root = backend::root_for(&root, &scope);
    std::fs::create_dir_all(&binding_root).unwrap();
    let mut binding_file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(binding_root.join("framework-binding.json")).unwrap();
    binding_file.write_all(&serde_json::to_vec(&binding).unwrap()).unwrap();
    let (image, native_hash) = physical_support::native_terminal::copy_executable(&root);
    let native = physical_support::native(native_hash);
    let mut descriptor = physical_support::descriptor("single", &native);
    // Select the public managed Pi dialect, but retain the explicitly controlled
    // fixture executable/native identity. This is not provider-level acceptance.
    descriptor.identity.id = "pi_tui";
    descriptor.session.fresh = FreshSession::NotApplicable;
    let hooks = physical_support::native_terminal::hooks();
    let request = LaunchRequest { identity: identity.clone(), provider: "pi_tui".into(), native,
        operation: Operation::Fresh, mode: LaunchMode::FullWorker, auth: AuthMode::NativeSubscription,
        model: Some("single".into()), role_effort: None, team_effort: None, prompt: Some("controlled D10 terminal".into()),
        bypass: false, paths: LaunchPaths { candidate, cwd, runtime_root: transport_root.clone(),
            executable: image.clone(), launcher: physical_support::public_launcher(&image, native_hash) },
        channel: Channel::Tmux, input_profile: Some("physical-fixture".into()), evidence_kind: EvidenceKind::Native,
        preassigned_session: None, resume: None, fork: None };
    let resolved = resolve_launch(&descriptor, &hooks, &request, None).unwrap();
    let adapter = Adapter { descriptor: &descriptor, hooks: &hooks, catalog: None };
    let plan = physical_support::native_terminal::PLAN.plan(&resolved).unwrap();
    let operation = OperationId::new("d10-start").unwrap();
    let mut io = ScopedMaterializer::new(&descriptor, &resolved, &plan, operation.clone(), RealCommandRunner::default()).unwrap();
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(Registration { descriptor: &descriptor, hooks: &hooks, controls: &[],
        startup: None, session_panel: None }, PhysicalSettings { tmux: tmux.clone(), limits: limits(),
        action_budget: Duration::from_secs(10), freshness: Duration::from_secs(2), journal_bytes: 2 * 1024 * 1024 },
        operation.clone(), &clock).unwrap();
    physical.executing_candidate = Some(candidate_sha256);
    let started = Lifecycle { store: &mut store, host: &mut physical, io: &mut io }.startup(&adapter, &request,
        Routing { pane: "unpublished".into(), binding_key: "unpublished-d10".into(), server_key: "fixture-server".into() }, operation).unwrap();
    assert_eq!(started.outcome, Outcome::Committed, "{started:?}");
    let seat = store.assert_current(&identity).unwrap();
    assert_eq!(seat.status, SeatStatus::Starting, "exercise the real lifecycle value, never seed Ready");
    assert!(!store.leased(&identity.seat).unwrap());
    assert!(store.unfinished().unwrap().is_empty());
    owned.target = seat.physical.clone();
    let snapshot = json!({"team_key":"d10","active_team_key":"d10","teams":{"d10":{"team_key":"d10",
        "agents":{"worker":{"provider":"pi_tui","runtime_family":"contract","status":"running",
            "native_endpoint":seat.endpoint,"pane_id":seat.pane,"native_pid":seat.process.as_ref().unwrap().pid}}}}});
    std::fs::write(team_agent::model::paths::runtime_dir(&root).join("state.json"), serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let located = team_agent::direct_delivery::locate(&root, Some("d10")).unwrap();
    assert!(located["endpoints"].as_array().unwrap().iter().any(|entry| entry["agent"] == "worker"), "{located}");
    let directory = team_agent::direct_delivery::directory(&root, Some("d10")).unwrap();
    let sent = directory.send("worker", "D10 real owned delivery").unwrap();
    assert_eq!(sent["status"], "sent");
    let log = transport_root.join(physical_support::native_terminal::event_log_name(&identity));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let events: Vec<Value> = std::fs::read_to_string(&log).unwrap_or_default().lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect();
        if events.iter().any(|event| event["kind"] == "native_accepted") {
            assert_eq!(events.iter().find(|event| event["kind"] == "paste").unwrap()["payload"], json!(b"D10 real owned delivery".to_vec()));
            assert_eq!(events.iter().filter(|event| event["kind"] == "paste").count(), 1);
            assert_eq!(events.iter().filter(|event| event["kind"] == "key").count(), 1);
            eprintln!("D10: real Lifecycle committed Starting; locate found worker; exact payload pasted once, Enter once, controlled terminal accepted");
            break;
        }
        assert!(Instant::now() < deadline, "controlled host did not observe the submission");
        std::thread::sleep(Duration::from_millis(10));
    }
    let stopped = Lifecycle { store: &mut store, host: &mut physical, io: &mut io }
        .teardown(&identity, OperationId::new("d10-stop").unwrap()).unwrap();
    assert_eq!(stopped.outcome, Outcome::Committed, "{stopped:?}");
    owned.target = None;
    owned.remove = true;
    assert!(directory.send("worker", "MUST NOT TYPE").unwrap_err().contains("recipient_not_running"));
    let unavailable = team_agent::direct_delivery::locate(&root, Some("d10")).unwrap();
    assert!(unavailable["endpoints"].as_array().unwrap().is_empty(), "{unavailable}");
    assert!(unavailable["unavailable"].as_array().unwrap().iter().any(|entry|
        entry["agent"] == "worker" && entry["delivery_error"].as_str().unwrap().contains("recipient_not_running")), "{unavailable}");
}
