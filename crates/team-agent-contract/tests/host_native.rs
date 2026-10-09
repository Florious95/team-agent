#![cfg(any(target_os = "linux", target_os = "macos"))]
mod physical_support;
use physical_support::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
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

fn raw_tmux(
    host: &mut TmuxHost<RealCommandRunner>,
    binary: &std::path::Path,
    endpoint: &std::path::Path,
    arguments: &[&str],
) -> CommandReceipt {
    let mut argv = vec!["-S".into(), endpoint.as_os_str().to_owned()];
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

fn native_case(mode: &str, content: &str, expected_keys: usize) {
    let tmux = configured_binary("CONTRACT_TMUX");
    let python = configured_binary("CONTRACT_PYTHON");
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
    let root = PathBuf::from("/tmp").join(format!("tac-k2-{}-{nonce}", std::process::id()));
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
    let native_hash = fingerprint_file(&python, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    let n = native(native_hash);
    let d = descriptor(mode, &n);
    let h = hooks();
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
                executable: python,
                candidate: std::env::current_exe().unwrap(),
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
    let plan = team_agent_contract::contract::hooks::PlanHook::plan(&HOOK, &launch).unwrap();
    let clock = RealClock::new();
    let mut host = TmuxHost::new(
        directory,
        tmux.clone(),
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
        },
        RealCommandRunner::default(),
    )
    .unwrap();
    let spawn = host.spawn_owned(
        &d,
        &launch,
        &plan,
        &MaterializeReceipt { resources: vec![] },
        &clock,
        clock.now() + Duration::from_secs(20),
    );
    assert!(spawn.problem.is_none(), "spawn receipt: {spawn:?}");
    let target = spawn.target.unwrap();
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
            .unwrap();
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
    let p = policy(&d, &h, &target, Operation::OrdinarySend);
    let envelope = envelope("native-message", content);
    let input = PreparedInput::business(&envelope);
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
            operation: Operation::OrdinarySend,
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
        .read_file("native-events.jsonl", 2 * 1024 * 1024)
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
    assert_eq!(pastes.len(), 1);
    assert_eq!(
        pastes[0]["payload_sha256"],
        digest_hex(digest(envelope.rendered().as_bytes()))
    );
    assert_eq!(pastes[0]["payload_bytes"], envelope.rendered().len());
    let keys: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "key")
        .collect();
    assert_eq!(keys.len(), expected_keys);
    assert!(keys.iter().all(|key| key["key"] == "Enter"));
    assert!(!events
        .iter()
        .any(|event| event["kind"] == "unexpected_byte"));
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
    let wait_until = clock.now() + Duration::from_secs(3);
    while sample_process(&server) == ProcessState::Alive && clock.now() < wait_until {
        clock.sleep(Duration::from_millis(10));
    }
    assert_eq!(sample_process(&server), ProcessState::Exited);
    host.runner_mut().reap_owned();
    assert!(host.runner_mut().pending_children().is_empty());
    // Retain the exact owned fixture/journal files for the builder's receipt/archive.
    // Never manually unlink a tmux socket, kill a server, or touch a default endpoint.
    let summary = json!({"fixture":"not-kiro","case":mode,"root":host.directory().path(),"payload_sha256":digest_hex(digest(envelope.rendered().as_bytes())),
        "native_pid":target.process.identity.pid,"native_birth":target.process.identity.birth_identity,
        "native_executable_sha256":digest_hex(target.native.executable_sha256),"candidate_sha256":digest_hex(target.candidate_sha256),
        "policy_sha256":digest_hex(POLICY),"endpoint":target.endpoint,"pane":target.pane,
        "tmux_server_pid":server.identity.pid,"tmux_server_birth":server.identity.birth_identity,
        "paste_confirmed":report.counts.paste.confirmed,"keys_issued":report.counts.keys_issued(),"initial":report.counts.initial.confirmed,
        "confirmation":report.counts.confirmation.confirmed,"retry":report.counts.retry.confirmed,"wrap_gap":report.counts.wrap_gap.confirmed,
        "queue":report.counts.queue.confirmed,"native_exited":true,"private_tmux_server_exited":true,"socket_preserved":close.socket_preserved,
        "native_events":events});
    std::fs::write(
        host.directory().path().join("fixture-receipt.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("CONTROLLED_NATIVE_RECEIPT={summary}");
}

#[test]
fn real_tmux_single_paste_preserves_long_multiline_cr_quotes_unicode_and_marker_collision() {
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
