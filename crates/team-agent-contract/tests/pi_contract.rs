//! Controlled Pi grammar/identity regressions, not native Team G1–G5. The two
//! TUI captures and catalog tables are legacy measured Pi receipts; the other
//! frames follow the pi 1.4 pi-tui editor, footer and `/session` renderers.
#![cfg(unix)]
mod support;
use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, path::{Path, PathBuf}, time::Duration};
use team_agent_contract::contract::{delivery::*, descriptor::*, hooks::*, plan::*, probe::*, session::*, types::*};
use team_agent_contract::host::{command::*, digest, process::resolve_cwd};
use team_agent_contract::providers::pi::{catalog::*, interaction::*, launcher::*, *};

const SID: &str = "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b";
const OTHER: &str = "019a2b3c-0000-7000-8000-000000000000";
const HEADER: &str = "Team Agent message from leader for task-1:";
const TOKEN: &str = "[team-agent-token:message-1]";
const MODEL: &str = "openai-codex/gpt-6-luna";
const R6_IDLE: &str = include_str!("fixtures/pi-tui-idle-24.txt");
const R7_FOLDED: &str = include_str!("fixtures/pi-tui-folded-24.txt");
const SPACED: &str = include_str!("fixtures/pi-list-models-spaced-ids.txt");
const G0: &str = include_str!("fixtures/pi-list-models-g0.txt");
const FOOTER: &str = "~/…/workspaces/worker\n0.0%/272k (auto)                                                                    (openai-codex) gpt-6-luna • high";

