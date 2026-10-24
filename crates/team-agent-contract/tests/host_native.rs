#![cfg(any(target_os = "linux", target_os = "macos"))]
mod physical_support;
use physical_support::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use team_agent_contract::contract::delivery::*;
use team_agent_contract::contract::descriptor::*;
use team_agent_contract::contract::plan::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::host::clock::{Clock, RealClock};
use team_agent_contract::host::command::*;
use team_agent_contract::host::files::ScopedDirectory;
use team_agent_contract::host::process::*;
use team_agent_contract::host::tmux::*;
use team_agent_contract::host::transport::PhysicalTransport;
use team_agent_contract::host::{digest, digest_hex};
use team_agent_contract::runtime::delivery::*;
use team_agent_contract::runtime::journal::*;
use team_agent_contract::runtime::probes::ProtocolRequirement;

fn configured_binary(key: &str) -> PathBuf {
    let value = std::env::var_os(key)
        .unwrap_or_else(|| panic!("test preparation must set {key} to an absolute executable"));
    let path = PathBuf::from(value).canonicalize().unwrap();
    assert!(path.is_absolute());
    path
}

/// Reproduce a clean caller without changing the test process's environment.
/// Every actual tmux client, including spawn/query/capture/close, lacks locale hints.
#[derive(Default)]
struct NoLocaleRunner(RealCommandRunner, Vec<Value>);

// Only controlled fixture bytes, never real provider output. Keep bounded ends
// and explicit truncation so a partial helper event cannot masquerade as complete.
fn fixture_bytes(bytes: &[u8]) -> Value {
    let head = bytes.len().min(2048);
    let tail = bytes.len().saturating_sub(2048).max(head);
    json!({
        "bytes": bytes.len(), "sha256": digest_hex(digest(bytes)),
        "ends_lf": bytes.ends_with(b"\n"), "truncated": tail > head,
        "head": String::from_utf8_lossy(&bytes[..head]),
        "tail": String::from_utf8_lossy(&bytes[tail..])
    })
}

impl CommandRunner for NoLocaleRunner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        let mut environment = request.environment.clone();
        for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
            environment.set.remove(key);
            environment.remove.insert(key.into());
        }
        let receipt = self.0.run(&CommandRequest {
            executable: request.executable.clone(),
            arguments: request.arguments.clone(),
            cwd: request.cwd.clone(),
            environment,
            stdin: request.stdin.clone(),
            budget: request.budget,
            limits: request.limits,
            reject_stdout: request.reject_stdout,
        });
        if request.arguments.iter().any(|arg| arg == "capture-pane") {
            if self.1.len() == 3 {
                self.1.remove(0);
            }
            self.1.push(json!({
                "end": format!("{:?}", receipt.end), "exit_code": receipt.exit_code,
                "elapsed_ms": receipt.elapsed.as_millis(), "stdout": fixture_bytes(&receipt.stdout)
            }));
        }
        receipt
    }
}

fn raw_tmux(
    host: &mut TmuxHost<NoLocaleRunner>,
    binary: &std::path::Path,
    endpoint: &std::path::Path,
    arguments: &[&str],
) -> CommandReceipt {
    let mut argv = vec!["-u".into(), "-S".into(), endpoint.as_os_str().to_owned()];
    argv.extend(
        arguments
            .iter()
            .map(|argument| std::ffi::OsString::from(*argument)),
    );
    host.runner_mut().run(&CommandRequest {
        executable: binary.to_path_buf(),
        arguments: argv,
        cwd: None,
        environment: EnvironmentDelta {
            remove: BTreeSet::from(["TMUX".into(), "TMUX_PANE".into()]),
            set: BTreeMap::new(),
        },
        stdin: None,
        reject_stdout: None,
        budget: Duration::from_secs(3),
        limits: OutputLimits {
            stdout: 65536,
            stderr: 65536,
            stdin: 1,
        },
    })
}

// An ignored helper in this very test executable is the native image. Libtest
// accepts multiple positional filters: only the fixed helper is an ignored
// test; the remaining filters carry controlled model/path/prompt/server argv.
// The `--` boundary preserves literal operands (including quotes and newlines).
#[test]
#[ignore = "internal owned TTY fixture; invoked only by host_native cases"]
fn native_terminal_fixture() {
    native_terminal::run();
}

