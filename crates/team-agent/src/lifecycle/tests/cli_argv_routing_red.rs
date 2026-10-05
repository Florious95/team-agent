//! Independent Issue #283 lifecycle properties, using only frozen-baseline seams.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
#[path = "../../../tests/support/hermetic.rs"]
mod hermetic;
use crate::leader::{LeaderStartMode, LeaderStartPlan};
use crate::lifecycle::launch::{
    add_agent_with_transport, fork_agent_with_transport, quick_start_with_transport_in_workspace,
};
use crate::lifecycle::{
    launch_with_transport_in_workspace, reset_agent_with_transport, restart_with_transport,
    start_agent_with_transport,
};
use crate::model::enums::Provider;
use crate::model::ids::AgentId;
use crate::state::persist::{load_runtime_state, save_runtime_state};
use crate::transport::test_support::OfflineTransport;
use crate::transport::{PaneId, PaneInfo, SessionName, WindowName};
use serde_json::{json, Value};
use serial_test::serial;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SWITCH: &str = "TEAM_AGENT_CLI_ARGV_ROUTING";
const CANONICAL: &[(&str, Provider)] = &[
    ("claude", Provider::Claude),
    ("claude", Provider::ClaudeCode),
    ("codex", Provider::Codex),
    ("copilot", Provider::Copilot),
    ("gemini_cli", Provider::GeminiCli),
    ("grok", Provider::Grok),
    ("cursor_agent", Provider::CursorAgent),
    ("pi", Provider::Pi),
];
struct Fixture {
    guards: Vec<hermetic::EnvOverride>,
    env: hermetic::HermeticTestEnv,
    ws: PathBuf,
    team: PathBuf,
    bin: PathBuf,
    probes: PathBuf,
}
impl Fixture {
    fn new(tag: &str) -> Self {
        let env = hermetic::HermeticTestEnv::enter(tag);
        let ws = env.workspace("workspace");
        let team = ws.join("roles");
        fs::create_dir_all(team.join("agents")).unwrap();
        let bin = env.root().join("bin");
        fs::create_dir(&bin).unwrap();
        let probes = env.root().join("probe-argv");
        let native = env.root().join("native-argv");
        // Synthetic noncredential metadata; no user auth store or login is read.
        fs::create_dir_all(env.home().join(".grok")).unwrap();
        fs::write(
            env.home().join(".grok/auth.json"),
            r#"{"fixture_only":true}"#,
        )
        .unwrap();
        let script = format!("#!/bin/sh\ncase \"$1\" in\n--version) printf '%s\\n' \"$*\" >> '{}'; echo 0.87.1; exit 0;;\n--list-models) printf '%s\\n' \"$*\" >> '{}'; printf 'provider model\\nteam-agent qwen3.8-27b\\n'; exit 0;;\nplugin|mcp|auth|list) printf '%s\\n' \"$*\" >> '{}'; exit 0;;\nesac\nprintf '%s\\0' \"$@\" > '{}'\n/bin/kill -TERM \"$PPID\"\n", probes.display(), probes.display(), probes.display(), native.display());
        for name in ["claude", "codex", "copilot", "gemini", "grok", "agent"] {
            executable(&bin.join(name), &script);
        }
        let real_pi = env.root().join("pi-real");
        executable(&real_pi, &script);
        std::os::unix::fs::symlink(&real_pi, bin.join("pi")).unwrap();
        executable(&bin.join("tmux"), "#!/bin/sh\ncase \"$*\" in\n*-V*) echo 'tmux 3.4';;\n*has-session*) exit 1;;\n*list-sessions*|*list-windows*|*list-panes*) exit 0;;\n*) exit 0;;\nesac\n");
        let guards = vec![
            env.with_env("PATH", &format!("{}:/usr/bin:/bin", bin.display())),
            env.with_env(SWITCH, "off"),
            env.with_env("GROK_FOLDER_TRUST", "0"),
            env.with_env(
                "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON",
                "[\"/bin/zsh\"]",
            ),
            env.with_env("TEAM_AGENT_RESTART_SESSION_CAPTURE_DEADLINE_MS", "25"),
            env.with_env("TEAM_AGENT_RESTART_SESSION_CAPTURE_POLL_MS", "5"),
        ];
        seed_coordinator(&ws);
        Self {
            guards,
            env,
            ws,
            team,
            bin,
            probes,
        }
    }
    fn config(&self) -> PathBuf {
        self.env.home().join(".team-agent/argv-routing.json")
    }
    fn seed(&self, enabled: bool, providers: Value) {
        self.raw(&json!({"schema_version":1,"enabled":enabled,"providers":providers}).to_string());
    }
    fn raw(&self, raw: &str) {
        fs::write(self.config(), raw).unwrap();
    }
    fn switch(&self, value: Option<&str>) {
        unsafe {
            if let Some(v) = value {
                std::env::set_var(SWITCH, v);
            } else {
                std::env::remove_var(SWITCH);
            }
        }
    }
    fn spec(&self, provider: &str, ids: &[&str]) -> PathBuf {
        fs::write(self.team.join("TEAM.md"), "---\nname: argvteam\nobjective: Literal argv route fixture.\ndisplay_backend: none\n---\n\nTeam.\n").unwrap();
        for id in ids {
            fs::write(
                self.team.join(format!("agents/{id}.md")),
                role(provider, id),
            )
            .unwrap();
        }
        let spec = crate::compiler::compile_team(&self.team).unwrap();
        let path = self.team.join("team.spec.yaml");
        fs::write(&path, crate::model::yaml::dumps(&spec)).unwrap();
        path
    }
    fn cold(&self, provider: &str) -> Vec<String> {
        let spec = self.spec(provider, &["worker"]);
        let t = offline();
        // Each property sample is a new fixture-owned cold generation, not an
        // unauthorized overwrite of a live topology. No real pane/process exists.
        let state_path = crate::state::persist::runtime_state_path(&self.ws);
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        // Missing files can legitimately recover from the process-local state
        // cache. A literal empty fixture snapshot starts a new cold generation.
        fs::write(state_path, "{}").unwrap();
        let result = launch_with_transport_in_workspace(&self.ws, &spec, false, false, true, &t);
        assert!(
            result.is_ok(),
            "fixture library cold boundary must reach native plan: {result:?}"
        );
        let spawns = t.spawn_records();
        assert_eq!(spawns.len(), 1);
        spawns[0].1.clone()
    }
    fn leader(
        &self,
        provider: Provider,
        external: bool,
        attach: bool,
    ) -> Result<LeaderStartPlan, crate::leader::LeaderError> {
        crate::leader::start::leader_start_plan_after_ambient_authority_check(
            provider,
            &[],
            &self.ws,
            attach,
            attach,
            None,
            external,
        )
    }
    fn start_team(&self, provider: &str, ids: &[&str]) -> OfflineTransport {
        self.spec(provider, ids);
        let t = offline();
        let report = quick_start_with_transport_in_workspace(
            &self.ws,
            &self.team,
            None,
            true,
            Some("argvteam"),
            &t,
        )
        .unwrap();
        assert!(
            matches!(report, crate::lifecycle::QuickStartReport::Ready { .. }),
            "setup must produce real spawn: {report:?}"
        );
        assert_eq!(t.spawn_records().len(), ids.len());
        t
    }
    fn mutate_agent(&self, id: &str, f: impl FnOnce(&mut Value)) {
        let selected =
            crate::state::projection::select_runtime_state(&self.ws, Some("argvteam")).unwrap();
        let mut state = selected;
        f(&mut state["agents"][id]);
        crate::state::projection::save_team_scoped_state_with_lifecycle_topology_authority(
            &self.ws,
            &state,
            "argvteam",
            &[id],
        )
        .unwrap();
    }
    fn native_file(&self) -> PathBuf {
        self.env.root().join("native-argv")
    }
}
// Field guards restore changed environment before HermeticTestEnv removes its owned root.
impl Drop for Fixture {
    fn drop(&mut self) {
        self.guards.clear();
    }
}
fn offline() -> OfflineTransport {
    let mut t = OfflineTransport::new();
    for index in 1..=8 {
        t = t.with_capture_for_pane(
            format!("%{index}"),
            "Claude Code\n> \nOpenAI Codex\ncodex>\n❯\n",
        );
    }
    t
}
fn executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn role(provider: &str, id: &str) -> String {
    let model = if provider == "pi" {
        "team-agent/qwen3.8-27b"
    } else {
        "gpt-5.5"
    };
    format!("---\nname: {id}\nrole: {id} Worker\nprovider: {provider}\nmodel: {model}\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nLiteral route worker.\n")
}
fn seed_coordinator(ws: &Path) {
    let workspace = crate::coordinator::WorkspacePath::new(ws.to_path_buf());
    let pid = crate::coordinator::Pid::new(std::process::id());
    fs::create_dir_all(crate::model::paths::runtime_dir(ws)).unwrap();
    crate::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        crate::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    fs::write(
        crate::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .unwrap();
}
fn normalized(argv: &[String]) -> Vec<String> {
    let mut result = argv.to_vec();
    for i in 1..result.len() {
        if result[i - 1] == "--session-id" {
            result[i] = "<baseline-dynamic-session-id>".into();
        }
    }
    result
}
fn injected(base: &[String], route: &[String], actual: &[String]) {
    let mut expected = base[..1].to_vec();
    expected.extend_from_slice(route);
    expected.extend_from_slice(&base[1..]);
    assert_eq!(
        normalized(actual),
        normalized(&expected),
        "R must be inserted once immediately after executable; native tail unchanged"
    );
}
fn assert_route(actual: &[String], route: &[&str]) {
    assert!(
        actual.len() > route.len(),
        "native executable and tail must exist"
    );
    assert_eq!(
        &actual[1..=route.len()],
        route,
        "route missing/incorrect at final native spawn: {actual:?}"
    );
    for token in route {
        assert_eq!(
            actual.iter().filter(|v| v.as_str() == *token).count(),
            1,
            "duplicate route token {token}"
        );
    }
}
fn same_leader_env(actual: &BTreeMap<String, String>, base: &BTreeMap<String, String>) {
    let mut actual = actual.clone();
    let mut base = base.clone();
    // This intentional caller-input difference is not product env injection.
    actual.remove(SWITCH);
    base.remove(SWITCH);
    assert!(
        actual == base,
        "native env changed beyond the caller's routing override"
    );
}
fn live_pane(ws: &Path) -> PaneInfo {
    let state = crate::state::projection::select_runtime_state(ws, Some("argvteam")).unwrap();
    PaneInfo {
        pane_id: PaneId::new("%old"),
        session: SessionName::new(state["session_name"].as_str().unwrap()),
        window_index: None,
        window_name: Some(WindowName::new("worker")),
        pane_index: None,
        tty: None,
        current_command: Some("codex".into()),
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

#[test]
#[serial(env)]
fn a01_default_off_matches_explicit_off_at_cold_common_and_leader_boundaries() {
    let f = Fixture::new("argv-a01");
    f.switch(None);
    let cold = f.cold("codex");
    assert!(!f.config().exists());
    unsafe {
        std::env::set_var("TMUX", "fixture,1,0");
    }
    let leader = f.leader(Provider::Codex, true, false).unwrap();
    f.seed(true, json!({"codex":["must-not-appear"]}));
    f.switch(Some("off"));
    assert_eq!(normalized(&f.cold("codex")), normalized(&cold));
    let off_leader = f.leader(Provider::Codex, true, false).unwrap();
    assert_eq!(off_leader.argv, leader.argv);
    same_leader_env(&off_leader.leader_env, &leader.leader_env);
    assert_eq!(off_leader.workspace, leader.workspace);
    assert_eq!(off_leader.identity, leader.identity);
    unsafe {
        std::env::remove_var("TMUX");
    }
    drop(f);
    let f = Fixture::new("argv-a01-common");
    f.switch(None);
    f.start_team("codex", &["worker"]);
    let baseline = offline();
    start_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        true,
        false,
        false,
        Some("argvteam"),
        &baseline,
    )
    .unwrap();
    f.seed(true, json!({"codex":["must-not-appear"]}));
    f.switch(Some("off"));
    let t = offline();
    start_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        true,
        false,
        false,
        Some("argvteam"),
        &t,
    )
    .unwrap();
    assert_eq!(
        normalized(&t.spawn_records()[0].1),
        normalized(&baseline.spawn_records()[0].1)
    );
    assert_eq!(t.spawn_cwd_records(), baseline.spawn_cwd_records());
    assert!(!t.spawn_records()[0]
        .1
        .iter()
        .any(|v| v == "must-not-appear"));
    assert!(!f.config().with_file_name("argv-routing.lock").exists());
}

#[test]
#[serial(env)]
fn a02_off_invalid_and_unopted_bad_json_preserve_native_startup() {
    let f = Fixture::new("argv-a02");
    let base = f.cold("codex");
    for raw in [
        "bad JSON",
        r#"{"enabled":false,"providers":"bad"}"#,
        r#"{"enabled":true,"providers":{"codex":[0]}}"#,
    ] {
        f.raw(raw);
        for value in [Some("off"), Some(" 0 "), Some(""), Some("bogus")] {
            f.switch(value);
            assert_eq!(normalized(&f.cold("codex")), normalized(&base));
        }
    }
    for raw in [
        "bad JSON",
        r#"{"enabled":false,"providers":"bad"}"#,
        r#"{"enabled":"unknown"}"#,
    ] {
        f.raw(raw);
        f.switch(None);
        assert_eq!(normalized(&f.cold("codex")), normalized(&base));
    }
}

#[test]
#[serial(env)]
fn a02_forced_off_proves_no_config_open_with_fifo_and_bounded_child() {
    let f = Fixture::new("argv-fifo");
    let path = std::ffi::CString::new(f.config().as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lifecycle::tests::cli_argv_routing_red::off_fifo_child",
            "--ignored",
        ])
        .env(SWITCH, "off")
        .env("ARGV_RED_WORKSPACE", &f.ws)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "OFF child plan must succeed without reading FIFO"
            );
            break;
        }
        if start.elapsed() > Duration::from_secs(3) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("OFF opened FIFO instead of short-circuiting before configuration read");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "invoked only by bounded no-config-open property"]