/// Pi's own `--list-models` layout: every cell padded to its column, joined by two spaces.
fn table(rows: &[[&str; 6]]) -> String {
    let header = ["provider", "model", "context", "max-out", "thinking", "images"];
    let widths: Vec<usize> = (0..6).map(|i| rows.iter().map(|row| row[i].chars().count()).chain([header[i].len()]).max().unwrap()).collect();
    let line = |cells: &[&str; 6]| cells.iter().zip(&widths).map(|(cell, width)| format!("{cell:<width$}", width = *width)).collect::<Vec<_>>().join("  ");
    std::iter::once(line(&header)).chain(rows.iter().map(line)).map(|line| line + "\n").collect()
}
fn catalog_table() -> String {
    table(&[["openai-codex", "gpt-6-luna", "272K", "128K", "yes", "yes"], ["openai-codex", "gpt-6-luna-mini", "272K", "128K", "no", "yes"],
        ["team-agent", "Claude Opus 4.6 (Thinking)", "200K", "32.8K", "yes", "yes"]])
}
fn adapter() -> PiAdapter {
    PiAdapter::new(McpStdio { executable: "/fake/candidate".into(), arguments: vec!["contract-mcp".into(), "a b/中文".into()],
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
fn rule() -> String { "─".repeat(120) }
/// The working status embedded after the top border's `── ` prefix.
fn working() -> String { format!("── ⠋ Working (esc to interrupt) {}", "─".repeat(87)) }
/// History, the status rows, the editor between its two borders, then the footer.
fn framed(history: &str, top: &str, composer: &str) -> String {
    format!(" pi v1.4.0\n [Extensions]\n   team-agent-mcp.ts\n\n{history}\n\n\n{top}\n{composer}\n{}\n{FOOTER}\n", rule())
}
fn screen(history: &str, composer: &str) -> String { framed(history, &rule(), composer) }
fn info(id: &str) -> String {
    format!(" Session Info\n\n Name: worker\n File: /Users/someone/.pi/agent/sessions/--private-tmp-ta-workspaces-worker--/2026-10-26T01-02-03-456Z_{id}.jsonl\n ID: {id}\n\n Messages\n Total: 0\n User: 0\n Assistant: 0\n Tools: 0 calls, 0 results\n\n Tokens\n Input: 0\n Output: 0\n Total: 0")
}
fn shape(text: &str) -> InputShape {
    InputShape { sha256: digest(text.as_bytes()), bytes: text.len(), lines: text.lines().count(), first_line: text.lines().next().unwrap().into() }
}
fn frame(text: String, operation: Operation, step: Option<StepKind>, latch: PasteLatch) -> CaptureFrame {
    let payload = format!("{HEADER}\n\nCompute 123 * 45.\n\n{TOKEN}");
    CaptureFrame { scope: scope(2), text, cwd: Some("/owned/cwd".into()), input: Some(shape(&payload)),
        baseline: Some(CaptureBaseline { scope: scope(1), text: screen("", "") }), profile_id: PROFILE.into(), operation,
        message: matches!(operation, Operation::FirstBusiness | Operation::OrdinarySend).then(|| MessageId::new("message-1").unwrap()),
        attempt: Some(AttemptId::new("attempt-1").unwrap()), after_step: step, paste_latch: latch }
}
fn evidence(text: String) -> ScopedSessionEvidence {
    ScopedSessionEvidence { scope: scope(1), provider: ProviderId::new(ID).unwrap(),
        native: NativeIdentity { version: "1.4.0".into(), harness: "pi".into(), ui: "tui".into(), platform: Platform::MacOs, executable_sha256: digest(b"native") },
        cwd: CwdIdentity { path: "/owned/cwd".into(), identity: digest(b"cwd") }, origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: EvidenceKind::Fixture, evidence_sha256: digest(text.as_bytes()), record: text.into_bytes() }
}

#[test]
fn all_facets_and_hooks_disclose_the_owned_extension_server_under_a_distinct_key() {
    let a = adapter(); let hooks = a.hooks();
    assert_eq!(PI_DESCRIPTOR.identity.id, "pi_tui");
    assert_eq!(PI_DESCRIPTOR.identity.binary, "pi");
    assert!(PI_DESCRIPTOR.identity.aliases.is_empty(), "the shared legacy key is a declaration, never an alias");
    for operation in [Operation::Catalog, Operation::Fresh, Operation::Resume, Operation::FirstBusiness, Operation::SessionInspect, Operation::Stop] {
        validate_descriptor(&PI_DESCRIPTOR, &hooks, operation).unwrap();
    }
    for operation in [Operation::NativeNewSeat, Operation::NewSeatFullSnapshot, Operation::InWindowBranch] {
        assert!(validate_descriptor(&PI_DESCRIPTOR, &hooks, operation).is_err(), "{operation:?}");
    }
    assert_eq!(PI_DESCRIPTOR.mcp.carrier, Support::Supported(McpCarrier::ScopedConfig));
    assert_eq!(PI_DESCRIPTOR.mcp.scope, ResourceScope::WorkingDirectory);
    assert_eq!(PI_DESCRIPTOR.mcp.isolation, McpIsolation::OwnedServerIsolated);
    assert_eq!(PI_DESCRIPTOR.tool_names.naming, Support::Supported(ToolNaming::RuntimeBound));
    assert_eq!(PI_DESCRIPTOR.prompt.carrier, Support::Supported(PromptCarrier::RuntimeBridge));
    assert_eq!(PI_DESCRIPTOR.session.fresh, FreshSession::CaptureAfterLaunch);
    assert_eq!(PI_DESCRIPTOR.session.resume, Support::Supported(ResumeMode::ExactId));
    assert_eq!(PI_DESCRIPTOR.effort.carrier, Support::Supported(ValueCarrier::Flag("--thinking")));
    assert!(PI_DESCRIPTOR.effort.model_dependent && !PI_DESCRIPTOR.effort.inherit_team_default);
    for effort in [Effort::Low, Effort::Medium, Effort::High] {
        assert_eq!(PI_DESCRIPTOR.effort.admission[effort.index()], EffortAdmission::Pass);
    }
    for effort in [Effort::Xhigh, Effort::Max, Effort::Ultra] {
        assert!(matches!(PI_DESCRIPTOR.effort.admission[effort.index()], EffortAdmission::Reject(_)), "{effort:?}");
    }
    assert!(matches!(PI_DESCRIPTOR.bypass.intent, Support::Unsupported(_)), "Pi has no approval gate to bypass");
    assert!(matches!(PI_DESCRIPTOR.startup.authorized_consent, Support::Unsupported(_)));
    assert_eq!(POLICY, digest(POLICY_RECIPE.as_bytes()));
    assert_eq!(CONTROLS.iter().map(|control| control.command).collect::<Vec<_>>(), ["/session", "/quit"]);
    assert!(CONTROLS.iter().all(|control| control.input_mode == team_agent_contract::runtime::delivery::ControlInputMode::ProfilePaste));
    PROFILES[0].policy.require("policy").unwrap().validate().unwrap();
    assert_eq!(PROFILES[0].policy.require("policy").unwrap().max_submit_keys, 1);
}
#[test]
fn evergreen_version_is_bare_semver_and_bounds_bad_input() {
    for version in ["1.4.0", "1.4.0-beta.1", "12.345.6789", "0.0.1"] {
        assert_eq!(parse_version(format!("{version}\n").as_bytes()).unwrap(), version);
    }
    for bad in ["pi 1.4.0", "1.4", "01.4.0", "1.4.0-", "1.4.0-a_b", "1.4.0\nextra", "v1.4.0"] {
        assert!(parse_version(bad.as_bytes()).is_err(), "{bad}");
    }
    let error = parse_version(&[b'x'; 900]).unwrap_err().to_string();
    assert!(error.contains("truncated=true")); assert!(error.len() < 450);
}
#[test]
fn catalog_columns_keep_exact_spaced_ids_and_reasoning_rows_only() {
    let spaced = parse_catalog(SPACED.as_bytes()).unwrap();
    let ids: Vec<_> = spaced.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(ids.len(), 8);
    for id in ["cursor/claude-opus-4-7@1m:fast", "openai-codex/gpt-5.6-luna", "team-agent/Claude Opus 4.6 (Thinking)", "team-agent/qwen3.8-27b"] {
        assert!(ids.contains(&id), "{id}");
    }
    let g0 = parse_catalog(G0.as_bytes()).unwrap();
    assert_eq!(g0.len(), 221);
    let auto = g0.iter().find(|record| record.id == "cursor/auto").unwrap();
    assert_eq!(auto.efforts, Support::Supported(vec![]), "a non-reasoning model admits no effort");
    let reasoning = g0.iter().find(|record| record.id == "cursor/claude-fable-5@1m").unwrap();
    assert_eq!(reasoning.efforts, Support::Supported(vec![Effort::Low, Effort::Medium, Effort::High]));
    let built = catalog_table();
    assert_eq!(parse_catalog(built.as_bytes()).unwrap().iter().map(|record| record.id.as_str()).collect::<Vec<_>>(),
        ["openai-codex/gpt-6-luna", "openai-codex/gpt-6-luna-mini", "team-agent/Claude Opus 4.6 (Thinking)"]);
    let duplicate = table(&[["a", "m", "1K", "1K", "yes", "no"], ["a", "m", "2K", "1K", "no", "no"]]);
    assert!(matches!(parse_catalog(duplicate.as_bytes()), Err(ContractError::AmbiguousModel)));
    let misaligned = built.replacen("openai-codex  gpt-6-luna ", "openai-codex   gpt-6-luna", 1);
    for bad in [String::new(), "No models available. Use /login to authenticate.\n".into(), built.replace("thinking", "reasoning"),
        built.lines().next().unwrap().to_owned(), misaligned, built.replace("  yes       yes", "  maybe     yes"),
        table(&[["open ai", "m", "1K", "1K", "yes", "no"]]), table(&[["a/b", "m", "1K", "1K", "yes", "no"]])]
    {
        assert!(parse_catalog(bad.as_bytes()).is_err(), "{bad}");
    }
}
#[test]
fn owned_extension_carries_only_this_generation_server_as_data() {
    let owner = identity();
    let key = server_key(&owner);
    assert!(key.starts_with("team_") && key.len() == 37);
    assert!(format!("mcp__{key}__report_result").len() <= 64, "Pi caps a tool name at 64 characters");
    let request = adapter().mcp_resource(owner.clone(), "/owned/cwd".into()).unwrap();
    assert_eq!((request.kind, request.path.relative()), (ResourceKind::McpConfig, Path::new("team-agent-mcp.ts")));
    let text = String::from_utf8(request.contents).unwrap();
    let (first, module) = text.split_once('\n').unwrap();
    let server: serde_json::Value = serde_json::from_str(first.strip_prefix("const teamMcp = ").unwrap().strip_suffix(';').unwrap()).unwrap();
    assert_eq!(server, serde_json::json!({ "key": key, "command": "/fake/candidate", "args": ["contract-mcp", "a b/中文"],
        "env": { "OWNED_VALUE": "quoted \"text\"\nnext" }, "tools": ["send_message", "report_result", "get_team_status"] }));
    assert!(module.contains("export default function teamAgentMcp(pi)"));
    assert!(module.contains("pi.registerMcpServer(key"));
    for forbidden in ["writeFileSync", "renameSync", "randomUUID", "settings.json", "mcp.json"] {
        assert!(!module.contains(forbidden), "{forbidden}: the extension writes no file and mints no other name");
    }
    let mut other = owner.clone(); other.generation = Generation(2); assert_ne!(key, server_key(&other));
    other = owner; other.seat = SeatId::new("peer").unwrap(); assert_ne!(key, server_key(&other));
}

struct Runner;
impl CommandRunner for Runner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        // Probes run the PATH entry itself and leave the user's environment alone.
        assert_eq!(request.arguments, [OsString::from("--version")]);
        assert_eq!(request.executable.file_name().unwrap(), "pi");
        assert!(request.environment.set.is_empty() && request.environment.remove.is_empty());
        CommandReceipt { end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(42), child_reaped: true,
            stdout: b"1.4.0\n".to_vec(), stderr: b"(node:1) [UNDICI-EHPA] Warning: EnvHttpProxyAgent is experimental\n".to_vec(), elapsed: Duration::from_millis(1) }
    }
}
struct CatalogRunner(String);
impl CommandRunner for CatalogRunner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        assert_eq!(request.arguments, [OsString::from("--no-approve"), OsString::from("--list-models")]);
        assert_eq!(request.executable.file_name().unwrap(), "pi");
        assert!(request.environment.set.is_empty() && request.environment.remove.is_empty() && request.stdin.is_none());
        CommandReceipt { end: CommandEnd::Exited, exit_code: Some(0), child_pid: Some(43), child_reaped: true,
            stdout: self.0.clone().into_bytes(), stderr: vec![], elapsed: Duration::from_millis(1) }
    }
}
fn write_executable(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
/// vendor/bin/pi -> vendor/lib/cli.js (`#!/usr/bin/env node`) and a `node` on
/// the same PATH, the layout an npm install leaves. `node` resolves to `image`
/// (a real native program, so exec-through is observable).
fn npm_install(root: &Path, image: &Path) -> (PathBuf, PathBuf) {
    let (bin, script, runtime) = (root.join("vendor/bin"), root.join("vendor/lib/cli.js"), root.join("runtime/bin"));
    write_executable(&script, b"#!/usr/bin/env node\nimport { createRequire } from \"node:module\";\n");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&script, bin.join("pi")).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    std::os::unix::fs::symlink(image, runtime.join("node")).unwrap();
    (bin, runtime)
}
fn pi_launch(sandbox: &support::Sandbox, command: &PublicCommand, project: &Path) -> (PiAdapter, LaunchRequest, CatalogObservation, RuntimeInstallation) {
    let bounds = ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 };
    let metadata = probe_launcher(command, &mut Runner, bounds).unwrap();
    let mut runtime = probe_runtime(command, &sandbox.parent, &mut Runner, bounds).unwrap(); runtime.native.platform = Platform::MacOs;
    let catalog = CatalogObservation { provider: ProviderId::new(ID).unwrap(), native: metadata, schema: CATALOG_SOURCE.schema.into(), models: parse_catalog(catalog_table().as_bytes()).unwrap() };
    let catalog = runtime.bind_catalog(&catalog).unwrap();
    let a = adapter().with_runtime(runtime.clone()).with_project(project.into()).unwrap();
    std::fs::create_dir_all(&sandbox.root).unwrap();
    let mut r = support::request(&sandbox.root, "worker"); r.provider = ID.into(); r.native = runtime.native.clone();
    r.paths.launcher = runtime.launcher(); r.paths.executable = runtime.engine.clone(); r.paths.candidate = "/fake/candidate".into();
    r.model = Some(MODEL.into()); r.prompt = Some("user\nproject\nrole\nTEAM".into()); r.input_profile = Some(PROFILE.into());
    (a, r, catalog, runtime)
}
fn plan(a: &PiAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> Result<(ResolvedLaunch, LaunchPlan), ContractError> {
    let resolved = resolve_launch(&PI_DESCRIPTOR, &a.hooks(), r, Some(catalog))?;
    let plan = a.plan(&resolved)?; validate_launch_plan(&PI_DESCRIPTOR, &resolved, &plan)?;
    Ok((resolved, plan))
}
fn plan_ok_result(a: &PiAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> Result<LaunchPlan, ContractError> { plan(a, r, catalog).map(|(_, plan)| plan) }
fn plan_ok(a: &PiAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> LaunchPlan { plan_ok_result(a, r, catalog).unwrap() }
fn plan_err(a: &PiAdapter, r: &LaunchRequest, catalog: &CatalogObservation) -> ContractError { plan_ok_result(a, r, catalog).err().unwrap() }
fn strings(plan: &LaunchPlan) -> Vec<String> { plan.arguments.iter().map(|arg| arg.to_str().unwrap().to_owned()).collect() }

#[test]
fn engine_is_derived_from_the_path_route_never_a_guessed_install() {
    let sandbox = support::Sandbox::new();
    let echo = Path::new("/bin/echo").canonicalize().unwrap();
    // npm: the route ends in an `env node` launcher; PATH's own node is the image.
    let (bin, runtime) = npm_install(&sandbox.parent, Path::new("/bin/echo"));
    let wrappers = sandbox.parent.join("user wrappers");
    write_executable(&wrappers.join("pi"), format!("#!/bin/sh\nexec '{}' \"$@\"\n", bin.join("pi").display()).as_bytes());
    let path = std::env::join_paths([wrappers.as_path(), bin.as_path(), runtime.as_path()]).unwrap();
    let command = PublicCommand::resolve("pi", &path).unwrap();
    assert_eq!(command.entry, wrappers.join("pi"), "the user wrapper is the public entry");
    assert_eq!(command.env_launcher_image("node").unwrap(), echo);
    // Standalone: the first non-script image on the route.
    let standalone = sandbox.parent.join("standalone/bin");
    std::fs::create_dir_all(&standalone).unwrap();
    std::os::unix::fs::symlink(&echo, standalone.join("pi")).unwrap();
    assert_eq!(PublicCommand::resolve("pi", standalone.as_os_str()).unwrap().env_launcher_image("node").unwrap(), echo);
    // A wrapper with nothing native after it, or an env-node launcher without
    // node on this PATH, is refused rather than resolved elsewhere.
    assert!(PublicCommand::resolve("pi", wrappers.as_os_str()).unwrap().env_launcher_image("node").is_err());
    let no_node = std::env::join_paths([wrappers.as_path(), bin.as_path()]).unwrap();
    assert!(PublicCommand::resolve("pi", &no_node).unwrap().env_launcher_image("node").is_err());
}
#[test]
fn launch_runs_the_user_wrapper_prepended_to_path_then_the_node_image() {
    let sandbox = support::Sandbox::new();
    let (bin, runtime) = npm_install(&sandbox.parent, Path::new("/bin/echo"));
    let wrappers = sandbox.parent.join("user wrappers");
    let marker = sandbox.parent.join("wrapper-invocations");
    write_executable(&wrappers.join("pi"), format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"${{0##*/}}\" \"$1\" >> '{}'\nexec '{}' \"$@\"\n",
        marker.display(), bin.join("pi").display()).as_bytes());
    let path = std::env::join_paths([wrappers.as_path(), bin.as_path(), runtime.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    let command = PublicCommand::resolve("pi", &path).unwrap();
    let project = sandbox.parent.join("project");
    let (a, r, catalog, runtime) = pi_launch(&sandbox, &command, &project);
    let (_, plan) = plan(&a, &r, &catalog).unwrap();
    assert_eq!(plan.launcher.executable, wrappers.join("pi"));
    assert_eq!(plan.executable, Path::new("/bin/echo").canonicalize().unwrap());
    assert_eq!(runtime.engine, plan.executable);
    let script = team_agent_contract::host::shell::launch_script(&plan, &Default::default()).unwrap();
    let text = String::from_utf8(script.clone()).unwrap();
    assert!(text.contains(&format!("\nexport PATH='{}'\nexec 'pi' '--no-approve' '-e'", path.to_str().unwrap())), "{text}");
    for forbidden in ["/usr/local/bin/pi", "/opt/homebrew", "node_modules", ".local/bin", "pi-coding-agent", "cli.js"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
    assert!(!text.contains(&format!("exec '{}", plan.executable.display())), "the image is a fence, never exec'd");
    // Run the generated script with the real system shell: PATH lookup of bare
    // `pi` reaches the user wrapper, which execs the vendor launcher, whose
    // `env node` re-resolves through the exported PATH to the native image.
    let launch = sandbox.parent.join("launch.sh");
    std::fs::write(&launch, &script).unwrap();
    let output = std::process::Command::new("/bin/sh").arg(&launch).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&marker).expect("the user wrapper genuinely ran"), "pi|--no-approve\n");
    let echoed = String::from_utf8(output.stdout).unwrap();
    let extension = sandbox.root.join("team-agent-mcp.ts");
    assert!(echoed.starts_with(&format!("{} --no-approve -e {} --model {MODEL} --append-system-prompt The actual task project is {}.",
        bin.join("pi").display(), extension.display(), project.display())), "{echoed}");
}
#[test]
fn catalog_runs_the_path_entry_with_no_project_trust_and_no_env_delta() {
    let sandbox = support::Sandbox::new();
    let entry = sandbox.parent.join("bin/pi");
    write_executable(&entry, b"#!/usr/bin/env node\n");
    let command = PublicCommand::resolve("pi", entry.parent().unwrap().as_os_str()).unwrap();
    let bounds = ReadBounds { deadline: Duration::from_secs(10), max_output_bytes: 65536 };
    let native = probe_launcher(&command, &mut Runner, bounds).unwrap();
    assert_eq!((native.version.as_str(), native.harness.as_str(), native.ui.as_str()), ("1.4.0", "pi-launcher", "metadata"));
    let request = CatalogRequest { provider: ProviderId::new(ID).unwrap(), native, executable: command.entry.clone(),
        cwd: resolve_cwd(&sandbox.parent).unwrap(), source: CATALOG_SOURCE, bounds };
    let mut host = NativeCatalog::new(request.clone(), CatalogRunner(catalog_table())).unwrap();
    let observed = adapter().discover(&request, &mut host).unwrap();
    assert_eq!(observed.schema, "pi-list-models-table-v1");
    assert_eq!(observed.models.len(), 3);
    let mut foreign = request.clone(); foreign.source = CatalogSource { arguments: &["--list-models"], schema: "pi-list-models-table-v1" };
    assert!(NativeCatalog::new(foreign, CatalogRunner(String::new())).is_err(), "only the trust-free catalog grant runs");
}
#[test]
fn plan_carries_model_effort_extension_prompt_and_exact_resume_without_startup_keys() {
    let sandbox = support::Sandbox::new();
    let image = sandbox.parent.join("vendor/lib/pi-native");
    write_executable(&image, b"controlled final native");
    let bin = sandbox.parent.join("vendor/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&image, bin.join("pi")).unwrap();
    let command = PublicCommand::resolve("pi", bin.as_os_str()).unwrap();
    let project = sandbox.parent.join("project");
    let (a, mut r, catalog, runtime) = pi_launch(&sandbox, &command, &project);
    assert_eq!(runtime.native.executable_sha256, digest(b"controlled final native"));
    r.role_effort = Some("high".into());
    let plan = plan_ok(&a, &r, &catalog);
    let args = strings(&plan);
    let extension = sandbox.root.join("team-agent-mcp.ts");
    assert_eq!(&args[..5], ["--no-approve", "-e", extension.to_str().unwrap(), "--model", MODEL]);
    assert_eq!(args[5], "--append-system-prompt");
    assert!(args[6].starts_with(&format!("The actual task project is {}.", project.display())));
    assert_eq!(&args[7..], ["--thinking", "high"]);
    assert!(!args.iter().any(|arg| arg == "--approve" || arg.starts_with("--session") || arg == "--continue" || arg == "--resume"));
    assert_eq!(plan.carriers.model, CarrierUse::Specified(CarrierRef::Arguments(vec![3, 4])));
    assert_eq!(plan.carriers.effort, CarrierUse::Specified(CarrierRef::Arguments(vec![7, 8])));
    assert_eq!(plan.carriers.bypass, CarrierUse::NotRequested);
    assert!(plan.environment.remove.is_empty(), "the user's own Pi auth routes are untouched");
    assert_eq!(plan.environment.set.keys().collect::<Vec<_>>(), ["HOME"]);
    assert_eq!(plan.materialization.iter().map(|resource| (resource.kind, resource.path.relative().to_str().unwrap())).collect::<Vec<_>>(),
        [(ResourceKind::McpConfig, "team-agent-mcp.ts"), (ResourceKind::Prompt, "team-agent-prompt.txt")]);
    assert_eq!(plan.materialization[1].contents, b"user\nproject\nrole\nTEAM");
    assert_eq!(plan.carriers.mcp, CarrierUse::Specified(CarrierRef::Resource(plan.materialization[0].path.clone())));
    assert_eq!(plan.expected_session, ExpectedSession::CaptureAfterLaunch);
    // A spaced exact catalog ID stays one argv element.
    r.model = Some("team-agent/Claude Opus 4.6 (Thinking)".into());
    assert_eq!(strings(&plan_ok(&a, &r, &catalog))[4], "team-agent/Claude Opus 4.6 (Thinking)");
    // Levels Pi could clamp are refused, and a non-reasoning model takes none.
    for effort in ["xhigh", "max", "ultra"] {
        r.role_effort = Some(effort.into());
        assert!(matches!(plan_err(&a, &r, &catalog), ContractError::EffortRejected(_)), "{effort}");
    }
    r.model = Some("openai-codex/gpt-6-luna-mini".into()); r.role_effort = Some("low".into());
    assert!(matches!(plan_err(&a, &r, &catalog), ContractError::EffortNotAvailableForModel));
    r.role_effort = None;
    assert!(!strings(&plan_ok(&a, &r, &catalog)).iter().any(|arg| arg == "--thinking"));
    // No approval gate exists, so a bypass intent is refused, never dropped.
    r.bypass = true;
    assert!(plan_ok_result(&a, &r, &catalog).is_err());
    r.bypass = false;
    // Model omission, partial or invented IDs never launch.
    for model in [None, Some("gpt-6-luna"), Some("openai-codex/gpt-6-luna-invented")] {
        r.model = model.map(Into::into);
        assert!(resolve_launch(&PI_DESCRIPTOR, &a.hooks(), &r, Some(&catalog)).is_err(), "{model:?}");
    }
    r.model = Some(MODEL.into());
    // The project itself can never be the private primary.
    let mut shared = r.clone(); shared.paths.cwd.path = project.clone();
    assert!(plan_ok_result(&a, &shared, &catalog).is_err());
    // A launcher other than the probed PATH route can never be planned.
    let mut wrong = r.clone(); wrong.paths.launcher.sha256 = digest(b"another entry");
    assert!(plan_ok_result(&a, &wrong, &catalog).is_err());
    // Exact resume: `--session <uuid>` resolves only in this cwd's own store.
    let binding = ResumeBinding { provider: ProviderId::new(ID).unwrap(), native_session: NativeSessionId::new(SID).unwrap(), storage: SessionStorage::Local,
        cwd: r.paths.cwd.clone(), native: r.native.clone(), source: r.identity.clone(), origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: r.evidence_kind, evidence_sha256: digest(b"session info"), backing: None };
    let mut resume = r.clone(); resume.operation = Operation::Resume; resume.identity.generation = Generation(2);
    resume.resume = Some(ResumeRequest { binding, expected_source: r.identity.clone() });
    let args = strings(&plan_ok(&a, &resume, &catalog));
    assert!(args.ends_with(&["--session".into(), SID.into()]));
    resume.resume.as_mut().unwrap().binding.native_session = NativeSessionId::new("019a2b3c").unwrap();
    assert!(plan_ok_result(&a, &resume, &catalog).is_err(), "a partial ID could match another session");
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
fn materialization_writes_exactly_the_owned_extension_and_prompt() {
    let extension = adapter().mcp_resource(identity(), "/owned/cwd".into()).unwrap();
    let prompt = OwnedResourceRequest { owner: identity(), path: OwnedPath::new("/owned/cwd".into(), "team-agent-prompt.txt".into()).unwrap(), kind: ResourceKind::Prompt, contents: b"role".to_vec() };
    let mut io = Io(vec![]);
    assert_eq!(adapter().materialize(&[extension.clone(), prompt.clone()], &mut io).unwrap().resources.len(), 2);
    let mut tampered = extension.clone(); tampered.contents.extend_from_slice(b"\nimport('evil');");
    let mut foreign = extension.clone(); foreign.owner.generation = Generation(2);
    let mut project_local = extension.clone(); project_local.path = OwnedPath::new("/owned/cwd".into(), ".pi/extensions/team.ts".into()).unwrap();
    let mut blank = prompt.clone(); blank.contents = b"  \n".to_vec();
    for bad in [vec![], vec![extension.clone()], vec![prompt.clone(), extension.clone()], vec![tampered, prompt.clone()],
        vec![foreign, prompt.clone()], vec![project_local, prompt.clone()], vec![extension.clone(), blank]]
    {
        let mut io = Io(vec![]);
        assert!(adapter().materialize(&bad, &mut io).is_err());
        assert!(io.0.is_empty(), "refused before any effect");
    }
}
#[test]
fn surfaces_follow_the_editor_borders_and_never_key_a_dialog() {
    // Legacy measured Pi captures: an idle editor below a closed update notice,
    // and a folded paste capsule.
    assert_eq!(surface(R6_IDLE), InputSurface::ComposerReady);
    assert_eq!(surface(R7_FOLDED), InputSurface::ComposerContainsPaste);
    assert_eq!(surface(&screen("", "")), InputSurface::ComposerReady);
    assert_eq!(surface(&framed("", &working(), "")), InputSurface::Busy);
    // An editor that cannot embed the status shows a spinner row above it.
    assert_eq!(surface(&screen("⠙ Working (esc to interrupt)", "")), InputSurface::Busy);
    assert_eq!(surface(&screen("⠹ Retrying (1/3) in 2s... (esc to cancel)", "")), InputSurface::Busy);
    assert_eq!(surface(&screen("", "draft text")), InputSurface::ComposerContainsPaste);
    // Transcript rulers far above the editor, and a scrolled editor label, are not borders.
    let history = format!("{}\n assistant output after a markdown rule\n\n more output\n line\n line\n line\n line\n line\n line\n line\n line\n line\n line", rule());
    assert_eq!(surface(&screen(&history, "")), InputSurface::ComposerReady);
    let scrolled = format!("{} ↑ 3 more {}", "─".repeat(54), "─".repeat(55));
    assert_eq!(surface(&framed("", &scrolled, "last visible line")), InputSurface::ComposerContainsPaste);
    // No footer under the last ruler, a plain shell, or a dialog between rulers is never ready input.
    assert_eq!(surface(&format!("{}\n\n{}\n", rule(), rule())), InputSurface::ShellOrUnknown);
    assert_eq!(surface("plain shell $ "), InputSurface::ShellOrUnknown);
    let dialog = format!("{}\n Trust project-local files?\n → Yes\n   No\n{}\n{FOOTER}\n", rule(), rule());
    assert_ne!(surface(&dialog), InputSurface::ComposerReady);
    // Wrong profile never yields a surface.
    let mut wrong = frame(screen("", ""), Operation::OrdinarySend, None, PasteLatch::NeverSeen); wrong.profile_id = "other".into();
    assert_eq!(adapter().interpret(&wrong).surface, InputSurface::ShellOrUnknown);
}
#[test]
fn inline_paste_is_seen_by_its_token_and_accepted_only_from_history() {
    let composer = format!("{HEADER}\n\nCompute 123 * 45.\n\n{TOKEN}");
    let pasted = frame(screen("", &composer), Operation::OrdinarySend, Some(StepKind::Paste), PasteLatch::NeverSeen);
    let observed = adapter().interpret(&pasted);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    assert_eq!(observed.paste_latch, PasteLatch::Seen { native_identity: TOKEN.into() });
    assert_eq!(observed.current_attempt, pasted.attempt);
    // A replayed token already on screen before this paste is not this input.
    let mut replay = pasted.clone(); replay.baseline.as_mut().unwrap().text = screen(&format!(" {TOKEN}"), "");
    assert_eq!(adapter().interpret(&replay).paste_latch, PasteLatch::NeverSeen);
    let history = format!(" {HEADER}\n\n Compute 123 * 45.\n\n {TOKEN}");
    let submitted = frame(framed(&history, &working(), ""), Operation::OrdinarySend, Some(StepKind::InitialSubmit), observed.paste_latch.clone());
    let accepted = adapter().interpret(&submitted);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.paste_latch, PasteLatch::Gone { native_identity: TOKEN.into() });
    // The token still in the composer (Enter not consumed) is not acceptance.
    let held = frame(screen("", &composer), Operation::OrdinarySend, Some(StepKind::InitialSubmit), observed.paste_latch);
    assert_ne!(adapter().interpret(&held).surface, InputSurface::NativeAccepted);
}
#[test]
fn folded_paste_is_bound_to_this_attempt_and_accepted_by_the_full_token() {
    let mut pasted = frame(screen("", "[paste #1 +12 lines]"), Operation::FirstBusiness, Some(StepKind::Paste), PasteLatch::NeverSeen);
    pasted.input.as_mut().unwrap().lines = 12;
    let observed = adapter().interpret(&pasted);
    assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
    let PasteLatch::Seen { native_identity } = &observed.paste_latch else { panic!("{:?}", observed.paste_latch) };
    assert!(native_identity.starts_with("pi-owned-paste-v1:"));
    assert_eq!((observed.current_message.as_ref(), observed.current_attempt.as_ref()), (pasted.message.as_ref(), pasted.attempt.as_ref()));
    for case in 0..8 {
        let mut bad = pasted.clone();
        match case {
            0 => bad.after_step = None,
            1 => bad.input.as_mut().unwrap().lines = 11, // another payload's line count
            2 => bad.text = screen("", "[paste #1 +9 lines]"), // never folded at this size
            3 => bad.baseline.as_mut().unwrap().text = screen("", "unsent draft"),
            4 => bad.baseline.as_mut().unwrap().scope.identity.generation = Generation(9),
            5 => bad.attempt = None,
            6 => bad.text = screen("", "[paste #1 +12 lines] typed suffix"),
            _ => bad.text = screen("", "[paste #01 +12 lines]"),
        }
        assert!(matches!(adapter().interpret(&bad).paste_latch, PasteLatch::NeverSeen), "case {case}");
    }
    // Up to ten lines, Pi folds by UTF-16 characters, never more than the payload bytes.
    let mut chars = frame(screen("", "[paste #2 1234 chars]"), Operation::FirstBusiness, Some(StepKind::Paste), PasteLatch::NeverSeen);
    chars.input.as_mut().unwrap().bytes = 1290;
    assert!(matches!(adapter().interpret(&chars).paste_latch, PasteLatch::Seen { .. }));
    chars.input.as_mut().unwrap().bytes = 1233;
    assert!(matches!(adapter().interpret(&chars).paste_latch, PasteLatch::NeverSeen));
    chars.input.as_mut().unwrap().bytes = 1290; chars.text = screen("", "[paste #2 999 chars]");
    assert!(matches!(adapter().interpret(&chars).paste_latch, PasteLatch::NeverSeen));
    let history = format!(" {HEADER}\n …\n {TOKEN}");
    let mut submitted = pasted.clone(); submitted.text = framed(&history, &working(), "");
    submitted.after_step = Some(StepKind::InitialSubmit); submitted.paste_latch = observed.paste_latch.clone();
    let accepted = adapter().interpret(&submitted);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert!(matches!(accepted.paste_latch, PasteLatch::Gone { .. }));
    // Without the full token in history the folded submit stays unresolved.
    submitted.text = framed(" [paste #1 +12 lines]", &working(), "");
    assert_ne!(adapter().interpret(&submitted).surface, InputSurface::NativeAccepted);
}
#[test]
fn session_control_is_pasted_then_accepted_only_by_a_new_info_block() {
    let typed = frame(screen("", "/session"), Operation::SessionInspect, Some(StepKind::Paste), PasteLatch::NeverSeen);
    let seen = adapter().interpret(&typed);
    assert_eq!(seen.paste_latch, PasteLatch::Seen { native_identity: "/session".into() });
    let mut card = frame(screen(&info(SID), ""), Operation::SessionInspect, Some(StepKind::InitialSubmit), seen.paste_latch.clone());
    assert_eq!(adapter().interpret(&card).surface, InputSurface::NativeAccepted);
    card.text = screen(&info("not-a-uuid"), "");
    assert_ne!(adapter().interpret(&card).surface, InputSurface::NativeAccepted);
    // An old block already in the baseline is not this control's result.
    card.text = screen(&info(SID), "");
    card.baseline.as_mut().unwrap().text = card.text.clone();
    assert_ne!(adapter().interpret(&card).surface, InputSurface::NativeAccepted);
    let quit = adapter().interpret(&frame(screen("", "/quit"), Operation::Stop, Some(StepKind::Paste), PasteLatch::NeverSeen));
    assert_eq!(quit.paste_latch, PasteLatch::Seen { native_identity: "/quit".into() });
}
#[test]
fn session_binding_reads_only_the_newest_complete_info_block() {
    let text = screen(&format!(" {OTHER} mentioned in chat\n\n{}\n\n{}", info(OTHER), info(SID)), "");
    let binding = adapter().bind(&evidence(text)).unwrap();
    assert_eq!(binding.native_session.as_str(), SID);
    assert_eq!((binding.origin, binding.storage, binding.backing.as_ref()), (CaptureOrigin::CurrentNativeSession, SessionStorage::Local, None));
    // The File row wraps at the pane width; its pieces still name this session.
    let wrapped = info(SID).replace("--private-tmp-ta-workspaces", "--private-tmp-ta-\n workspaces");
    assert_eq!(adapter().bind(&evidence(screen(&wrapped, ""))).unwrap().native_session.as_str(), SID);
    let clipped = format!(" Session Info\n\n File: /s/2026_{SID}.jsonl\n ID: {SID}");
    let duplicate = info(SID).replace(" Messages", &format!(" ID: {SID}\n Messages"));
    let mismatched = info(SID).replace(&format!("_{SID}.jsonl"), &format!("_{OTHER}.jsonl"));
    let memory = info(SID).replace(&format!(" File: /Users/someone/.pi/agent/sessions/--private-tmp-ta-workspaces-worker--/2026-10-26T01-02-03-456Z_{SID}.jsonl"), " File: In-memory");
    for bad in [screen("", ""), screen(&clipped, ""), screen(&duplicate, ""), screen(&mismatched, ""), screen(&memory, ""), info(SID)] {
        assert!(adapter().bind(&evidence(bad.clone())).is_err(), "{bad}");
    }
    let mut forged = evidence(screen(&info(SID), "")); forged.origin = CaptureOrigin::ExplicitResumeLaunch;
    assert!(adapter().bind(&forged).is_err());
    let mut foreign = evidence(screen(&info(SID), "")); foreign.native.harness = "codex".into();
    assert!(adapter().bind(&foreign).is_err());
}
#[test]
fn private_primary_rejects_git_and_inherited_pi_project_context_without_reading_bodies() {
    let sandbox = support::Sandbox::new();
    let parent = sandbox.parent.join("primary-owner"); let cwd = parent.join("seat");
    std::fs::create_dir_all(&cwd).unwrap(); let identity = resolve_cwd(&cwd).unwrap();
    require_private_primary(&identity).unwrap();
    for name in [".git", ".pi", ".agents", "AGENTS.md", "AGENTS.override.md", "CLAUDE.md"] {
        let path = parent.join(name);
        std::fs::write(&path, b"not read").unwrap();
        assert!(require_private_primary(&identity).is_err(), "{name}");
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::create_dir(cwd.join(".pi")).unwrap();
    assert!(require_private_primary(&identity).is_err(), "the primary itself must hold no project resources");
}
#[test]
fn semantic_reader_maps_only_busy_and_ready() {
    let read = |text: &str| adapter().read(&NativeRecord { scope: scope(1), kind: NativeRecordKind::Surface, bytes: text.as_bytes().to_vec() }).outcome;
    assert_eq!(read(&framed("", &working(), "")), ProbeOutcome::Observed(SemanticEvidence::Busy));
    assert_eq!(read(&screen("", "")), ProbeOutcome::Observed(SemanticEvidence::Idle));
    assert!(matches!(read("plain"), ProbeOutcome::Unknown(_)));
}
