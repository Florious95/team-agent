#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "e2e/framework.rs"]
mod framework;

use framework::{quick_start_workers_available, run_ta_env, TestWorkspace};
use rusqlite::Connection;
use serde_json::Value as JsonValue;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use team_agent::model::yaml::Value as YamlValue;

const PLAIN_PROMPT: &str = "Coordinate the release and report verified outcomes.";
const SEED_PROMPT: &str = "A bootstrap worker for this isolated team.";

struct ProviderShims {
    path: String,
}

impl ProviderShims {
    fn install(ws: &TestWorkspace) -> Self {
        let dir = ws.path().join(".role-config-provider-shims");
        fs::create_dir_all(&dir).expect("create provider shim directory");
        fs::create_dir_all(ws.path().join(".role-config-launch"))
            .expect("create launch capture directory");
        let binary = shell_quote(&framework::ta_binary().to_string_lossy());
        let adapter = dir.join("pi-mcp-adapter");
        fs::create_dir_all(&adapter).expect("create fake Pi adapter package");
        fs::write(
            adapter.join("package.json"),
            r#"{"name":"pi-mcp-adapter","version":"1.0.0","pi":{"extensions":["./index.ts"]}}"#,
        )
        .expect("write fake Pi adapter metadata");
        fs::write(adapter.join("index.ts"), "// isolated test adapter\n")
            .expect("write fake Pi adapter entry");

        let pi_target = dir.join("pi-test-target");
        let pi_script = format!(
            "#!/bin/sh\nset -eu\ncase \"${{1:-}}\" in\n  --version) echo 'pi-test-target 1.0.0'; exit 0 ;;\n  --list-models) printf 'provider model\\nopenai-codex gpt-6-luna\\n'; exit 0 ;;\n  list) printf 'npm:pi-mcp-adapter\\n%s\\n' {}; exit 0 ;;\nesac\nif [ -z \"${{TEAM_AGENT_AGENT_ID:-}}\" ] || [ -z \"${{TEAM_AGENT_WORKSPACE:-}}\" ]; then exit 0; fi\nprintf '%s\\0' 'pi' \"$@\" > \"${{TEAM_AGENT_WORKSPACE}}/.role-config-launch/${{TEAM_AGENT_AGENT_ID}}.argv\"\nexec {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\n",
            shell_quote(&adapter.to_string_lossy()),
            binary,
        );
        write_executable(&pi_target, &pi_script);
        symlink(&pi_target, dir.join("pi")).expect("install Pi symlink entry");

        let codex_script = format!(
            "#!/bin/sh\nset -eu\nif [ -z \"${{TEAM_AGENT_AGENT_ID:-}}\" ] || [ -z \"${{TEAM_AGENT_WORKSPACE:-}}\" ]; then exit 0; fi\nprintf '%s\\0' 'codex' \"$@\" > \"${{TEAM_AGENT_WORKSPACE}}/.role-config-launch/${{TEAM_AGENT_AGENT_ID}}.argv\"\nexec {} fake-worker --workspace \"${{TEAM_AGENT_WORKSPACE}}\" --agent-id \"${{TEAM_AGENT_AGENT_ID}}\"\n",
            binary,
        );
        write_executable(&dir.join("codex"), &codex_script);
        let inherited = std::env::var("PATH").unwrap_or_default();
        let path = format!("{}:{inherited}", dir.display());
        Self { path }
    }

    fn env(&self) -> [(&str, &str); 1] {
        [("PATH", &self.path)]
    }

    fn capture(&self, ws: &TestWorkspace, agent_id: &str) -> Vec<String> {
        let path = ws
            .path()
            .join(".role-config-launch")
            .join(format!("{agent_id}.argv"));
        let deadline = Instant::now() + Duration::from_secs(12);
        while !path.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let bytes = fs::read(&path).unwrap_or_else(|error| {
            panic!("no startup plan for {agent_id} at {}: {error}", path.display())
        });
        bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8(part.to_vec()).expect("captured argv is UTF-8"))
            .collect()
    }
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable fixture");
    let mut permissions = fs::metadata(path)
        .expect("stat executable fixture")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make executable fixture executable");
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_plain_team(ws: &TestWorkspace, agents: &[(&str, &str)]) {
    fs::write(
        ws.path().join("TEAM.md"),
        "Coordinate the work using the plain-text team objective.\n",
    )
    .expect("write plain-text TEAM.md");
    let agents_dir = ws.path().join("agents");
    fs::create_dir_all(&agents_dir).expect("create agents directory");
    for (id, prompt) in agents {
        fs::write(agents_dir.join(format!("{id}.md")), prompt)
            .unwrap_or_else(|error| panic!("write pure-text role {id}: {error}"));
    }
}

