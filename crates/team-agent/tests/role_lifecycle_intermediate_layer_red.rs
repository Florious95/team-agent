#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "e2e/framework.rs"]
mod framework;

use framework::{quick_start_workers_available, run_ta_env, TestWorkspace};
use serde_json::Value as Json;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use team_agent::model::yaml::{self, Value as Yaml};

struct Shims { path: String, capture_dir: PathBuf, workspace: String, tmux_capture: PathBuf }

impl Shims {
    fn install(ws: &TestWorkspace) -> Self {
        let dir = ws.path().join(".role-lifecycle-shims");
        let capture_dir = ws.path().join(".role-lifecycle-launch");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&capture_dir).unwrap();
        let binary = quote(&framework::ta_binary().to_string_lossy());
        let pi_target = dir.join("pi-test-target");
        executable(&pi_target, &format!(
            "#!/bin/sh\nset -eu\ncase \"${{1:-}}\" in\n--version) exit 0;;\n--list-models) printf 'provider model\\nopenai-codex gpt-6-luna\\nopenai-codex gpt-5.6-luna\\n'; exit 0;;\nlist) exit 64;;\nesac\n[ -n \"${{TEAM_AGENT_AGENT_ID:-}}\" ] && [ -n \"${{TEAM_AGENT_WORKSPACE:-}}\" ] || exit 0\nprintf '%s\\0' pi \"$@\" > \"${{TEAM_AGENT_WORKSPACE}}/.role-lifecycle-launch/${{TEAM_AGENT_AGENT_ID}}.argv\"\n[ ! -f \"${{TEAM_AGENT_WORKSPACE}}/.role-lifecycle-fail-launch\" ] || exit 73\nexec {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\n", binary));
        symlink(&pi_target, dir.join("pi")).unwrap();
        executable(&dir.join("codex"), &format!(
            "#!/bin/sh\nset -eu\n[ -n \"${{TEAM_AGENT_AGENT_ID:-}}\" ] && [ -n \"${{TEAM_AGENT_WORKSPACE:-}}\" ] || exit 0\nprintf '%s\\0' codex \"$@\" > \"${{TEAM_AGENT_WORKSPACE}}/.role-lifecycle-launch/${{TEAM_AGENT_AGENT_ID}}.argv\"\n[ ! -f \"${{TEAM_AGENT_WORKSPACE}}/.role-lifecycle-fail-launch\" ] || exit 73\nexec {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\n", binary));
        let inherited = std::env::var("PATH").unwrap_or_default();
        let real_tmux = std::env::split_paths(&inherited).map(|p| p.join("tmux")).find(|p| p.is_file()).expect("tmux available for isolated E2E tests");
        executable(&dir.join("tmux"), &format!(
            "#!/bin/sh\nset -eu\nif [ -f \"$ROLE_LIFECYCLE_WORKSPACE/.role-lifecycle-fail-launch\" ]; then\n  case \"$*\" in *new-window*|*new-session*|*split-window*) printf '%s\\0' \"$@\" > \"$ROLE_LIFECYCLE_WORKSPACE/.role-lifecycle-tmux.argv\"; cp \"$ROLE_LIFECYCLE_WORKSPACE/agents/seed.md\" \"$ROLE_LIFECYCLE_WORKSPACE/.role-lifecycle-role-at-failure\"; cp \"$ROLE_LIFECYCLE_WORKSPACE/.team/runtime/rolelife/team.spec.yaml\" \"$ROLE_LIFECYCLE_WORKSPACE/.role-lifecycle-spec-at-failure\"; exit 73;; esac\nfi\nexec {} \"$@\"\n",
            quote(&real_tmux.to_string_lossy()),
        ));
        let workspace = ws.path().to_string_lossy().into_owned();
        let tmux_capture = ws.path().join(".role-lifecycle-tmux.argv");
        Self { path: format!("{}:{inherited}", dir.display()), capture_dir, workspace, tmux_capture }
    }
    fn env(&self) -> [(&str, &str); 2] { [("PATH", &self.path), ("ROLE_LIFECYCLE_WORKSPACE", &self.workspace)] }
    fn capture_path(&self, id: &str) -> PathBuf { self.capture_dir.join(format!("{id}.argv")) }
    fn capture(&self, id: &str) -> Vec<String> {
        let path = self.capture_path(id);
        let until = Instant::now() + Duration::from_secs(12);
        while !path.is_file() && Instant::now() < until { thread::sleep(Duration::from_millis(50)); }
        fs::read(path).expect("provider shim must capture launch argv").split(|b| *b == 0)
            .filter(|b| !b.is_empty()).map(|b| String::from_utf8(b.to_vec()).unwrap()).collect()
    }
}

