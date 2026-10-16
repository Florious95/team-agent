//! Controlled command/plan/grammar fixtures. These do NOT execute Cursor, prove
//! subscription dispatch, empty-chat durability, or replace the native micro-probe.
#![cfg(unix)]
mod support;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use support::Sandbox;
use team_agent_contract::contract::{delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*};
use team_agent_contract::host::{command::*, digest, materialize::ScopedMaterializer, process::*};
use team_agent_contract::providers::cursor::{interaction::*, native::*, *};

const VERSION: &str = "2026.10.01-e373342";
const SID: &str = "12345678-1234-1234-1234-123456789012";
const LAUNCHER: &[u8] = b"controlled launcher fixture";
struct Runner { outputs: VecDeque<CommandReceipt>, calls: Vec<(PathBuf, Vec<OsString>)> }
impl Runner {
    fn outputs(outputs: &[&[u8]]) -> Self {
        Self {
            outputs: outputs.iter().map(|stdout| CommandReceipt {
                end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(42), child_reaped: true,
                stdout: stdout.to_vec(), stderr: vec![], elapsed: Duration::from_millis(1),
            }).collect(), calls: vec![],
        }
    }
}
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        assert!(request.stdin.is_none());
        if request.arguments.last().is_some_and(|argument| argument == "create-chat") {
            assert_eq!(request.environment.set.get("CURSOR_INVOKED_AS"), Some(&OsString::from("cursor-agent")));
            assert!(request.environment.set.contains_key("HOME"));
            assert!(request.environment.remove.contains("CURSOR_API_KEY"));
            assert!(request.environment.remove.contains("CURSOR_API_ENDPOINT"));
        }
        self.calls.push((request.executable.clone(), request.arguments.clone()));
        self.outputs.pop_front().expect("no extra command or fallback")
    }
}
fn bounds() -> ReadBounds { ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 } }
fn installation(sandbox: &Sandbox) -> (RuntimeInstallation, CatalogObservation, Runner) {
    let dir = sandbox.parent.join(VERSION);
    std::fs::create_dir(&dir).unwrap();
    let launcher = dir.join("cursor-agent");
    for (path, bytes) in [(launcher.clone(), LAUNCHER), (dir.join("node"), b"controlled node image".as_slice()), (dir.join("index.js"), b"controlled entry module".as_slice())] {
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let home = sandbox.parent.join("auth-home");
    std::fs::create_dir(&home).unwrap();
    let mut runner = Runner::outputs(&[format!("{VERSION}\n").as_bytes(), b"v22.19.0\n"]);
    let runtime = probe_runtime(&launcher, &home, &mut runner, bounds()).unwrap();
    let metadata = NativeIdentity {
        harness: METADATA_HARNESS.into(), ui: METADATA_UI.into(), executable_sha256: digest(LAUNCHER),
        ..runtime.native.clone()
    };
    let catalog = CatalogObservation {
        provider: ProviderId::new("cursor").unwrap(), native: metadata, schema: CATALOG_SOURCE.schema.into(),
        models: vec![ModelRecord { id: "gpt-5.6-luna-xhigh".into(), efforts: Support::Unverified(UNVERIFIED) }],
    };
    (runtime, catalog, runner)
}
fn adapter(runtime: RuntimeInstallation, project: PathBuf) -> CursorAdapter {
    CursorAdapter::new(McpStdio {
        executable: "/fake/candidate".into(), arguments: vec!["contract-mcp".into()], environment: Default::default(),
    }).unwrap().with_runtime(runtime).with_project(project).unwrap()
}
fn fixture(sandbox: &Sandbox) -> (CursorAdapter, LaunchRequest, CatalogObservation) {
    let (mut runtime, metadata, _) = installation(sandbox);
    // Only macOS is natively surveyed. This Linux-runnable plan fixture explicitly
    // supplies a synthetic Mac identity; it does not claim Linux native admission.
    runtime.native.platform = Platform::MacOs;
    let catalog = runtime.bind_catalog(&metadata).unwrap();
    let project = sandbox.parent.join("project");
    let cwd = sandbox.parent.join("private-seat");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let mut r = support::request(&sandbox.root, "worker");
    r.provider = "cursor".into();
    r.native = runtime.native.clone();
    r.paths.executable = runtime.engine.clone();
    r.paths.cwd = resolve_cwd(&cwd).unwrap();
    r.model = Some("gpt-5.6-luna-xhigh".into());
    r.prompt = Some("user global\nproject global\nrole\nexplicit TEAM".into());
    r.input_profile = Some(PROFILE.into());
    r.preassigned_session = Some(NativeSessionId::new(SID).unwrap());
    (adapter(runtime, project), r, catalog)
}
#[test]
fn direct_runtime_and_catalog_bridge_bind_node_and_entry_not_launcher_bytes() {
    let sandbox = Sandbox::new();
    let (runtime, metadata, runner) = installation(&sandbox);
    assert_eq!(runner.calls.len(), 2);
    assert_eq!(runner.calls[1].0, runtime.engine);
    assert_eq!(runner.calls[1].1, vec![OsString::from("--use-system-ca"), OsString::from("--version")]);
    assert_eq!(runtime.native.executable_sha256, digest(b"controlled node image"));
    assert_ne!(runtime.native.executable_sha256, metadata.native.executable_sha256);
    assert_eq!(runtime.executable_input(1).sha256, digest(b"controlled entry module"));
    assert_eq!(runtime.bind_catalog(&metadata).unwrap().native, runtime.native);
    let mut wrong = metadata.clone(); wrong.native.version = "2027.01.01-abcdef".into();
    assert!(runtime.bind_catalog(&wrong).is_err());
    wrong = metadata; wrong.provider = ProviderId::new("kiro").unwrap();
    assert!(runtime.bind_catalog(&wrong).is_err());
    let global = runtime.home().join(".cursor"); std::fs::create_dir(&global).unwrap();
    // Metadata-only refusal: bytes need never be read or merged.
    std::fs::write(global.join("mcp.json"), b"not inspected").unwrap();
    assert!(require_no_global_mcp(runtime.home()).is_err());
}
#[test]
fn native_allocator_needs_real_exit_zero_and_never_reuses_timeout_uuid() {
    let sandbox = Sandbox::new();
    let (runtime, _, _) = installation(&sandbox);
    let cwd = resolve_cwd(&sandbox.parent).unwrap();
    let mut runner = Runner::outputs(&[format!("{SID}\n").as_bytes()]);
    assert_eq!(allocate_chat(&runtime, &cwd, &mut runner, bounds()).unwrap().as_str(), SID);
    assert_eq!(runner.calls[0].1, vec![OsString::from("--use-system-ca"), runtime.entry().as_os_str().to_owned(), OsString::from("create-chat")]);
    for (end, code) in [(CommandEnd::TimedOut, None), (CommandEnd::Exited, Some(1)), (CommandEnd::OutputLimit, None)] {
        let mut runner = Runner::outputs(&[SID.as_bytes()]);
        runner.outputs[0].end = end; runner.outputs[0].exit_code = code;
        assert!(allocate_chat(&runtime, &cwd, &mut runner, bounds()).is_err());
        assert_eq!(runner.calls.len(), 1);
    }
    for bad in ["", "latest", "1234", "chat: 12345678-1234-1234-1234-123456789012", "12345678-1234-1234-1234-123456789012\nextra"] {
        assert!(parse_chat_id(bad.as_bytes()).is_err());
    }
}
#[test]
fn fresh_plan_uses_private_mcp_approved_exact_session_and_prompt_bridge() {
    let sandbox = Sandbox::new();
    let (adapter, mut r, catalog) = fixture(&sandbox);
    for bypass in [false, true] {
        r.bypass = bypass;
        let resolved = resolve_launch(&CURSOR_DESCRIPTOR, &adapter.hooks(), &r, Some(&catalog)).unwrap();
        let plan = adapter.plan(&resolved).unwrap();
        validate_launch_plan(&CURSOR_DESCRIPTOR, &resolved, &plan).unwrap();
        assert!(plan.arguments.iter().any(|arg| arg == "--approve-mcps"));
        assert!(plan.arguments.windows(2).any(|pair| pair[0] == "--resume" && pair[1] == SID));
        assert!(plan.arguments.windows(2).any(|pair| pair[0] == "--workspace" && pair[1] == r.paths.cwd.path.as_os_str()));
        assert_eq!(plan.arguments.iter().any(|arg| arg == "--force"), bypass);
        assert!(!plan.arguments.iter().any(|arg| arg == "--continue" || arg == "--session-id" || arg == "--sandbox"));
        assert_eq!(plan.executable_inputs.len(), 1);
        assert_eq!(plan.arguments[plan.executable_inputs[0].argument], plan.executable_inputs[0].path.as_os_str());
        assert_eq!(plan.materialization.len(), 2);
        assert_eq!(plan.materialization[1].kind, ResourceKind::Prompt);
        assert_eq!(plan.materialization[1].contents, r.prompt.as_ref().unwrap().as_bytes());
        assert!(plan.arguments.iter().all(|arg| arg != r.prompt.as_ref().unwrap().as_str()));
        let config: serde_json::Value = serde_json::from_slice(&plan.materialization[0].contents).unwrap();
        assert_eq!(config["mcpServers"]["team"]["command"], "/fake/candidate");
        assert!(plan.environment.remove.contains("CURSOR_API_KEY"));
        let mut invalid = plan.clone(); invalid.executable_inputs[0].argument = 0;
        assert!(validate_launch_plan(&CURSOR_DESCRIPTOR, &resolved, &invalid).is_err());
    }
}
#[test]
fn pure_preflight_cannot_materialize_or_promote_a_missing_native_session_id() {
    let sandbox = Sandbox::new();
    let (adapter, mut r, catalog) = fixture(&sandbox);
    r.preassigned_session = None;
    assert!(resolve_launch(&CURSOR_DESCRIPTOR, &adapter.hooks(), &r, Some(&catalog)).is_err());
    let resolved = resolve_launch_preflight(&CURSOR_DESCRIPTOR, &adapter.hooks(), &r, Some(&catalog)).unwrap();
    assert_eq!(resolved.expected_session(), &ExpectedSession::PendingAllocation);
    let plan = adapter.plan(&resolved).unwrap();
    validate_launch_plan(&CURSOR_DESCRIPTOR, &resolved, &plan).unwrap();
    let runner = Runner::outputs(&[]);
    assert!(ScopedMaterializer::new(&CURSOR_DESCRIPTOR, &resolved, &plan, OperationId::new("preflight").unwrap(), runner).is_err());
    assert!(!r.paths.cwd.path.join(".cursor").exists());
}
#[test]
fn exact_resume_never_allocates_a_new_session_or_switches_cwd() {
    let sandbox = Sandbox::new();
    let (adapter, mut r, catalog) = fixture(&sandbox);
    let source = r.identity.clone();
    let mut evidence = ScopedSessionEvidence {
        scope: EvidenceScope { identity: source.clone(), ..scope() }, provider: ProviderId::new("cursor").unwrap(),
        cwd: r.paths.cwd.clone(), native: r.native.clone(), origin: CaptureOrigin::ExplicitResumeLaunch,
        evidence_kind: EvidenceKind::Fixture, evidence_sha256: digest(SID.as_bytes()), record: SID.as_bytes().to_vec(),
    };
    let binding = adapter.bind(&evidence).unwrap();
    assert_eq!(binding.origin, CaptureOrigin::ExplicitResumeLaunch);
    assert_eq!(binding.native_session.as_str(), SID);
    assert_eq!(binding.source, source);
    assert_eq!(binding.storage, SessionStorage::Local);
    assert!(binding.backing.is_none()); // not fabricated from an account-store scan
    evidence.origin = CaptureOrigin::CurrentNativeSession;
    assert!(adapter.bind(&evidence).is_err());
    evidence.origin = CaptureOrigin::ExplicitResumeLaunch;
    evidence.evidence_sha256 = digest(b"different record");
    assert!(adapter.bind(&evidence).is_err());
    r.operation = Operation::Resume; r.preassigned_session = None;
    r.identity.instance = InstanceId::new("worker-2").unwrap(); r.identity.generation = Generation(2);
    r.resume = Some(ResumeRequest { binding, expected_source: source });
    let resolved = resolve_launch(&CURSOR_DESCRIPTOR, &adapter.hooks(), &r, Some(&catalog)).unwrap();
    let plan = adapter.plan(&resolved).unwrap();
    assert!(plan.arguments.windows(2).any(|pair| pair[0] == "--resume" && pair[1] == SID));
    r.paths.cwd.identity = digest(b"replaced cwd");
    assert!(resolve_launch(&CURSOR_DESCRIPTOR, &adapter.hooks(), &r, Some(&catalog)).is_err());
}
#[test]
fn interpreter_entry_stamps_reject_replacement_without_process_argv_reads() {
    let sandbox = Sandbox::new();
    let (runtime, _, _) = installation(&sandbox);
    let inputs = capture_executable_inputs(&[runtime.executable_input(1)], 1024 * 1024, Duration::from_secs(2)).unwrap();
    assert!(verify_executable_inputs(&inputs).is_ok());
    std::fs::write(runtime.entry(), b"replacement module with a different length").unwrap();
    assert!(verify_executable_inputs(&inputs).is_err());
    assert!(capture_executable_inputs(&[runtime.executable_input(1)], 1024 * 1024, Duration::from_secs(2)).is_err());
}
fn scope() -> EvidenceScope {
    EvidenceScope {
        identity: support::request(std::path::Path::new("/fixture"), "worker").identity,
        endpoint: "/fixture/socket".into(), pane: "%0".into(), binding: "fixture".into(), session: None,
        sequence: 1, observed_at: Duration::ZERO, valid_for: Duration::from_secs(2), source: "controlled-cursor-token-frame".into(),
    }
}
fn text(body: &str) -> String { format!("Cursor Agent\nv{VERSION}\n\n{body}\n\nGPT-5.6 Luna Extra High Run Everything\n/owned cwd") }
#[test]
fn business_acceptance_reuses_seen_then_submit_and_transcript_anti_replay_fence() {
    let adapter = CursorAdapter::new(McpStdio { executable: "/fixture/candidate".into(), arguments: vec![], environment: Default::default() }).unwrap();
    let token = "[team-agent-token:msg-current]";
    let mut baseline_scope = scope();
    baseline_scope.session = Some(NativeSessionId::new(SID).unwrap());
    let mut current_scope = baseline_scope.clone();
    current_scope.sequence += 1;
    let mut frame = CaptureFrame {
        scope: current_scope, text: text(&format!("→ payload\n  {token}")),
        cwd: None, input: None,
        baseline: Some(CaptureBaseline { scope: baseline_scope, text: text("→ Add a follow-up") }),
        profile_id: PROFILE.into(), operation: Operation::FirstBusiness,
        message: Some(MessageId::new("msg-current").unwrap()), attempt: Some(AttemptId::new("attempt-current").unwrap()),
        after_step: Some(StepKind::Paste), paste_latch: PasteLatch::NeverSeen,
    };
    // Each missing prerequisite independently refuses this otherwise valid
    // controlled frame; do not relax production grammar to repair the fixture.
    let mut no_footer = frame.clone();
    no_footer.text = no_footer.text.replace(" Run Everything", "");
    let mut no_session = frame.clone();
    no_session.scope.session = None;
    no_session.baseline.as_mut().unwrap().scope.session = None;
    let mut same_sequence = frame.clone();
    same_sequence.scope.sequence = same_sequence.baseline.as_ref().unwrap().scope.sequence;
    for invalid in [no_footer, no_session, same_sequence] {
        let observed = adapter.interpret(&invalid);
        assert_eq!(observed.paste_latch, PasteLatch::NeverSeen);
        assert!(observed.current_message.is_none() && observed.current_attempt.is_none());
        assert_ne!(observed.surface, InputSurface::NativeAccepted);
    }
    frame.paste_latch = adapter.interpret(&frame).paste_latch;
    assert!(matches!(frame.paste_latch, PasteLatch::Seen { .. }));
    frame.scope.sequence += 1;
    frame.text = text(&format!("payload\n{token}\n\n⠘⠆ Working  1 token\n\n→ Add a follow-up  ctrl+c to stop"));
    assert_ne!(adapter.interpret(&frame).surface, InputSurface::NativeAccepted);
    frame.scope.sequence += 1;
    frame.after_step = Some(StepKind::InitialSubmit);
    let accepted = adapter.interpret(&frame);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.current_message, frame.message);
    assert_eq!(accepted.current_attempt, frame.attempt);
    for bad in [text("→ Add a follow-up"), text(&format!("→ draft {token}")), text(&format!("{token}\n→ new draft"))] {
        frame.scope.sequence += 1;
        frame.text = bad;
        assert_ne!(adapter.interpret(&frame).surface, InputSurface::NativeAccepted);
    }
    frame.scope.sequence += 1;
    frame.after_step = Some(StepKind::Paste); frame.paste_latch = PasteLatch::NeverSeen;
    frame.text = text(&format!("→ payload {token}"));
    frame.baseline.as_mut().unwrap().text = text(&format!("old {token}\n→ Add a follow-up"));
    assert_eq!(adapter.interpret(&frame).paste_latch, PasteLatch::NeverSeen);
}

