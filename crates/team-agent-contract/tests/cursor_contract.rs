//! Controlled leaf regressions, not native launch/Team acceptance. Catalog fixture
//! is stdout from cursor-recon/receipts/direct-catalog.json (exit 0), SHA256
//! 7454b02927fa2f3016fc4b24f82008b7e04b9dc6b9a6209a0fbf2a11df7c7c51.
#![cfg(unix)]
mod support;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use support::{request, Sandbox};
use team_agent_contract::contract::{
    delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*,
};
use team_agent_contract::host::{command::*, digest, process::resolve_cwd};
use team_agent_contract::providers::cursor::{interaction::*, native::*, *};

const CATALOG: &[u8] = include_bytes!("fixtures/cursor-2026.10.01-models.txt");
const LAUNCHER: &[u8] = b"fixture public launcher, never executed";
fn adapter() -> CursorAdapter {
    CursorAdapter::new(McpStdio {
        executable: "/fixture/candidate".into(),
        arguments: vec!["contract-mcp".into(), "scope with spaces".into()],
        environment: BTreeMap::from([("FIXTURE_SCOPE".into(), "owned".into())]),
    }).unwrap()
}
fn bounds() -> ReadBounds {
    ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 1024 * 1024 }
}
fn grant(sandbox: &Sandbox) -> CatalogRequest {
    let executable = sandbox.parent.join("cursor-agent");
    std::fs::write(&executable, LAUNCHER).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    CatalogRequest {
        provider: ProviderId::new("cursor").unwrap(),
        native: NativeIdentity {
            version: "2026.10.01-e373342".into(), harness: METADATA_HARNESS.into(),
            ui: METADATA_UI.into(), platform: Platform::Linux, executable_sha256: digest(LAUNCHER),
        },
        executable, cwd: resolve_cwd(&sandbox.parent).unwrap(), source: CATALOG_SOURCE, bounds: bounds(),
    }
}
struct Runner {
    args: Vec<&'static str>, output: Option<CommandReceipt>, calls: usize, replace: Option<PathBuf>,
}
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        self.calls += 1;
        assert_eq!(request.arguments.iter().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>(), self.args);
        assert!(request.stdin.is_none());
        assert!(request.environment.set.is_empty());
        assert!(request.environment.remove.is_empty());
        assert!(request.budget <= bounds().deadline);
        if let Some(path) = &self.replace { std::fs::write(path, b"replaced launcher").unwrap(); }
        self.output.take().expect("no fallback or retry")
    }
}
fn runner(args: Vec<&'static str>, end: CommandEnd, code: Option<i32>, stdout: &[u8]) -> Runner {
    Runner {
        args, calls: 0, replace: None,
        output: Some(CommandReceipt {
            end, exit_code: code, child_pid: Some(42), child_reaped: true,
            stdout: stdout.to_vec(), stderr: vec![], elapsed: Duration::from_millis(1),
        }),
    }
}