fn quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }
fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut mode = fs::metadata(path).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(path, mode).unwrap();
}
fn role(id: &str, provider: &str, model: &str, effort: Option<&str>, bypass: bool, prompt: &str) -> String {
    let model = if provider == "pi" && !model.contains('/') { format!("openai-codex/{model}") } else { model.to_string() };
    let mut s = format!("---\nname: {id}\nrole: Lifecycle worker\nprovider: {provider}\nmodel: {model}\nauth_mode: subscription\ndangerously_skip_permissions: {bypass}\ntools:\n  - mcp_team\n");
    if let Some(e) = effort { s.push_str(&format!("effort: {e}\n")); }
    s.push_str(&format!("---\n{prompt}\n")); s
}
fn ws_seed(tag: &str, provider: &str, model: &str, effort: Option<&str>) -> (TestWorkspace, Shims) {
    let ws = TestWorkspace::new(tag);
    fs::write(ws.path().join("TEAM.md"), "Isolated lifecycle team.\n").unwrap();
    fs::create_dir_all(ws.path().join("agents")).unwrap();
    fs::write(ws.path().join("agents/seed.md"), role("seed", provider, model, effort, false, "Seed prompt.")).unwrap();
    let shims = Shims::install(&ws);
    let root = ws.path().to_string_lossy().into_owned();
    let out = run_ta_env(&ws, &["quick-start", &root, "--workspace", &root, "--team-id", "rolelife", "--yes", "--no-display", "--json"], &shims.env());
    assert!(quick_start_workers_available(&out), "quick-start failed: {} {}", out.stdout, out.stderr);
    if let Some(s) = ws.read_state().get("tmux_socket").and_then(Json::as_str) { ws.register_owned_tmux_socket(Path::new(s)); }
    (ws, shims)
}
fn run(ws: &TestWorkspace, shims: &Shims, args: &[String]) -> framework::TaResult {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>(); run_ta_env(ws, &refs, &shims.env())
}
fn root(ws: &TestWorkspace) -> String { ws.path().to_string_lossy().into_owned() }
fn cli(ws: &TestWorkspace, cmd: &str, id: &str, flags: &[&str]) -> Vec<String> {
    let mut v = vec![cmd.to_string(), id.to_string()]; v.extend(flags.iter().map(|s| s.to_string()));
    v.extend(["--workspace".into(), root(ws), "--json".into()]); v
}
fn start(ws: &TestWorkspace, id: &str, flags: &[&str]) -> Vec<String> {
    let mut v = vec!["start-agent".into(), id.into()]; v.extend(flags.iter().map(|s| s.to_string()));
    v.extend(["--allow-fresh".into(), "--workspace".into(), root(ws), "--json".into()]); v
}
fn stop(ws: &TestWorkspace, shims: &Shims, id: &str) {
    let r = run(ws, shims, &cli(ws, "stop-agent", id, &[]));
    assert!(r.is_success(), "stop-agent: {} {}", r.stdout, r.stderr);
}
fn role_path(ws: &TestWorkspace, id: &str) -> PathBuf { ws.path().join("agents").join(format!("{id}.md")) }
fn spec_path(ws: &TestWorkspace) -> PathBuf {
    let state = ws.read_state(); let key = state["active_team_key"].as_str().unwrap();
    team_agent::model::paths::runtime_spec_path(ws.path(), key)
}
fn compiled_from(raw: &str, id: &str) -> Yaml {
    let spec = yaml::loads(raw).unwrap();
    spec.get("agents").and_then(Yaml::as_list).and_then(|agents| agents.iter().find(|x| x.get("id").and_then(Yaml::as_str) == Some(id))).unwrap().clone()
}
fn compiled(ws: &TestWorkspace, id: &str) -> Yaml {
    compiled_from(&fs::read_to_string(spec_path(ws)).unwrap(), id)
}
fn codex_profile_role(id: &str, profile: &str, prompt: &str) -> String {
    role(id, "codex", "gpt-5.6-luna", None, false, prompt)
        .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n")
        .replace("tools:\n", &format!("profile: {profile}\ntools:\n"))
}
fn write_codex_profile(dir: &Path, name: &str, provider_id: &str, base_url: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{name}.env")), format!(
        "AUTH_MODE=compatible_api\nMODEL_PROVIDER={provider_id}\nBASE_URL={base_url}\nAPI_KEY=role-lifecycle-fake-only\nMODEL=gpt-5.6-luna\n"
    )).unwrap();
}
fn assert_codex_profile(argv: &[String], provider_id: &str, base_url: &str) {
    let selector = format!("model_provider=\"{provider_id}\"");
    let endpoint = format!("model_providers.{provider_id}.base_url=\"{base_url}\"");
    assert!(pair(argv, "-c", &selector), "missing Codex profile selector: {argv:?}");
    assert!(pair(argv, "-c", &endpoint), "missing self-contained Codex profile endpoint: {argv:?}");
}
fn assert_config(ws: &TestWorkspace, id: &str, provider: &str, model: &str, effort: Option<&str>, bypass: bool) {
    let v = compiled(ws, id);
    assert_eq!(v.get("provider").and_then(Yaml::as_str), Some(provider), "{v:?}");
    let expected_model = if provider == "pi" && !model.contains('/') { format!("openai-codex/{model}") } else { model.to_string() };
    assert_eq!(v.get("model").and_then(Yaml::as_str), Some(expected_model.as_str()), "{v:?}");
    assert_eq!(v.get("effort").and_then(Yaml::as_str), effort, "{v:?}");
    assert!(matches!(v.get("dangerously_skip_permissions"), Some(Yaml::Bool(actual)) if *actual == bypass), "{v:?}");
}
#[derive(Debug, PartialEq)]
struct Snap { role: Option<Vec<u8>>, spec: Option<Vec<u8>>, state_agent: Option<Json>, windows: Vec<String>, launch: Option<Vec<u8>> }
fn snapshot(ws: &TestWorkspace, shims: &Shims, id: &str) -> Snap {
    let state = ws.read_state(); let session = state["session_name"].as_str().unwrap_or("");
    Snap { role: fs::read(role_path(ws, id)).ok(), spec: fs::read(spec_path(ws)).ok(),
        state_agent: state.pointer(&format!("/agents/{id}")).map(|agent| {
            let mut stable = serde_json::Map::new();
            for key in ["status", "pane_id", "window_name", "session_id", "provider", "model"] {
                if let Some(value) = agent.get(key) { stable.insert(key.to_string(), value.clone()); }
            }
            Json::Object(stable)
        }),
        windows: framework::tmux_windows_on_socket(state["tmux_socket"].as_str().unwrap_or(""), session),
        launch: fs::read(shims.capture_path(id)).ok() }
}
fn reject(r: &framework::TaResult, why: &str) {
    assert!(!r.is_success(), "{why} must reject: {}", r.stdout);
    let text = format!("{} {}", r.stdout, r.stderr).to_lowercase();
    assert!(["error", "reject", "already", "conflict", "provider", "running", "invalid", "unknown", "unsupported", "missing", "usage", "requires", "unexpected"]
        .iter().any(|word| text.contains(*word)), "missing refusal reason: {} {}", r.stdout, r.stderr);
}
fn pair(argv: &[String], key: &str, value: &str) -> bool { argv.windows(2).any(|w| w[0] == key && w[1] == value) }

