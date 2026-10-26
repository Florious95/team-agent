//! Controlled Codex 0.160.1 grammar/identity regressions, not native Team G1–G5.
//! Frames follow the tagged openai/codex TUI snapshots (`--no-alt-screen`).
#![cfg(unix)]
mod support;
use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, path::{Path, PathBuf}, time::Duration};
use team_agent_contract::contract::{delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*};
use team_agent_contract::host::{command::*, digest, process::resolve_cwd};
use team_agent_contract::providers::codex::{catalog::*, interaction::*, launcher::*, *};

const SID: &str = "01a13e52-77d5-7580-81cc-2f5d4749aba1";
const HEADER: &str = "Team Agent message from leader for task-1:";
const TOKEN: &str = "[team-agent-token:message-1]";
const PLACEHOLDER: &str = "Ask Codex to do anything";
const FOOTER: &str = "? for shortcuts                                                          100% context left";
const CATALOG: &str = r#"{"models":[
 {"slug":"gpt-6-luna","display_name":"GPT-6-Luna","visibility":"list","supported_reasoning_levels":[{"effort":"low","description":"l"},{"effort":"medium","description":"m"},{"effort":"high","description":"h"},{"effort":"xhigh","description":"x"},{"effort":"max","description":"M"}]},
 {"slug":"gpt-6-sol","display_name":"GPT-6-Sol","visibility":"list","supported_reasoning_levels":[{"effort":"none","description":"n"},{"effort":"low","description":"l"},{"effort":"ultra","description":"u"}]},
 {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide","supported_reasoning_levels":[]}
]}"#;

