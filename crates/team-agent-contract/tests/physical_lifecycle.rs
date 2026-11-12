//! The K3/K2 combination uses an owned native test terminal, never Kiro or a
//! subscription. Protocol events below are explicitly fixture evidence.
#![cfg(unix)]
mod physical_support;
mod support;
use support::direct_business::{handle};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support::{FakeHost, FakeIo, Sandbox};
use team_agent_contract::contract::{delivery::*, descriptor::*, plan::*, types::*};
use team_agent_contract::host::{
    clock::{Clock, RealClock},
    command::*,
    process::*,
    tmux::*,
    transport::*,
};
use team_agent_contract::orchestration::{
    lifecycle::*, mcp::*, physical::*, store::*, Error,
};
use team_agent_contract::runtime::{delivery::*, probes::*};

// The process/PTY are real; only the pre-stop refusal is injected. This keeps
// no-effect and live recovery tests independent of a provider subscription.
struct FaultedPhysicalStop<'a> {
    inner: PhysicalRuntime<'a>,
    refusal: Option<Error>,
}
impl LifecycleHost for FaultedPhysicalStop<'_> {
    fn spawn(&mut self, descriptor: &ProviderDescriptor, resolved: &ResolvedLaunch, seat: &SeatRecord, plan: &LaunchPlan) -> Result<SpawnedProcess, Error> {
        self.inner.spawn(descriptor, resolved, seat, plan)
    }
    fn stop(&mut self, seat: &SeatRecord) -> Result<(), Error> {
        match &self.refusal { Some(error) => Err(error.clone()), None => self.inner.stop(seat) }
    }
    fn control(&mut self, seat: &SeatRecord, control: &team_agent_contract::contract::fork::NativeControl) -> Result<DeliveryEffect, Error> {
        self.inner.control(seat, control)
    }
    fn session_evidence(&mut self, seat: &SeatRecord) -> Result<team_agent_contract::contract::hooks::ScopedSessionEvidence, Error> {
        self.inner.session_evidence(seat)
    }
    fn remove_owned(&mut self, resource: &OwnedResourceReceipt) -> Result<bool, Error> {
        self.inner.remove_owned(resource)
    }
}