#[test]
fn traditional_quick_start_and_add_role_file_without_cli_provider_or_bypass_regressions() {
    let legacy = TestWorkspace::new("rolelife-legacy-quick-start");
    fs::write(legacy.path().join("TEAM.md"), "Traditional plain-text team objective.\n").unwrap();
    fs::create_dir_all(legacy.path().join("agents")).unwrap();
    let legacy_role = b"Traditional worker role without provider/bypass frontmatter.\n";
    fs::write(legacy.path().join("agents/legacy.md"), legacy_role).unwrap();
    let legacy_shims = Shims::install(&legacy);
    let legacy_root = legacy.path().to_string_lossy().into_owned();
    let quick = run_ta_env(&legacy, &["quick-start", &legacy_root, "--workspace", &legacy_root, "--team-id", "rolelife-legacy", "--yes", "--no-display", "--json"], &legacy_shims.env());
    assert!(quick_start_workers_available(&quick), "legacy quick-start: {} {}", quick.stdout, quick.stderr);
    assert_eq!(fs::read(legacy.path().join("agents/legacy.md")).unwrap(), legacy_role);
    assert!(!legacy_shims.capture("legacy").is_empty(), "legacy role must still launch without add-agent admission fields");
    if let Some(socket) = legacy.read_state().get("tmux_socket").and_then(Json::as_str) { legacy.register_owned_tmux_socket(Path::new(socket)); }

    let (ws, shims) = ws_seed("rolelife-traditional", "pi", "gpt-6-luna", None);
    assert_eq!(shims.capture("seed").first().map(String::as_str), Some("pi"));
    let source = ws.path().join("traditional.md");
    let bytes = role("traditional", "pi", "gpt-6-luna", None, false, "Traditional role-file prompt.");
    fs::write(&source, &bytes).unwrap();
    let add = run(&ws, &shims, &cli(&ws, "add-agent", "traditional", &["--role-file", source.to_str().unwrap(), "--no-display"]));
    assert!(add.is_success(), "traditional role-file add: {} {}", add.stdout, add.stderr);
    assert_eq!(fs::read(&source).unwrap(), bytes.as_bytes());
    assert_eq!(shims.capture("traditional").first().map(String::as_str), Some("pi"));
}