#[test]
fn descriptor_and_seven_hooks_keep_unknown_lifecycle_and_legacy_aliases_closed() {
    let adapter = adapter();
    let hooks = adapter.hooks();
    assert_eq!(CURSOR_DESCRIPTOR.identity.id, "cursor");
    assert!(CURSOR_DESCRIPTOR.identity.aliases.is_empty());
    assert!(validate_descriptor(&CURSOR_DESCRIPTOR, &hooks, Operation::Catalog).is_ok());
    assert!(hooks.catalog.require("H1").is_ok());
    assert!(hooks.plan.require("H2").is_ok());
    assert!(hooks.materialize.require("H3").is_ok());
    assert!(hooks.session.require("H4").is_ok());
    assert!(hooks.semantic.require("H5").is_ok());
    assert!(hooks.interaction.require("H6").is_ok());
    assert!(hooks.fork.require("H7").is_ok());
    assert_eq!(CURSOR_DESCRIPTOR.mcp.isolation, McpIsolation::AmbientExcluded);
    assert_eq!(CURSOR_DESCRIPTOR.prompt.carrier, Support::Supported(PromptCarrier::RuntimeBridge));
    assert_eq!(CURSOR_DESCRIPTOR.tool_names.naming, Support::Supported(ToolNaming::StaticServer { key: "team" }));
    assert_eq!(CURSOR_DESCRIPTOR.session.fresh, FreshSession::Preassigned);
    assert_eq!(CURSOR_DESCRIPTOR.session.resume, Support::Supported(ResumeMode::ExactId));
    assert_eq!(CURSOR_DESCRIPTOR.probe_sources.client_binding, Support::Supported(ProbeSource::NativeRuntime));
    assert_eq!(CURSOR_DESCRIPTOR.probe_sources.round_trip, Support::Supported(ProbeSource::NativeRuntime));
    assert_eq!(CURSOR_DESCRIPTOR.model.omitted, OmittedModel::Reject);
    assert!(!CURSOR_DESCRIPTOR.effort.inherit_team_default);
    for admission in &CURSOR_DESCRIPTOR.effort.admission { assert!(matches!(admission, EffortAdmission::Reject(_))); }
    for op in [Operation::InWindowBranch, Operation::NewSeatFullSnapshot, Operation::NativeNewSeat] {
        assert!(matches!(validate_descriptor(&CURSOR_DESCRIPTOR, &hooks, op), Err(ContractError::Unverified { .. })));
    }
    // Even an independently resolved controlled fake must not get a Cursor plan.
    let r = request(std::path::Path::new("/fixture"), "worker");
    let resolved = resolve_launch(&support::descriptor(), &support::hooks(), &r, None).unwrap();
    assert!(matches!(adapter.plan(&resolved), Err(ContractError::Unverified { .. })));
}

#[test]
fn metadata_probe_is_evergreen_but_never_claims_node_or_a_tui_profile() {
    let sandbox = Sandbox::new();
    let grant = grant(&sandbox);
    for version in ["2026.10.01-e373342", "2027.01.02-aabbccdd"] {
        let mut runner = runner(vec!["--version"], CommandEnd::Exited, Some(0), format!("{version}\n").as_bytes());
        let native = probe_launcher(&grant.executable, &mut runner, bounds()).unwrap();
        assert_eq!(native.version, version);
        assert_eq!(native.executable_sha256, digest(LAUNCHER));
        assert_eq!(native.harness, METADATA_HARNESS);
        assert_eq!(native.ui, METADATA_UI);
        assert!(!PROFILES[0].matches_native(&native));
        assert_eq!(runner.calls, 1);
    }
    for invalid in ["2.28.0", "2026.10.01-e373342\nextra", "2026.13.01-abc", "2026.01.00-abc", "2026.10.01-shell"] {
        assert!(parse_version(invalid.as_bytes()).is_err());
    }
    let error = parse_version(b"unexpected-banner").unwrap_err().to_string();
    assert!(error.contains("unexpected-banner"));
    let alias = sandbox.parent.join("agent");
    std::os::unix::fs::symlink(&grant.executable, &alias).unwrap();
    assert_eq!(resolve_launcher(&alias).unwrap(), grant.executable.canonicalize().unwrap());
    let proxy = sandbox.parent.join("cursor");
    std::fs::write(&proxy, LAUNCHER).unwrap();
    assert!(resolve_launcher(&proxy).is_err());
}

