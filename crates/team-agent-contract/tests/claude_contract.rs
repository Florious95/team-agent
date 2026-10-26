//! Controlled Claude Code grammar/identity regressions, not native Team G1–G5.
//! Frames are trimmed tmux 120x40 captures of 2.1.282/2.1.309 (`paste-buffer -r -S`).
#![cfg(unix)]
mod support;
use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, path::{Path, PathBuf}, time::Duration};
use team_agent_contract::contract::{delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*};
use team_agent_contract::host::{command::*, digest, process::resolve_cwd};
use team_agent_contract::providers::claude::{catalog::*, interaction::*, launcher::*, *};

const SID: &str = "52c3b38f-8f71-4aa6-8720-e260b24e9ee2";
const HEADER: &str = "Team Agent message from leader for task-1:";
const TOKEN: &str = "[team-agent-token:message-1]";
/// A trimmed `initialize` control response from 2.1.309 (account/command fields dropped).
const CATALOG: &str = concat!(
    r#"{"type":"system","subtype":"ui_invalidate","event":"ui.render","uuid":"8c6d3b3a-c51c-4c41-a91c-44fc51f48b47","session_id":"715e859a-844f-40fb-aa28-e9dd3bac1289"}"#, "\n",
    r#"{"type":"control_response","response":{"subtype":"success","request_id":"team-agent-model-catalog","response":{"claude_code_version":"2.1.309","models":["#,
    r#"{"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)","supportsEffort":true,"supportedEffortLevels":["low","medium","high","xhigh","max"]},"#,
    r#"{"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus","supportsEffort":true,"supportedEffortLevels":["low","medium","high","xhigh","max"]},"#,
    r#"{"value":"claude-fable-5[1m]","resolvedModel":"claude-fable-5","displayName":"Fable","supportsEffort":true,"supportedEffortLevels":["low","medium","high","xhigh","max"]},"#,
    r#"{"value":"sonnet","resolvedModel":"claude-sonnet-5-5","displayName":"Sonnet","supportsEffort":true,"supportedEffortLevels":["low","medium","high"]},"#,
    r#"{"value":"haiku","resolvedModel":"claude-haiku-5-5","displayName":"Haiku","supportsEffort":false}"#,
    r#"]}}}"#, "\n");

fn adapter() -> ClaudeAdapter {
    ClaudeAdapter::new(McpStdio { executable: "/fake/candidate".into(), arguments: vec!["contract-mcp".into(), "a b/中文".into()],
        environment: BTreeMap::from([("OWNED_VALUE".into(), "quoted \"text\"\nnext".into())]) }).unwrap()
}
fn identity() -> InstanceIdentity {
    InstanceIdentity { scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(), instance: InstanceId::new("instance").unwrap(), generation: Generation(1) }
}
fn scope(sequence: u64) -> EvidenceScope {
    EvidenceScope { identity: identity(), endpoint: "/owned/tmux.sock".into(), pane: "%0".into(), binding: "owned".into(),
        session: Some(NativeSessionId::new(SID).unwrap()), sequence, observed_at: Duration::from_millis(sequence * 100),
        valid_for: Duration::from_secs(2), source: "controlled-fixture".into() }
}
const RULE: &str = "────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────";
/// Banner, history, the rows just above the composer (spinner/tip/hint), the
/// composer between two rules, then the user's status line and mode footer.
fn screen(history: &str, above: &str, composer: &str) -> String {
    format!(" ▐▛███▛█   Claude Code v2.1.309\n▝▜██████▀  Haiku 5.5 · Claude API\n ▝▝   ▝▝   /owned/cwd\n\n{history}\n\n{above}\n                                                                                                     ◐ medium · /effort\n{RULE}\n❯ {composer}\n{RULE}\n  Ctx: n/a | Haiku 5.5 | worker\n  ⏵⏵ auto mode on (shift+tab to cycle)\n")
}
fn ready(history: &str) -> String { screen(history, "", "") }
fn payload() -> String { format!("{HEADER}\n\nCompute 123 * 45.\n\n{TOKEN}") }
fn shape(text: &str) -> InputShape {
    InputShape { sha256: digest(text.as_bytes()), bytes: text.len(), lines: text.lines().count(), first_line: text.lines().next().unwrap().into() }
}
/// The full turn as Claude draws it in history after Enter: `❯` then indented rows.
fn turn(text: &str) -> String {
    text.lines().enumerate().map(|(i, line)| if i == 0 { format!("❯ {line}") } else if line.is_empty() { String::new() } else { format!("  {line}") }).collect::<Vec<_>>().join("\n")
}
fn frame(text: String, operation: Operation, step: Option<StepKind>, latch: PasteLatch) -> CaptureFrame {
    CaptureFrame { scope: scope(2), text, cwd: Some("/owned/cwd".into()), input: Some(shape(&payload())),
        baseline: Some(CaptureBaseline { scope: scope(1), text: ready("⏺ earlier reply") }), profile_id: PROFILE.into(), operation,
        message: matches!(operation, Operation::FirstBusiness | Operation::OrdinarySend).then(|| MessageId::new("message-1").unwrap()),
        attempt: Some(AttemptId::new("attempt-1").unwrap()), after_step: step, paste_latch: latch }
}

#[test]
fn all_facets_and_hooks_disclose_owned_argv_server_and_preassigned_session_under_a_distinct_key() {
    let a = adapter(); let hooks = a.hooks();
    assert_eq!(CLAUDE_DESCRIPTOR.identity.id, "claude_tui");
    assert_eq!(CLAUDE_DESCRIPTOR.identity.binary, "claude");
    assert!(CLAUDE_DESCRIPTOR.identity.aliases.is_empty(), "legacy claude/claude_code keys are never aliases");
    for operation in [Operation::Catalog, Operation::Fresh, Operation::Resume, Operation::FirstBusiness, Operation::OrdinarySend, Operation::Stop] {
        validate_descriptor(&CLAUDE_DESCRIPTOR, &hooks, operation).unwrap();
    }
    for operation in [Operation::NativeNewSeat, Operation::NewSeatFullSnapshot, Operation::InWindowBranch] {
        assert!(validate_descriptor(&CLAUDE_DESCRIPTOR, &hooks, operation).is_err(), "{operation:?}");
    }
    assert_eq!(CLAUDE_DESCRIPTOR.mcp.carrier, Support::Supported(McpCarrier::Argv));
    assert_eq!(CLAUDE_DESCRIPTOR.mcp.isolation, McpIsolation::OwnedServerIsolated);
    assert_eq!(CLAUDE_DESCRIPTOR.tool_names.naming, Support::Supported(ToolNaming::RuntimeBound));
    assert_eq!(CLAUDE_DESCRIPTOR.prompt.carrier, Support::Supported(PromptCarrier::RuntimeBridge));
    assert_eq!(CLAUDE_DESCRIPTOR.session.fresh, FreshSession::Preassigned);
    assert_eq!(CLAUDE_DESCRIPTOR.session.resume, Support::Supported(ResumeMode::ExactId));
    assert_eq!(CLAUDE_DESCRIPTOR.effort.carrier, Support::Supported(ValueCarrier::Flag("--effort")));
    assert!(CLAUDE_DESCRIPTOR.effort.model_dependent && !CLAUDE_DESCRIPTOR.effort.inherit_team_default);
    assert!(CLAUDE_DESCRIPTOR.effort.admission[..5].iter().all(|admission| *admission == EffortAdmission::Pass));
    assert!(matches!(CLAUDE_DESCRIPTOR.effort.admission[Effort::Ultra.index()], EffortAdmission::Reject(_)));
    assert!(matches!(CLAUDE_DESCRIPTOR.startup.authorized_consent, Support::Unsupported(_)), "folder trust is never keyed");
    let bypass = CLAUDE_DESCRIPTOR.bypass.intent.require("bypass").unwrap();
    assert_eq!(bypass.enabled_arguments, ["--dangerously-skip-permissions"]);
    assert!(!bypass.requires_startup_consent);
    assert_eq!(POLICY, digest(POLICY_RECIPE.as_bytes()));
    assert_eq!(CONTROLS.iter().map(|control| control.command).collect::<Vec<_>>(), ["/exit"]);
    PROFILES[0].policy.require("policy").unwrap().validate().unwrap();
    assert_eq!(PROFILES[0].policy.require("policy").unwrap().max_submit_keys, 1);
}
#[test]
fn evergreen_version_and_bounded_bad_input() {
    for version in ["2.1.309", "2.1.282", "3.0.0-beta.1", "12.345.6789"] {
        assert_eq!(parse_version(format!("{version} (Claude Code)\n").as_bytes()).unwrap(), version);
    }
    for bad in ["2.1.309", "claude 2.1.309", "2.1 (Claude Code)", "02.1.309 (Claude Code)", "2.1.309- (Claude Code)", "2.1.309 (Claude Code)\nextra"] {
        assert!(parse_version(bad.as_bytes()).is_err(), "{bad}");
    }
    let error = parse_version(&[b'x'; 900]).unwrap_err().to_string();
    assert!(error.contains("truncated=true")); assert!(error.len() < 450);
}
#[test]
fn catalog_lists_resolved_ids_and_picker_values_with_their_own_team_efforts() {
    let records = parse_catalog(CATALOG.as_bytes(), "2.1.309").unwrap();
    let ids: Vec<_> = records.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(ids, ["claude-opus-5-5", "opus", "claude-fable-5", "claude-fable-5[1m]", "claude-sonnet-5-5", "sonnet", "claude-haiku-5-5", "haiku"]);
    let all = Support::Supported(vec![Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max]);
    assert_eq!(records[0].efforts, all);
    assert_eq!(records[4].efforts, Support::Supported(vec![Effort::Low, Effort::Medium, Effort::High]));
    assert_eq!(records[6].efforts, Support::Supported(vec![]), "no effort levels, no effort carrier");
    // A reported version must be the installation the launcher probe saw;
    // 2.1.282 reports none, and its hash-fenced read still parses.
    assert!(matches!(parse_catalog(CATALOG.as_bytes(), "2.1.282"), Err(ContractError::Mismatch(_))));
    assert!(matches!(parse_catalog(CATALOG.replace("\"claude_code_version\":\"2.1.309\",", "\"claude_code_version\":2109,").as_bytes(), "2.1.309"), Err(ContractError::Mismatch(_))));
    assert_eq!(parse_catalog(CATALOG.replace("\"claude_code_version\":\"2.1.309\",", "").as_bytes(), "2.1.282").unwrap().len(), 8);
    assert!(matches!(parse_catalog(CATALOG.replace("\"value\":\"sonnet\"", "\"value\":\"opus\"").as_bytes(), "2.1.309"), Err(ContractError::AmbiguousModel)));
    // A selector shared with another row keeps only the efforts both list.
    let shared = CATALOG.replace("\"value\":\"sonnet\",\"resolvedModel\":\"claude-sonnet-5-5\"", "\"value\":\"sonnet\",\"resolvedModel\":\"claude-opus-5-5\"");
    let records = parse_catalog(shared.as_bytes(), "2.1.309").unwrap();
    assert_eq!(records[0].id, "claude-opus-5-5");
    assert_eq!(records[0].efforts, Support::Supported(vec![Effort::Low, Effort::Medium, Effort::High]));
    let second = CATALOG.lines().nth(1).unwrap();
    for bad in [CATALOG.replace("team-agent-model-catalog", "other-request"), CATALOG.replace("\"success\"", "\"error\""),
        CATALOG.replace("claude-fable-5[1m]", "claude fable"), r#"{"type":"control_response","response":{"subtype":"success","request_id":"team-agent-model-catalog","response":{"claude_code_version":"2.1.309","models":[]}}}"#.into(),
        CATALOG.replace("\"resolvedModel\":\"claude-haiku-5-5\",", ""),
        format!("{CATALOG}{second}\n"), "not json".into(), CATALOG.lines().next().unwrap().into()]
    {
        assert!(parse_catalog(bad.as_bytes(), "2.1.309").is_err(), "{bad}");
    }
}
#[test]
fn owned_server_is_one_json_value_with_a_per_generation_key_and_allow_rule() {
    let owner = identity();
    let key = server_key(&owner);
    assert!(key.starts_with("team_") && key.len() == 37);
    let values: Vec<String> = adapter().mcp_arguments(&owner).unwrap().into_iter().map(|value| value.into_string().unwrap()).collect();
    assert_eq!(values[0], "--mcp-config");
    let config: serde_json::Value = serde_json::from_str(&values[1]).unwrap();
    assert_eq!(config, serde_json::json!({"mcpServers": {key.as_str(): {"type": "stdio", "command": "/fake/candidate",
        "args": ["contract-mcp", "a b/中文"], "env": {"OWNED_VALUE": "quoted \"text\"\nnext"}}}}));
    assert_eq!(&values[2..], ["--allowedTools".to_string(), format!("mcp__{key}")]);
    let mut other = owner.clone(); other.generation = Generation(2); assert_ne!(key, server_key(&other));
    other = owner; other.seat = SeatId::new("peer").unwrap(); assert_ne!(key, server_key(&other));
}
#[test]
fn fresh_session_id_is_a_v4_uuid_derived_from_the_instance() {
    let id = session_id(&identity());
    assert_eq!(id, session_id(&identity()), "replanning the same instance names the same session");
    let text = id.as_str();
    assert_eq!(text.len(), 36);
    assert_eq!(&text[14..15], "4");
    assert!("89ab".contains(&text[19..20]));
    let mut next = identity(); next.instance = InstanceId::new("instance-2").unwrap(); next.generation = Generation(2);
    assert_ne!(session_id(&next), id);
}

struct Runner(&'static [u8]);
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        // Probes run the PATH entry itself and leave the user's environment alone.
        assert_eq!(request.arguments, [OsString::from("--version")]);
        assert_eq!(request.executable.file_name().unwrap(), "claude");
        assert!(request.environment.set.is_empty() && request.environment.remove.is_empty() && request.stdin.is_none());
        CommandReceipt { end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(42), child_reaped: true,
            stdout: self.0.to_vec(), stderr: vec![], elapsed: Duration::from_millis(1) }
    }
}
const V309: &[u8] = b"2.1.309 (Claude Code)\n";
fn bounds() -> ReadBounds { ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 } }
fn write_executable(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
/// bin/claude -> share/versions/<version>, the native installer's layout.
fn native_install(root: &Path, version: &str) -> PathBuf {
    let image = root.join("share/versions").join(version);
    write_executable(&image, b"controlled version-named native image");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&image, bin.join("claude")).unwrap();
    bin
}
#[test]
fn version_named_image_must_be_the_release_its_path_route_reports() {
    let sandbox = support::Sandbox::new();
    let bin = native_install(&sandbox.parent, "2.1.309");
    let wrappers = sandbox.parent.join("user wrappers");
    write_executable(&wrappers.join("claude"), b"#!/bin/sh\nexec claude-next \"$@\"\n");
    let path = std::env::join_paths([wrappers.as_path(), bin.as_path()]).unwrap();
    let command = PublicCommand::resolve("claude", &path).unwrap();
    assert_eq!(command.entry, wrappers.join("claude"), "the user wrapper is the public entry");
    let runtime = probe_runtime(&command, &sandbox.parent, &mut Runner(V309), bounds()).unwrap();
    assert_eq!(runtime.engine, sandbox.parent.join("share/versions/2.1.309").canonicalize().unwrap());
    assert_eq!(runtime.native.version, "2.1.309");
    assert_eq!(runtime.native.executable_sha256, digest(b"controlled version-named native image"));
    assert_eq!((runtime.native.harness.as_str(), runtime.native.ui.as_str()), ("claude", "tui"));
    // The route reports another release than the image it would exec: refused.
    assert!(probe_runtime(&command, &sandbox.parent, &mut Runner(b"2.1.282 (Claude Code)\n"), bounds()).is_err());
    assert!(version_fence(&runtime.engine, "2.1.282").is_err());
    // An image without a version name carries no version claim.
    version_fence(Path::new("/opt/vendor/bin/claude"), "2.1.282").unwrap();
    // A wrapper with nothing native after it is refused, never resolved elsewhere.
    assert!(probe_runtime(&PublicCommand::resolve("claude", wrappers.as_os_str()).unwrap(), &sandbox.parent, &mut Runner(V309), bounds()).is_err());
}
fn claude_launch(sandbox: &support::Sandbox, command: &PublicCommand, project: &Path) -> (ClaudeAdapter, LaunchRequest, CatalogObservation, RuntimeInstallation) {
    let metadata = probe_launcher(command, &mut Runner(V309), bounds()).unwrap();
    let mut runtime = probe_runtime(command, &sandbox.parent, &mut Runner(V309), bounds()).unwrap(); runtime.native.platform = Platform::MacOs;
    let catalog = CatalogObservation { provider: ProviderId::new(ID).unwrap(), native: metadata, schema: CATALOG_SOURCE.schema.into(), models: parse_catalog(CATALOG.as_bytes(), "2.1.309").unwrap() };
    let catalog = runtime.bind_catalog(&catalog).unwrap();
    let a = adapter().with_runtime(runtime.clone()).with_project(project.into()).unwrap();
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let mut r = support::request(&sandbox.root, "worker"); r.provider = ID.into(); r.native = runtime.native.clone();
    r.paths.launcher = runtime.launcher(); r.paths.executable = runtime.engine.clone(); r.paths.candidate = "/fake/candidate".into();
    r.model = Some("claude-sonnet-5-5".into()); r.prompt = Some("user\nproject\nrole\nTEAM".into()); r.input_profile = Some(PROFILE.into()); r.bypass = true;
    r.preassigned_session = Some(session_id(&r.identity));
    (a, r, catalog, runtime)
}
fn plan(a: &ClaudeAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> Result<LaunchPlan, ContractError> {
    let resolved = resolve_launch(&CLAUDE_DESCRIPTOR, &a.hooks(), r, Some(catalog))?;
    let plan = a.plan(&resolved)?; validate_launch_plan(&CLAUDE_DESCRIPTOR, &resolved, &plan)?;
    Ok(plan)
}
fn strings(plan: &LaunchPlan) -> Vec<String> { plan.arguments.iter().map(|arg| arg.to_str().unwrap().to_owned()).collect() }

#[test]
fn launch_runs_the_user_wrapper_prepended_to_path_then_the_native_image() {
    let sandbox = support::Sandbox::new();
    // The native image here is /bin/echo behind a vendor symlink, so exec-through is observable.
    let vendor = sandbox.parent.join("vendor/bin");
    std::fs::create_dir_all(&vendor).unwrap();
    std::os::unix::fs::symlink("/bin/echo", vendor.join("claude")).unwrap();
    let wrappers = sandbox.parent.join("user wrappers");
    let marker = sandbox.parent.join("wrapper-invocations");
    write_executable(&wrappers.join("claude"), format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"${{0##*/}}\" \"$1\" >> '{}'\nexec '{}' \"$@\"\n",
        marker.display(), vendor.join("claude").display()).as_bytes());
    let path = std::env::join_paths([wrappers.as_path(), vendor.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let command = PublicCommand::resolve("claude", &path).unwrap();
    let project = sandbox.parent.join("project");
    let (a, r, catalog, runtime) = claude_launch(&sandbox, &command, &project);
    let plan = plan(&a, &r, &catalog).unwrap();
    assert_eq!(plan.launcher.executable, wrappers.join("claude"));
    assert_eq!(plan.executable, Path::new("/bin/echo").canonicalize().unwrap());
    assert_eq!(runtime.engine, plan.executable);
    let script = team_agent_contract::host::shell::launch_script(&plan, &Default::default()).unwrap();
    let text = String::from_utf8(script.clone()).unwrap();
    assert!(text.contains(&format!("\nexport PATH='{}'\nexec 'claude' '--model' 'claude-sonnet-5-5'", path.to_str().unwrap())), "{text}");
    for unset in ["unset ANTHROPIC_API_KEY\n", "unset ANTHROPIC_AUTH_TOKEN\n", "unset CLAUDECODE\n"] { assert!(text.contains(unset), "{unset}"); }
    for forbidden in ["/opt/homebrew", "node_modules", ".local/bin", "CLAUDE_REAL", "claude.exe", "versions/"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
    assert!(!text.contains(&format!("exec '{}", plan.executable.display())), "the image is a fence, never exec'd");
    // The real system shell: PATH lookup of bare `claude` reaches the user
    // wrapper, which execs the vendor entry, which is the native image.
    let launch = sandbox.parent.join("launch.sh");
    std::fs::write(&launch, &script).unwrap();
    let output = std::process::Command::new("/bin/sh").arg(&launch).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&marker).expect("the user wrapper genuinely ran"), "claude|--model\n");
    let echoed = String::from_utf8(output.stdout).unwrap();
    assert!(echoed.starts_with(&format!("--model claude-sonnet-5-5 --add-dir {}", project.display())), "{echoed}");
    assert!(echoed.contains("--dangerously-skip-permissions"));
    assert!(echoed.trim_end().ends_with(&format!("--session-id {}", session_id(&r.identity).as_str())), "{echoed}");
}
#[test]
fn plan_carries_model_effort_bypass_mcp_and_exact_sessions_without_startup_keys() {
    let sandbox = support::Sandbox::new();
    let command = PublicCommand::resolve("claude", native_install(&sandbox.parent, "2.1.309").as_os_str()).unwrap();
    let project = sandbox.parent.join("project");
    let (a, mut r, catalog, _) = claude_launch(&sandbox, &command, &project);
    r.role_effort = Some("high".into());
    let built = plan(&a, &r, &catalog).unwrap();
    let args = strings(&built);
    let key = server_key(&r.identity);
    assert_eq!(&args[..5], ["--model", "claude-sonnet-5-5", "--add-dir", project.to_str().unwrap(), "--append-system-prompt"]);
    assert!(args[5].starts_with("The actual task project is "));
    assert_eq!(args[6], "--mcp-config");
    assert_eq!(&args[8..10], ["--allowedTools".to_string(), format!("mcp__{key}")]);
    assert_eq!(&args[10..], ["--dangerously-skip-permissions", "--effort", "high", "--session-id", session_id(&r.identity).as_str()]);
    assert_eq!(built.carriers.model, CarrierUse::Specified(CarrierRef::Arguments(vec![0, 1])));
    assert_eq!(built.carriers.mcp, CarrierUse::Specified(CarrierRef::Arguments(vec![6, 7, 8, 9])));
    assert_eq!(built.carriers.bypass, CarrierUse::Specified(CarrierRef::Arguments(vec![10])));
    assert_eq!(built.environment.remove, std::collections::BTreeSet::from(["ANTHROPIC_API_KEY".to_string(), "ANTHROPIC_AUTH_TOKEN".to_string(), "CLAUDECODE".to_string()]));
    assert!(!built.environment.set.contains_key("PATH") && built.environment.set.contains_key("HOME"));
    assert_eq!(built.materialization.len(), 1);
    assert_eq!(built.materialization[0].kind, ResourceKind::Prompt);
    assert_eq!(built.materialization[0].contents, b"user\nproject\nrole\nTEAM");
    assert_eq!(built.expected_session, ExpectedSession::Preassigned(session_id(&r.identity)));
    // Effort is admitted per catalog model: sonnet lists high but not max; ultra never.
    r.role_effort = Some("max".into());
    assert!(matches!(plan(&a, &r, &catalog), Err(ContractError::EffortNotAvailableForModel)));
    r.role_effort = Some("ultra".into());
    assert!(matches!(plan(&a, &r, &catalog), Err(ContractError::EffortRejected(_))));
    r.model = Some("claude-fable-5[1m]".into()); r.role_effort = Some("max".into());
    assert!(strings(&plan(&a, &r, &catalog).unwrap()).windows(2).any(|pair| pair == ["--effort", "max"]));
    r.model = Some("haiku".into()); r.role_effort = Some("low".into());
    assert!(matches!(plan(&a, &r, &catalog), Err(ContractError::EffortNotAvailableForModel)));
    r.role_effort = None; r.bypass = false;
    let args = strings(&plan(&a, &r, &catalog).unwrap());
    assert_eq!(&args[..2], ["--model", "haiku"]);
    assert!(!args.iter().any(|arg| arg.contains("dangerously") || arg == "--effort" || arg == "--permission-mode"));
    // Model omission, the moving `default` selector or invented IDs never launch.
    for model in [None, Some("default"), Some("claude-opus-invented")] {
        r.model = model.map(Into::into);
        assert!(resolve_launch(&CLAUDE_DESCRIPTOR, &a.hooks(), &r, Some(&catalog)).is_err(), "{model:?}");
    }
    r.model = Some("opus".into());
    // The project itself can never be the private primary.
    let mut shared = r.clone(); shared.paths.cwd.path = project.clone();
    assert!(plan(&a, &shared, &catalog).is_err());
    // A launcher other than the probed PATH route can never be planned.
    let mut wrong = r.clone(); wrong.paths.launcher.sha256 = digest(b"another entry");
    assert!(plan(&a, &wrong, &catalog).is_err());
    // Only the session derived from this instance may be preassigned.
    let mut foreign = r.clone(); foreign.preassigned_session = Some(NativeSessionId::new(SID).unwrap());
    assert!(plan(&a, &foreign, &catalog).is_err());
    // Pure preflight plans no session flag and cannot be launched.
    let mut preflight = r.clone(); preflight.preassigned_session = None;
    assert!(resolve_launch(&CLAUDE_DESCRIPTOR, &a.hooks(), &preflight, Some(&catalog)).is_err());
    let resolved = resolve_launch_preflight(&CLAUDE_DESCRIPTOR, &a.hooks(), &preflight, Some(&catalog)).unwrap();
    assert_eq!(resolved.expected_session(), &ExpectedSession::PendingAllocation);
    let args = strings(&a.plan(&resolved).unwrap());
    assert!(!args.iter().any(|arg| arg == "--session-id" || arg == "--resume"));
    // Exact resume: `--resume <uuid>` in the same cwd, never a fresh ID.
    let source = r.identity.clone();
    let evidence = ScopedSessionEvidence { scope: EvidenceScope { identity: source.clone(), ..scope(1) }, provider: ProviderId::new(ID).unwrap(),
        native: r.native.clone(), cwd: r.paths.cwd.clone(), origin: CaptureOrigin::ExplicitResumeLaunch, evidence_kind: r.evidence_kind,
        evidence_sha256: digest(SID.as_bytes()), record: SID.as_bytes().to_vec() };
    let binding = a.bind(&evidence).unwrap();
    let mut resume = r.clone(); resume.operation = Operation::Resume; resume.preassigned_session = None;
    resume.identity.instance = InstanceId::new("worker-2").unwrap(); resume.identity.generation = Generation(2);
    resume.resume = Some(ResumeRequest { binding, expected_source: source });
    let args = strings(&plan(&a, &resume, &catalog).unwrap());
    assert_eq!(&args[args.len() - 2..], ["--resume", SID]);
    assert!(!args.iter().any(|arg| arg == "--session-id" || arg == "--fork-session" || arg == "--continue"));
    resume.resume.as_mut().unwrap().binding.native_session = NativeSessionId::new("not-a-uuid").unwrap();
    assert!(plan(&a, &resume, &catalog).is_err());
}

struct Io(Vec<OwnedResourceReceipt>);
impl OwnedIo for Io {
    fn create_exclusive(&mut self, request: &OwnedResourceRequest) -> Result<OwnedResourceReceipt, PartialFailure> {
        let receipt = OwnedResourceReceipt { path: request.path.clone(), owner: request.owner.clone(), operation: OperationId::new("op").unwrap(),
            kind: request.kind, disposition: ResourceDisposition::OwnedRemovable, write_effect: ResourceWriteEffect::Written { bytes_sha256: digest(&request.contents) },
            exclusive: true, creation_identity: None };
        self.0.push(receipt.clone()); Ok(receipt)
    }
    fn read_bound_session(&mut self, _: &ResumeBinding, _: ReadBounds) -> Result<ReadOutput, ReadFailure> { Err(ReadFailure::Unknown(UNVERIFIED)) }
    fn validate_configuration(&mut self, _: &OwnedValidationRequest) -> Result<ReadOutput, ReadFailure> { Err(ReadFailure::Unknown(UNVERIFIED)) }
}
#[test]
fn materialization_writes_exactly_the_owned_prompt() {
    let prompt = OwnedResourceRequest { owner: identity(), path: OwnedPath::new("/owned/cwd".into(), "team-agent-prompt.txt".into()).unwrap(), kind: ResourceKind::Prompt, contents: b"role".to_vec() };
    let mut io = Io(vec![]);
    assert_eq!(adapter().materialize(std::slice::from_ref(&prompt), &mut io).unwrap().resources.len(), 1);
    let mut wrong_kind = prompt.clone(); wrong_kind.kind = ResourceKind::McpConfig;
    let mut wrong_path = prompt.clone(); wrong_path.path = OwnedPath::new("/owned/cwd".into(), ".mcp.json".into()).unwrap();
    let mut blank = prompt.clone(); blank.contents = b"  \n".to_vec();
    for bad in [vec![], vec![prompt.clone(), prompt.clone()], vec![wrong_kind], vec![wrong_path], vec![blank]] {
        let mut io = Io(vec![]);
        assert!(adapter().materialize(&bad, &mut io).is_err());
        assert!(io.0.is_empty(), "refused before any effect");
    }
}
#[test]
fn surfaces_follow_the_composer_between_the_last_two_rules() {
    assert_eq!(surface(&ready("")), InputSurface::ComposerReady);
    for spinner in ["✻ Baking…", "✽ Beboppin'…", "· Percolating…", "✢ Boondoggling… (3s · thinking)", "✶ Sprouting…", "✳ Tomfoolering…"] {
        assert_eq!(surface(&screen("❯ question", spinner, "")), InputSurface::Busy, "{spinner}");
        // A tip row may sit between the spinner and the hint row.
        assert_eq!(surface(&screen("❯ question", &format!("{spinner}\n  ⎿  Tip: Use ctrl+v to paste images from your clipboard"), "")), InputSurface::Busy);
    }
    // A finished turn's summary, an assistant bullet or a history turn is not a spinner.
    for idle in ["✻ Worked for 3s", "  · item…", "⏺ Thinking…", "❯ Writing…"] {
        assert_eq!(surface(&screen("", idle, "")), InputSurface::ComposerReady, "{idle}");
    }
    // Reply text streaming without a spinner reads ready; queued input is busy.
    assert_eq!(surface(&ready("❯ Write a story\n\n⏺ The lighthouse keeper climbed")), InputSurface::ComposerReady);
    assert_eq!(surface(&screen(&format!("{}\n  ctrl+x ctrl+s to send now", turn(&payload())), "", "Press up to edit queued messages")), InputSurface::Busy);
    assert_eq!(surface(&screen("", "", "draft text")), InputSurface::ComposerContainsPaste);
    assert_eq!(surface(&screen("", "", "[Pasted text #1 +4 lines]")), InputSurface::ComposerContainsPaste);
    // History turns above the composer also start with `❯`.
    assert_eq!(surface(&ready(&turn(&payload()))), InputSurface::ComposerReady);
    // The slash menu sits above the composer; its own selection carries `❯`.
    let slash = screen("", "  ❯ /exit                       Exit the CLI\n    /context                    Visualize current context usage as a colored grid", "/exit");
    assert_eq!(surface(&slash), InputSurface::ComposerContainsPaste);
    assert_eq!(surface("plain shell $ "), InputSurface::ShellOrUnknown);
    assert_eq!(surface(&format!("Resume this session with:\nclaude --resume {SID}\n")), InputSurface::ShellOrUnknown);
    // Wrong profile never yields a surface.
    let mut wrong = frame(ready(""), Operation::OrdinarySend, None, PasteLatch::NeverSeen); wrong.profile_id = "other".into();
    assert_eq!(adapter().interpret(&wrong).surface, InputSurface::ShellOrUnknown);
}
#[test]
fn folder_trust_is_named_but_never_ready_or_keyed() {
    let trust = format!("{RULE}\n Accessing workspace:\n\n /owned/cwd\n\n Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source\n project, or work from your team). If not, take a moment to review what's in this folder first.\n\n Claude Code'll be able to read, edit, and execute files here.\n\n Security guide\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n");
    assert_eq!(surface(&trust), InputSurface::StartupRiskWarning);
    for operation in [Operation::FirstBusiness, Operation::OrdinarySend, Operation::Stop] {
        let observed = adapter().interpret(&frame(trust.clone(), operation, None, PasteLatch::NeverSeen));
        assert_eq!(observed.surface, InputSurface::StartupRiskWarning);
        assert_eq!(observed.predicate.as_deref(), Some("claude-folder-trust-modal"));
    }
    assert_eq!(surface(&trust.replace(" Enter to confirm · Esc to cancel\n", "")), InputSurface::ShellOrUnknown);
}
#[test]
fn inline_paste_is_seen_by_its_token_and_accepted_only_from_history() {
    let line = format!("Reply with exactly: PONG {TOKEN}");
    let mut pasted = frame(screen("", "", &line), Operation::OrdinarySend, Some(StepKind::Paste), PasteLatch::NeverSeen);
    pasted.input = Some(shape(&line));
    let observed = adapter().interpret(&pasted);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    assert_eq!(observed.paste_latch, PasteLatch::Seen { native_identity: TOKEN.into() });
    let mut submitted = pasted.clone();
    submitted.text = screen(&format!("❯ {line}"), "✶ Sprouting…", ""); submitted.after_step = Some(StepKind::InitialSubmit);
    submitted.paste_latch = observed.paste_latch.clone();
    let accepted = adapter().interpret(&submitted);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.paste_latch, PasteLatch::Gone { native_identity: TOKEN.into() });
    // The token still in the composer (Enter not consumed) is not acceptance.
    submitted.text = screen("", "", &line);
    assert_ne!(adapter().interpret(&submitted).surface, InputSurface::NativeAccepted);
}
#[test]
fn folded_paste_is_bound_to_this_attempt_and_accepted_by_the_expanded_turn() {
    let folded = "[Pasted text #3 +4 lines]";
    let pasted = frame(screen("⏺ earlier reply", "", folded), Operation::FirstBusiness, Some(StepKind::Paste), PasteLatch::NeverSeen);
    let observed = adapter().interpret(&pasted);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    let PasteLatch::Seen { native_identity } = &observed.paste_latch else { panic!("{:?}", observed.paste_latch) };
    assert!(native_identity.starts_with("claude-owned-paste-v1:"));
    assert_eq!((observed.current_message.as_ref(), observed.current_attempt.as_ref()), (pasted.message.as_ref(), pasted.attempt.as_ref()));
    for case in 0..8 {
        let mut bad = pasted.clone();
        match case {
            0 => bad.after_step = None,
            1 => bad.text = screen("", "", "[Pasted text #3 +9 lines]"), // not this payload's shape
            2 => bad.text = screen("", "", "[Pasted text #3]"), // single-line placeholder for a multi-line payload
            3 => bad.baseline.as_mut().unwrap().text = screen("", "", "unsent draft"),
            4 => bad.baseline.as_mut().unwrap().scope.identity.generation = Generation(9),
            5 => bad.attempt = None,
            6 => bad.text = screen("", "", "[Pasted text #3 +4 lines] typed suffix"),
            _ => bad.baseline.as_mut().unwrap().text = ready(&turn(&payload())), // replayed token
        }
        assert!(matches!(adapter().interpret(&bad).paste_latch, PasteLatch::NeverSeen), "case {case}");
    }
    // A long single line folds without a line count.
    let mut long = pasted.clone(); long.text = screen("", "", "[Pasted text #4]");
    let line = format!("Reply {} {TOKEN}", "x".repeat(1600)); long.input = Some(shape(&line));
    assert!(matches!(adapter().interpret(&long).paste_latch, PasteLatch::Seen { .. }));
    let mut submitted = pasted.clone();
    submitted.text = screen(&turn(&payload()), "✻ Philosophizing…\n  ⎿  Tip: Use ctrl+v to paste images from your clipboard", "");
    submitted.after_step = Some(StepKind::InitialSubmit); submitted.paste_latch = observed.paste_latch.clone();
    let accepted = adapter().interpret(&submitted);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert!(matches!(accepted.paste_latch, PasteLatch::Gone { .. }));
    // Queued behind a streaming reply: Claude holds it and submits at the turn boundary.
    submitted.text = screen(&format!("❯ Write a story\n\n⏺ The keeper climbed\n\n{}\n  ctrl+x ctrl+s to send now", turn(&payload())), "", "Press up to edit queued messages");
    assert_eq!(adapter().interpret(&submitted).surface, InputSurface::NativeAccepted);
    // Without the token above the composer the folded submit stays unresolved.
    submitted.text = screen("⏺ earlier reply", "✻ Philosophizing…", "");
    assert_ne!(adapter().interpret(&submitted).surface, InputSurface::NativeAccepted);
    submitted.text = screen("", "", folded);
    assert_ne!(adapter().interpret(&submitted).surface, InputSurface::NativeAccepted);
}
#[test]
fn exit_control_is_typed_into_an_empty_composer_only() {
    let typed = frame(screen("", "  ❯ /exit                       Exit the CLI", "/exit"), Operation::Stop, Some(StepKind::Paste), PasteLatch::NeverSeen);
    assert_eq!(adapter().interpret(&typed).paste_latch, PasteLatch::Seen { native_identity: "/exit".into() });
    let mut stale = typed.clone(); stale.baseline.as_mut().unwrap().text = screen("", "", "draft");
    assert_eq!(adapter().interpret(&stale).paste_latch, PasteLatch::NeverSeen);
    let mut other = typed; other.text = screen("", "", "/exit now");
    assert_eq!(adapter().interpret(&other).paste_latch, PasteLatch::NeverSeen);
}
#[test]
fn session_binding_accepts_only_the_explicit_launch_uuid() {
    let evidence = |record: &str, origin| ScopedSessionEvidence { scope: scope(1), provider: ProviderId::new(ID).unwrap(),
        native: NativeIdentity { version: "2.1.309".into(), harness: "claude".into(), ui: "tui".into(), platform: Platform::MacOs, executable_sha256: digest(b"native") },
        cwd: CwdIdentity { path: "/owned/cwd".into(), identity: digest(b"cwd") }, origin, evidence_kind: EvidenceKind::Fixture,
        evidence_sha256: digest(record.as_bytes()), record: record.as_bytes().to_vec() };
    let binding = adapter().bind(&evidence(SID, CaptureOrigin::ExplicitResumeLaunch)).unwrap();
    assert_eq!(binding.native_session.as_str(), SID);
    assert_eq!(binding.storage, SessionStorage::Local);
    assert!(binding.backing.is_none());
    assert!(adapter().bind(&evidence(SID, CaptureOrigin::CurrentNativeSession)).is_err(), "never a screen echo");
    assert!(adapter().bind(&evidence("not-a-uuid", CaptureOrigin::ExplicitResumeLaunch)).is_err());
    let mut forged = evidence(SID, CaptureOrigin::ExplicitResumeLaunch); forged.evidence_sha256 = digest(b"other");
    assert!(adapter().bind(&forged).is_err());
}
#[test]
fn private_primary_rejects_git_and_inherited_claude_configuration_without_reading_bodies() {
    let sandbox = support::Sandbox::new();
    let parent = sandbox.parent.join("primary-owner"); let cwd = parent.join("seat");
    std::fs::create_dir_all(&cwd).unwrap(); let identity = resolve_cwd(&cwd).unwrap();
    require_private_primary(&identity).unwrap();
    for name in [".git", ".mcp.json", "CLAUDE.md", ".claude"] {
        std::fs::write(parent.join(name), b"not read").unwrap();
        assert!(require_private_primary(&identity).is_err(), "{name}");
        std::fs::remove_file(parent.join(name)).unwrap();
    }
    require_private_primary(&identity).unwrap();
}
#[test]
fn semantic_reader_maps_only_busy_and_ready() {
    let read = |text: &str| adapter().read(&NativeRecord { scope: scope(1), kind: NativeRecordKind::Surface, bytes: text.as_bytes().to_vec() }).outcome;
    assert_eq!(read(&screen("", "✻ Baking…", "")), ProbeOutcome::Observed(SemanticEvidence::Busy));
    assert_eq!(read(&ready("")), ProbeOutcome::Observed(SemanticEvidence::Idle));
    assert!(matches!(read("plain"), ProbeOutcome::Unknown(_)));
}