#[test]
fn add_required_values_cli_generation_and_file_completion() {
    let (ws, shims) = ws_seed("rolelife-add-required", "pi", "gpt-6-luna", None);
    let no_provider = ws.path().join("no-provider.md");
    fs::write(&no_provider, "---\nname: no-provider\nrole: Missing provider\ndangerously_skip_permissions: false\n---\nPrompt.\n").unwrap();
    let no_bypass = ws.path().join("no-bypass.md");
    fs::write(&no_bypass, "---\nname: no-bypass\nrole: Missing bypass\nprovider: pi\n---\nPrompt.\n").unwrap();
    for (id, source, flags) in [
        ("no-provider", no_provider, vec!["--role-file", "", "--bypass", "false", "--prompt", "prompt"]),
        ("no-bypass", no_bypass, vec!["--role-file", "", "--provider", "pi", "--prompt", "prompt"]),
    ] {
        let mut flags = flags;
        flags[1] = source.to_str().unwrap();
        let before = snapshot(&ws, &shims, id);
        let r = run(&ws, &shims, &cli(&ws, "add-agent", id, &flags)); reject(&r, "missing required add-agent field");
        assert_eq!(snapshot(&ws, &shims, id), before); assert!(!role_path(&ws, id).exists());
    }
    let generated = run(&ws, &shims, &cli(&ws, "add-agent", "generated", &["--provider", "pi", "--bypass", "false", "--model", "openai-codex/gpt-5.6-luna", "--effort", "high", "--prompt", "CLI prompt.", "--no-display"]));
    assert!(generated.is_success(), "CLI-only add: {} {}", generated.stdout, generated.stderr);
    assert!(fs::read_to_string(role_path(&ws, "generated")).unwrap().contains("CLI prompt."));
    assert_config(&ws, "generated", "pi", "gpt-5.6-luna", Some("high"), false);
    let source = ws.path().join("partial.md"); fs::write(&source, "---\nname: filled\nrole: Filled\n---\nCLI body.\n").unwrap();
    let filled = run(&ws, &shims, &cli(&ws, "add-agent", "filled", &["--role-file", source.to_str().unwrap(), "--provider", "pi", "--bypass", "false", "--model", "openai-codex/gpt-5.6-luna", "--prompt", "CLI body."]));
    assert!(filled.is_success(), "CLI completion: {} {}", filled.stdout, filled.stderr);
    assert_config(&ws, "filled", "pi", "gpt-5.6-luna", None, false);
}

#[test]
fn invalid_add_and_start_parameter_syntax_rejects_before_any_mutation() {
    let (ws, shims) = ws_seed("rolelife-invalid-args", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    let add_cases: [(&str, &[&str]); 5] = [
        ("bare-bypass", &["--role-file", "", "--bypass"]),
        ("invalid-bypass", &["--role-file", "", "--bypass", "maybe"]),
        ("forbidden-no-bypass", &["--role-file", "", "--no-bypass"]),
        ("unknown-provider", &["--role-file", "", "--provider", "imaginary"]),
        ("invalid-effort", &["--role-file", "", "--effort", "impossible"]),
    ];
    for (id, fields) in add_cases {
        let source = ws.path().join(format!("{id}-source.md"));
        let source_bytes = role(id, "pi", "gpt-6-luna", None, false, "Valid source.");
        fs::write(&source, &source_bytes).unwrap();
        let mut flags = fields.to_vec(); flags[1] = source.to_str().unwrap();
        let before = snapshot(&ws, &shims, id); let seed_before = snapshot(&ws, &shims, "seed");
        let r = run(&ws, &shims, &cli(&ws, "add-agent", id, &flags));
        assert_eq!(snapshot(&ws, &shims, id), before); assert_eq!(snapshot(&ws, &shims, "seed"), seed_before);
        assert!(!role_path(&ws, id).exists()); assert_eq!(fs::read(&source).unwrap(), source_bytes.as_bytes());
        reject(&r, "invalid add parameter");
    }
    let start_cases: [&[&str]; 5] = [
        &["--bypass"], &["--bypass", "maybe"], &["--no-bypass"],
        &["--provider", "imaginary"], &["--provider", "pi", "--effort", "ultra"],
    ];
    for fields in start_cases {
        let before = snapshot(&ws, &shims, "seed");
        let r = run(&ws, &shims, &start(&ws, "seed", fields));
        assert_eq!(snapshot(&ws, &shims, "seed"), before);
        reject(&r, "invalid start parameter");
    }
}

#[test]
fn add_local_role_is_in_place_and_external_template_is_imported_then_reloaded_locally() {
    let (ws, shims) = ws_seed("rolelife-add-source", "pi", "gpt-6-luna", None);
    let local = role_path(&ws, "local");
    let local_bytes = role("local", "pi", "gpt-5.6-luna", None, false, "Local prompt."); fs::write(&local, &local_bytes).unwrap();
    let r = run(&ws, &shims, &cli(&ws, "add-agent", "local", &["--role-file", local.to_str().unwrap()]));
    assert!(r.is_success(), "canonical add: {} {}", r.stdout, r.stderr); assert_eq!(fs::read(&local).unwrap(), local_bytes.as_bytes());
    assert_eq!(fs::read_dir(ws.path().join("agents")).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("local")).count(), 1, "canonical role must not be duplicated");
    let source = ws.path().join("templates/external.md"); fs::create_dir_all(source.parent().unwrap()).unwrap();
    let bytes = role("external", "pi", "gpt-6-luna", None, false, "Imported prompt."); fs::write(&source, &bytes).unwrap();
    let r = run(&ws, &shims, &cli(&ws, "add-agent", "external", &["--role-file", source.to_str().unwrap()]));
    assert!(r.is_success(), "external add: {} {}", r.stdout, r.stderr);
    let canonical = role_path(&ws, "external"); assert_eq!(fs::read(&canonical).unwrap(), bytes.as_bytes());
    fs::write(source, "mutated source").unwrap(); stop(&ws, &shims, "external");
    let restarted = run(&ws, &shims, &start(&ws, "external", &[]));
    assert!(restarted.is_success(), "canonical restart: {} {}", restarted.stdout, restarted.stderr);
    assert!(shims.capture("external").iter().any(|a| a.contains("Imported prompt.")));
}

