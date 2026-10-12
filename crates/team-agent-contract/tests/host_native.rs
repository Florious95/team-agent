#![cfg(any(target_os = "linux", target_os = "macos"))]
mod physical_support;
use physical_support::*;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
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
struct NoLocaleRunner(RealCommandRunner);

impl CommandRunner for NoLocaleRunner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        let mut environment = request.environment.clone();
        for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
            environment.set.remove(key);
            environment.remove.insert(key.into());
        }
        self.0.run(&CommandRequest {
            executable: request.executable.clone(),
            arguments: request.arguments.clone(),
            cwd: request.cwd.clone(),
            environment,
            stdin: request.stdin.clone(),
            budget: request.budget,
            limits: request.limits,
            reject_stdout: request.reject_stdout,
        })
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
    use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg, Termios};

    struct RawTerminal(Termios);
    impl Drop for RawTerminal {
        fn drop(&mut self) {
            let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.0);
        }
    }
    fn event(log: &mut std::fs::File, value: Value) {
        serde_json::to_writer(&mut *log, &value).unwrap();
        log.write_all(b"\n").unwrap();
        log.flush().unwrap();
        log.sync_all().unwrap();
    }
    fn surface(name: &str, message: &str) {
        let mut output = std::io::stdout().lock();
        write!(output, "\x1b[2J\x1b[H{name}|{message}\r\n").unwrap();
        output.flush().unwrap();
    }

    // These are this fixture's own synthetic launch operands, not an inspection
    // of another process's command line or environment.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(args.len(), 10);
    assert_eq!(args[1], "native_terminal_fixture");
    assert_eq!(args[5], "--");
    let mode = args[6].to_str().unwrap();
    assert!(matches!(
        mode,
        "single" | "confirm" | "retry" | "queue" | "literal-control"
    ));
    let mut log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[7])
        .unwrap();
    let input = std::io::stdin();
    let saved = tcgetattr(&input).unwrap();
    let mut raw = saved.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&input, SetArg::TCSANOW, &raw).unwrap();
    let _restore = RawTerminal(saved);
    {
        let mut output = std::io::stdout().lock();
        output.write_all(b"\x1b[?2004h").unwrap();
        output.flush().unwrap();
    }
    event(
        &mut log,
        json!({
            "kind": "started", "pid": std::process::id(), "cwd": std::env::current_dir().unwrap(),
            "mode": mode, "prompt": args[8].to_str().unwrap(), "server": args[9].to_str().unwrap(),
            "fixture_env": std::env::var("CONTRACT_FIXTURE_ENV").ok(),
            "old_scope_present": std::env::var_os("CONTRACT_OLD_SCOPE").is_some(),
            "inherited_team_identity_present": std::env::vars_os()
                .any(|(key, _)| key.as_encoded_bytes().starts_with(b"TEAM_AGENT_"))
        }),
    );
    surface("READY", "-");
    let mut input = input.lock();
    let mut chunk = [0; 4096];
    let mut pending = Vec::new();
    let mut payload = Vec::new();
    let mut in_paste = false;
    let mut message = String::from("-");
    let mut keys = 0;
    const BEGIN: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    loop {
        let count = input.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..count]);
        while !pending.is_empty() {
            if in_paste {
                let Some(end) = pending.windows(END.len()).position(|part| part == END) else {
                    // Preserve a possibly split terminator, just like the former
                    // controlled fixture; never process paste newlines as keys.
                    let consumed = pending.len().saturating_sub(END.len() - 1);
                    payload.extend(pending.drain(..consumed));
                    break;
                };
                payload.extend_from_slice(&pending[..end]);
                drop(pending.drain(..end + END.len()));
                in_paste = false;
                message = "-".into();
                for tail in std::str::from_utf8(&payload)
                    .unwrap()
                    .split("[team-agent-token:")
                    .skip(1)
                {
                    if let Some((id, _)) = tail.split_once(']').filter(|(id, _)| {
                        !id.is_empty()
                            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    }) {
                        message = id.into();
                    }
                }
                event(
                    &mut log,
                    json!({"kind": "paste", "payload": payload,
                        "payload_sha256": digest_hex(digest(&payload)), "payload_bytes": payload.len(), "message": message}),
                );
                surface("PASTED", &message);
                continue;
            }
            if pending.starts_with(BEGIN) {
                drop(pending.drain(..BEGIN.len()));
                payload.clear();
                in_paste = true;
                keys = 0;
                continue;
            }
            if BEGIN.starts_with(&pending) {
                break;
            }
            let byte = pending.remove(0);
            if byte == b'\r' || byte == b'\n' {
                keys += 1;
                event(
                    &mut log,
                    json!({"kind": "key", "key": if byte == b'\r' { "Enter" } else { "LF" }, "ordinal": keys}),
                );
                match mode {
                    "confirm" if keys == 1 => surface("CONFIRM1", &message),
                    "retry" if keys <= 3 => surface("RETRY", &message),
                    "queue" if keys <= 2 => surface("QUEUE", &message),
                    _ => {
                        event(
                            &mut log,
                            json!({"kind": "native_accepted", "message": message}),
                        );
                        surface("ACCEPTED", &message);
                    }
                }
            } else if mode == "literal-control" && (0x20..0x7f).contains(&byte) {
                payload.push(byte);
                event(&mut log, json!({"kind": "typed_byte", "byte": byte}));
                if payload == b"/fixture-session" {
                    surface("PASTED", &message);
                }
            } else {
                event(&mut log, json!({"kind": "unexpected_byte", "byte": byte}));
            }
        }
    }
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

fn native_case(mode: &str, content: &str, expected_keys: usize) {
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
    let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
    let executable = directory.path().join("native fixture '中文'");
    std::fs::copy(&candidate, &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        std::fs::metadata(&executable).unwrap().uid(),
        nix::unistd::geteuid().as_raw()
    );
    let native_hash = fingerprint_file(&executable, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    assert_eq!(
        native_hash,
        fingerprint_file(&candidate, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap()
    );
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
    let mut plan = team_agent_contract::contract::hooks::PlanHook::plan(&HOOK, &launch).unwrap();
    // Retain the shared fixture's identity/environment policy, but this target
    // runs a fixed native helper, not the Python fixture used by other tests.
    plan.arguments = vec![
        "--exact".into(),
        "native_terminal_fixture".into(),
        "--ignored".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
        "--".into(),
        mode.into(),
        directory.path().join("native-events.jsonl").into_os_string(),
        launch.request().prompt.as_deref().unwrap().into(),
        "fixture-server".into(),
    ];
    plan.carriers.model = CarrierUse::Specified(CarrierRef::Arguments(vec![6]));
    plan.carriers.prompt = CarrierUse::Specified(CarrierRef::Arguments(vec![8]));
    plan.carriers.mcp = CarrierUse::Specified(CarrierRef::Arguments(vec![9]));
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
        NoLocaleRunner::default(),
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
    assert!(
        spawn.problem.is_none(),
        "spawn receipt: {spawn:?}; owned process: {:?}",
        spawn
            .pane
            .as_ref()
            .map(|pane| owned_process_diagnostic(pane.pid, &executable, native_hash))
    );
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