#[test]
fn dynamic_catalog_preserves_exact_ids_without_default_or_effort_invention() {
    let sandbox = Sandbox::new();
    let grant = grant(&sandbox);
    let mut reader = NativeCatalog::new(grant.clone(), runner(vec!["--list-models"], CommandEnd::Exited, Some(0), CATALOG)).unwrap();
    let catalog = adapter().discover(&grant, &mut reader).unwrap();
    let ids: Vec<_> = catalog.models.iter().map(|model| model.id.as_str()).collect();
    assert!(ids.len() > 100);
    assert!(ids.contains(&"gpt-5.6-luna-xhigh"));
    assert!(ids.contains(&"gpt-5.6-luna-max-fast"));
    assert_eq!(ids[0], "auto"); // present in catalog, never selected by the adapter
    assert!(!ids.contains(&"GPT-5.6 Luna 1M Extra High"));
    assert_eq!(catalog.provider, grant.provider);
    assert_eq!(catalog.native, grant.native);
    assert_eq!(reader.runner_mut().calls, 1);
    assert!(catalog.models.iter().all(|model| matches!(model.efforts, Support::Unverified(_))));
    let text = b"Available models\n\nFuture-Case-42 - Future model\n\nTip: use --model <id> (or /model <id> in interactive mode) to switch.\n";
    assert_eq!(parse_catalog(text).unwrap()[0].id, "Future-Case-42");
}

#[test]
fn catalog_schema_bounds_exit_and_captured_identity_cannot_fall_back() {
    for bad in [
        "", "{}", "Available models\n", "Available models\nmodel - Name\n",
        "Available models\nx - Name\nx - Other\nTip: use --model <id> (or /model <id> in interactive mode) to switch.",
        "Available models\nx - Name\nTip: use --model <id> (or /model <id> in interactive mode) to switch.\ntrailing",
    ] { assert!(parse_catalog(bad.as_bytes()).is_err()); }
    for (end, code) in [(CommandEnd::TimedOut, None), (CommandEnd::OutputLimit, None), (CommandEnd::Exited, Some(1)), (CommandEnd::NotStarted, None)] {
        let sandbox = Sandbox::new();
        let grant = grant(&sandbox);
        let mut reader = NativeCatalog::new(grant.clone(), runner(vec!["--list-models"], end, code, CATALOG)).unwrap();
        assert!(adapter().discover(&grant, &mut reader).is_err());
        assert_eq!(reader.runner_mut().calls, 1);
    }
    let sandbox = Sandbox::new();
    let grant = grant(&sandbox);
    let mut reader = NativeCatalog::new(grant.clone(), runner(vec!["--list-models"], CommandEnd::Exited, Some(0), CATALOG)).unwrap();
    let mut wrong = grant.clone();
    wrong.source.arguments = &["login"];
    assert!(reader.read_catalog(&wrong).is_err());
    wrong = grant.clone();
    wrong.native.harness = "cursor-agent".into();
    assert!(adapter().discover(&wrong, &mut reader).is_err());
    std::fs::write(&grant.executable, b"replacement").unwrap();
    assert!(reader.read_catalog(&grant).is_err());
    assert_eq!(reader.runner_mut().calls, 0);
}

#[test]
fn launcher_mutation_during_metadata_read_is_rejected() {
    let sandbox = Sandbox::new();
    let grant = grant(&sandbox);
    let mut run = runner(vec!["--list-models"], CommandEnd::Exited, Some(0), CATALOG);
    run.replace = Some(grant.executable.clone());
    let mut reader = NativeCatalog::new(grant.clone(), run).unwrap();
    assert!(matches!(reader.read_catalog(&grant), Err(ReadFailure::Error(ContractError::Mismatch(_)))));
    assert_eq!(reader.runner_mut().calls, 1);
}