#[test]
fn add_file_cli_arbitration_accepts_equal_and_rejects_conflicting_values_atomically() {
    let (ws, shims) = ws_seed("rolelife-arbitration", "pi", "gpt-6-luna", None);
    for (id, fields, should_pass) in [
        ("same", vec!["--provider", "pi", "--model", "openai-codex/gpt-5.6-luna", "--effort", "high", "--bypass", "false", "--prompt", "Body."], true),
        ("same-prompt-trim", vec!["--prompt", " \tBody.\n "], true),
        ("conf-provider", vec!["--provider", "codex"], false),
        ("conf-bypass", vec!["--bypass", "true"], false),
        ("conf-model", vec!["--model", "openai-codex/gpt-6-luna"], false),
        ("conf-effort", vec!["--effort", "xhigh"], false),
        ("conf-prompt", vec!["--prompt", "Different body."], false),
    ] {
        let source = ws.path().join(format!("{id}-source.md"));
        let source_bytes = role(id, "pi", "gpt-5.6-luna", Some("high"), false, "Body.");
        fs::write(&source, &source_bytes).unwrap();
        let mut flags = vec!["--role-file", source.to_str().unwrap()]; flags.extend(fields);
        let before = snapshot(&ws, &shims, id); let r = run(&ws, &shims, &cli(&ws, "add-agent", id, &flags));
        if should_pass { assert!(r.is_success(), "equal values: {} {}", r.stdout, r.stderr); assert_eq!(fs::read(&source).unwrap(), source_bytes.as_bytes()); }
        else { reject(&r, "file/CLI conflict"); assert_eq!(snapshot(&ws, &shims, id), before); assert!(!role_path(&ws, id).exists()); assert_eq!(fs::read(&source).unwrap(), source_bytes.as_bytes()); }
    }
    let source = ws.path().join("file-only.md"); fs::write(&source, role("file-only", "pi", "gpt-5.6-luna", None, false, "File only.")).unwrap();
    let r = run(&ws, &shims, &cli(&ws, "add-agent", "file-only", &["--role-file", source.to_str().unwrap()]));
    assert!(r.is_success(), "traditional file-only definition: {} {}", r.stdout, r.stderr);
}

#[test]
fn duplicate_add_running_or_stopped_is_fail_closed_with_zero_session_side_effects() {
    let (ws, shims) = ws_seed("rolelife-duplicate", "pi", "gpt-6-luna", None);
    let source = ws.path().join("other.md"); fs::write(&source, role("seed", "pi", "gpt-5.6-luna", None, false, "Other." )).unwrap();
    for stopped in [false, true] {
        if stopped { stop(&ws, &shims, "seed"); }
        let before = snapshot(&ws, &shims, "seed");
        let r = run(&ws, &shims, &cli(&ws, "add-agent", "seed", &["--role-file", source.to_str().unwrap()]));
        reject(&r, "duplicate seat"); assert_eq!(snapshot(&ws, &shims, "seed"), before);
    }
}