fn quick_start(ws: &TestWorkspace, team_id: &str, shims: &ProviderShims) {
    let root = ws.path().to_str().expect("workspace path is UTF-8");
    let result = run_ta_env(
        ws,
        &[
            "quick-start",
            root,
            "--workspace",
            root,
            "--team-id",
            team_id,
            "--yes",
            "--no-display",
            "--json",
        ],
        &shims.env(),
    );
    assert!(
        quick_start_workers_available(&result),
        "quick-start failed: exit={} stdout={} stderr={}",
        result.exit_code,
        result.stdout,
        result.stderr
    );
    let state = ws.read_state();
    if let Some(socket) = state.get("tmux_socket").and_then(JsonValue::as_str) {
        ws.register_owned_tmux_socket(Path::new(socket));
    }
}

fn add_agent(
    ws: &TestWorkspace,
    agent_id: &str,
    role_file: &Path,
    extra_args: &[&str],
    shims: &ProviderShims,
) -> framework::TaResult {
    let mut args = vec![
        "add-agent",
        agent_id,
        "--role-file",
        role_file.to_str().expect("role path is UTF-8"),
    ];
    args.extend_from_slice(extra_args);
    args.extend_from_slice(&[
        "--workspace",
        ws.path().to_str().expect("workspace path is UTF-8"),
        "--no-display",
        "--json",
    ]);
    run_ta_env(ws, &args, &shims.env())
}

fn assert_silent_add(result: &framework::TaResult, agent_id: &str) {
    assert!(
        result.is_success(),
        "add-agent {agent_id} must succeed: exit={} stdout={} stderr={}",
        result.exit_code,
        result.stdout,
        result.stderr
    );
    let json = result.json();
    assert_eq!(json["ok"], true, "add-agent must report success: {json}");
    assert_eq!(json["agent_id"], agent_id, "agent registration: {json}");
    for field in ["errors", "warnings", "deprecations", "rejections"] {
        if let Some(value) = json.get(field) {
            assert!(
                value.as_array().is_some_and(Vec::is_empty),
                "add-agent must report no {field}: {value}"
            );
        }
    }
    let output = format!("{}\n{}", result.stdout, result.stderr).to_lowercase();
    for forbidden in ["deprecated", "deprecation", "rejected", "warning:", "error:"] {
        assert!(
            !output.contains(forbidden),
            "legacy-compatible add must not emit {forbidden:?}: {}",
            result.stdout
        );
    }
}

fn runtime_spec(ws: &TestWorkspace) -> YamlValue {
    let state = ws.read_state();
    let team_key = state
        .get("active_team_key")
        .and_then(JsonValue::as_str)
        .expect("runtime state records active team key");
    let path = team_agent::model::paths::runtime_spec_path(ws.path(), team_key);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read runtime spec {}: {error}", path.display()));
    team_agent::model::yaml::loads(&raw).expect("runtime spec parses")
}

fn spec_agent<'a>(spec: &'a YamlValue, agent_id: &str) -> &'a YamlValue {
    spec.get("agents")
        .and_then(YamlValue::as_list)
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent.get("id").and_then(YamlValue::as_str) == Some(agent_id))
        })
        .unwrap_or_else(|| panic!("compiled spec lacks {agent_id}: {spec:?}"))
}

fn string_field<'a>(value: &'a YamlValue, field: &str) -> &'a str {
    value
        .get(field)
        .and_then(YamlValue::as_str)
        .unwrap_or_else(|| panic!("compiled field {field} must be string: {value:?}"))
}

fn assert_running(ws: &TestWorkspace, agent_id: &str) {
    let state = ws.read_state();
    let agent = state
        .pointer(&format!("/agents/{agent_id}"))
        .unwrap_or_else(|| panic!("runtime does not register {agent_id}: {state}"));
    assert_eq!(agent["status"], "running", "agent must be started: {agent}");
}