struct Io { writes: usize, wrong_hash: bool }
impl OwnedIo for Io {
    fn create_exclusive(&mut self, r: &OwnedResourceRequest) -> Result<OwnedResourceReceipt, PartialFailure> {
        self.writes += 1;
        Ok(OwnedResourceReceipt {
            path: r.path.clone(), owner: r.owner.clone(), operation: OperationId::new("fixture-write").unwrap(),
            kind: r.kind, disposition: ResourceDisposition::OwnedRemovable,
            write_effect: ResourceWriteEffect::Written { bytes_sha256: if self.wrong_hash { digest(b"wrong") } else { digest(&r.contents) } },
            exclusive: true, creation_identity: Some(digest(b"fixture inode")),
        })
    }
    fn read_bound_session(&mut self, _: &ResumeBinding, _: ReadBounds) -> Result<ReadOutput, ReadFailure> { panic!("no session I/O") }
    fn validate_configuration(&mut self, _: &OwnedValidationRequest) -> Result<ReadOutput, ReadFailure> { panic!("no guessed native validator") }
}
#[test]
fn owned_mcp_shape_preserves_stdio_arguments_without_global_merge_or_launch_admission() {
    let adapter = adapter();
    let owner = request(std::path::Path::new("/fixture"), "worker").identity;
    let resource = adapter.mcp_resource(owner.clone(), "/owned workspace".into()).unwrap();
    assert_eq!(resource.path.relative(), std::path::Path::new(".cursor/mcp.json"));
    assert_eq!(resource.kind, ResourceKind::McpConfig);
    let value: serde_json::Value = serde_json::from_slice(&resource.contents).unwrap();
    assert_eq!(value["mcpServers"]["team"]["args"][1], "scope with spaces");
    assert_eq!(value["mcpServers"]["team"]["command"], "/fixture/candidate");
    assert_eq!(value.as_object().unwrap().len(), 1);
    let mut io = Io { writes: 0, wrong_hash: false };
    let receipt = adapter.materialize(std::slice::from_ref(&resource), &mut io).unwrap();
    assert_eq!(receipt.resources[0].owner, owner);
    assert_eq!(io.writes, 1);
    let mut wrong = resource.clone();
    wrong.contents.extend_from_slice(b" "); // only the exact framework-owned encoding is admitted
    assert!(adapter.materialize(&[wrong], &mut io).is_err());
    wrong = resource.clone();
    wrong.path = OwnedPath::new("/owned workspace".into(), ".kiro/mcp.json".into()).unwrap();
    assert!(adapter.materialize(&[wrong], &mut io).is_err());
    assert_eq!(io.writes, 1);
    io.wrong_hash = true;
    let failure = adapter.materialize(&[resource], &mut io).unwrap_err();
    assert_eq!(failure.receipt.resources.len(), 1); // preserve possible write effect
}

fn scope() -> EvidenceScope {
    EvidenceScope {
        identity: request(std::path::Path::new("/fixture"), "worker").identity,
        endpoint: "/fixture/socket".into(), pane: "%0".into(), binding: "fixture".into(), session: None,
        sequence: 1, observed_at: Duration::ZERO, valid_for: Duration::from_secs(2), source: "controlled-cursor-shape".into(),
    }
}
fn frame(body: &str) -> CaptureFrame {
    CaptureFrame {
        scope: scope(), text: format!("  Cursor Agent\n  v2026.10.01-e373342\n\n{body}\n\n  GPT-5.6 Luna 1M Extra High   Run Everything\n  /owned workspace\n"),
        cwd: None, input: None,
        baseline: None, profile_id: PROFILE.into(), operation: Operation::OrdinarySend,
        message: None, attempt: None, after_step: None, paste_latch: PasteLatch::NeverSeen,
    }
}
#[test]
fn current_cursor_composer_and_braille_busy_status_do_not_use_kiro_grammar() {
    let adapter = adapter();
    for placeholder in ["Plan, search, build anything", "Add a follow-up"] {
        assert_eq!(adapter.interpret(&frame(&format!("  → {placeholder}"))).surface, InputSurface::ComposerReady);
        assert_eq!(adapter.interpret(&frame(&format!("  → {placeholder}   ctrl+c to stop"))).surface, InputSurface::Busy);
    }
    for spinner in ["⠀⠞", "⠘⠆", "⠘⠣", "⠘⠤", "⠠⠛", "⠠⠜", "⠰⠰", "⠰⠳"] {
        for counter in ["", "  1 token", "  4 tokens"] {
            let busy = format!(" {spinner} Working{counter}\n  Tip: Use subagents to parallelize work and preserve context.\n\n  → Add a follow-up");
            assert_eq!(adapter.interpret(&frame(&busy)).surface, InputSurface::Busy);
        }
        let old = format!(" {spinner} Working\n\n  Natural answer\n\n  → Add a follow-up");
        assert_eq!(adapter.interpret(&frame(&old)).surface, InputSurface::ComposerReady);
    }
    assert_eq!(adapter.interpret(&frame("  → draft Working ctrl+c to stop")).surface, InputSurface::ComposerContainsPaste);
    assert_eq!(adapter.interpret(&frame("  › ask a question")).surface, InputSurface::ShellOrUnknown);
    let mut wrong = frame("  → Add a follow-up");
    wrong.profile_id = "kiro-v2-tui-macos".into();
    assert_eq!(adapter.interpret(&wrong).surface, InputSurface::ShellOrUnknown);
}