#[test]
fn running_start_refuses_plain_and_parameterized_calls_without_mutation() {
    let (ws, shims) = ws_seed("rolelife-running", "pi", "gpt-6-luna", None);
    for flags in [&[][..], &["--model", "openai-codex/gpt-5.6-luna", "--effort", "high", "--bypass", "true"][..]] {
        let before = snapshot(&ws, &shims, "seed");
        let r = run(&ws, &shims, &start(&ws, "seed", flags)); reject(&r, "start already-running seat");
        let text = format!("{} {}", r.stdout, r.stderr).to_lowercase(); assert!(text.contains("running") || text.contains("already"));
        assert_eq!(snapshot(&ws, &shims, "seed"), before);
    }
}

#[test]
fn s5_codex_stop_change_model_effort_restart_updates_frontmatter_spec_and_native_argv() {
    let (ws, shims) = ws_seed("rolelife-s5-codex", "codex", "gpt-5.6-luna", Some("medium")); stop(&ws, &shims, "seed");
    let r = run(&ws, &shims, &start(&ws, "seed", &["--provider", "codex", "--model", "gpt-6-luna", "--effort", "ultra"]));
    assert!(r.is_success(), "Codex patch: {} {}", r.stdout, r.stderr);
    assert!(fs::read_to_string(role_path(&ws, "seed")).unwrap().contains("effort: ultra"));
    assert_config(&ws, "seed", "codex", "gpt-6-luna", Some("ultra"), false);
    let argv = shims.capture("seed"); assert!(pair(&argv, "--model", "gpt-6-luna"));
    assert!(pair(&argv, "-c", "model_reasoning_effort=ultra"), "authoritative Codex wire key: {argv:?}");
}