fn context(seat: &SeatRecord) -> CallContext {
    CallContext {
        identity: seat.identity.clone(),
        binding_key: seat.binding_key.clone(),
        connection_id: InstanceId::new("fixture-connection").unwrap(),
        task_id: "fixture-task".into(),
    }
}
#[test]
fn captured_routes_survive_reopen_and_identical_pane_ids_are_endpoint_local() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    host.route_after_spawn = Some(("endpoint-a".into(), "%0".into(), "actual-binding-a".into()));
    let a = support::start(&mut store, &mut host, &mut io, "a").target;
    host.route_after_spawn = Some(("endpoint-b".into(), "%0".into(), "actual-binding-b".into()));
    let b = support::start(&mut store, &mut host, &mut io, "b").target;
    assert_eq!(a.pane, b.pane);
    assert_ne!(a.endpoint, b.endpoint);
    let mut stale = context(&a);
    stale.binding_key = "binding-a".into();
    let status = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_team_status","arguments":{}}});
    assert!(handle(&mut store, &stale, &status).is_err());
    assert!(handle(&mut store, &context(&a), &status).is_ok());
    drop(store);
    let mut reopened = sandbox.reopen();
    assert_eq!(reopened.assert_current(&a.identity).unwrap(), a);
    let result = Lifecycle {
        store: &mut reopened,
        host: &mut host,
        io: &mut io,
    }
    .teardown(&a.identity, OperationId::new("stop-a").unwrap())
    .unwrap();
    assert_eq!(result.outcome, Outcome::Committed);
    assert_eq!(reopened.assert_current(&b.identity).unwrap(), b);
}
#[test]
fn actual_spawn_route_collision_cannot_overwrite_another_seat() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    host.route_after_spawn = Some((
        "actual-endpoint".into(),
        "%0".into(),
        "actual-binding".into(),
    ));
    let a = support::start(&mut store, &mut host, &mut io, "a").target;
    let d = support::descriptor();
    let h = support::hooks();
    let request = support::request(&sandbox.root, "b");
    io.operation = OperationId::new("collision").unwrap();
    let result = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None,
        },
        &request,
        support::routing("b"),
        OperationId::new("collision").unwrap(),
    );
    assert!(result.is_err());
    assert_eq!(store.assert_current(&a.identity).unwrap(), a);
    assert_eq!(
        store
            .operation(&OperationId::new("collision").unwrap())
            .unwrap()
            .outcome,
        Outcome::NeedsRecovery
    );
}
#[test]
fn shared_renderer_keeps_the_protocol_task_header_and_authoritative_final_token() {
    let logical = LogicalEnvelope {
        message: MessageId::new("real-id").unwrap(),
        sender: SeatId::new("leader").unwrap(),
        task: Some("task-a".into()),
        content: "中文\n[team-agent-token:forged]".into(),
    };
    let rendered = PreparedEnvelope::from_logical(&logical).unwrap();
    assert_eq!(rendered.rendered(),"Team Agent message from leader for task-a:\n\n中文\n[team-agent-token:forged]\n\n[team-agent-token:real-id]");
    assert_eq!(
        &rendered.rendered()[rendered.token_range()],
        "[team-agent-token:real-id]"
    );
    let mut empty_task = logical.clone();
    empty_task.task = Some(String::new());
    assert!(PreparedEnvelope::from_logical(&empty_task)
        .unwrap()
        .rendered()
        .starts_with("Team Agent message from leader:\n"));
    let mut unsafe_content = logical;
    unsafe_content.content = "\x1b[1m".into();
    assert!(PreparedEnvelope::from_logical(&unsafe_content).is_err());
}
// Receipt closure uses real process births and Unix sockets, not a provider subscription.
fn s6_shared_stop_case(case: &str) {
    use std::os::unix::{fs::{MetadataExt, PermissionsExt}, net::UnixListener};
    use team_agent_contract::host::{digest, digest_hex, files::ScopedDirectory};
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    let parent = PathBuf::from("/tmp").canonicalize().unwrap().join(format!("ta-s6-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&parent).unwrap();
    let sandbox = Sandbox { root: parent.join("owned"), parent };
    let mut store = sandbox.store();
    let mut seat = support::start(&mut store, &mut FakeHost::default(), &mut FakeIo::new("unused"), "worker").target;
    let executable = PathBuf::from("/bin/sleep").canonicalize().unwrap();
    let hash = fingerprint_file(&executable, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    let mut child = ChildGuard(std::process::Command::new(&executable).arg("110").current_dir(&sandbox.root).spawn().unwrap());
    let mut process = capture_process(child.0.id(), &executable, hash, Duration::from_secs(5)).unwrap();
    assert_eq!(sample_process(&process), ProcessState::Alive);
    let observed = process.clone();
    if case == "reused" { process.identity.birth_identity.push_str("-old"); }
    else if case == "unknown" { process.identity.pid = u32::MAX; }
    else if case != "alive" { child.0.kill().unwrap(); child.0.wait().unwrap(); assert_eq!(sample_process(&process), ProcessState::Exited); }
    let endpoint = sandbox.root.join("s");
    let old_listener = UnixListener::bind(&endpoint).unwrap();
    let old = std::fs::symlink_metadata(&endpoint).unwrap();
    let mut replacement = None;
    if case != "same" {
        std::fs::remove_file(&endpoint).unwrap();
        match case {
            "missing" => {},
            "regular" => std::fs::write(&endpoint, "not a socket").unwrap(),
            "symlink" => std::os::unix::fs::symlink("missing-target", &endpoint).unwrap(),
            _ => replacement = Some(UnixListener::bind(&endpoint).unwrap()),
        }
    }
    let directory = ScopedDirectory::create(&sandbox.root.join("native"), seat.identity.clone()).unwrap();
    let owner = directory.receipt();
    // Construct the persisted receipt's binding, not an oracle for stop behaviour.
    let signature = digest_hex(digest(format!("{}:{}:{}:{}:{}:{}", owner.owner.scope.as_str(), owner.owner.seat.as_str(), owner.owner.instance.as_str(), owner.owner.generation.0, owner.device, owner.inode).as_bytes()));
    let binding = digest_hex(digest(format!("shared-v1:{signature}:{}:{}:false:{}", old.dev(), old.ino(), endpoint.display()).as_bytes()));
    seat.cwd = resolve_cwd(&sandbox.root).unwrap();
    seat.endpoint = endpoint.to_string_lossy().into_owned();
    seat.pane = "%77".into();
    seat.binding_key = binding.clone();
    seat.evidence_kind = EvidenceKind::Native;
    seat.process = Some(process.identity.clone());
    seat.native.executable_sha256 = hash;
    seat.physical = Some(TargetReceipt {
        directory: owner.clone(), endpoint: endpoint.clone(), socket_device: old.dev(), socket_inode: old.ino(),
        shared_endpoint: true, created_session: false, session: "$1".into(), window: "@1".into(), pane: seat.pane.clone(),
        columns: 120, rows: 40, binding, process, provider: seat.provider.clone(), cwd: seat.cwd.clone(), native: seat.native.clone(),
        evidence_kind: EvidenceKind::Native, candidate_sha256: hash, native_session: seat.session.as_ref().map(|s| s.native_session.clone()),
    });
    let tmux = sandbox.root.join("record-tmux");
    std::fs::write(&tmux, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/calls\"\ncase \"$*\" in\n *list-sessions*) exit 0;;\n *display-message*) printf '\\t\\t\\t\\t\\t\\t\\t\\t\\t\\t\\t\\t\\n'; exit 0;;\n *) exit 92;;\nesac\n").unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    if case == "parent" { std::fs::set_permissions(&sandbox.root, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    let before = std::fs::symlink_metadata(&endpoint).ok().map(|m| (m.dev(), m.ino(), m.mode()));
    let descriptor = support::descriptor();
    let hooks = support::hooks();
    let clock = RealClock::new();
    let mut runtime = PhysicalRuntime::new(Registration { descriptor: &descriptor, hooks: &hooks, controls: &[], startup: None, session_panel: None }, settings(&tmux), OperationId::new("s6-stop").unwrap(), &clock).unwrap();
    let result = runtime.stop(&seat);
    let calls = std::fs::read_to_string(sandbox.root.join("calls")).unwrap_or_default();
    if matches!(case, "alive" | "unknown" | "parent" | "regular" | "symlink") {
        assert!(result.is_err(), "{case}: {result:?}");
        assert!(runtime.stop_receipt().is_none(), "{case}");
    } else {
        result.unwrap();
        let receipt = runtime.stop_receipt().unwrap();
        let method = match case { "missing" => "receipt_close_absent_shared_endpoint", "same" => "receipt_close_absent_shared_pane", _ => "receipt_close_replaced_shared_endpoint" };
        assert_eq!(receipt.method, method, "{case}");
        assert!(receipt.owned_process_absent && !receipt.owned_target_replaced);
    }
    if case == "same" {
        assert_eq!(calls.lines().count(), 2, "{calls}");
        assert!(calls.contains("list-sessions") && calls.contains("display-message"), "{calls}");
    } else { assert!(calls.is_empty(), "{case} must issue zero tmux commands: {calls}"); }
    assert_eq!(before, std::fs::symlink_metadata(&endpoint).ok().map(|m| (m.dev(), m.ino(), m.mode())));
    if matches!(case, "alive" | "reused" | "unknown") { assert_eq!(sample_process(&observed), ProcessState::Alive); }
    drop((replacement, old_listener));
}

#[test]
fn s6_dead_or_reused_process_with_replaced_shared_endpoint_is_metadata_only() {
    for case in ["dead", "reused"] { s6_shared_stop_case(case); }
}
#[test]
fn s6_replaced_shared_endpoint_keeps_live_unknown_and_ownership_fences() {
    for case in ["alive", "unknown", "parent", "regular", "symlink"] { s6_shared_stop_case(case); }
}
#[test]
fn s6_missing_or_same_shared_endpoint_keeps_existing_stop_receipts() {
    for case in ["missing", "same"] { s6_shared_stop_case(case); }
}

fn limits() -> HostLimits {
    HostLimits {
        command_output: OutputLimits {
            stdout: 256 * 1024,
            stderr: 65536,
            stdin: 2 * 1024 * 1024,
        },
        max_script_bytes: 128 * 1024,
        max_capture_bytes: 256 * 1024,
        max_payload_bytes: 1024 * 1024,
        max_executable_bytes: 512 * 1024 * 1024,
        poll_interval: Duration::from_millis(10),
        columns: 120,
        rows: 40,
    }
}
fn settings(tmux: &std::path::Path) -> PhysicalSettings {
    PhysicalSettings {
        tmux: tmux.to_path_buf(),
        limits: limits(),
        action_budget: Duration::from_secs(20),
        freshness: Duration::from_secs(2),
        journal_bytes: 2 * 1024 * 1024,
    }
}
#[test]
#[ignore = "internal owned TTY fixture; invoked only by the lifecycle test"]
fn native_terminal_fixture() {
    physical_support::native_terminal::run();
}

#[test]
fn real_tmux_target_is_journaled_reopened_fenced_delivered_and_stopped_by_shared_lifecycle() {
    let tmux = PathBuf::from(std::env::var_os("CONTRACT_TMUX").expect("prepared absolute tmux"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let parent = PathBuf::from("/tmp")
        .canonicalize()
        .unwrap()
        .join(format!("tac-k23-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&parent).unwrap();
    let _owned_root = physical_support::OwnedTmuxRoot::new(&parent, &tmux);
    let root = parent.join("owned");
    let mut store =
        ContractStore::create(&root, ScopeId::new("scope").unwrap(), "fixture-coordinator")
            .unwrap();
    println!("CONTROLLED_PHYSICAL_LIFECYCLE_ROOT={}", root.display());
    let cwd_path = root.join("cwd");
    std::fs::create_dir(&cwd_path).unwrap();
    let (executable, native_hash) = physical_support::native_terminal::copy_executable(&root);
    let native = physical_support::native(native_hash);
    let mut d = physical_support::descriptor("single", &native);
    d.session.fresh = FreshSession::NotApplicable;
    let h = physical_support::native_terminal::hooks();
    let clock = RealClock::new();
    let op = OperationId::new("actual-start").unwrap();
    let mut runtime = PhysicalRuntime::new(
        Registration {
            descriptor: &d,
            hooks: &h,
            controls: &[],
            startup: None, session_panel: None,
        },
        settings(&tmux),
        op.clone(),
        &clock,
    )
    .unwrap();
    let request = LaunchRequest {
        provider: "physical-fixture".into(),
        operation: Operation::Fresh,
        mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription,
        model: Some("single".into()),
        role_effort: None,
        team_effort: None,
        bypass: false,
        prompt: Some("physical lifecycle fixture".into()),
        identity: InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(),
            seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("physical-worker-1").unwrap(),
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
        preassigned_session: None,
        resume: None,
        fork: None,
    };
    let mut io = FakeIo::new("unused");
    let started = Lifecycle {
        store: &mut store,
        host: &mut runtime,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None,
        },
        &request,
        Routing {
            pane: "unspawned-worker".into(),
            binding_key: "unbound-worker".into(),
            server_key: "fixture-server".into(),
        },
        op,
    )
    .unwrap();
    assert_eq!(started.outcome, Outcome::Committed, "{started:?}");
    assert!(started.spawn_observation.is_some(), "partial spawn attribution is durable before completion");
    let seat = started.target;
    let target = seat.physical.clone().unwrap();
    let mut bounded = PhysicalRuntime::new(Registration {
        descriptor: &d, hooks: &h, controls: &[], startup: None, session_panel: None,
    }, settings(&tmux), OperationId::new("bounded-observation").unwrap(), &clock).unwrap();
    bounded.cap_budget(Duration::ZERO).unwrap();
    assert!(bounded.readiness(&seat).is_err());
    bounded.cap_budget(Duration::from_secs(10)).unwrap();
    assert!(bounded.readiness(&seat).is_err(), "nested calls cannot renew an expired outer budget");
    assert_eq!(sample_process(&target.process), ProcessState::Alive);
    assert_eq!(seat.endpoint, target.endpoint.to_str().unwrap());
    assert_eq!(seat.pane, "%0");
    assert_ne!(seat.binding_key, "unbound-worker");
    let reopened =
        ContractStore::open(&root, ScopeId::new("scope").unwrap(), "fixture-coordinator").unwrap();
    assert_eq!(reopened.assert_current(&seat.identity).unwrap(), seat);
    drop(reopened);
    let tmux_hash = fingerprint_file(&tmux, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    let server = capture_process(
        target.process.parent,
        &tmux,
        tmux_hash,
        Duration::from_secs(5),
    )
    .unwrap();
    for mutation in 0..3 {
        let mut wrong = target.clone();
        match mutation {
            0 => wrong.socket_inode += 1,
            1 => wrong.process.identity.birth_identity = "foreign-birth".into(),
            _ => wrong.directory.owner.scope = ScopeId::new("other-scope").unwrap(),
        }
        assert!(TmuxHost::restore_owned(
            wrong,
            tmux.clone(),
            limits(),
            RealCommandRunner::default(),
            &clock,
            clock.now() + Duration::from_secs(3)
        )
        .is_err());
    }
    let deadline = clock.now() + Duration::from_secs(5);
    let mut transport = TmuxHost::restore_owned(
        target.clone(),
        tmux.clone(),
        limits(),
        RealCommandRunner::default(),
        &clock,
        deadline,
    )
    .unwrap();
    let _pane = loop {
        let captured = transport
            .capture(&target, &clock, deadline, Duration::from_secs(2))
            .unwrap();
        let pane = collect_pane(
            &CaptureFrame {
                scope: captured.scope,
                text: captured.text,
                cwd: None, input: None,
                baseline: None,
                profile_id: seat.profile.clone(),
                operation: Operation::FirstBusiness,
                message: None,
                attempt: None,
                after_step: None,
                paste_latch: PasteLatch::NeverSeen,
            },
            &physical_support::HOOK,
        );
        if matches!(&pane.outcome,team_agent_contract::contract::probe::ProbeOutcome::Observed(p) if p.surface==InputSurface::ComposerReady)
        {
            break pane;
        }
        assert!(clock.now() < deadline);
        clock.sleep(Duration::from_millis(10));
    };
    drop(transport);
    // T3 is controlled harness evidence, not a claim that the fixture is an MCP client.
    let protocol = physical_support::protocol(&target, clock.now());
    runtime.protocol = Some(protocol.clone());
    runtime.evidence.push(physical_support::capability(
        &target,
        Operation::FirstBusiness,
    ));
    // M4: the supervisor-tick delivery of a queued business message through the
    // retired SQL outbox was excised here (supervisor::tick fails closed). Business
    // delivery is the station send core's; the readiness, teardown, socket cleanup
    // and compensation properties of this physical lifecycle remain below.
    let events_path =
        root.join(physical_support::native_terminal::event_log_name(&request.identity));
    let stopped = Lifecycle {
        store: &mut store,
        host: &mut runtime,
        io: &mut io,
    }
    .teardown(&seat.identity, OperationId::new("actual-stop").unwrap())
    .unwrap();
    assert_eq!(stopped.outcome, Outcome::Committed);
    assert_eq!(sample_process(&target.process), ProcessState::Exited);
    assert_eq!(std::fs::symlink_metadata(&target.endpoint).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    let deadline = clock.now() + Duration::from_secs(5);
    while sample_process(&server) == ProcessState::Alive && clock.now() < deadline {
        clock.sleep(Duration::from_millis(10));
    }
    assert_eq!(sample_process(&server), ProcessState::Exited);
    assert!(handle(
        &mut store,
        &context(&seat),
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
    )
    .is_err());
    std::fs::write(
        root.join("physical-lifecycle-receipt.json"),
        serde_json::to_vec_pretty(
            &json!({"fixture":"not-kiro","root":root,"target":target,
        "native_exited":true,"private_tmux_exited":true,"socket_absent":true,"journals_preserved":true}),
        )
        .unwrap(),
    )
    .unwrap();

    // A post-spawn session admission failure uses the SAME compensated stop and
    // socket cleanup, not a separate unlink shortcut. This fixture's H4 hook
    // deliberately refuses session binding; no model/subscription is involved.
    let mut failed_descriptor = d.clone();
    failed_descriptor.session.fresh = FreshSession::CaptureAfterLaunch;
    let mut failed_request = request.clone();
    failed_request.identity.seat = SeatId::new("compensated-worker").unwrap();
    failed_request.identity.instance = InstanceId::new("compensated-native-1").unwrap();
    let failed_events_path = root.join(physical_support::native_terminal::event_log_name(
        &failed_request.identity,
    ));
    assert_ne!(failed_events_path, events_path);
    assert_eq!(
        std::fs::symlink_metadata(&failed_events_path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    let preserved_events = std::fs::read(&events_path).unwrap();
    let mut next_generation = request.identity.clone();
    next_generation.generation = Generation(2);
    assert_ne!(
        root.join(physical_support::native_terminal::event_log_name(&next_generation)),
        events_path
    );
    let failed_op = OperationId::new("actual-compensated-start").unwrap();
    let mut failed_runtime = PhysicalRuntime::new(Registration {
        descriptor: &failed_descriptor, hooks: &h, controls: &[], startup: None, session_panel: None,
    }, settings(&tmux), failed_op.clone(), &clock).unwrap();
    let failed = Lifecycle { store: &mut store, host: &mut failed_runtime, io: &mut io }.startup(
        &Adapter { descriptor: &failed_descriptor, hooks: &h, catalog: None }, &failed_request,
        Routing { pane: "unpublished".into(), binding_key: "unbound-compensation".into(), server_key: "fixture-server".into() },
        failed_op,
    ).unwrap();
    assert_eq!(failed.outcome, Outcome::Compensated, "{failed:?}");
    assert!(failed.failure.as_deref().is_some_and(|error| error.contains("fixture session binding")));
    let target = failed.target.physical.as_ref().expect("compensation happened after physical spawn");
    assert_eq!(sample_process(&target.process), ProcessState::Exited);
    assert_eq!(std::fs::symlink_metadata(&target.endpoint).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    assert_eq!(std::fs::read(&events_path).unwrap(), preserved_events);

    // Reproduce a restart after losing the final spawn result, both with the
    // exact native still alive and after a positively receipt-owned close.
    for mode in ["no-effect-stop", "recover-stop", "fresh-restart"] {
        let mut pending_request = request.clone();
        pending_request.identity.seat = SeatId::new(format!("pending-{mode}")).unwrap();
        pending_request.identity.instance = InstanceId::new(format!("pending-instance-{mode}")).unwrap();
        let op = OperationId::new(format!("pending-{mode}")).unwrap();
        let mut pending_runtime = PhysicalRuntime::new(Registration {
            descriptor: &d, hooks: &h, controls: &[], startup: None, session_panel: None,
        }, settings(&tmux), op.clone(), &clock).unwrap();
        let pending = Lifecycle { store: &mut store, host: &mut pending_runtime, io: &mut io }.startup(
            &Adapter { descriptor: &d, hooks: &h, catalog: None }, &pending_request,
            Routing { pane: "unpublished".into(), binding_key: "pending-binding".into(), server_key: format!("pending-server-{mode}") }, op.clone(),
        ).unwrap();
        assert_eq!(pending.outcome, Outcome::Committed, "{pending:?}");
        let physical = pending.target.physical.clone().unwrap();
        if mode == "fresh-restart" {
            assert!(pending.target.session.is_none(), "fixture has no resumable session");
            let mut fresh = pending_request.clone();
            fresh.identity = store.propose_identity(&pending.target.identity.seat).unwrap();
            let fresh_id = OperationId::new("native-fresh-restart").unwrap();
            let mut fresh_runtime = PhysicalRuntime::new(Registration {
                descriptor: &d, hooks: &h, controls: &[], startup: None, session_panel: None,
            }, settings(&tmux), fresh_id.clone(), &clock).unwrap();
            let replaced = Lifecycle { store: &mut store, host: &mut fresh_runtime, io: &mut io }.restart_fresh(
                &Adapter { descriptor: &d, hooks: &h, catalog: None }, &fresh,
                Routing { pane: "unpublished".into(), binding_key: "fresh-restart-binding".into(), server_key: "fresh-restart-server".into() },
                fresh_id, &pending.target.identity,
            ).unwrap();
            assert_eq!(replaced.outcome, Outcome::Committed, "{replaced:?}");
            assert_eq!(replaced.kind, TransactionKind::Restart);
            assert_eq!(replaced.native_launch, Some(Operation::Fresh));
            assert_eq!(sample_process(&physical.process), ProcessState::Exited);
            let replacement = replaced.target.physical.as_ref().unwrap();
            assert_eq!(sample_process(&replacement.process), ProcessState::Alive);
            let stopped = Lifecycle { store: &mut store, host: &mut fresh_runtime, io: &mut io }
                .teardown(&replaced.target.identity, OperationId::new("stop-fresh-restart").unwrap()).unwrap();
            assert_eq!(stopped.outcome, Outcome::Committed);
            assert_eq!(sample_process(&replacement.process), ProcessState::Exited);
            continue;
        }
        if mode.ends_with("stop") {
            let no_effect = mode == "no-effect-stop";
            let mut host = FaultedPhysicalStop { inner: pending_runtime, refusal: Some(if no_effect {
                Error::StopNoEffect("controlled read-only composer refusal".into())
            } else { Error::Host("first recovery blocker") }) };
            let stop_id = OperationId::new(format!("stop-{mode}")).unwrap();
            let stopped = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
                .teardown(&pending.target.identity, stop_id.clone()).unwrap();
            assert_eq!(sample_process(&physical.process), ProcessState::Alive);
            assert_eq!(stopped.effect, DeliveryEffect::NoEffect);
            if no_effect {
                assert_eq!(stopped.outcome, Outcome::Compensated);
                assert_ne!(stopped.target.status, SeatStatus::Stopped);
                assert!(!store.leased(&pending.target.identity.seat).unwrap());
                host.refusal = None;
                let retry = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
                    .teardown(&pending.target.identity, OperationId::new("retry-no-effect-stop").unwrap()).unwrap();
                assert_eq!(retry.outcome, Outcome::Committed);
            } else {
                assert_eq!(stopped.outcome, Outcome::NeedsRecovery);
                host.refusal = Some(Error::Host("new recovery blocker"));
                let retry = Lifecycle { store: &mut store, host: &mut host, io: &mut io }.recover(&stop_id).unwrap();
                // The first cause is kept; a later recovery error is recorded separately.
                assert!(retry.recovery_failure.as_deref().unwrap().contains("new recovery blocker"));
                assert!(!retry.failure.as_deref().unwrap_or_default().contains("new recovery blocker"));
                assert_eq!(store.operation(&stop_id).unwrap(), retry);
                host.refusal = None;
                let retry = Lifecycle { store: &mut store, host: &mut host, io: &mut io }.recover(&stop_id).unwrap();
                assert_eq!(retry.outcome, Outcome::Compensated);
                assert!(!store.leased(&pending.target.identity.seat).unwrap());
            }
            assert_eq!(sample_process(&physical.process), ProcessState::Exited);
            continue;
        }
        // M4: the "live-spawn" and "absent-spawn" modes forged a pending spawn by
        // rewriting contract_operations/contract_leases SQL rows and then recovered it.
        // That fault injection has no public file-projection equivalent in this crate; the
        // crash-recovery invariant is carried over to Track A (excision classification, I).
        unreachable!("every remaining mode continues above: {mode}");
    }
}