fn assert_safe_launch(ws: &TestWorkspace, agent_id: &str) {
    let state = ws.read_state();
    let agent = state
        .pointer(&format!("/agents/{agent_id}"))
        .unwrap_or_else(|| panic!("runtime does not register {agent_id}: {state}"));
    assert_eq!(
        agent["as_launched_dangerously_skip_permissions"], false,
        "legacy or plain-text fields must not bypass permissions: {agent}"
    );
}

fn assert_native_plain_defaults(
    ws: &TestWorkspace,
    spec: &YamlValue,
    plan: &[String],
    agent_id: &str,
    prompt: &str,
) {
    let agent = spec_agent(spec, agent_id);
    let state = ws.read_state();
    let runtime_agent = state
        .pointer(&format!("/agents/{agent_id}"))
        .expect("compiled role is registered in runtime state");
    let actual_prompt = agent
        .get("system_prompt")
        .and_then(|value| value.get("inline"))
        .and_then(YamlValue::as_str);
    let mut failures = Vec::new();
    if string_field(agent, "id") != agent_id {
        failures.push(format!("compiled id is not {agent_id:?}"));
    }
    if string_field(agent, "provider") != "pi" || plan.first().map(String::as_str) != Some("pi") {
        failures.push("provider is not pi in both compiled spec and startup plan".to_string());
    }
    if !matches!(agent.get("model"), None | Some(YamlValue::Null)) {
        failures.push("compiled model is not None/null".to_string());
    }
    if !matches!(agent.get("effort"), None | Some(YamlValue::Null)) {
        failures.push("compiled effort is not None/null".to_string());
    }
    if !matches!(agent.get("dangerously_skip_permissions"), Some(YamlValue::Bool(false)))
        || runtime_agent["as_launched_dangerously_skip_permissions"] != false
    {
        failures.push("bypass is not false in spec and actual launch state".to_string());
    }
    if plan.iter().any(|arg| arg == "--model" || arg.starts_with("--model=")) {
        failures.push("startup plan injects a model override".to_string());
    }
    if plan.iter().any(|arg| arg == "--thinking" || arg.starts_with("--thinking=")) {
        failures.push("startup plan injects a thinking-effort override".to_string());
    }
    if actual_prompt != Some(prompt) {
        failures.push("compiled system prompt differs from plain role body".to_string());
    }
    if runtime_agent["status"] != "running" {
        failures.push("plain role was not running".to_string());
    }
    let prompt_arg = plan
        .iter()
        .position(|arg| arg == "--append-system-prompt")
        .and_then(|index| plan.get(index + 1));
    let id_arg = plan
        .iter()
        .position(|arg| arg == "--name")
        .and_then(|index| plan.get(index + 1));
    if !prompt_arg.is_some_and(|actual| actual.contains(prompt))
        || id_arg.map(String::as_str) != Some(agent_id)
    {
        failures.push("startup prompt or agent name differs from role source".to_string());
    }
    assert!(
        failures.is_empty(),
        "F2 provider-native defaults failed: {failures:?}; compiled={agent:?}; startup_plan={plan:?}; runtime={runtime_agent}"
    );
}

fn assert_launch_identity_and_prompt(plan: &[String], agent_id: &str, prompt: &str) {
    assert_eq!(plan.first().map(String::as_str), Some("pi"), "launch provider: {plan:?}");
    let prompt_index = plan
        .iter()
        .position(|arg| arg == "--append-system-prompt")
        .expect("Pi launch includes the system prompt");
    assert!(
        plan.get(prompt_index + 1)
            .is_some_and(|actual| actual.contains(prompt)),
        "Pi startup prompt must include the role source text: {plan:?}"
    );
    let name_index = plan
        .iter()
        .position(|arg| arg == "--name")
        .expect("Pi launch names the agent");
    assert_eq!(plan.get(name_index + 1).map(String::as_str), Some(agent_id));
}

fn correlated_reply_exists(db: &Path, message_id: &str) -> bool {
    let Ok(conn) = Connection::open(db) else {
        return false;
    };
    let Ok(mut stmt) = conn.prepare("select task_id, envelope from results order by created_at desc") else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) else {
        return false;
    };
    let found = rows.filter_map(Result::ok).any(|(task_id, raw)| {
        task_id == message_id
            && serde_json::from_str::<JsonValue>(&raw)
                .ok()
                .and_then(|envelope| envelope["summary"].as_str().map(str::to_string))
                .is_some_and(|summary| summary.contains(message_id))
    });
    found
}