fn off_fifo_child() {
    let ws = PathBuf::from(std::env::var_os("ARGV_RED_WORKSPACE").expect("subprocess-only helper"));
    crate::leader::start::leader_start_plan_after_ambient_authority_check(
        Provider::Codex,
        &[],
        &ws,
        false,
        false,
        None,
        false,
    )
    .unwrap();
}

#[test]
#[serial(env)]
fn a03_persisted_enabled_and_env_override_apply_at_native_cold_boundary() {
    let f = Fixture::new("argv-a03");
    let base = f.cold("codex");
    for (persisted, env, should_route) in [
        (true, None, true),
        (false, None, false),
        (false, Some("on"), true),
        (true, Some("false"), false),
    ] {
        f.seed(persisted, json!({"codex":["route-priority"]}));
        f.switch(env);
        let argv = f.cold("codex");
        injected(
            &base,
            &if should_route {
                vec!["route-priority".into()]
            } else {
                vec![]
            },
            &argv,
        );
    }
}

#[test]
#[serial(env)]
fn a05_generated_literal_tokens_roundtrip_cli_json_native_and_posix_wrappers() {
    let f = Fixture::new("argv-literal");
    let base = f.cold("pi");
    let sentinel = f.env.root().join("INJECTION-MUST-NOT-RUN");
    for seed in 0..16 {
        let route = vec![
            "".into(),
            format!(" 路由{seed} 🚀 "),
            "\"x\" 'y'\nline".into(),
            format!(
                "; touch {}; $(touch {}); `touch {}`",
                sentinel.display(),
                sentinel.display(),
                sentinel.display()
            ),
            "{workspace}".into(),
            "--help".into(),
            "--json".into(),
            "--no-display".into(),
            "--".into(),
        ];
        let mut cmd = Command::new(super::test_binary_path());
        cmd.current_dir(&f.ws)
            .args(["route", "set", "pi", "--json", "--"])
            .args(&route)
            .env(SWITCH, "off");
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "CLI literal property requires successful storage: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let persisted: Value = serde_json::from_slice(&fs::read(f.config()).unwrap()).unwrap();
        assert_eq!(persisted["providers"]["pi"], json!(route));
        f.switch(Some("on"));
        let actual = f.cold("pi");
        injected(&base, &route, &actual);
        let mut recorded = actual.clone();
        recorded[0] = f.bin.join("pi").to_string_lossy().into_owned();
        for leader in [false, true] {
            let line = if leader {
                crate::tmux_backend::leader_shell_wrapper_command(
                    &recorded,
                    &f.ws,
                    &BTreeMap::new(),
                    &[],
                    "pi",
                )
            } else {
                crate::tmux_backend::worker_shell_wrapper_command(
                    &recorded,
                    &f.ws,
                    &BTreeMap::new(),
                    &[],
                    "pi",
                )
            };
            // Recorder terminates its exact parent shell after capturing argv; no inert-tail orphan.
            let status = Command::new("/bin/sh")
                .args(["-c", &line])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(!status.success());
            let bytes = fs::read(f.native_file()).unwrap();
            let args = bytes
                .split(|b| *b == 0)
                .take(recorded.len() - 1)
                .map(|v| String::from_utf8(v.to_vec()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(args, recorded[1..]);
            assert!(!sentinel.exists());
        }
        f.switch(Some("off"));
    }
}

fn provider_property(key: &str, provider: Provider) {
    let f = Fixture::new("argv-provider");
    let _leader = f.env.with_env("TEAM_AGENT_LEADER_PROVIDER", "pi");
    let _caller = f.env.with_env("TEAM_AGENT_CALLER_PROVIDER", "pi");
    let wire = crate::provider::wire::provider_wire(provider);
    let base = f.cold(wire);
    let providers = CANONICAL
        .iter()
        .map(|(k, _)| ((*k).to_string(), json!([format!("route-only-{k}")])))
        .collect::<serde_json::Map<_, _>>();
    f.seed(true, Value::Object(providers));
    f.switch(None);
    let actual = f.cold(wire);
    f.seed(true, json!({"pi":["pi-must-not-leak"]}));
    if provider != Provider::Pi {
        assert_eq!(normalized(&f.cold(wire)), normalized(&base));
    }
    injected(&base, &[format!("route-only-{key}")], &actual);
    assert_eq!(
        actual
            .iter()
            .filter(|v| v.starts_with("route-only-"))
            .count(),
        1
    );
}
macro_rules! provider_tests { ($($name:ident => $key:literal, $provider:ident;)*) => { $(#[test] #[serial(env)] fn $name() { provider_property($key, Provider::$provider); })* }; }
provider_tests! {
    a06_claude => "claude", Claude;
    a06_claude_code => "claude", ClaudeCode;
    a06_codex => "codex", Codex;
    a06_copilot => "copilot", Copilot;
    a06_gemini_cli => "gemini_cli", GeminiCli;
    a06_grok => "grok", Grok;
    a06_cursor_agent => "cursor_agent", CursorAgent;
    a06_pi => "pi", Pi;
}

#[test]
#[serial(env)]
fn a06_fake_worker_is_outside_native_router_even_under_invalid_optin() {
    let f = Fixture::new("argv-fake");
    let base = f.cold("fake");
    f.raw("invalid JSON");
    f.switch(Some("on"));
    assert_eq!(f.cold("fake"), base);
    assert!(base.iter().any(|v| v == "fake-worker"));
}

#[test]
#[serial(env)]
fn a07_leader_new_existing_exec_managed_new_session_and_attach_modes() {
    let mut samples = Vec::new();
    for existing in [false, true] {
        let f = Fixture::new("argv-leader");
        if existing {
            save_runtime_state(
                &f.ws,
                &json!({"active_team_key":"argvteam","session_name":"team-argvteam","agents":{}}),
            )
            .unwrap();
        }
        for (tmux, external, expected_mode) in [
            (true, true, LeaderStartMode::ExecProvider),
            (false, false, LeaderStartMode::ManagedTmuxClient),
            (false, true, LeaderStartMode::NewTmuxSession),
        ] {
            unsafe {
                if tmux {
                    std::env::set_var("TMUX", "fixture,1,0");
                } else {
                    std::env::remove_var("TMUX");
                }
            }
            f.switch(Some("off"));
            let base = f.leader(Provider::Pi, external, false).unwrap();
            f.seed(true, json!({"pi":["--mode","rpc"]}));
            f.switch(None);
            let routed = f.leader(Provider::Pi, external, false).unwrap();
            assert_eq!(routed.mode, expected_mode);
            same_leader_env(&routed.leader_env, &base.leader_env);
            assert_eq!(routed.workspace, base.workspace);
            assert_eq!(routed.identity, base.identity);
            if routed.mode == LeaderStartMode::ExecProvider {
                assert_eq!(routed.argv, routed.provider_argv);
            }
            if routed.mode == LeaderStartMode::ManagedTmuxClient {
                assert!(!routed.argv.iter().any(|v| v == "--mode" || v == "rpc"));
            }
            if routed.mode == LeaderStartMode::NewTmuxSession {
                // Execute the actual encoded shell payload against a recorder,
                // not tmux or a real provider. This path execs, so exit normally.
                executable(
                    &f.env.root().join("pi-real"),
                    &format!(
                        "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\nexit 0\n",
                        f.native_file().display()
                    ),
                );
                let shell = routed.argv.last().unwrap();
                assert!(shell.starts_with("cd "));
                assert!(Command::new("/bin/sh")
                    .args(["-c", shell])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success());
                let bytes = fs::read(f.native_file()).unwrap();
                let actual = bytes
                    .split(|b| *b == 0)
                    .take(routed.provider_argv.len() - 1)
                    .map(|v| String::from_utf8(v.to_vec()).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(actual, routed.provider_argv[1..]);
            }
            samples.push((base.provider_argv, routed.provider_argv));
        }
        unsafe {
            std::env::remove_var("TMUX");
        }
        f.raw("invalid but opt-in");
        f.switch(Some("on"));
        let attach = f.leader(Provider::Pi, true, true).unwrap();
        assert_eq!(attach.mode, LeaderStartMode::AttachExisting);
        assert!(attach.provider_argv.is_empty());
    }
    assert_eq!(samples.len(), 6);
    for (base, routed) in samples {
        injected(&base, &["--mode".into(), "rpc".into()], &routed);
    }
}

#[test]
#[serial(env)]
fn a07_pi_explicit_invalid_arguments_remain_rejected_under_routing() {
    let f = Fixture::new("argv-pi-reject");
    f.seed(true, json!({"pi":["--mode","rpc"]}));
    f.switch(None);
    for args in [
        vec!["--thinking".to_string(), "invalid".into()],
        vec!["--unknown".into()],
    ] {
        let result = crate::leader::start::leader_start_plan_after_ambient_authority_check(
            Provider::Pi,
            &args,
            &f.ws,
            false,
            false,
            None,
            false,
        );
        assert!(
            result.is_err(),
            "route must not weaken Pi explicit input validation"
        );
    }
}

#[test]
#[serial(env)]
fn a08_quick_start_first_later_paused_dry_run_and_live_noop() {
    let f = Fixture::new("argv-quick");
    f.seed(true, json!({"codex":["cold-route"]}));
    f.switch(None);
    let t = f.start_team("codex", &["worker", "mate"]);
    let observed_spawns = t.spawn_records();
    let noop =
        quick_start_with_transport_in_workspace(&f.ws, &f.team, None, true, Some("argvteam"), &t);
    assert!(noop.is_ok());
    assert_eq!(t.spawn_records().len(), 2, "live-noop must not spawn again");
    let spec = f.spec("codex", &["worker", "mate"]);
    f.raw("invalid schema");
    f.switch(Some("on"));
    let dry = offline();
    assert!(launch_with_transport_in_workspace(&f.ws, &spec, true, false, true, &dry).is_ok());
    assert!(dry.spawn_records().is_empty());
    drop(f);
    let f = Fixture::new("argv-paused");
    let spec = f.spec("codex", &["worker", "mate"]);
    f.raw("invalid schema");
    f.switch(Some("on"));
    // Typed paused flags, not assumptions about YAML renderer indentation.
    use crate::model::yaml::Value as Yaml;
    let mut paused = crate::model::yaml::loads(&fs::read_to_string(&spec).unwrap()).unwrap();
    let Yaml::Map(top) = &mut paused else {
        panic!("fixture spec object");
    };
    let Yaml::List(agents) = &mut top.iter_mut().find(|(key, _)| key == "agents").unwrap().1 else {
        panic!("fixture agents list");
    };
    for agent in agents {
        let Yaml::Map(fields) = agent else {
            panic!("fixture agent object");
        };
        if let Some((_, value)) = fields.iter_mut().find(|(key, _)| key == "paused") {
            *value = Yaml::Bool(true);
        } else {
            fields.push(("paused".into(), Yaml::Bool(true)));
        }
    }
    fs::write(&spec, crate::model::yaml::dumps(&paused)).unwrap();
    let paused_t = offline();
    let result = launch_with_transport_in_workspace(&f.ws, &spec, false, false, true, &paused_t);
    assert!(
        result.is_ok(),
        "paused-only must not validate opt-in routing: {result:?}"
    );
    assert!(paused_t.spawn_records().is_empty());
    for (_, argv) in observed_spawns {
        assert_route(&argv, &["cold-route"]);
    }
}

#[test]
#[serial(env)]
fn a09_start_fresh_missing_backing_reset_and_add_use_new_route_once() {
    let f = Fixture::new("argv-single");
    f.start_team("codex", &["worker"]);
    f.seed(true, json!({"codex":["single-route"]}));
    f.switch(None);
    let mut samples = Vec::new();
    for backing in [false, true] {
        f.mutate_agent("worker", |a| {
            a["status"] = json!("stopped");
            if backing {
                a["session_id"] = json!("lost-session");
                a["rollout_path"] = json!(f.ws.join("missing.jsonl"));
                a["captured_at"] = json!("2026-01-01T00:00:00Z");
                a["captured_via"] = json!("session_scan");
            }
        });
        let t = offline();
        let result = start_agent_with_transport(
            &f.ws,
            &AgentId::new("worker"),
            false,
            false,
            true,
            Some("argvteam"),
            &t,
        );
        assert!(
            result.is_ok(),
            "allow-fresh missing backing fixture: {result:?}"
        );
        assert_eq!(t.spawn_records().len(), 1);
        samples.push(t.spawn_records()[0].1.clone());
    }
    let reset = offline();
    let result = reset_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        true,
        false,
        Some("argvteam"),
        &reset,
    );
    assert!(result.is_ok(), "reset fixture: {result:?}");
    assert_eq!(reset.spawn_records().len(), 1);
    samples.push(reset.spawn_records()[0].1.clone());
    let role_file = f.ws.join("mate-role.md");
    fs::write(&role_file, role("codex", "mate")).unwrap();
    let added = offline().with_session_present(true);
    add_agent_with_transport(
        &f.team,
        &AgentId::new("mate"),
        &role_file,
        false,
        Some("argvteam"),
        &added,
    )
    .unwrap();
    assert_eq!(added.spawn_records().len(), 1);
    samples.push(added.spawn_records()[0].1.clone());
    assert_eq!(samples.len(), 4);
    for argv in samples {
        assert_route(&argv, &["single-route"]);
    }
}

#[test]
#[serial(env)]
fn a09_resume_retains_session_metadata_while_routes_change() {
    let f = Fixture::new("argv-resume");
    f.start_team("pi", &["worker"]);
    let selected = crate::state::projection::select_runtime_state(&f.ws, Some("argvteam")).unwrap();
    let pending = selected["agents"]["worker"]["_pending_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let session = crate::lifecycle::launch::pi_mcp::pi_seat_paths(&f.ws, "argvteam", "worker")
        .sessions
        .join("valid.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(&session, format!("{}\n{}\n", json!({"type":"session","version":3,"id":pending,"cwd":f.ws,"timestamp":"2026-01-01T00:00:00Z"}), json!({"type":"message","id":"a","parentId":null,"timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"seed"}]}}))).unwrap();
    f.mutate_agent("worker", |a| {
        a["status"] = json!("stopped");
        a["session_id"] = json!(pending);
        a["rollout_path"] = json!(session);
        a["captured_at"] = json!("2026-01-01T00:00:01Z");
        a["captured_via"] = json!("session_scan");
        a["capture_state"] = json!("captured");
        a["first_send_at"] = json!("2026-01-01T00:00:01Z");
    });
    f.seed(true, json!({"pi":["resume-route"]}));
    f.switch(None);
    let t = offline();
    start_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        false,
        false,
        false,
        Some("argvteam"),
        &t,
    )
    .unwrap();
    assert_eq!(t.spawn_records().len(), 1);
    let argv = t.spawn_records()[0].1.clone();
    assert!(argv
        .windows(2)
        .any(|p| p[0] == "--session" && Path::new(&p[1]) == session));
    assert!(!argv.iter().any(|v| v == "--session-id"));
    let restarted = offline()
        .with_session_present(true)
        .with_default_liveness(crate::transport::PaneLiveness::Dead);
    let report = restart_with_transport(&f.ws, false, Some("argvteam"), &restarted);
    assert!(report.is_ok(), "in-session resume fixture: {report:?}");
    assert_eq!(restarted.spawn_records().len(), 1);
    let restart_argv = restarted.spawn_records()[0].1.clone();
    assert!(restart_argv
        .windows(2)
        .any(|p| p[0] == "--session" && Path::new(&p[1]) == session));
    assert_route(&argv, &["resume-route"]);
    assert_route(&restart_argv, &["resume-route"]);
}