#[test]
fn first_business_prompt_is_receipt_bound_verbatim_and_not_repeated_on_ordinary_send() {
    use team_agent_contract::orchestration::{physical::prepare_business_envelope, supervisor::DeliveryJob};
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = support::FakeHost::default();
    let mut io = support::FakeIo::new("prompt-fixture");
    let mut target = support::start(&mut store, &mut host, &mut io, "worker").target;
    let prompt = "user global\nproject global\nrole [team-agent-token:decoy]\nTEAM exact instructions";
    target.resources[0].write_effect = ResourceWriteEffect::Written { bytes_sha256: digest(prompt.as_bytes()) };
    let mut descriptor = support::descriptor();
    descriptor.prompt.carrier = Support::Supported(PromptCarrier::RuntimeBridge);
    let mut job = DeliveryJob {
        target, envelope: LogicalEnvelope {
            message: MessageId::new("message-real").unwrap(), sender: SeatId::new("leader").unwrap(),
            task: Some("task-real".into()), content: "123 * 45".into(),
        },
        attempt: AttemptId::new("attempt-real").unwrap(), operation: Operation::FirstBusiness,
        channel: Channel::Tmux, policy_sha256: POLICY,
    };
    let prepared = prepare_business_envelope(&descriptor, &job, Some(prompt)).unwrap();
    assert_eq!(prepared.rendered(), format!("Team Agent message from leader for task-real:\n\n{prompt}\n\n123 * 45\n\n[team-agent-token:message-real]"));
    assert_eq!(&prepared.rendered()[prepared.token_range()], "[team-agent-token:message-real]");
    assert_eq!(job.envelope.content, "123 * 45");
    assert!(prepare_business_envelope(&descriptor, &job, None).is_err());
    assert!(prepare_business_envelope(&descriptor, &job, Some("different prompt")).is_err());
    job.target.resources[0].owner.generation = Generation(99);
    assert!(prepare_business_envelope(&descriptor, &job, Some(prompt)).is_err());
    job.operation = Operation::OrdinarySend;
    let ordinary = prepare_business_envelope(&descriptor, &job, Some(prompt)).unwrap();
    assert!(!ordinary.rendered().contains("decoy"));
    assert!(ordinary.rendered().contains("123 * 45"));
}