#[test]
fn f1_legacy_fields_are_silently_tolerated_by_public_add_and_worker_replies() {
    let team_id = "rolecfgf1";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["seed"]);
    let shims = ProviderShims::install(&ws);
    let original = b"---\nprovider: pi\ndangerously_skip_permissions: false\ntools: ['fs_read', 'fs_write']\npermission_mode: trusted\nlabel: legacy\ndangerous_auto_approve: true\n---\nHandle the assigned message and return a concise result.\n";
    let role = ws.path().join("roles/legacy.md");
    fs::create_dir_all(role.parent().expect("roles parent")).expect("create roles dir");
    fs::write(&role, original).expect("write historical-format role file");

    quick_start(&ws, team_id, &shims);
    let add = add_agent(&ws, "legacy", &role, &[], &shims);
    assert_silent_add(&add, "legacy");
    assert_eq!(fs::read(&role).expect("legacy role remains readable"), original);
    assert_running(&ws, "legacy");

    let spec = runtime_spec(&ws);
    let legacy = spec_agent(&spec, "legacy");
    assert_eq!(string_field(legacy, "provider"), "pi");
    let legacy_plan = shims.capture(&ws, "legacy");
    assert_launch_identity_and_prompt(
        &legacy_plan,
        "legacy",
        "Handle the assigned message and return a concise result.",
    );

    let canary = format!("legacy-config-natural-reply-{}", std::process::id());
    let send = run_ta_env(
        &ws,
        &[
            "send",
            "legacy",
            &canary,
            "--workspace",
            ws.path().to_str().expect("workspace path is UTF-8"),
            "--json",
        ],
        &shims.env(),
    );
    assert!(send.is_success(), "send failed: {} {}", send.stdout, send.stderr);
    let message_id = send
        .json()
        .get("message_id")
        .and_then(JsonValue::as_str)
        .expect("send returns message id")
        .to_string();
    let db = ws.path().join(".team/runtime/team.db");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !correlated_reply_exists(&db, &message_id) {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        correlated_reply_exists(&db, &message_id),
        "started worker must naturally reply to message {message_id}"
    );
    assert!(matches!(
        legacy.get("dangerously_skip_permissions"),
        Some(YamlValue::Bool(false))
    ), "dangerous_auto_approve must not elevate permissions: {legacy:?}");
    assert_safe_launch(&ws, "legacy");
}

#[test]
fn f2_first_compile_plain_text_role_uses_provider_native_safe_defaults() {
    let team_id = "rolecfgf2first";
    let ws = TestWorkspace::new(team_id);
    write_plain_team(&ws, &[("plain", PLAIN_PROMPT)]);
    let shims = ProviderShims::install(&ws);

    quick_start(&ws, team_id, &shims);
    assert_running(&ws, "plain");
    let spec = runtime_spec(&ws);
    let plan = shims.capture(&ws, "plain");
    assert_native_plain_defaults(&ws, &spec, &plan, "plain", PLAIN_PROMPT);
}

#[test]
fn f2_dynamic_add_plain_text_role_uses_provider_native_safe_defaults() {
    let team_id = "rolecfgf2add";
    let ws = TestWorkspace::new(team_id);
    write_plain_team(&ws, &[("seed", SEED_PROMPT)]);
    let shims = ProviderShims::install(&ws);
    quick_start(&ws, team_id, &shims);

    let role = ws.path().join("roles/plain.md");
    fs::create_dir_all(role.parent().expect("roles parent")).expect("create roles dir");
    fs::write(&role, PLAIN_PROMPT).expect("write pure-text role");
    let add = add_agent(
        &ws,
        "plain",
        &role,
        &["--provider", "pi", "--bypass", "false"],
        &shims,
    );
    assert_silent_add(&add, "plain");
    assert_running(&ws, "plain");
    let spec = runtime_spec(&ws);
    let plan = shims.capture(&ws, "plain");
    assert_native_plain_defaults(&ws, &spec, &plan, "plain", PLAIN_PROMPT);
}