#[test]
#[serial(env)]
fn a09_remove_rollback_respawns_original_seat_with_route_once() {
    let f = Fixture::new("argv-rollback");
    f.start_team("codex", &["worker", "mate"]);
    f.mutate_agent("worker", |a| {
        a["pane_id"] = json!("%old");
        a["window"] = json!("worker");
    });
    let _fail = f.env.with_env(
        "TEAM_AGENT_TEST_FAIL_REMOVE_AFTER_AGENT_HEALTH_DELETE",
        "argv-route-red",
    );
    f.seed(true, json!({"codex":["rollback-route"]}));
    f.switch(None);
    let t = offline()
        .with_session_present(true)
        .with_targets(vec![live_pane(&f.ws)]);
    let result = crate::lifecycle::restart::remove_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        true,
        true,
        Some("argvteam"),
        &t,
    );
    assert!(result.is_err());
    assert_eq!(
        t.spawn_records().len(),
        1,
        "fault must reach rollback respawn: {result:?}"
    );
    assert_route(&t.spawn_records()[0].1, &["rollback-route"]);
    let state = crate::state::projection::select_runtime_state(&f.ws, Some("argvteam")).unwrap();
    assert!(
        state["agents"]["worker"].is_object(),
        "rollback must restore original seat"
    );
}