// Failure-only, metadata-only sampling of this test's owned child. A fresh
// capture is diagnostic, never a replacement stamp adopted by the host.
fn owned_process_diagnostic(pid: u32, executable: &Path, hash: Digest) -> String {
    #[cfg(target_os = "macos")]
    let image = i32::try_from(pid)
        .ok()
        .and_then(|pid| libproc::libproc::proc_pid::pidpath(pid).ok());
    #[cfg(target_os = "linux")]
    let image = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    let observed = capture_process(pid, executable, hash, Duration::from_secs(2));
    format!("pid={pid}; kernel_image={image:?}; fixed_image_capture={observed:?}")
}

static NEXT_FIXTURE_ROOT: AtomicU64 = AtomicU64::new(0);

fn native_fixture_root(nonce: u128) -> PathBuf {
    let sequence = NEXT_FIXTURE_ROOT.fetch_add(1, Ordering::Relaxed);
    PathBuf::from("/tmp").join(format!("tac-k2-{}-{nonce}-{sequence}", std::process::id()))
}

fn native_case(mode: &str, content: &str, expected_keys: usize) {
    native_case_on(mode, content, expected_keys, false, false);
}

fn native_case_on(mode: &str, content: &str, expected_keys: usize, shared_workspace: bool, new_session: bool) {
    let tmux = configured_binary("CONTRACT_TMUX");
    assert_eq!(
        std::env::var("CONTRACT_OLD_SCOPE").as_deref(),
        Ok("fixture-old-scope"),
        "prepare the positive unset control"
    );
    assert_eq!(
        std::env::var("TEAM_AGENT_K2_FOREIGN_PROBE").as_deref(),
        Ok("fixture-foreign-team"),
        "prepare the inherited identity control"
    );
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = native_fixture_root(nonce);
    let mut identity = owner();
    identity.instance = InstanceId::new(format!("native-{mode}-{nonce}")).unwrap();
    let directory = ScopedDirectory::create(&root, identity.clone()).unwrap();
    println!(
        "CONTROLLED_NATIVE_FIXTURE_ROOT={}",
        directory.path().display()
    );
    let cwd_path = directory.path().join("work '中文'");
    std::fs::create_dir(&cwd_path).unwrap();
    let cwd = resolve_cwd(&cwd_path).unwrap();
    let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
    let (executable, native_hash) = native_terminal::copy_executable(directory.path());
    let n = native(native_hash);
    let d = descriptor(mode, &n);
    let h = native_terminal::hooks();
    let launch = resolve_launch(
        &d,
        &h,
        &LaunchRequest {
            provider: "physical-fixture".into(),
            operation: Operation::Fresh,
            mode: LaunchMode::FullWorker,
            auth: AuthMode::NativeSubscription,
            model: Some(mode.into()),
            role_effort: None,
            team_effort: None,
            bypass: false,
            prompt: Some("role 'quotes' 中文\nnext line".into()),
            identity,
            native: n,
            paths: LaunchPaths {
                executable: executable.clone(),
                candidate,
                cwd: cwd.clone(),
                runtime_root: directory.path().to_path_buf(),
            },
            channel: Channel::Tmux,
            input_profile: Some("physical-fixture".into()),
            evidence_kind: EvidenceKind::Fixture,
            preassigned_session: None,
            resume: None,
            fork: None,
        },
        None,
    )
    .unwrap();
    let plan = team_agent_contract::contract::hooks::PlanHook::plan(&native_terminal::PLAN, &launch)
        .unwrap();
    let events_name = native_terminal::event_log_name(&launch.request().identity);
    let clock = RealClock::new();
    let limits = HostLimits {
        command_output: OutputLimits { stdout: 256 * 1024, stderr: 65536, stdin: 2 * 1024 * 1024 },
        max_script_bytes: 128 * 1024, max_capture_bytes: 256 * 1024,
        max_payload_bytes: 1024 * 1024, max_executable_bytes: 512 * 1024 * 1024,
        poll_interval: Duration::from_millis(10), columns: 120, rows: 40,
    };
    // An actual independent native peer owns the fixture server. The second
    // host may borrow it, but can never close the peer or remove its socket.
    let mut anchor = None;
    let shared = if shared_workspace {
        let mut request = launch.request().clone();
        request.identity.seat = SeatId::new("workspace-peer").unwrap();
        request.identity.instance = InstanceId::new(format!("anchor-{nonce}")).unwrap();
        let anchor_dir = ScopedDirectory::create(&directory.path().join("anchor"), request.identity.clone()).unwrap();
        request.paths.runtime_root = anchor_dir.path().to_owned();
        let anchor_launch = resolve_launch(&d, &h, &request, None).unwrap();
        let anchor_plan = team_agent_contract::contract::hooks::PlanHook::plan(&native_terminal::PLAN, &anchor_launch).unwrap();
        let mut peer = TmuxHost::new(anchor_dir, tmux.clone(), limits, NoLocaleRunner::default()).unwrap();
        let spawned = peer.spawn_owned(&d, &anchor_launch, &anchor_plan, &MaterializeReceipt { resources: vec![] },
            &clock, clock.now() + Duration::from_secs(20));
        assert!(spawned.problem.is_none(), "anchor: {spawned:?}");
        let session_name = spawned.pane.as_ref().unwrap().session_name.clone();
        let target = spawned.target.unwrap();
        assert!(std::env::var_os("TEAM_AGENT_SHARED_SERVER_ONLY_IDENTITY").is_none());
        assert!(raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["set-environment", "-g", "TEAM_AGENT_SHARED_SERVER_ONLY_IDENTITY", "foreign-server-identity"]).success());
        assert!(raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["set-window-option", "-g", "remain-on-exit", "off"]).success());
        let global = raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["show-options", "-gwv", "remain-on-exit"]);
        assert!(global.success(), "global option baseline: end={:?} exit={:?} stderr={}",
            global.end, global.exit_code, String::from_utf8_lossy(&global.stderr));
        assert_eq!(global.stdout, b"off\n");
        let shared = SharedTmux { endpoint: target.endpoint.clone(),
            socket: Some((target.socket_device, target.socket_inode)),
            session_name: if new_session { format!("team-shared-{nonce}") } else { session_name },
            session: if new_session { None } else { Some(target.session.clone()) } };
        let refused_dir = ScopedDirectory::create(&directory.path().join("refused"), launch.request().identity.clone()).unwrap();
        let mut wrong = shared.clone(); wrong.session_name = "not-the-authorized-session".into();
        wrong.session = Some(target.session.clone());
        let mut refused = TmuxHost::new_shared(refused_dir, tmux.clone(), limits, NoLocaleRunner::default(), wrong).unwrap();
        let before = raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["list-windows", "-a", "-F", "#{window_id}"]).stdout;
        let result = refused.spawn_owned(&d, &launch, &plan, &MaterializeReceipt { resources: vec![] },
            &clock, clock.now() + Duration::from_secs(5));
        assert!(result.problem.is_some()); assert!(!result.may_have_spawned); assert!(result.target.is_none());
        assert_eq!(raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["list-windows", "-a", "-F", "#{window_id}"]).stdout, before);
        let bad_dir = ScopedDirectory::create(&directory.path().join("wrong-inode"), launch.request().identity.clone()).unwrap();
        let mut wrong_inode = shared.clone(); wrong_inode.socket.as_mut().unwrap().1 += 1;
        assert!(TmuxHost::new_shared(bad_dir, tmux.clone(), limits, NoLocaleRunner::default(), wrong_inode).is_err());
        anchor = Some((peer, target, global.stdout));
        Some(shared)
    } else { None };
    let mut host = match shared {
        Some(shared) => TmuxHost::new_shared(directory, tmux.clone(), limits, NoLocaleRunner::default(), shared),
        None => TmuxHost::new(directory, tmux.clone(), limits, NoLocaleRunner::default()),
    }.unwrap();
    let spawn = host.spawn_owned(
        &d,
        &launch,
        &plan,
        &MaterializeReceipt { resources: vec![] },
        &clock,
        clock.now() + Duration::from_secs(20),
    );
    assert!(
        spawn.problem.is_none(),
        "spawn receipt: {spawn:?}; owned process: {:?}",
        spawn
            .pane
            .as_ref()
            .map(|pane| owned_process_diagnostic(pane.pid, &executable, native_hash))
    );
    let target = spawn.target.unwrap();
    assert_eq!(target.shared_endpoint, shared_workspace);
    if let Some((_, peer, _)) = &anchor {
        assert_eq!(target.endpoint, peer.endpoint);
        assert_eq!(target.created_session, new_session);
        if new_session { assert_ne!(target.session, peer.session); }
        else { assert_eq!(target.session, peer.session); }
        assert_ne!(target.pane, peer.pane);
        assert_ne!(target.window, peer.window);
        let mut forged = target.clone(); forged.shared_endpoint = false;
        assert!(TmuxHost::restore_owned(forged, tmux.clone(), limits, NoLocaleRunner::default(),
            &clock, clock.now() + Duration::from_secs(5)).is_err());
        let mut forged = target.clone(); forged.created_session = !target.created_session;
        assert!(TmuxHost::restore_owned(forged, tmux.clone(), limits, NoLocaleRunner::default(),
            &clock, clock.now() + Duration::from_secs(5)).is_err());
        host = TmuxHost::restore_owned(target.clone(), tmux.clone(), limits, NoLocaleRunner::default(),
            &clock, clock.now() + Duration::from_secs(5)).unwrap();
    }
    let tmux_hash = fingerprint_file(&tmux, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    let server = capture_process(
        target.process.parent,
        &tmux,
        tmux_hash,
        Duration::from_secs(5),
    )
    .unwrap();
    let ready_until = clock.now() + Duration::from_secs(5);
    loop {
        let frame = host
            .capture(&target, &clock, ready_until, Duration::from_millis(500))
            .unwrap_or_else(|error| {
                panic!(
                    "controlled fixture capture: {error}; expected={:?}; {}",
                    target.process,
                    owned_process_diagnostic(target.process.identity.pid, &executable, native_hash)
                )
            });
        if frame.text.lines().rfind(|line| !line.trim().is_empty()) == Some("READY|-") {
            break;
        }
        assert!(
            clock.now() < ready_until,
            "controlled fake composer did not become ready"
        );
        clock.sleep(Duration::from_millis(10));
    }
    assert!(raw_tmux(
        &mut host,
        &tmux,
        &target.endpoint,
        &["set-buffer", "-b", "foreign-fixture-buffer", "preserve-me"]
    )
    .success());
    let direct = mode == "literal-control";
    let operation = if direct {
        Operation::SessionInspect
    } else {
        Operation::OrdinarySend
    };
    let p = policy(&d, &h, &target, operation);
    let envelope = envelope("native-message", content);
    let input = if direct {
        PreparedInput::control(
            OperationId::new("native-control").unwrap(),
            &team_agent_contract::contract::fork::NativeControl::InspectSession,
            &ControlDefinition {
                profile_id: "physical-fixture",
                policy_sha256: POLICY,
                operation,
                kind: ControlKind::InspectSession,
                command: "/fixture-session",
                input_mode: ControlInputMode::DirectTyping,
            },
        )
        .unwrap()
    } else {
        PreparedInput::business(&envelope)
    };
    let attempt = AttemptId::new("native-attempt").unwrap();
    let proto = protocol(&target, clock.now());
    let mut journal = FileJournal::new(
        ScopedDirectory::reopen(host.directory().receipt().clone()).unwrap(),
        2 * 1024 * 1024,
    )
    .unwrap();
    let report = deliver_envelope(
        &mut host,
        InjectionRequest {
            target: &target,
            input: &input,
            attempt: &attempt,
            operation,
            policy: &p,
            protocol: &proto,
            protocol_requirement: ProtocolRequirement::ServerAndClientBinding,
            server_key: "fixture-server",
            deadline: clock.now() + Duration::from_secs(10),
            freshness: Duration::from_millis(500),
            bootstrap: None,
            lane: None,
        },
        &mut journal,
        None,
        &clock,
    );
    if report.disposition != InjectionDisposition::NativeAccepted {
        let events = match host.directory().read_file(&events_name, 2 * 1024 * 1024) {
            Ok(bytes) => fixture_bytes(&bytes),
            Err(error) => json!({"read_error": error.to_string()}),
        };
        // Preserve the actual captures used during injection, not a new capture
        // after the deadline. No corrective input, retry, or extra native action.
        println!("CONTROLLED_NATIVE_FAILURE={}", json!({
            "case": mode, "last_captures": host.runner_mut().1, "helper_events": events
        }));
    }
    assert_eq!(
        report.disposition,
        InjectionDisposition::NativeAccepted,
        "physical receipt: {report:?}"
    );
    assert_eq!(report.effect_floor, DeliveryEffect::Submitted);
    assert_eq!(report.counts.paste.confirmed, 1);
    assert_eq!(report.counts.keys_issued() as usize, expected_keys);
    assert_eq!(
        journal.recover(&report.metadata),
        RecoveryState::Complete(DeliveryEffect::Submitted)
    );
    assert_eq!(
        raw_tmux(
            &mut host,
            &tmux,
            &target.endpoint,
            &["save-buffer", "-b", "foreign-fixture-buffer", "-"]
        )
        .stdout,
        b"preserve-me"
    );
    let names = raw_tmux(
        &mut host,
        &tmux,
        &target.endpoint,
        &["list-buffers", "-F", "#{buffer_name}"],
    );
    assert!(names.success());
    assert!(!String::from_utf8(names.stdout)
        .unwrap()
        .lines()
        .any(|name| name.starts_with("tac-")));
    let raw = host
        .directory()
        .read_file(&events_name, 2 * 1024 * 1024)
        .unwrap();
    let events: Vec<Value> = std::str::from_utf8(&raw)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let pastes: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "paste")
        .collect();
    if direct {
        assert!(pastes.is_empty(), "direct control must not use bracketed paste");
        let typed: Vec<u8> = events
            .iter()
            .filter(|event| event["kind"] == "typed_byte")
            .map(|event| u8::try_from(event["byte"].as_u64().unwrap()).unwrap())
            .collect();
        assert_eq!(typed.as_slice(), input.bytes());
    } else {
        assert_eq!(pastes.len(), 1);
        assert_eq!(pastes[0]["payload_sha256"], digest_hex(digest(input.bytes())));
        assert_eq!(pastes[0]["payload_bytes"], input.bytes().len());
        assert_eq!(pastes[0]["payload"], json!(input.bytes()));
    }
    let keys: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "key")
        .collect();
    assert_eq!(keys.len(), expected_keys);
    assert!(keys.iter().all(|key| key["key"] == "Enter"));
    assert!(!events
        .iter()
        .any(|event| event["kind"] == "unexpected_byte"));
    assert_eq!(events[0]["pid"], target.process.identity.pid);
    assert_eq!(events[0]["mode"], mode);
    assert_eq!(events[0]["prompt"], launch.request().prompt.as_deref().unwrap());
    assert_eq!(events[0]["server"], "fixture-server");
    assert_eq!(events[0]["cwd"], cwd.path.to_str().unwrap());
    assert_eq!(events[0]["fixture_env"], "quotes ' 中文\nnew line");
    assert_eq!(events[0]["old_scope_present"], false);
    assert_eq!(events[0]["inherited_team_identity_present"], false);
    let close = host.close_owned_pane(&target, &clock, clock.now() + Duration::from_secs(5));
    assert_eq!(
        close.pane_close.outcome,
        StepOutcome::Confirmed,
        "close receipt: {close:?}"
    );
    assert_eq!(
        close.native,
        ProcessState::Exited,
        "close receipt: {close:?}"
    );
    assert_eq!(close.socket_cleanup.outcome, StepOutcome::Confirmed, "{close:?}");
    assert!(close.socket_cleanup.error.is_none());
    assert_eq!(close.shared_endpoint, shared_workspace);
    if let Some((mut peer, peer_target, global_before)) = anchor {
        assert!(close.socket_preserved, "borrowed socket must remain: {close:?}");
        assert_eq!(sample_process(&peer_target.process), ProcessState::Alive);
        let current = std::fs::symlink_metadata(&target.endpoint).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!((current.dev(), current.ino()), (target.socket_device, target.socket_inode));
        assert_eq!(raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["show-buffer", "-b", "foreign-fixture-buffer"]).stdout, b"preserve-me");
        let global = raw_tmux(&mut peer, &tmux, &target.endpoint,
            &["show-options", "-gwv", "remain-on-exit"]);
        assert!(global.success(), "global option after borrowed close: end={:?} exit={:?} stderr={}",
            global.end, global.exit_code, String::from_utf8_lossy(&global.stderr));
        assert_eq!(global.stdout, global_before);
        let peer_closed = peer.close_owned_pane(&peer_target, &clock, clock.now() + Duration::from_secs(5));
        assert_eq!(peer_closed.pane_close.outcome, StepOutcome::Confirmed, "{peer_closed:?}");
        assert_eq!(peer_closed.native, ProcessState::Exited);
        assert!(!peer_closed.socket_preserved, "the independent owner cleans up only after its own final close");
        peer.runner_mut().0.reap_owned();
        assert!(peer.runner_mut().0.pending_children().is_empty());
    } else { assert!(!close.socket_preserved); }
    assert_eq!(std::fs::symlink_metadata(&target.endpoint).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    let wait_until = clock.now() + Duration::from_secs(3);
    while sample_process(&server) == ProcessState::Alive && clock.now() < wait_until {
        clock.sleep(Duration::from_millis(10));
    }
    assert_eq!(sample_process(&server), ProcessState::Exited);
    host.runner_mut().0.reap_owned();
    assert!(host.runner_mut().0.pending_children().is_empty());
    // Retain the exact owned fixture/journal files for the builder's receipt/archive.
    // Never manually unlink a tmux socket, kill a server, or touch a default endpoint.
    let summary = json!({"fixture":"not-kiro","case":mode,"tmux_client_locale":"LC_ALL/LC_CTYPE/LANG unset","root":host.directory().path(),"payload_sha256":digest_hex(digest(input.bytes())),
        "native_pid":target.process.identity.pid,"native_birth":target.process.identity.birth_identity,
        "native_executable_sha256":digest_hex(target.native.executable_sha256),"candidate_sha256":digest_hex(target.candidate_sha256),
        "policy_sha256":digest_hex(POLICY),"endpoint":target.endpoint,"pane":target.pane,
        "tmux_server_pid":server.identity.pid,"tmux_server_birth":server.identity.birth_identity,
        "paste_confirmed":report.counts.paste.confirmed,"keys_issued":report.counts.keys_issued(),"initial":report.counts.initial.confirmed,
        "confirmation":report.counts.confirmation.confirmed,"retry":report.counts.retry.confirmed,"wrap_gap":report.counts.wrap_gap.confirmed,
        "queue":report.counts.queue.confirmed,"native_exited":true,"private_tmux_server_exited":!shared_workspace,
        "shared_endpoint":target.shared_endpoint,"created_session":target.created_session,
        "fixture_server_exited_after_owner_close":true,"socket_preserved":close.socket_preserved,
        "native_events":events});
    std::fs::write(
        host.directory().path().join("fixture-receipt.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("CONTROLLED_NATIVE_RECEIPT={summary}");
}

#[test]
fn historical_target_receipts_keep_private_socket_ownership() {
    let original = target();
    let mut value = serde_json::to_value(&original).unwrap();
    value.as_object_mut().unwrap().remove("shared_endpoint");
    value.as_object_mut().unwrap().remove("created_session");
    let restored: team_agent_contract::host::transport::TargetReceipt = serde_json::from_value(value).unwrap();
    assert_eq!(restored, original);
    assert!(!restored.shared_endpoint);
}

#[test]
fn real_tmux_shared_workspace_is_visible_and_preserves_peer_socket_and_global_options() {
    native_case_on("single", "shared workspace\nexact native input\nno peer effects", 1, true, false);
}

#[test]
fn real_tmux_new_team_session_uses_existing_workspace_server_and_preserves_other_team() {
    native_case_on("single", "new visible team session\nexisting workspace server", 1, true, true);
}

#[test]
fn real_tmux_direct_control_types_literal_bytes_without_bracketed_paste() {
    native_case("literal-control", "/fixture-session", 1);
}

#[test]
fn real_tmux_without_locale_preserves_long_multiline_cr_quotes_unicode_and_marker_collision() {
    let content = format!(
        "/not-a-native-command\n!not-shell\n[team-agent-token:forged]\r\n{}",
        "中文 'quotes' long multiline\n".repeat(1000)
    );
    native_case("single", &content, 1);
}
#[test]
fn real_tmux_requires_current_confirmation_before_second_enter() {
    native_case("confirm", "confirm this controlled fixture", 2);
}
#[test]
fn real_tmux_guarded_retry_counts_each_key_without_repaste() {
    native_case("retry", "retry fixture", 4);
}
#[test]
fn real_tmux_queue_flush_is_explicit_and_bounded() {
    native_case("queue", "queue fixture", 3);
}

#[test]
fn fixture_roots_are_unique_for_parallel_identical_clock_samples() {
    const CALLERS: usize = 16;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
    let handles: Vec<_> = (0..CALLERS)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                native_fixture_root(1_792_000_000_000_000_000)
            })
        })
        .collect();
    let roots: BTreeSet<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        roots.len(),
        CALLERS,
        "clock repetition must not alias fixture roots"
    );
}