#[test]
fn s5_pi_stop_change_model_effort_restart_updates_frontmatter_spec_and_thinking_argv() {
    let (ws, shims) = ws_seed("rolelife-s5-pi", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    let r = run(&ws, &shims, &start(&ws, "seed", &["--provider", "pi", "--model", "openai-codex/gpt-5.6-luna", "--effort", "xhigh"]));
    assert!(r.is_success(), "Pi patch: {} {}", r.stdout, r.stderr);
    let text = fs::read_to_string(role_path(&ws, "seed")).unwrap(); assert!(text.contains("effort: xhigh"));
    assert_config(&ws, "seed", "pi", "gpt-5.6-luna", Some("xhigh"), false);
    let argv = shims.capture("seed"); assert!(pair(&argv, "--model", "openai-codex/gpt-5.6-luna")); assert!(pair(&argv, "--thinking", "xhigh"));
}

#[test]
fn stopped_start_partial_patches_bypass_and_prompt_and_preserves_omitted_values() {
    let (ws, shims) = ws_seed("rolelife-partial", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    for (value, expected) in [("true", true), ("false", false)] {
        let r = run(&ws, &shims, &start(&ws, "seed", &["--bypass", value, "--prompt", "Patched prompt."]));
        assert!(r.is_success(), "bypass={value}: {} {}", r.stdout, r.stderr);
        assert_config(&ws, "seed", "pi", "gpt-6-luna", None, expected);
        assert!(fs::read_to_string(role_path(&ws, "seed")).unwrap().contains("Patched prompt."));
        assert!(shims.capture("seed").iter().any(|arg| arg.contains("Patched prompt.")));
        stop(&ws, &shims, "seed");
    }
}

#[test]
fn cross_provider_changes_are_rejected_both_ways_but_same_provider_is_allowed() {
    for (provider, model, effort, target) in [("pi", "gpt-6-luna", None, "codex"), ("codex", "gpt-5.6-luna", Some("medium"), "pi")] {
        let (ws, shims) = ws_seed(&format!("rolelife-cross-{provider}"), provider, model, effort); stop(&ws, &shims, "seed");
        let before = snapshot(&ws, &shims, "seed"); let r = run(&ws, &shims, &start(&ws, "seed", &["--provider", target]));
        reject(&r, "cross-provider switch"); assert!(format!("{} {}", r.stdout, r.stderr).to_lowercase().contains("provider"));
        assert_eq!(snapshot(&ws, &shims, "seed"), before);
    }
    let (ws, shims) = ws_seed("rolelife-same-provider", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    let r = run(&ws, &shims, &start(&ws, "seed", &["--provider", "pi"]));
    assert!(r.is_success(), "same provider must start: {} {}", r.stdout, r.stderr);
}

#[test]
fn hand_edited_stopped_role_is_recompiled_and_used_by_start() {
    let (ws, shims) = ws_seed("rolelife-manual-edit", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    let manual = role("seed", "pi", "gpt-5.6-luna", Some("high"), false, "Manual edit prompt.");
    fs::write(role_path(&ws, "seed"), &manual).unwrap();
    let r = run(&ws, &shims, &start(&ws, "seed", &[])); assert!(r.is_success(), "manual reload: {} {}", r.stdout, r.stderr);
    assert_config(&ws, "seed", "pi", "gpt-5.6-luna", Some("high"), false);
    let argv = shims.capture("seed"); assert!(pair(&argv, "--model", "openai-codex/gpt-5.6-luna")); assert!(pair(&argv, "--thinking", "high"));
}

#[test]
fn failed_parameterized_start_restores_existing_role_and_spec_bytes() {
    let (ws, shims) = ws_seed("rolelife-rollback-existing", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    let spec = spec_path(&ws); let before = snapshot(&ws, &shims, "seed");
    fs::write(ws.path().join(".role-lifecycle-fail-launch"), b"injected tmux spawn failure").unwrap();
    let r = run(&ws, &shims, &start(&ws, "seed", &["--model", "openai-codex/gpt-5.6-luna", "--effort", "high"]));
    assert!(!r.is_success(), "injected spawn failure must surface: {} {}", r.stdout, r.stderr);
    let failed_argv = fs::read(&shims.tmux_capture).expect("tmux wrapper proves spawn failure").split(|b| *b == 0).filter(|b| !b.is_empty()).map(|b| String::from_utf8(b.to_vec()).unwrap()).collect::<Vec<_>>();
    assert!(failed_argv.iter().any(|arg| ["new-session", "new-window", "split-window"].contains(&arg.as_str())), "failure must reach tmux creation: {failed_argv:?}");
    let staged_role = fs::read_to_string(ws.path().join(".role-lifecycle-role-at-failure")).expect("role captured immediately before late failure");
    let staged_spec = fs::read_to_string(ws.path().join(".role-lifecycle-spec-at-failure")).expect("spec captured immediately before late failure");
    let staged_agent = compiled_from(&staged_spec, "seed");
    assert_eq!(snapshot(&ws, &shims, "seed"), before, "failed start must restore files, runtime binding, and pane topology");
    assert!(spec.exists());
    assert!(!ws.path().join("agents/seed.md.tmp").exists()); assert!(!ws.path().join("agents/seed.md.bak").exists());
    assert!(staged_role.contains("model: openai-codex/gpt-5.6-luna") && staged_role.contains("effort: high"), "new role config must reach failure boundary: {staged_role}");
    assert_eq!(staged_agent.get("model").and_then(Yaml::as_str), Some("openai-codex/gpt-5.6-luna"));
    assert_eq!(staged_agent.get("effort").and_then(Yaml::as_str), Some("high"));
}

#[test]
fn failed_prompt_bypass_start_restores_complete_existing_role_and_spec() {
    let (ws, shims) = ws_seed("rolelife-rollback-prompt-bypass", "pi", "gpt-6-luna", None); stop(&ws, &shims, "seed");
    let spec = spec_path(&ws); let before = snapshot(&ws, &shims, "seed");
    fs::write(ws.path().join(".role-lifecycle-fail-launch"), b"injected tmux spawn failure").unwrap();
    let r = run(&ws, &shims, &start(&ws, "seed", &["--bypass", "true", "--prompt", "F2 rollback prompt."]));
    assert!(!r.is_success(), "injected late spawn failure must surface: {} {}", r.stdout, r.stderr);
    let failed_argv = fs::read(&shims.tmux_capture).expect("F2 must reach the tmux spawn boundary").split(|b| *b == 0).filter(|b| !b.is_empty()).map(|b| String::from_utf8(b.to_vec()).unwrap()).collect::<Vec<_>>();
    assert!(failed_argv.iter().any(|arg| ["new-session", "new-window", "split-window"].contains(&arg.as_str())), "F2 must fail at tmux creation: {failed_argv:?}");
    let staged_role = fs::read_to_string(ws.path().join(".role-lifecycle-role-at-failure")).expect("F2 role captured before late failure");
    let staged_spec = fs::read_to_string(ws.path().join(".role-lifecycle-spec-at-failure")).expect("F2 spec captured before late failure");
    let staged_agent = compiled_from(&staged_spec, "seed");
    assert_eq!(snapshot(&ws, &shims, "seed"), before, "F2 must restore exact role/spec/runtime/pane state");
    assert!(spec.exists());
    assert!(!ws.path().join("agents/seed.md.tmp").exists()); assert!(!ws.path().join("agents/seed.md.bak").exists());
    assert!(staged_role.contains("dangerously_skip_permissions: true") && staged_role.contains("F2 rollback prompt."), "F2 update must reach failure boundary: {staged_role}");
    assert!(matches!(staged_agent.get("dangerously_skip_permissions"), Some(Yaml::Bool(true))));
    assert_eq!(staged_agent.get("system_prompt").and_then(|v| v.get("inline")).and_then(Yaml::as_str), Some("F2 rollback prompt."));
}

#[test]
fn stopped_start_profile_patch_uses_workspace_profile_values() {
    let (ws, shims) = ws_seed("rolelife-start-profile", "codex", "gpt-5.6-luna", None); stop(&ws, &shims, "seed");
    let role_without_profile = role("seed", "codex", "gpt-5.6-luna", None, false, "Profile patch.")
        .replace("auth_mode: subscription\n", "auth_mode: compatible_api\n");
    fs::write(role_path(&ws, "seed"), role_without_profile).unwrap();
    write_codex_profile(&ws.path().join("profiles"), "workspace-only", "workspace_local", "http://workspace.invalid/v1");
    let r = run(&ws, &shims, &start(&ws, "seed", &["--profile", "workspace-only"]));
    assert!(r.is_success(), "start profile patch: {} {}", r.stdout, r.stderr);
    assert!(fs::read_to_string(role_path(&ws, "seed")).unwrap().contains("profile: workspace-only"));
    assert_eq!(compiled(&ws, "seed").get("profile").and_then(Yaml::as_str), Some("workspace-only"));
    assert_codex_profile(&shims.capture("seed"), "workspace_local", "http://workspace.invalid/v1");
}

#[test]
fn imported_role_profile_is_workspace_local_not_an_external_template_dependency() {
    let (ws, shims) = ws_seed("rolelife-import-profile", "pi", "gpt-6-luna", None);
    let external_root = ws.path().join("external-template");
    let source = external_root.join("agent.md"); fs::create_dir_all(&external_root).unwrap();
    let bytes = codex_profile_role("profiled", "workspace-only", "Imported profile role.");
    fs::write(&source, &bytes).unwrap();
    write_codex_profile(&ws.path().join("profiles"), "workspace-only", "workspace_local", "http://workspace.invalid/v1");
    write_codex_profile(&external_root.join("profiles"), "workspace-only", "external_ghost", "http://external.invalid/v1");
    let add = run(&ws, &shims, &cli(&ws, "add-agent", "profiled", &["--role-file", source.to_str().unwrap(), "--no-display"]));
    assert!(add.is_success(), "external profiled add: {} {}", add.stdout, add.stderr);
    assert_eq!(fs::read(&source).unwrap(), bytes.as_bytes());
    assert_eq!(fs::read(role_path(&ws, "profiled")).unwrap(), bytes.as_bytes());
    assert_codex_profile(&shims.capture("profiled"), "workspace_local", "http://workspace.invalid/v1");
    assert_eq!(compiled(&ws, "profiled").get("profile").and_then(Yaml::as_str), Some("workspace-only"));
    stop(&ws, &shims, "profiled");
    fs::remove_dir_all(&external_root).unwrap();
    let restarted = run(&ws, &shims, &start(&ws, "profiled", &["--profile", "workspace-only"]));
    assert!(restarted.is_success(), "restart must remain workspace-self-contained: {} {}", restarted.stdout, restarted.stderr);
    assert_codex_profile(&shims.capture("profiled"), "workspace_local", "http://workspace.invalid/v1");
}

#[test]
fn runtime_spec_and_launch_capture_are_not_substitutes_for_required_live_ui_check() {
    // The final-candidate LIVE-S5 pane/UI verification is separately frozen in
    // the design document. This hermetic fake-provider contract asserts argv
    // only and intentionally makes no claim about what a real UI displays.
    let (ws, shims) = ws_seed("rolelife-ui-boundary", "pi", "gpt-6-luna", None);
    assert!(shims.capture_path("seed").starts_with(ws.path()));
    assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/framework.rs").is_file());
}

#[test]
fn codex_max_effort_uses_issue_238_authoritative_wire_key() {
    let (ws, shims) = ws_seed("rolelife-codex-max", "codex", "gpt-5.6-luna", Some("medium")); stop(&ws, &shims, "seed");
    let r = run(&ws, &shims, &start(&ws, "seed", &["--effort", "max"]));
    assert!(r.is_success(), "Codex max: {} {}", r.stdout, r.stderr);
    assert!(pair(&shims.capture("seed"), "-c", "model_reasoning_effort=max"));
}

#[test]
fn external_role_conflict_and_duplicate_refusals_leave_canonical_files_and_spec_unchanged() {
    let (ws, shims) = ws_seed("rolelife-zero-side-effects", "pi", "gpt-6-luna", None);
    let source = ws.path().join("conflict.md"); fs::write(&source, role("conflict", "pi", "gpt-5.6-luna", None, false, "File body.")).unwrap();
    let before = snapshot(&ws, &shims, "conflict");
    let r = run(&ws, &shims, &cli(&ws, "add-agent", "conflict", &["--role-file", source.to_str().unwrap(), "--provider", "codex"]));
    reject(&r, "provider conflict"); assert_eq!(snapshot(&ws, &shims, "conflict"), before); assert!(!role_path(&ws, "conflict").exists());
}