#[test]
#[serial(env)]
fn a09_pi_fork_inherits_session_and_routes_only_new_native_spawn() {
    let f = Fixture::new("argv-fork");
    f.start_team("pi", &["worker"]);
    let session = crate::lifecycle::launch::pi_mcp::pi_seat_paths(&f.ws, "argvteam", "worker")
        .sessions
        .join("source.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    let id = "fc317e54-bdbb-4ce5-8c1a-2b1bf33a9700";
    fs::write(&session, format!("{}\n{}\n", json!({"type":"session","version":3,"id":id,"cwd":f.ws,"timestamp":"2026-01-01T00:00:00Z"}), json!({"type":"message","id":"seed01","parentId":null,"timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"fork seed"}]}}))).unwrap();
    f.mutate_agent("worker", |a| {
        a["session_id"] = json!(id);
        a["rollout_path"] = json!(session);
        a["captured_at"] = json!("2026-01-01T00:00:01Z");
        a["captured_via"] = json!("session_scan");
        a["capture_state"] = json!("captured");
    });
    let original = fs::read(&session).unwrap();
    f.seed(true, json!({"pi":["fork-route"]}));
    f.switch(None);
    let t = offline().with_session_present(true);
    let report = fork_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        &AgentId::new("forked"),
        None,
        false,
        Some("argvteam"),
        &t,
    );
    assert!(
        report.is_ok(),
        "valid v3 Pi fork must reach native spawn: {report:?}"
    );
    assert_eq!(t.spawn_records().len(), 1);
    assert_route(&t.spawn_records()[0].1, &["fork-route"]);
    assert_eq!(fs::read(session).unwrap(), original);
}

#[test]
#[serial(env)]
fn a09_public_clone_reaches_native_argv_without_inheriting_old_route() {
    let f = Fixture::new("argv-clone");
    f.start_team("pi", &["worker"]);
    // Public clone has no Transport seam: this fixture tmux executes the actual
    // generated provider shell line against the argv recorder, never real tmux.
    let calls = f.env.root().join("clone-spawn-calls");
    executable(&f.bin.join("tmux"), &format!("#!/bin/sh\nif [ \"$1\" = -S ] || [ \"$1\" = -L ]; then shift 2; fi\nop=$1; shift\ncase \"$op\" in\n-V) echo 'tmux 3.4';;\nhas-session) exit 0;;\nnew-window|new-session|split-window) for last do :; done; printf '%s\\n' \"$op\" >> '{}'; /bin/sh -c \"$last\" >/dev/null 2>&1; echo %99; exit 0;;\ndisplay-message) for last do :; done; case \"$last\" in *pane_id*) echo %99;; *pane_dead*) echo 0;; *pane_width*) echo 120;; esac;;\ncapture-pane) echo '> '; ;;
*) exit 0;;\nesac\n", calls.display()));
    f.seed(true, json!({"pi":["clone-route"]}));
    f.switch(None);
    let cloned = crate::lifecycle::clone_agent(
        &f.ws,
        &AgentId::new("worker"),
        &AgentId::new("cloned"),
        None,
        false,
        Some("argvteam"),
    );
    assert!(
        cloned.is_ok(),
        "public clone fixture must complete: {cloned:?}"
    );
    assert_eq!(fs::read_to_string(calls).unwrap().lines().count(), 1);
    let bytes = fs::read(f.native_file()).unwrap();
    let args = bytes
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| String::from_utf8(b.to_vec()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(args.first().map(String::as_str), Some("clone-route"));
    assert_eq!(
        args.iter().filter(|v| v.as_str() == "clone-route").count(),
        1
    );
}

#[test]
#[serial(env)]
fn a09_allowed_force_recreate_uses_route_once_after_old_seat_is_dead() {
    let f = Fixture::new("argv-force");
    f.start_team("codex", &["worker"]);
    let role_file = f.ws.join("replacement.md");
    fs::write(&role_file, role("codex", "worker")).unwrap();
    f.seed(true, json!({"codex":["force-route"]}));
    f.switch(None);
    let t = offline();
    let result = crate::lifecycle::launch::add_agent_with_transport_force(
        &f.team,
        &AgentId::new("worker"),
        &role_file,
        false,
        Some("argvteam"),
        true,
        &t,
    );
    assert!(
        result.is_ok(),
        "allowed dead-seat force fixture must complete: {result:?}"
    );
    assert_eq!(t.spawn_records().len(), 1);
    assert_route(&t.spawn_records()[0].1, &["force-route"]);
}

#[test]
#[serial(env)]
fn a10_live_start_noop_is_not_validated_against_bad_route_config() {
    let f = Fixture::new("argv-keep");
    f.start_team("codex", &["worker"]);
    f.mutate_agent("worker", |a| {
        a["pane_id"] = json!("%old");
        a["window"] = json!("worker");
    });
    f.raw("invalid JSON");
    f.switch(Some("on"));
    let t = offline()
        .with_session_present(true)
        .with_targets(vec![live_pane(&f.ws)])
        .with_windows(vec![WindowName::new("worker")])
        .with_liveness("%old", crate::transport::PaneLiveness::Live);
    // The baseline restart library deliberately rebuilds live seats. Observe
    // the actual existing live-seat Noop surface, without inventing new policy.
    let result = start_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        false,
        false,
        false,
        Some("argvteam"),
        &t,
    );
    assert!(
        matches!(result, Ok(crate::lifecycle::StartAgentOutcome::Noop { .. })),
        "live Noop must not consult router: {result:?}"
    );
    assert!(t.spawn_records().is_empty());
}

#[test]
#[serial(env)]
fn a10_restart_first_parallel_later_reloads_mapping_without_stacking() {
    let f = Fixture::new("argv-restart");
    f.start_team("codex", &["worker", "mate", "third"]);
    let mut samples = Vec::new();
    for marker in ["route-generation-one", "route-generation-two"] {
        f.seed(true, json!({"codex":[marker]}));
        f.switch(None);
        let t = offline();
        let result = restart_with_transport(&f.ws, true, Some("argvteam"), &t);
        assert!(
            result.is_ok(),
            "restart fixture must reach plans: {result:?}"
        );
        assert_eq!(t.spawn_records().len(), 3);
        for (_, argv) in t.spawn_records() {
            samples.push((marker, argv));
        }
    }
    assert_eq!(samples.len(), 6);
    for (marker, argv) in samples {
        assert_route(&argv, &[marker]);
        if marker.ends_with("two") {
            assert!(!argv.iter().any(|v| v == "route-generation-one"));
        }
    }
}

#[test]
#[serial(env)]
fn a11_enabled_bad_schema_fails_before_native_spawn_at_all_three_boundaries() {
    let f = Fixture::new("argv-errors");
    f.start_team("codex", &["worker"]);
    for raw in [
        "bad JSON",
        r#"{"schema_version":2,"enabled":true,"providers":{}}"#,
        r#"{"schema_version":1,"enabled":true,"providers":{"unknown":[]}}"#,
        r#"{"schema_version":1,"enabled":true,"providers":{"pi":[7]}}"#,
        r#"{"schema_version":1,"enabled":true,"providers":{"pi":["\u0000"]}}"#,
        r#"{"schema_version":1,"enabled":true,"providers":{"pi":[],"pi":[]}}"#,
    ] {
        f.raw(raw);
        f.switch(Some("on"));
        let spec = f.spec("codex", &["worker"]);
        let cold = offline();
        assert!(
            launch_with_transport_in_workspace(&f.ws, &spec, false, false, true, &cold).is_err(),
            "ON cold must reject {raw}"
        );
        assert!(cold.spawn_records().is_empty());
        let single = offline();
        assert!(
            start_agent_with_transport(
                &f.ws,
                &AgentId::new("worker"),
                true,
                false,
                false,
                Some("argvteam"),
                &single
            )
            .is_err(),
            "ON single must reject {raw}"
        );
        assert!(single.spawn_records().is_empty());
        let leader = f.leader(Provider::Codex, false, false);
        assert!(leader.is_err(), "ON leader must reject {raw}");
    }
    f.seed(true, json!({"pi":[]}));
    f.switch(None);
    fs::remove_file(f.config()).unwrap();
    fs::create_dir(f.config()).unwrap();
    f.switch(Some("on"));
    assert!(f.leader(Provider::Codex, false, false).is_err());
    fs::remove_dir(f.config()).unwrap();
    let _home = f.env.with_env("HOME", "relative-home");
    assert!(f.leader(Provider::Codex, false, false).is_err());
}

#[test]
#[serial(env)]
fn a13_materializer_version_catalog_and_nonagent_command_builders_are_not_routed() {
    let f = Fixture::new("argv-probes");
    f.seed(
        true,
        json!({"pi":["--mode","rpc"],"copilot":["must-not-pollute-probe"]}),
    );
    f.switch(None);
    let base = f.cold("pi");
    let _ = crate::lifecycle::launch::pi_mcp::pi_model_candidates("team-agent/qwen3.8-27b");
    let probes = fs::read_to_string(&f.probes).unwrap();
    assert!(probes.contains("--version"));
    assert!(probes.contains("--list-models"));
    for line in probes.lines() {
        assert!(!line.contains("rpc") && !line.contains("--mode"));
    }
    assert!(
        !f.native_file().exists(),
        "plan/materializer must not execute native provider session"
    );
    assert_route(&base, &["--mode", "rpc"]);
}

#[test]
#[serial(env)]
fn a07_attach_existing_never_opens_enabled_invalid_route_config() {
    let f = Fixture::new("argv-attach");
    f.raw("invalid JSON");
    f.switch(Some("on"));
    let plan = f.leader(Provider::Pi, true, true).unwrap();
    assert_eq!(plan.mode, LeaderStartMode::AttachExisting);
    assert!(plan.provider_argv.is_empty());
    assert!(!plan.argv.iter().any(|v| v == "rpc"));
}

#[test]
#[serial(env)]
fn a13_all_provider_version_auth_and_cursor_mcp_auxiliary_argv_are_native() {
    let f = Fixture::new("argv-aux");
    let mappings = CANONICAL
        .iter()
        .map(|(k, _)| ((*k).to_string(), json!(["route-must-not-enter-aux"])))
        .collect::<serde_json::Map<_, _>>();
    f.seed(true, Value::Object(mappings));
    f.switch(None);
    for (_, provider) in CANONICAL {
        assert!(crate::provider::get_adapter(*provider).version().is_ok());
    }
    let _ = crate::provider::get_adapter(Provider::ClaudeCode)
        .auth_hint(crate::model::enums::AuthMode::Subscription);
    assert_eq!(
        crate::lifecycle::launch::cursor_mcp_enable_argv(),
        ["agent", "mcp", "enable", "team_orchestrator"]
    );
    let _ = f.cold("copilot");
    let probes = fs::read_to_string(&f.probes).unwrap();
    assert!(!probes.contains("route-must-not-enter-aux"));
    assert!(probes.contains("--version"));
}

#[test]
#[serial(env)]
fn a14_posix_native_recorder_literal_vector_positive_guard() {
    let f = Fixture::new("argv-posix-positive");
    let sentinel = f.env.root().join("MUST-NOT-EXIST");
    let argv = vec![
        f.bin.join("pi").to_string_lossy().into_owned(),
        "".into(),
        " x y ".into(),
        "路由🚀".into(),
        "'\"\\\n".into(),
        format!(
            ";touch {};$(touch {});`touch {}`",
            sentinel.display(),
            sentinel.display(),
            sentinel.display()
        ),
        "--help".into(),
        "--json".into(),
        "--no-display".into(),
        "--".into(),
    ];
    for leader in [false, true] {
        let line = if leader {
            crate::tmux_backend::leader_shell_wrapper_command(
                &argv,
                &f.ws,
                &BTreeMap::new(),
                &[],
                "pi",
            )
        } else {
            crate::tmux_backend::worker_shell_wrapper_command(
                &argv,
                &f.ws,
                &BTreeMap::new(),
                &[],
                "pi",
            )
        };
        let status = Command::new("/bin/sh")
            .args(["-c", &line])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success());
        let bytes = fs::read(f.native_file()).unwrap();
        let received = bytes
            .split(|b| *b == 0)
            .take(argv.len() - 1)
            .map(|v| String::from_utf8(v.to_vec()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(received, argv[1..]);
        assert!(!sentinel.exists());
    }
}

#[test]
#[serial(env)]
fn a14_conpty_spawn_request_frame_preserves_routed_vector_not_windows_live_pass() {
    use conpty_transport::protocol::{read_frame, write_frame, SpawnRequest};
    let f = Fixture::new("argv-conpty");
    f.seed(
        true,
        json!({"codex":["", "a b", "路由", "\"x\"\\", "line\nbreak"]}),
    );
    f.switch(None);
    let argv = f.cold("codex");
    assert_eq!(&argv[1..6], &["", "a b", "路由", "\"x\"\\", "line\nbreak"]);
    let request = SpawnRequest {
        session: "team-argvteam".into(),
        window: "worker".into(),
        argv: argv.clone(),
        cwd: f.ws.to_string_lossy().into_owned(),
        env: BTreeMap::new(),
        env_unset: vec![],
        cols: 120,
        rows: 30,
    };
    let mut wire = Vec::new();
    write_frame(&mut wire, &serde_json::to_vec(&request).unwrap()).unwrap();
    let decoded: SpawnRequest =
        serde_json::from_slice(&read_frame(&mut wire.as_slice()).unwrap()).unwrap();
    assert_eq!(decoded.argv, argv);
    assert_eq!(decoded.cwd, request.cwd);
}

#[test]
#[serial(env)]
fn a15_route_does_not_relax_running_start_reset_discard_or_unsupported_fork() {
    let f = Fixture::new("argv-old-refusals");
    f.start_team("codex", &["worker"]);
    f.mutate_agent("worker", |a| {
        a["pane_id"] = json!("%old");
        a["window"] = json!("worker");
    });
    f.seed(true, json!({"codex":["refusal-route"]}));
    f.switch(None);
    let t = offline()
        .with_session_present(true)
        .with_targets(vec![live_pane(&f.ws)])
        .with_windows(vec![WindowName::new("worker")])
        .with_liveness("%old", crate::transport::PaneLiveness::Live);
    let start = start_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        false,
        false,
        false,
        Some("argvteam"),
        &t,
    );
    assert!(
        matches!(
            start,
            Err(_) | Ok(crate::lifecycle::StartAgentOutcome::Noop { .. })
        ),
        "existing live seat must refuse/noop without spawn"
    );
    assert!(t.spawn_records().is_empty());
    let reset = reset_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        false,
        false,
        Some("argvteam"),
        &t,
    )
    .unwrap();
    assert!(matches!(
        reset,
        crate::lifecycle::ResetAgentOutcome::Refused { .. }
    ));
    assert!(t.spawn_records().is_empty());
    let fork = fork_agent_with_transport(
        &f.ws,
        &AgentId::new("worker"),
        &AgentId::new("forked"),
        None,
        false,
        Some("argvteam"),
        &t,
    );
    assert!(fork.is_err());
    assert!(t.spawn_records().is_empty());
    let before = load_runtime_state(&f.ws).unwrap();
    assert!(before.pointer("/agents/forked").is_none());
}