fn adapter() -> CodexAdapter {
    CodexAdapter::new(McpStdio { executable: "/fake/candidate".into(), arguments: vec!["contract-mcp".into(), "a b/中文".into()],
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
/// History above the live bottom pane, then the composer row and footer.
fn screen(history: &str, composer: &str) -> String {
    format!("  >_ OpenAI Codex (v0.160.1)\n     seat\n\n  Shall we turn “huh?” into “aha!”?\n\n{history}\n\n› {composer}\n\n  {FOOTER}\n")
}
fn status(cwd: &str, id: &str) -> String {
    format!("› /status\n\n  >_ OpenAI Codex (v0.160.1)\n\n  Visit https://chatgpt.com/codex/settings/usage for up-to-date\n  information on rate limits and credits\n\n  Model:           gpt-6-luna (reasoning medium)\n  Directory:       {cwd}\n  Permissions:     Full Access\n  Agents.md:       <none>\n  Session:         {id}\n\n  Context window:  100% left (0 used / 272K)\n  Limits:          refreshing")
}
fn shape(text: &str) -> InputShape {
    InputShape { sha256: digest(text.as_bytes()), bytes: text.len(), lines: text.lines().count(), first_line: text.lines().next().unwrap().into() }
}
fn frame(text: String, operation: Operation, step: Option<StepKind>, latch: PasteLatch) -> CaptureFrame {
    let payload = format!("{HEADER}\n\nCompute 123 * 45.\n\n{TOKEN}");
    CaptureFrame { scope: scope(2), text, cwd: Some("/owned/cwd".into()), input: Some(shape(&payload)),
        baseline: Some(CaptureBaseline { scope: scope(1), text: screen("", PLACEHOLDER) }), profile_id: PROFILE.into(), operation,
        message: matches!(operation, Operation::FirstBusiness | Operation::OrdinarySend).then(|| MessageId::new("message-1").unwrap()),
        attempt: Some(AttemptId::new("attempt-1").unwrap()), after_step: step, paste_latch: latch }
}
fn evidence(text: String, version: &str) -> ScopedSessionEvidence {
    ScopedSessionEvidence { scope: scope(1), provider: ProviderId::new(ID).unwrap(),
        native: NativeIdentity { version: version.into(), harness: "codex".into(), ui: "tui".into(), platform: Platform::MacOs, executable_sha256: digest(b"native") },
        cwd: CwdIdentity { path: "/owned/cwd".into(), identity: digest(b"cwd") }, origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: EvidenceKind::Fixture, evidence_sha256: digest(text.as_bytes()), record: text.into_bytes() }
}

#[test]
fn all_facets_and_hooks_disclose_owned_argv_server_coexistence_under_a_distinct_key() {
    let a = adapter(); let hooks = a.hooks();
    assert_eq!(CODEX_DESCRIPTOR.identity.id, "codex_tui");
    assert_eq!(CODEX_DESCRIPTOR.identity.binary, "codex");
    assert!(CODEX_DESCRIPTOR.identity.aliases.is_empty(), "the shared legacy key is a declaration, never an alias");
    for operation in [Operation::Catalog, Operation::Fresh, Operation::Resume, Operation::FirstBusiness, Operation::SessionInspect, Operation::Stop] {
        validate_descriptor(&CODEX_DESCRIPTOR, &hooks, operation).unwrap();
    }
    for operation in [Operation::NativeNewSeat, Operation::NewSeatFullSnapshot, Operation::InWindowBranch] {
        assert!(validate_descriptor(&CODEX_DESCRIPTOR, &hooks, operation).is_err(), "{operation:?}");
    }
    assert_eq!(CODEX_DESCRIPTOR.mcp.carrier, Support::Supported(McpCarrier::Argv));
    assert_eq!(CODEX_DESCRIPTOR.mcp.isolation, McpIsolation::OwnedServerIsolated);
    assert_eq!(CODEX_DESCRIPTOR.tool_names.naming, Support::Supported(ToolNaming::RuntimeBound));
    assert_eq!(CODEX_DESCRIPTOR.prompt.carrier, Support::Supported(PromptCarrier::RuntimeBridge));
    assert_eq!(CODEX_DESCRIPTOR.session.fresh, FreshSession::CaptureAfterLaunch);
    assert_eq!(CODEX_DESCRIPTOR.session.resume, Support::Supported(ResumeMode::ExactId));
    assert_eq!(CODEX_DESCRIPTOR.effort.carrier, Support::Supported(ValueCarrier::ConfigArgument { flag: "-c", key: "model_reasoning_effort" }));
    assert!(CODEX_DESCRIPTOR.effort.model_dependent && !CODEX_DESCRIPTOR.effort.inherit_team_default);
    assert!(CODEX_DESCRIPTOR.effort.admission.iter().all(|admission| *admission == EffortAdmission::Pass));
    assert!(matches!(CODEX_DESCRIPTOR.startup.authorized_consent, Support::Unsupported(_)));
    let bypass = CODEX_DESCRIPTOR.bypass.intent.require("bypass").unwrap();
    assert_eq!(bypass.enabled_arguments, ["--dangerously-bypass-approvals-and-sandbox"]);
    assert!(!bypass.requires_startup_consent);
    assert_eq!(POLICY, digest(POLICY_RECIPE.as_bytes()));
    assert_eq!(CONTROLS.iter().map(|control| control.command).collect::<Vec<_>>(), ["/status", "/exit"]);
    PROFILES[0].policy.require("policy").unwrap().validate().unwrap();
    assert_eq!(PROFILES[0].policy.require("policy").unwrap().max_submit_keys, 1);
}
#[test]
fn evergreen_version_keeps_prerelease_and_bounds_bad_input() {
    for version in ["0.160.1", "0.162.1", "1.0.0-alpha.3", "12.345.6789"] {
        assert_eq!(parse_version(format!("codex-cli {version}\n").as_bytes()).unwrap(), version);
    }
    for bad in ["0.160.1", "codex 0.160.1", "codex-cli 0.160", "codex-cli 01.160.1", "codex-cli 0.160.1-", "codex-cli 0.160.1-a_b", "codex-cli 0.160.1\nextra"] {
        assert!(parse_version(bad.as_bytes()).is_err(), "{bad}");
    }
    let error = parse_version(&[b'x'; 900]).unwrap_err().to_string();
    assert!(error.contains("truncated=true")); assert!(error.len() < 450);
}
#[test]
fn catalog_keeps_listed_ids_and_their_own_team_efforts_without_fallback() {
    let records = parse_catalog(CATALOG.as_bytes()).unwrap();
    assert_eq!(records.iter().map(|record| record.id.as_str()).collect::<Vec<_>>(), ["gpt-6-luna", "gpt-6-sol"]);
    assert_eq!(records[0].efforts, Support::Supported(vec![Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max]));
    assert_eq!(records[1].efforts, Support::Supported(vec![Effort::Low, Effort::Ultra]), "none is not a Team tier");
    assert!(matches!(parse_catalog(CATALOG.replace("gpt-6-sol", "gpt-6-luna").as_bytes()), Err(ContractError::AmbiguousModel)));
    for bad in [CATALOG.replace("\"hide\"", "\"secret\""), CATALOG.replace("supported_reasoning_levels\":[]", "supported_reasoning_levels\":null"),
        CATALOG.replace("\"list\"", "\"hide\""), "{\"models\":[]}".into(), "not json".into(), CATALOG.replace("gpt-6-luna", "gpt 6 luna"),
        CATALOG.replace("{\"effort\":\"high\",\"description\":\"h\"}", "{\"effort\":\"low\",\"description\":\"h\"}")]
    {
        assert!(parse_catalog(bad.as_bytes()).is_err(), "{bad}");
    }
}
#[test]
fn owned_server_overrides_are_toml_values_with_a_per_generation_key() {
    let owner = identity();
    let key = server_key(&owner);
    let values: Vec<String> = adapter().mcp_overrides(&owner).unwrap().into_iter().map(|value| value.into_string().unwrap()).collect();
    assert!(values.chunks(2).all(|pair| pair[0] == "-c"));
    let values: Vec<&str> = values.iter().skip(1).step_by(2).map(String::as_str).collect();
    assert_eq!(values, [
        format!("mcp_servers.{key}.command=\"/fake/candidate\""),
        format!("mcp_servers.{key}.args=[\"contract-mcp\", \"a b/中文\"]"),
        format!("mcp_servers.{key}.env={{ \"OWNED_VALUE\" = \"quoted \\\"text\\\"\\nnext\" }}"),
        format!("mcp_servers.{key}.tool_timeout_sec=600.0"),
        format!("mcp_servers.{key}.default_tools_approval_mode=\"approve\""),
    ]);
    let mut other = owner.clone(); other.generation = Generation(2); assert_ne!(key, server_key(&other));
    other = owner; other.seat = SeatId::new("peer").unwrap(); assert_ne!(key, server_key(&other));
    assert!(CodexAdapter::new(McpStdio { executable: "/fake/candidate".into(), arguments: vec!["bell\u{7}".into()], environment: BTreeMap::new() })
        .unwrap().mcp_overrides(&identity()).is_err());
}

struct Runner;
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        // Probes run the PATH entry itself and leave the user's environment alone.
        assert_eq!(request.arguments, [OsString::from("--version")]);
        assert_eq!(request.executable.file_name().unwrap(), "codex");
        assert!(request.environment.set.is_empty() && request.environment.remove.is_empty());
        CommandReceipt { end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(42), child_reaped: true,
            stdout: b"codex-cli 0.160.1\n".to_vec(), stderr: vec![], elapsed: Duration::from_millis(1) }
    }
}
fn write_executable(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
/// vendor/bin/codex -> vendor/lib/codex.js (`#!/usr/bin/env node`) and a
/// `node` on the same PATH, the layout an npm install leaves. `node` resolves
/// to `image` (a real native program, so exec-through is observable).
fn npm_install(root: &Path, image: &Path) -> (PathBuf, PathBuf) {
    let (bin, script, runtime) = (root.join("vendor/bin"), root.join("vendor/lib/codex.js"), root.join("runtime/bin"));
    write_executable(&script, b"#!/usr/bin/env node\n// Unified entry point for the Codex CLI.\n");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&script, bin.join("codex")).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    std::os::unix::fs::symlink(image, runtime.join("node")).unwrap();
    (bin, runtime)
}
fn codex_launch(sandbox: &support::Sandbox, command: &PublicCommand, project: &Path) -> (CodexAdapter, LaunchRequest, CatalogObservation, RuntimeInstallation) {
    let bounds = ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 };
    let metadata = probe_launcher(command, &mut Runner, bounds).unwrap();
    let mut runtime = probe_runtime(command, &sandbox.parent, &mut Runner, bounds).unwrap(); runtime.native.platform = Platform::MacOs;
    let catalog = CatalogObservation { provider: ProviderId::new(ID).unwrap(), native: metadata, schema: CATALOG_SOURCE.schema.into(), models: parse_catalog(CATALOG.as_bytes()).unwrap() };
    let catalog = runtime.bind_catalog(&catalog).unwrap();
    let a = adapter().with_runtime(runtime.clone()).with_project(project.into()).unwrap();
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let mut r = support::request(&sandbox.root, "worker"); r.provider = ID.into(); r.native = runtime.native.clone();
    r.paths.launcher = runtime.launcher(); r.paths.executable = runtime.engine.clone(); r.paths.candidate = "/fake/candidate".into();
    r.model = Some("gpt-6-luna".into()); r.prompt = Some("user\nproject\nrole\nTEAM".into()); r.input_profile = Some(PROFILE.into()); r.bypass = true;
    (a, r, catalog, runtime)
}
fn plan(a: &CodexAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> Result<(ResolvedLaunch, LaunchPlan), ContractError> {
    let resolved = resolve_launch(&CODEX_DESCRIPTOR, &a.hooks(), r, Some(catalog))?;
    let plan = a.plan(&resolved)?; validate_launch_plan(&CODEX_DESCRIPTOR, &resolved, &plan)?;
    Ok((resolved, plan))
}
fn strings(plan: &LaunchPlan) -> Vec<String> { plan.arguments.iter().map(|arg| arg.to_str().unwrap().to_owned()).collect() }

#[test]
fn engine_is_derived_from_the_path_route_never_a_guessed_install() {
    let sandbox = support::Sandbox::new();
    let echo = Path::new("/bin/echo").canonicalize().unwrap();
    // npm: the route ends in an `env node` launcher; PATH's own node is the image.
    let (bin, runtime) = npm_install(&sandbox.parent, Path::new("/bin/echo"));
    let wrappers = sandbox.parent.join("user wrappers");
    write_executable(&wrappers.join("codex"), format!("#!/bin/sh\nexec '{}' \"$@\"\n", bin.join("codex").display()).as_bytes());
    let path = std::env::join_paths([wrappers.as_path(), bin.as_path(), runtime.as_path()]).unwrap();
    let command = PublicCommand::resolve("codex", &path).unwrap();
    assert_eq!(command.entry, wrappers.join("codex"), "the user wrapper is the public entry");
    assert_eq!(resolve_engine(&command).unwrap(), echo);
    // Standalone: the first non-script image on the route.
    let standalone = sandbox.parent.join("standalone/bin");
    std::fs::create_dir_all(&standalone).unwrap();
    std::os::unix::fs::symlink(&echo, standalone.join("codex")).unwrap();
    assert_eq!(resolve_engine(&PublicCommand::resolve("codex", standalone.as_os_str()).unwrap()).unwrap(), echo);
    // A wrapper with nothing native after it, or an env-node launcher without
    // node on this PATH, is refused rather than resolved elsewhere.
    assert!(resolve_engine(&PublicCommand::resolve("codex", wrappers.as_os_str()).unwrap()).is_err());
    let no_node = std::env::join_paths([wrappers.as_path(), bin.as_path()]).unwrap();
    assert!(resolve_engine(&PublicCommand::resolve("codex", &no_node).unwrap()).is_err());
}
#[test]
fn launch_runs_the_user_wrapper_prepended_to_path_then_the_node_image() {
    let sandbox = support::Sandbox::new();
    let (bin, runtime) = npm_install(&sandbox.parent, Path::new("/bin/echo"));
    let wrappers = sandbox.parent.join("user wrappers");
    let marker = sandbox.parent.join("wrapper-invocations");
    write_executable(&wrappers.join("codex"), format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"${{0##*/}}\" \"$1\" >> '{}'\nexec '{}' \"$@\"\n",
        marker.display(), bin.join("codex").display()).as_bytes());
    let path = std::env::join_paths([wrappers.as_path(), bin.as_path(), runtime.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let command = PublicCommand::resolve("codex", &path).unwrap();
    let project = sandbox.parent.join("project");
    let (a, r, catalog, runtime) = codex_launch(&sandbox, &command, &project);
    let (_, plan) = plan(&a, &r, &catalog).unwrap();
    assert_eq!(plan.launcher.executable, wrappers.join("codex"));
    assert_eq!(plan.executable, Path::new("/bin/echo").canonicalize().unwrap());
    assert_eq!(runtime.engine, plan.executable);
    let script = team_agent_contract::host::shell::launch_script(&plan, &Default::default()).unwrap();
    let text = String::from_utf8(script.clone()).unwrap();
    assert!(text.contains(&format!("\nexport PATH='{}'\nexec 'codex' '--no-alt-screen' '--no-daemon'", path.to_str().unwrap())), "{text}");
    for forbidden in ["/usr/local/bin/codex", "/opt/homebrew", "node_modules", ".local/bin", "CODEX_REAL", "codex.js"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
    assert!(!text.contains(&format!("exec '{}", plan.executable.display())), "the image is a fence, never exec'd");
    // Run the generated script with the real system shell: PATH lookup of bare
    // `codex` reaches the user wrapper, which execs the vendor launcher, whose
    // `env node` re-resolves through the exported PATH to the native image.
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let launch = sandbox.parent.join("launch.sh");
    std::fs::write(&launch, &script).unwrap();
    let output = std::process::Command::new("/bin/sh").arg(&launch).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&marker).expect("the user wrapper genuinely ran"), "codex|--no-alt-screen\n");
    let echoed = String::from_utf8(output.stdout).unwrap();
    assert!(echoed.starts_with(&format!("{} --no-alt-screen --no-daemon -c check_for_update_on_startup=false --model gpt-6-luna", bin.join("codex").display())), "{echoed}");
    assert!(echoed.contains("--dangerously-bypass-approvals-and-sandbox"));
}
#[test]
fn plan_carries_model_effort_bypass_mcp_and_exact_resume_without_startup_keys() {
    let sandbox = support::Sandbox::new();
    let image = sandbox.parent.join("vendor/lib/codex-native");
    write_executable(&image, b"controlled final native");
    let bin = sandbox.parent.join("vendor/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&image, bin.join("codex")).unwrap();
    let command = PublicCommand::resolve("codex", bin.as_os_str()).unwrap();
    let project = sandbox.parent.join("project");
    let (a, mut r, catalog, runtime) = codex_launch(&sandbox, &command, &project);
    assert_eq!(runtime.native.executable_sha256, digest(b"controlled final native"));
    r.role_effort = Some("max".into());
    let (_, plan) = plan(&a, &r, &catalog).unwrap();
    let args = strings(&plan);
    let key = server_key(&r.identity);
    assert_eq!(&args[..6], ["--no-alt-screen", "--no-daemon", "-c", "check_for_update_on_startup=false", "--model", "gpt-6-luna"]);
    assert_eq!(&args[6..8], ["--add-dir", project.to_str().unwrap()]);
    assert!(args[9].starts_with("developer_instructions=\"The actual task project is "));
    assert_eq!(args[11], format!("mcp_servers.{key}.command=\"/fake/candidate\""));
    assert!(args.ends_with(&["--dangerously-bypass-approvals-and-sandbox".into(), "-c".into(), "model_reasoning_effort=max".into()]));
    assert!(!args.iter().any(|arg| arg == "resume" || arg.contains("trust") || arg == "--last" || arg.contains("projects.")));
    assert_eq!(plan.carriers.model, CarrierUse::Specified(CarrierRef::Arguments(vec![4, 5])));
    assert!(matches!(&plan.carriers.mcp, CarrierUse::Specified(CarrierRef::Arguments(indices)) if indices.len() == 10 && indices[0] == 10));
    assert_eq!(plan.environment.remove, std::collections::BTreeSet::from(["CODEX_API_KEY".to_string(), "OPENAI_API_KEY".to_string()]));
    assert!(!plan.environment.set.contains_key("PATH") && !plan.environment.set.contains_key("CODEX_HOME"));
    assert_eq!(plan.materialization.len(), 1);
    assert_eq!(plan.materialization[0].kind, ResourceKind::Prompt);
    assert_eq!(plan.materialization[0].contents, b"user\nproject\nrole\nTEAM");
    assert_eq!(plan.expected_session, ExpectedSession::CaptureAfterLaunch);
    // Effort is admitted per catalog model: luna lists max but not ultra.
    r.role_effort = Some("ultra".into());
    assert!(matches!(plan_err(&a, &r, &catalog), ContractError::EffortNotAvailableForModel));
    r.model = Some("gpt-6-sol".into());
    assert!(strings(&plan_ok(&a, &r, &catalog)).ends_with(&["-c".into(), "model_reasoning_effort=ultra".into()]));
    r.role_effort = None; r.bypass = false;
    let args = strings(&plan_ok(&a, &r, &catalog));
    assert!(!args.iter().any(|arg| arg.contains("dangerously") || arg.contains("model_reasoning_effort")));
    // Model omission, hidden or invented IDs never launch.
    for model in [None, Some("codex-auto-review"), Some("gpt-6-luna-invented")] {
        r.model = model.map(Into::into);
        assert!(resolve_launch(&CODEX_DESCRIPTOR, &a.hooks(), &r, Some(&catalog)).is_err(), "{model:?}");
    }
    r.model = Some("gpt-6-luna".into());
    // The project itself can never be the private primary.
    let mut shared = r.clone(); shared.paths.cwd.path = project.clone();
    assert!(plan_ok_result(&a, &shared, &catalog).is_err());
    // A launcher other than the probed PATH route can never be planned.
    let mut wrong = r.clone(); wrong.paths.launcher.sha256 = digest(b"another entry");
    assert!(plan_ok_result(&a, &wrong, &catalog).is_err());
    // Exact resume: the native ID is the last operand of `codex resume`.
    let binding = ResumeBinding { provider: ProviderId::new(ID).unwrap(), native_session: NativeSessionId::new(SID).unwrap(), storage: SessionStorage::Local,
        cwd: r.paths.cwd.clone(), native: r.native.clone(), source: r.identity.clone(), origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: r.evidence_kind, evidence_sha256: digest(b"status card"), backing: None };
    let mut resume = r.clone(); resume.operation = Operation::Resume; resume.identity.generation = Generation(2);
    resume.resume = Some(ResumeRequest { binding: binding.clone(), expected_source: r.identity.clone() });
    let args = strings(&plan_ok(&a, &resume, &catalog));
    assert_eq!(args.first().map(String::as_str), Some("resume"));
    assert_eq!(args.last().map(String::as_str), Some(SID));
    resume.resume.as_mut().unwrap().binding.native_session = NativeSessionId::new("not-a-uuid").unwrap();
    assert!(plan_ok_result(&a, &resume, &catalog).is_err());
}
fn plan_ok_result(a: &CodexAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> Result<LaunchPlan, ContractError> { plan(a, r, catalog).map(|(_, plan)| plan) }
fn plan_ok(a: &CodexAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> LaunchPlan { plan_ok_result(a, r, catalog).unwrap() }
fn plan_err(a: &CodexAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> ContractError { plan_ok_result(a, r, catalog).err().unwrap() }

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
    let mut wrong_path = prompt.clone(); wrong_path.path = OwnedPath::new("/owned/cwd".into(), ".codex/config.toml".into()).unwrap();
    let mut blank = prompt.clone(); blank.contents = b"  \n".to_vec();
    for bad in [vec![], vec![prompt.clone(), prompt.clone()], vec![wrong_kind], vec![wrong_path], vec![blank]] {
        let mut io = Io(vec![]);
        assert!(adapter().materialize(&bad, &mut io).is_err());
        assert!(io.0.is_empty(), "refused before any effect");
    }
}
#[test]
fn surfaces_follow_the_bottom_prompt_row_and_never_key_a_modal() {
    assert_eq!(surface(&screen("", PLACEHOLDER)), InputSurface::ComposerReady);
    assert_eq!(surface(&screen("", "")), InputSurface::ComposerReady);
    assert_eq!(surface(&screen("• Working (3s • esc to interrupt)", PLACEHOLDER)), InputSurface::Busy);
    assert_eq!(surface(&screen("• Booting MCP server: team_x (0s • esc to interrupt) · 1 queued", PLACEHOLDER)), InputSurface::Busy);
    assert_eq!(surface(&screen("", "draft text")), InputSurface::ComposerContainsPaste);
    // The ultra-effort prompt glyph is still the composer.
    assert_eq!(surface(&screen("", PLACEHOLDER).replace("› Ask", "» Ask")), InputSurface::ComposerReady);
    // History turns above the composer also start with `›`.
    assert_eq!(surface(&screen("› 1. an earlier user turn\n  continued", PLACEHOLDER)), InputSurface::ComposerReady);
    // The startup draft is drawn while the session still loads.
    let loading = format!("  >_ OpenAI Codex (v0.160.1)\n     loading\n\n  Pull up a prompt.\n\n› {PLACEHOLDER}\n\n  ? for shortcuts\n");
    assert_eq!(surface(&loading), InputSurface::Starting);
    assert_eq!(surface(&loading.replace("  Pull up a prompt.", "  Resuming session…").replace("     loading", "     seat")), InputSurface::Starting);
    let update = "\n  Update available · 0.160.1 → 0.162.1\n  Release notes: https://github.com/openai/codex/releases/latest\n\n› 1. Update now (runs `npm install -g @openai/codex`)\n  2. Skip\n  3. Skip until next version\n\n  enter continue · esc skip\n";
    let trust = "\n  Folder access\n  /owned/cwd\n\n  Trust this folder? Codex can read, edit, and run files here,\n  subject to your permission settings.\n\n› 1. Trust and continue\n  2. Quit\n\n  enter continue · esc quit\n";
    let hooks = "\n  Hooks need review\n  2 hooks are new or changed.\n\n› 1. Review hooks\n  2. Trust all and continue\n\n  enter confirm · esc skip\n";
    for (text, predicate) in [(update, "codex-update-modal"), (trust, "codex-folder-trust-modal"), (hooks, "codex-hooks-review-modal")] {
        assert_eq!(surface(text), InputSurface::StartupRiskWarning);
        let observed = adapter().interpret(&frame(text.into(), Operation::OrdinarySend, None, PasteLatch::NeverSeen));
        assert_eq!(observed.predicate.as_deref(), Some(predicate));
        assert_ne!(observed.surface, InputSurface::ComposerReady);
    }
    let approval = "\n  Would you like to run the following command?\n\n  $ rm -rf build\n\n› 1. Yes, proceed (y)\n  2. No, and tell Codex what to do differently (esc)\n";
    assert_eq!(surface(approval), InputSurface::ShellOrUnknown);
    assert_eq!(surface("plain shell $ "), InputSurface::ShellOrUnknown);
    // Wrong profile never yields a surface.
    let mut wrong = frame(screen("", PLACEHOLDER), Operation::OrdinarySend, None, PasteLatch::NeverSeen); wrong.profile_id = "other".into();
    assert_eq!(adapter().interpret(&wrong).surface, InputSurface::ShellOrUnknown);
}
#[test]
fn inline_paste_is_seen_by_its_token_and_accepted_only_from_history() {
    let composer = format!("{HEADER}\n  \n  Compute 123 * 45.\n  \n  {TOKEN}");
    let pasted = frame(screen("", &composer), Operation::OrdinarySend, Some(StepKind::Paste), PasteLatch::NeverSeen);
    let observed = adapter().interpret(&pasted);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    assert_eq!(observed.paste_latch, PasteLatch::Seen { native_identity: TOKEN.into() });
    assert_eq!(observed.current_attempt, pasted.attempt);
    // A replayed token already on screen before this paste is not this input.
    let mut replay = pasted.clone(); replay.baseline.as_mut().unwrap().text = screen(&format!("› {TOKEN}"), PLACEHOLDER);
    assert_eq!(adapter().interpret(&replay).paste_latch, PasteLatch::NeverSeen);
    let history = format!("› {HEADER}\n  \n  Compute 123 * 45.\n  \n  {TOKEN}\n\n• Working (0s • esc to interrupt)");
    let submitted = frame(screen(&history, PLACEHOLDER), Operation::OrdinarySend, Some(StepKind::InitialSubmit), observed.paste_latch.clone());
    let accepted = adapter().interpret(&submitted);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.paste_latch, PasteLatch::Gone { native_identity: TOKEN.into() });
    // The token still in the composer (Enter not consumed) is not acceptance.
    let held = frame(screen("", &composer), Operation::OrdinarySend, Some(StepKind::InitialSubmit), observed.paste_latch);
    assert_ne!(adapter().interpret(&held).surface, InputSurface::NativeAccepted);
}
#[test]
fn folded_paste_is_bound_to_this_attempt_and_accepted_by_the_full_token() {
    let folded = "[Pasted Content 1234 chars]";
    let mut pasted = frame(screen("", folded), Operation::FirstBusiness, Some(StepKind::Paste), PasteLatch::NeverSeen);
    pasted.input.as_mut().unwrap().bytes = 1290;
    let observed = adapter().interpret(&pasted);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    let PasteLatch::Seen { native_identity } = &observed.paste_latch else { panic!("{:?}", observed.paste_latch) };
    assert!(native_identity.starts_with("codex-owned-paste-v1:"));
    assert_eq!((observed.current_message.as_ref(), observed.current_attempt.as_ref()), (pasted.message.as_ref(), pasted.attempt.as_ref()));
    for case in 0..7 {
        let mut bad = pasted.clone();
        match case {
            0 => bad.after_step = None,
            1 => bad.input.as_mut().unwrap().bytes = 1233, // more characters than payload bytes
            2 => bad.text = screen("", "[Pasted Content 999 chars]"), // never folded at this size
            3 => bad.baseline.as_mut().unwrap().text = screen("", "unsent draft"),
            4 => bad.baseline.as_mut().unwrap().scope.identity.generation = Generation(9),
            5 => bad.attempt = None,
            _ => bad.text = screen("", "[Pasted Content 1234 chars] typed suffix"),
        }
        assert!(matches!(adapter().interpret(&bad).paste_latch, PasteLatch::NeverSeen), "case {case}");
    }
    let history = format!("› {HEADER}\n  …\n  {TOKEN}\n\n• Working (1s • esc to interrupt)");
    let mut submitted = pasted.clone(); submitted.text = screen(&history, PLACEHOLDER);
    submitted.after_step = Some(StepKind::InitialSubmit); submitted.paste_latch = observed.paste_latch.clone();
    let accepted = adapter().interpret(&submitted);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert!(matches!(accepted.paste_latch, PasteLatch::Gone { .. }));
    // Without the full token in history the folded submit stays unresolved.
    submitted.text = screen("› [Pasted Content 1234 chars]\n\n• Working (1s • esc to interrupt)", PLACEHOLDER);
    assert_ne!(adapter().interpret(&submitted).surface, InputSurface::NativeAccepted);
}
#[test]
fn status_control_is_typed_then_accepted_only_by_a_new_card_for_this_cwd() {
    let typed = frame(screen("", "/status"), Operation::SessionInspect, Some(StepKind::Paste), PasteLatch::NeverSeen);
    let seen = adapter().interpret(&typed);
    assert_eq!(seen.paste_latch, PasteLatch::Seen { native_identity: "/status".into() });
    let mut card = frame(screen(&status("/owned/cwd", SID), PLACEHOLDER), Operation::SessionInspect, Some(StepKind::InitialSubmit), seen.paste_latch.clone());
    assert_eq!(adapter().interpret(&card).surface, InputSurface::NativeAccepted);
    for bad in [status("/other/cwd", SID), status("/owned/cwd", "not-a-uuid")] {
        card.text = screen(&bad, PLACEHOLDER);
        assert_ne!(adapter().interpret(&card).surface, InputSurface::NativeAccepted);
    }
    // An old card already in the baseline is not this control's result.
    card.text = screen(&status("/owned/cwd", SID), PLACEHOLDER);
    card.baseline.as_mut().unwrap().text = card.text.clone();
    assert_ne!(adapter().interpret(&card).surface, InputSurface::NativeAccepted);
    let exit = adapter().interpret(&frame(screen("", "/exit"), Operation::Stop, Some(StepKind::Paste), PasteLatch::NeverSeen));
    assert_eq!(exit.paste_latch, PasteLatch::Seen { native_identity: "/exit".into() });
}
#[test]
fn session_binding_reads_only_the_last_status_card_with_matching_cwd_and_version() {
    let text = screen(&format!("› Session: 11111111-1111-1111-1111-111111111111\n\n{}", status("/owned/cwd", SID)), PLACEHOLDER);
    let binding = adapter().bind(&evidence(text.clone(), "0.160.1")).unwrap();
    assert_eq!(binding.native_session.as_str(), SID);
    assert_eq!(binding.origin, CaptureOrigin::CurrentNativeSession);
    assert!(adapter().bind(&evidence(text, "0.162.1")).is_err(), "the TUI version must be this generation's");
    let older = format!("{}\n\n{}", status("/owned/cwd", "22222222-2222-2222-2222-222222222222"), status("/owned/cwd", SID));
    assert_eq!(adapter().bind(&evidence(screen(&older, PLACEHOLDER), "0.160.1")).unwrap().native_session.as_str(), SID);
    let duplicate = status("/owned/cwd", SID).replace("  Agents.md:", &format!("  Session:         {SID}\n  Agents.md:"));
    for bad in [screen("", PLACEHOLDER), screen(&status("/wrong/cwd", SID), PLACEHOLDER), screen(&duplicate, PLACEHOLDER)] {
        assert!(adapter().bind(&evidence(bad, "0.160.1")).is_err());
    }
    let mut forged = evidence(screen(&status("/owned/cwd", SID), PLACEHOLDER), "0.160.1"); forged.origin = CaptureOrigin::ExplicitResumeLaunch;
    assert!(adapter().bind(&forged).is_err());
}
#[test]
fn private_primary_rejects_git_and_inherited_codex_config_without_reading_bodies() {
    let sandbox = support::Sandbox::new();
    let parent = sandbox.parent.join("primary-owner"); let cwd = parent.join("seat");
    std::fs::create_dir_all(&cwd).unwrap(); let identity = resolve_cwd(&cwd).unwrap();
    require_private_primary(&identity).unwrap();
    std::fs::write(parent.join(".git"), b"not read").unwrap();
    assert!(require_private_primary(&identity).is_err());
    std::fs::remove_file(parent.join(".git")).unwrap();
    std::fs::create_dir(parent.join(".codex")).unwrap();
    std::fs::write(parent.join(".codex/config.toml"), b"not parsed or merged").unwrap();
    assert!(require_private_primary(&identity).is_err());
}
#[test]
fn semantic_reader_maps_only_busy_and_ready() {
    let read = |text: &str| adapter().read(&NativeRecord { scope: scope(1), kind: NativeRecordKind::Surface, bytes: text.as_bytes().to_vec() }).outcome;
    assert_eq!(read(&screen("• Working (3s • esc to interrupt)", PLACEHOLDER)), ProbeOutcome::Observed(SemanticEvidence::Busy));
    assert_eq!(read(&screen("", PLACEHOLDER)), ProbeOutcome::Observed(SemanticEvidence::Idle));
    assert!(matches!(read("plain"), ProbeOutcome::Unknown(_)));
}