#[test]
fn recipe_is_single_enter_but_metadata_or_echo_cannot_admit_business_acceptance() {
    let policy = submit_policy(InputTiming {
        capture_interval: Duration::from_millis(100), stable_window: Duration::from_millis(200),
        paste_to_submit_floor: Duration::from_millis(100), deadline: Duration::from_secs(30),
    }).unwrap(); // controlled timing values, not survey-derived production defaults
    assert_eq!(policy.paste_mode, PasteMode::Bracketed);
    assert_eq!(policy.payload_trailer, PayloadTrailer::None);
    assert_eq!(policy.initial_submit, PhysicalKey::Enter);
    assert_eq!(policy.key_upper_bound().unwrap(), 1);
    assert_eq!(policy.retry_budget, RetryBudget::Never);
    assert!(policy.queue_flush.is_empty());
    assert!(CONTROLS.is_empty());
    assert_eq!(PROFILES[0].policy.require("policy").unwrap(), &policy);
    let mut capture = frame("  [team-agent-token:msg-1]\n  → Add a follow-up");
    capture.message = Some(MessageId::new("msg-1").unwrap());
    capture.attempt = Some(AttemptId::new("attempt-1").unwrap());
    capture.after_step = Some(StepKind::InitialSubmit);
    let observed = adapter().interpret(&capture);
    assert_ne!(observed.surface, InputSurface::NativeAccepted);
    assert!(observed.current_message.is_none());
    assert!(observed.current_attempt.is_none());
    assert_eq!(observed.paste_latch, PasteLatch::NeverSeen);
}

#[test]
fn session_exit_hint_protocol_echo_and_exit_zero_are_not_lifecycle_proof() {
    let adapter = adapter();
    let sandbox = Sandbox::new();
    let grant = grant(&sandbox);
    let capture = ScopedSessionEvidence {
        scope: scope(), provider: grant.provider, native: grant.native, cwd: grant.cwd,
        origin: CaptureOrigin::OwnedExitHint, evidence_kind: EvidenceKind::Fixture,
        evidence_sha256: digest(b"resume hint"), record: b"Resume with cursor-agent --resume 12345678-1234-1234-1234-123456789012".to_vec(),
    };
    assert!(matches!(adapter.bind(&capture), Err(ContractError::Mismatch(_))));
    let record = NativeRecord { scope: scope(), kind: NativeRecordKind::Protocol, bytes: frame("  → Add a follow-up").text.into_bytes() };
    assert!(matches!(adapter.read(&record).outcome, ProbeOutcome::Unknown(_)));
    let semantic = adapter.read(&NativeRecord { kind: NativeRecordKind::Surface, ..record });
    assert!(matches!(semantic.outcome, ProbeOutcome::Observed(SemanticEvidence::Idle)));
    for code in [Some(0), Some(1), Some(3), Some(4), None] {
        let receipt = classify_exit(code, DeliveryEffect::MayHaveSubmitted);
        assert_eq!(receipt.delivery_floor, DeliveryEffect::MayHaveSubmitted);
        assert!(receipt.native_effects_may_have_occurred);
        assert_eq!(receipt.class, if code == Some(0) { ExitClass::CommandSucceeded } else { ExitClass::Unknown });
    }
}
