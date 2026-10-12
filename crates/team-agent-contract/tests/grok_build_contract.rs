//! Controlled Grok 1.0.52 grammar/identity regressions, not native Team G1–G5.
#![cfg(unix)]
mod support;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use team_agent_contract::contract::{delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*};
use team_agent_contract::host::{command::*, digest, process::*};
use team_agent_contract::providers::grok_build::{interaction::*, native::*, *};
const SID: &str = "1a021870-30bc-4a89-984c-f61fab7b5269";
const HEADER: &str = "Team Agent message from leader for task-1:";
const CATALOG: &str = "You are logged in with grok.com.\n\nDefault model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  - grok-4.7-build-fast\n  - grok-4.5\n  - grok-4.6\n";
fn adapter() -> GrokAdapter { GrokAdapter::new(McpStdio { executable: "/fake/candidate".into(), arguments: vec!["contract-mcp".into(), "a b/中文".into()], environment: BTreeMap::from([("OWNED_VALUE".into(), "quoted \"text\"\nnext".into())]) }).unwrap() }
fn scope(sequence: u64) -> EvidenceScope {
    EvidenceScope { identity: InstanceIdentity { scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(), instance: InstanceId::new("instance").unwrap(), generation: Generation(1) },
        endpoint: "/owned/tmux.sock".into(), pane: "%0".into(), binding: "owned".into(), session: Some(NativeSessionId::new(SID).unwrap()),
        sequence, observed_at: Duration::from_millis(sequence * 100), valid_for: Duration::from_secs(2), source: "controlled-fixture".into() }
}
fn screen(history: &str, input: &str) -> String {
    format!(" /owned/cwd   1.3K / 256K │ [Dashboard]\n{history}\n ╭──────────────────────────────────────────────────────────────────────────╮\n │ ❯ {input} │\n ╰────────────────────────────── Grok 4.7 (xhigh) · always-approve ─╯\n\n Ctrl+x:shortcuts\n")
}
fn frame() -> CaptureFrame {
    CaptureFrame { scope: scope(2), text: screen(&format!("│{HEADER}│"), "[Pasted: 161 lines]"),
        cwd: Some("/owned/cwd".into()), input: Some(InputShape { sha256: digest(b"immutable input"), lines: 161, first_line: HEADER.into() }),
        baseline: Some(CaptureBaseline { scope: scope(1), text: screen("", "") }), profile_id: PROFILE.into(), operation: Operation::FirstBusiness,
        message: Some(MessageId::new("message-1").unwrap()), attempt: Some(AttemptId::new("attempt-1").unwrap()), after_step: Some(StepKind::Paste), paste_latch: PasteLatch::NeverSeen }
}
fn session(cwd: &str, id: &str) -> String {
    format!("│ Session info   click or drag to copy │\n│ Shell version: │\n│ 1.0.52 │\n│ Session ID: │\n│ {id} │\n│ Working directory: │\n│ {cwd} │\n│ Model: Grok 4.7 │\n│ Tab switch  |  ↑/↓ scroll  |  c copy session ID  |  y copy all │\n│ Esc close │\n")
}
#[test]
fn all_facets_and_hooks_disclose_owned_server_coexistence_without_legacy_migration() {
    let a = adapter(); let hooks = a.hooks();
    assert_eq!(GROK_BUILD_DESCRIPTOR.identity.id, "grok_build");
    assert!(GROK_BUILD_DESCRIPTOR.identity.aliases.is_empty());
    for operation in [Operation::Catalog, Operation::Fresh, Operation::Resume, Operation::FirstBusiness, Operation::Stop] {
        validate_descriptor(&GROK_BUILD_DESCRIPTOR, &hooks, operation).unwrap();
    }
    assert_eq!(GROK_BUILD_DESCRIPTOR.mcp.isolation, McpIsolation::OwnedServerIsolated);
    assert_eq!(GROK_BUILD_DESCRIPTOR.tool_names.naming, Support::Supported(ToolNaming::RuntimeBound));
    hooks.catalog.require("H1").unwrap(); hooks.plan.require("H2").unwrap(); hooks.materialize.require("H3").unwrap();
    hooks.session.require("H4").unwrap(); hooks.semantic.require("H5").unwrap(); hooks.interaction.require("H6").unwrap(); hooks.fork.require("H7").unwrap();
    assert_eq!(GROK_BUILD_DESCRIPTOR.session.fresh, FreshSession::CaptureAfterLaunch);
    assert_eq!(GROK_BUILD_DESCRIPTOR.session.resume, Support::Supported(ResumeMode::ExactId));
    assert!(matches!(GROK_BUILD_DESCRIPTOR.effort.admission[4], EffortAdmission::Reject(_)));
    assert!(matches!(GROK_BUILD_DESCRIPTOR.effort.admission[5], EffortAdmission::Reject(_)));
}
#[test]
fn evergreen_version_keeps_actual_revision_and_bounded_bad_input() {
    for version in ["1.0.46 (2765805b9442)", "1.0.52 (0920c8169e27)", "2.3.101 (abcdef)"] {
        assert_eq!(parse_version(format!("grok {version}\n").as_bytes()).unwrap(), version);
    }
    for bad in ["1.0.52", "grok 1.0.52", "grok 1.0.52 (not-hex)", "grok 1.0.52 (aa)\nextra", "grok 01.0.52 (aa)"] { assert!(parse_version(bad.as_bytes()).is_err()); }
    let error = parse_version(&[b'x'; 900]).unwrap_err().to_string();
    assert!(error.contains("truncated=true")); assert!(error.len() < 450);
}
#[test]
fn exact_catalog_never_falls_back_or_invents_luna() {
    let records = parse_catalog(CATALOG.as_bytes()).unwrap();
    assert_eq!(records.iter().map(|record| record.id.as_str()).collect::<Vec<_>>(), ["grok-4.7", "grok-4.7-build-fast", "grok-4.5", "grok-4.6"]);
    for bad in [CATALOG.replace("logged in", "not logged in"), CATALOG.replace("  - grok-4.6", "  - grok-4.5"), CATALOG.replace("Default model: grok-4.7", "Default model: absent"), CATALOG.replace(" (default)", ""), format!("{CATALOG}auth error")] { assert!(parse_catalog(bad.as_bytes()).is_err()); }
}
#[test]
fn owned_toml_has_exact_stdio_env_and_per_generation_unique_key() {
    let identity = scope(1).identity;
    let resource = adapter().mcp_resource(identity.clone(), "/owned/cwd".into()).unwrap();
    assert_eq!(resource.path.relative(), Path::new(".grok/config.toml"));
    let text = String::from_utf8(resource.contents).unwrap();
    let key = server_key(&identity);
    assert!(text.contains(&format!("[mcp_servers.{key}]")));
    assert!(text.contains(&format!("[mcp_servers.{key}.env]")));
    assert!(text.contains("command = \"/fake/candidate\""));
    assert!(text.contains("enabled = true")); assert!(text.contains("a b/中文"));
    assert!(text.contains("\\\"text\\\"\\nnext"));
    let mut other = identity.clone(); other.generation = Generation(2); assert_ne!(key, server_key(&other));
    other = identity; other.seat = SeatId::new("peer").unwrap(); assert_ne!(key, server_key(&other));
}
#[test]
fn folded_seen_is_bound_to_confirmed_paste_payload_and_empty_composer() {
    let f = frame(); let observed = adapter().interpret(&f);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    assert!(matches!(observed.paste_latch, PasteLatch::Seen { .. }));
    assert_eq!(observed.current_attempt, f.attempt);
    for kind in 0..7 {
        let mut f = frame();
        match kind {
            0 => f.after_step = None, 1 => f.input = None,
            2 => f.input.as_mut().unwrap().lines = 160,
            3 => f.baseline.as_mut().unwrap().text = screen("", "unsent text"),
            4 => f.baseline.as_mut().unwrap().scope.identity.generation = Generation(9),
            5 => f.text = screen("unrelated preview", "[Pasted: 161 lines]"),
            _ => f.attempt = None,
        }
        assert!(matches!(adapter().interpret(&f).paste_latch, PasteLatch::NeverSeen), "case {kind}");
    }
}
#[test]
fn folded_enter_requires_new_matching_transcript_not_disappearance_or_busy_alone() {
    let mut f = frame(); f.paste_latch = adapter().interpret(&f).paste_latch;
    f.scope = scope(3); f.after_step = Some(StepKind::InitialSubmit);
    f.text = screen(&format!("    {HEADER}          12:10 PM\n    line-000 …\n\n  ⠼ Waiting for response… 0.2s       0.2s ⇣1.30k [stop]"), "");
    let accepted = adapter().interpret(&f);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert!(matches!(accepted.paste_latch, PasteLatch::Gone { .. }));
    for kind in 0..6 {
        let mut bad = f.clone();
        match kind {
            0 => bad.text = screen("⠼ Waiting for response… 0.2s", ""),
            1 => bad.after_step = Some(StepKind::Paste),
            2 => bad.attempt = Some(AttemptId::new("other").unwrap()),
            3 => bad.text = screen(HEADER, "remaining input"),
            4 => bad.baseline.as_mut().unwrap().text = screen(HEADER, ""),
            _ => bad.input.as_mut().unwrap().sha256 = digest(b"different payload"),
        }
        assert_ne!(adapter().interpret(&bad).surface, InputSurface::NativeAccepted, "case {kind}");
    }
}
#[test]
fn idle_single_enter_has_no_unconditional_confirmation_and_trust_has_separate_key() {
    assert_eq!(POLICY, digest(POLICY_RECIPE.as_bytes()));
    let business = PROFILES[0].policy.require("business").unwrap(); business.validate().unwrap();
    assert_eq!(business.initial_submit, PhysicalKey::Enter); assert!(business.confirmation_steps.is_empty());
    assert_eq!(business.retry_budget, RetryBudget::Never); assert_eq!(business.queue_flush.len(), 1);
    assert_eq!(business.queue_flush[0].max_presses, 1); assert_eq!(business.max_submit_keys, 2);
    let trust = PROFILES[1].policy.require("startup").unwrap(); trust.validate().unwrap();
    assert!(trust.queue_flush.is_empty()); assert_eq!(STARTUP.key, PhysicalKey::ConfirmYes);
    assert!(CONTROLS.iter().any(|control| control.command == "/exit" && control.operation == Operation::Stop));
    assert!(CONTROLS.iter().all(|control| control.command != "/tools"));
}
#[test]
fn queue_flush_requires_the_exact_current_visible_token_and_footer() {
    let mut f = frame(); f.input = None; f.scope = scope(3); f.after_step = Some(StepKind::InitialSubmit);
    let token = "[team-agent-token:message-1]";
    f.paste_latch = PasteLatch::Seen { native_identity: token.into() };
    f.text = screen(&format!("#1 {token}\nQueued · Enter to send now"), "");
    let queued = adapter().interpret(&f);
    assert_eq!(queued.surface, InputSurface::Queued);
    assert_eq!(queued.predicate.as_deref(), Some(PROFILES[0].policy.require("queue").unwrap().queue_flush[0].predicate));
    for bad in ["1 queued · Enter: send now", "Queued · Enter to send now old", "Enter:send now"] {
        let mut wrong = f.clone(); wrong.text = screen(&format!("#1 {token}\n{bad}"), "");
        assert!(adapter().interpret(&wrong).predicate.is_none());
    }
    let mut other = f.clone(); other.text = screen("#1 another message\nQueued · Enter to send now", "");
    assert!(adapter().interpret(&other).predicate.is_none());
    f.after_step = Some(StepKind::QueueFlush); f.text = screen(token, "");
    assert_eq!(adapter().interpret(&f).surface, InputSurface::NativeAccepted);
}
#[test]
fn visible_multiline_token_uses_existing_correlation_not_folded_fallback() {
    let mut f = frame(); f.input = None;
    f.text = "╭────────────────────────╮\n│ ❯ message first line │\n│ [team-agent-token:message-1] │\n╰──────── Grok 4.7 (xhigh) ─╯\n".into();
    let observed = adapter().interpret(&f);
    assert_eq!(observed.paste_latch, PasteLatch::Seen { native_identity: "[team-agent-token:message-1]".into() });
}
#[test]
fn trust_requires_exact_owned_cwd_and_never_uses_history_mentions() {
    let mut f = frame(); f.profile_id = STARTUP_PROFILE.into(); f.operation = Operation::StartupBypassAck;
    f.message = None;
    f.text = "Do you trust the contents of this directory?\n/owned/cwd\nGrok Build may run or modify contents in this directory,\nposing security risks.\nYes, proceed       y\nNo, quit        n\n".into();
    assert_eq!(adapter().interpret(&f).surface, InputSurface::StartupRiskWarning);
    f.cwd = Some("/other/cwd".into()); assert_ne!(adapter().interpret(&f).surface, InputSurface::StartupRiskWarning);
    f.cwd = Some("/owned/cwd".into()); f.text = screen(&f.text, ""); assert_ne!(adapter().interpret(&f).surface, InputSurface::StartupRiskWarning);
}
#[test]
fn current_session_is_native_modal_uuid_with_matching_cwd_not_launch_origin() {
    let text = session("/owned/cwd", SID);
    assert_eq!(session_id(&text, Path::new("/owned/cwd")).unwrap().as_str(), SID);
    for bad in [text.replace("Esc close", ""), text.replace("/owned/cwd", "/other"), text.replace(SID, "latest"), format!("Here is your Session ID: {SID}"), format!("{text}\n│ Session ID: │\n│ {SID} │")] { assert!(session_id(&bad, Path::new("/owned/cwd")).is_err()); }
    let mut evidence = ScopedSessionEvidence { scope: scope(1), provider: ProviderId::new("grok_build").unwrap(),
        native: NativeIdentity { version: "1.0.52 (0920c8169e27)".into(), harness: "grok-build".into(), ui: "tui".into(), platform: Platform::MacOs, executable_sha256: digest(b"native") },
        cwd: CwdIdentity { path: "/owned/cwd".into(), identity: digest(b"cwd") }, origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: EvidenceKind::Fixture, evidence_sha256: digest(text.as_bytes()), record: text.into_bytes() };
    assert_eq!(adapter().bind(&evidence).unwrap().native_session.as_str(), SID);
    evidence.origin = CaptureOrigin::ExplicitResumeLaunch; assert!(adapter().bind(&evidence).is_err());
    evidence.origin = CaptureOrigin::CurrentNativeSession; evidence.native.version = "1.0.53 (aaaa)".into(); assert!(adapter().bind(&evidence).is_err());
}
struct Runner;
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        assert_eq!(request.arguments, [std::ffi::OsString::from("--version")]);
        assert!(request.environment.set.contains_key("GROK_REAL"));
        CommandReceipt { end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(42), child_reaped: true,
            stdout: b"grok 1.0.52 (0920c8169e27)\n".to_vec(), stderr: vec![], elapsed: Duration::from_millis(1) }
    }
}
#[test]
fn runtime_catalog_and_launch_identity_fence_exact_native_without_release_pins() {
    let sandbox = support::Sandbox::new(); let engine = sandbox.parent.join("grok-native");
    std::fs::write(&engine, b"controlled final native").unwrap(); std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bounds = ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 };
    let metadata = probe_launcher(&engine, &mut Runner, bounds).unwrap();
    let mut runtime = probe_runtime(&engine, &sandbox.parent, &mut Runner, bounds).unwrap(); runtime.native.platform = Platform::MacOs;
    let catalog = CatalogObservation { provider: ProviderId::new("grok_build").unwrap(), native: metadata, schema: CATALOG_SOURCE.schema.into(), models: parse_catalog(CATALOG.as_bytes()).unwrap() };
    let catalog = runtime.bind_catalog(&catalog).unwrap();
    let a = adapter().with_runtime(runtime.clone()).with_project("/actual/project".into()).unwrap();
    let mut r = support::request(&sandbox.root, "worker"); r.provider = "grok_build".into(); r.native = runtime.native;
    r.paths.executable = engine; r.paths.candidate = "/fake/candidate".into(); r.model = Some("grok-4.7".into());
    r.prompt = Some("user\nproject\nrole\nTEAM".into()); r.input_profile = Some(PROFILE.into()); r.bypass = true;
    let resolved = resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &r, Some(&catalog)).unwrap();
    let plan = a.plan(&resolved).unwrap(); validate_launch_plan(&GROK_BUILD_DESCRIPTOR, &resolved, &plan).unwrap();
    assert!(matches!(plan.expected_session, ExpectedSession::CaptureAfterLaunch));
    assert!(!plan.arguments.iter().any(|arg| arg == "--session-id" || arg == "--continue" || arg == "--add-dir"));
    assert_eq!(plan.materialization[1].contents, r.prompt.as_ref().unwrap().as_bytes());
    let mut descriptor = GROK_BUILD_DESCRIPTOR.clone(); descriptor.tool_names.naming = Support::Supported(ToolNaming::StaticServer { key: "team" });
    assert!(resolve_launch(&descriptor, &a.hooks(), &r, Some(&catalog)).is_err());
    descriptor = GROK_BUILD_DESCRIPTOR.clone(); descriptor.workspace.shared_cwd = true;
    assert!(resolve_launch(&descriptor, &a.hooks(), &r, Some(&catalog)).is_err());
    r.model = Some("luna-invented".into()); assert!(resolve_launch(&GROK_BUILD_DESCRIPTOR, &a.hooks(), &r, Some(&catalog)).is_err());
}
#[test]
fn private_primary_rejects_git_and_inherited_config_without_reading_bodies() {
    let sandbox = support::Sandbox::new();
    let parent = sandbox.parent.join("primary-owner"); let cwd = parent.join("seat");
    std::fs::create_dir_all(&cwd).unwrap(); let identity = resolve_cwd(&cwd).unwrap();
    require_private_primary(&identity).unwrap();
    std::fs::write(parent.join(".git"), b"not read").unwrap();
    assert!(require_private_primary(&identity).is_err());
    std::fs::remove_file(parent.join(".git")).unwrap();
    std::fs::create_dir(parent.join(".grok")).unwrap();
    std::fs::write(parent.join(".grok/config.toml"), b"not parsed or merged").unwrap();
    assert!(require_private_primary(&identity).is_err());
}
#[test]
fn wrapper_bytes_and_stamp_are_distinct_from_final_image() {
    let sandbox = support::Sandbox::new(); let wrapper = sandbox.parent.join("approved-wrapper");
    std::fs::write(&wrapper, b"exec final image after approved route").unwrap();
    let stamp = capture_launch_file(&wrapper, digest(b"exec final image after approved route"), 1024, Duration::from_secs(2)).unwrap();
    verify_executable_inputs(std::slice::from_ref(&stamp)).unwrap();
    let descriptor = support::descriptor(); let request = support::request(&sandbox.root, "worker");
    let resolved = resolve_launch(&descriptor, &support::hooks(), &request, None).unwrap();
    let mut plan = support::Fake.plan(&resolved).unwrap();
    plan.launcher = Some(LaunchEntrypoint { executable: wrapper.clone(), sha256: stamp.sha256 });
    validate_launch_plan(&descriptor, &resolved, &plan).unwrap();
    let script = String::from_utf8(team_agent_contract::host::shell::launch_script(&plan, &Default::default()).unwrap()).unwrap();
    assert!(script.contains(&format!("exec '{}'", wrapper.display())));
    assert!(!script.contains("exec '/fake/bin'"));
    std::fs::write(&wrapper, b"replaced").unwrap(); assert!(verify_executable_inputs(&[stamp]).is_err());
    assert!(capture_launch_file(&wrapper, digest(b"original"), 1024, Duration::from_secs(2)).is_err());
}
