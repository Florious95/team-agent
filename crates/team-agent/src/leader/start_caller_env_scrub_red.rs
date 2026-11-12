//! ---
//! purpose: red oracle for BUG-ANALYSIS-ROLE-CREATION-FAILURE (视频制作 incident)
//! contract:
//!   - a Leader provider process must never inherit a stale Team Agent caller
//!     tuple (`TEAM_AGENT_CALLER_*`) or worker identity overlay from the tmux
//!     server / launching shell; the fail-closed caller gate itself must stay
//!   - RT-1..RT-4 are RED at baseline 36c890b4; RT-5 and RT-8 are GREEN guards
//! boundary:
//!   - every tmux server is test-owned (unique socket, RAII kill-server)
//!   - the test process environment is never mutated (RT-3 uses a re-exec child)
//! transcripts: $TA_RED_TRANSCRIPT_DIR/<RT-id>.log (default: temp_dir/ta-red-transcripts)
//! ---
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::layout::worker_env::{
    caller_provider_resolution_from_values, CallerProviderResolution, CALLER_ENDPOINT_ENV,
    CALLER_PANE_ENV, CALLER_PROVIDER_ENV,
};
use crate::leader::{
    LeaderIdentity, LeaderLaunchSocket, LeaderSessionUuidSource, LeaderStartMode, LeaderStartPlan,
};
use crate::model::ids::{LeaderSessionUuid, TeamKey};
use crate::provider::Provider;
use crate::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use crate::transport::{SessionName, WindowName};

/// Synthetic, incident-shaped values for inherited launcher state.
const STALE_CALLER_PROVIDER: &str = "claude";
const STALE_CALLER_PANE: &str = "%46";
const STALE_CALLER_ENDPOINT: &str = "/private/tmp/tmux-501/ta-a9fd5b7defbd";
const STALE_WORKER_ID: &str = "recovery-haiku";
const STALE_OWNER_TEAM: &str = "codex-effort-fix";
/// Synthetic, incident-shaped UTF-8 path re-encoded Latin-1 -> UTF-8 by the hand-off.
const DOUBLE_ENCODED_WORKSPACE: &str =
    "/private/tmp/rce-incident/\u{e8}\u{a7}\u{86}\u{e9}\u{a2}\u{91}\u{e5}\u{88}\u{b6}\u{e4}\u{bd}\u{9c}";
const NON_ASCII_WORKSPACE_NAME: &str = "视频制作";

/// Keys a Leader process must never inherit: the caller triple (decisive for the
/// incident) plus the worker-only identity overlays (latent sender misattribution).
const MUST_SCRUB: [&str; 6] = [
    CALLER_PROVIDER_ENV,
    CALLER_PANE_ENV,
    CALLER_ENDPOINT_ENV,
    "TEAM_AGENT_ID",
    "TEAM_AGENT_AGENT_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
];

const ALL_PROVIDERS: [Provider; 8] = [
    Provider::Claude,
    Provider::ClaudeCode,
    Provider::Codex,
    Provider::Copilot,
    Provider::GeminiCli,
    Provider::Grok,
    Provider::Pi,
    Provider::Fake,
];

static SEQ: AtomicU64 = AtomicU64::new(0);

fn stale_env() -> Vec<(&'static str, &'static str)> {
    vec![
        (CALLER_PROVIDER_ENV, STALE_CALLER_PROVIDER),
        (CALLER_PANE_ENV, STALE_CALLER_PANE),
        (CALLER_ENDPOINT_ENV, STALE_CALLER_ENDPOINT),
        ("TEAM_AGENT_ID", STALE_WORKER_ID),
        ("TEAM_AGENT_AGENT_ID", STALE_WORKER_ID),
        ("TEAM_AGENT_OWNER_TEAM_ID", STALE_OWNER_TEAM),
        ("TEAM_AGENT_AUTH_MODE", "subscription"),
        ("TEAM_AGENT_WORKSPACE", DOUBLE_ENCODED_WORKSPACE),
    ]
}

fn transcript(id: &str, body: &str) {
    let dir = std::env::var_os("TA_RED_TRANSCRIPT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ta-red-transcripts"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{id}.log")), body);
    eprintln!("===== {id} transcript =====\n{body}\n===== end {id} =====");
}

fn short_root(tag: &str) -> PathBuf {
    let base = if cfg!(target_os = "macos") { "/private/tmp" } else { "/tmp" };
    let root = PathBuf::from(base).join(format!(
        "ta-rce-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn identity_for(workspace: &Path) -> LeaderIdentity {
    LeaderIdentity {
        leader_session_uuid: LeaderSessionUuid::derive(
            "fp",
            &workspace.to_string_lossy(),
            "tester",
            "current",
        )
        .unwrap(),
        leader_session_uuid_source: LeaderSessionUuidSource::Derived,
        machine_fingerprint: "fp".into(),
        workspace_abspath: workspace.into(),
        os_user: "tester".into(),
        team_id: TeamKey::new("current"),
    }
}

fn managed_plan(
    provider: Provider,
    workspace: &Path,
    provider_argv: Vec<String>,
    leader_env: BTreeMap<String, String>,
) -> LeaderStartPlan {
    LeaderStartPlan {
        mode: LeaderStartMode::ManagedTmuxClient,
        provider,
        workspace: workspace.to_path_buf(),
        socket: LeaderLaunchSocket::Workspace,
        session_name: Some(SessionName::new("rce-leader")),
        argv: vec!["tmux".into(), "attach-session".into()],
        provider_argv,
        leader_prompt: None,
        session_capture: None,
        leader_window: Some(WindowName::new(super::provider_command_name(provider))),
        is_external_leader: false,
        leader_env,
        identity: Some(identity_for(workspace)),
        detached: false,
    }
}

// ---------------------------------------------------------------------------
// Recording CommandRunner: captures the real tmux argv TmuxBackend emits.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct RecordingRunner {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl RecordingRunner {
    fn spawn_command(&self) -> Option<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|argv| argv.iter().any(|a| a == "new-session" || a == "new-window"))
            .cloned()
    }
}

fn flag_value(argv: &[String], flag: &str) -> String {
    argv.iter()
        .position(|a| a == flag)
        .and_then(|i| argv.get(i + 1))
        .cloned()
        .unwrap_or_default()
}

impl CommandRunner for RecordingRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let ok = |stdout: String| {
            Ok(CommandOutput { success: true, code: Some(0), stdout, stderr: String::new() })
        };
        if argv.iter().any(|a| a == "new-session" || a == "new-window") {
            return ok("%7\n".into());
        }
        if argv.iter().any(|a| a == "list-panes") {
            let spawn = self.spawn_command().unwrap_or_default();
            let session = if spawn.iter().any(|a| a == "new-session") {
                flag_value(&spawn, "-s")
            } else {
                flag_value(&spawn, "-t")
            };
            let window = flag_value(&spawn, "-n");
            let f = "__TA_FIELD__";
            return ok(format!(
                "%7{f}{session}{f}0{f}{window}{f}0{f}/dev/ttys999{f}sh{f}1{f}/tmp{f}0{f}0{f}4242{f}\n"
            ));
        }
        ok(String::new())
    }
}

/// Run the private managed-leader spawn seam against a recording TmuxBackend and
/// return (full tmux argv, `sh -lc` wrapper command).
fn recorded_leader_spawn(plan: &LeaderStartPlan, existed: bool) -> (Vec<String>, String) {
    let runner = RecordingRunner::default();
    let backend = TmuxBackend::with_runner_for_tmux_endpoint(
        Box::new(runner.clone()),
        "/private/tmp/rce-recording.sock",
    );
    let session = plan.session_name.clone().unwrap();
    let window = plan.leader_window.clone().unwrap();
    super::ensure_managed_leader_pane(&backend, &session, &window, plan, &plan.workspace, existed)
        .unwrap_or_else(|error| panic!("recorded leader spawn failed: {error:?}"));
    let argv = runner.spawn_command().expect("tmux spawn argv recorded");
    let command = argv.last().cloned().unwrap_or_default();
    (argv, command)
}

/// Keys that are NOT unset in the wrapper before the provider argv starts.
fn missing_unsets(command: &str, provider_sentinel: &str) -> Vec<&'static str> {
    let provider_at = command.find(provider_sentinel).unwrap_or(usize::MAX);
    MUST_SCRUB
        .iter()
        .copied()
        .filter(|key| match command.find(&format!(" unset {key} ")) {
            Some(at) => at > provider_at,
            None => true,
        })
        .collect()
}

fn reassigned(command: &str) -> Vec<&'static str> {
    MUST_SCRUB
        .iter()
        .copied()
        .filter(|key| command.contains(&format!(" {key}=")))
        .collect()
}

// ---------------------------------------------------------------------------
// RT-1 (RED at baseline): every provider, first spawn and spawn-into.
// ---------------------------------------------------------------------------

#[test]
fn rt1_leader_spawn_argv_unsets_caller_tuple_and_worker_identity_for_every_provider() {
    let workspace = PathBuf::from("/private/tmp/rce-rt1").join(NON_ASCII_WORKSPACE_NAME);
    let mut log = String::from(
        "RT-1 contract: managed Leader spawn (first + into) must `unset` every key in MUST_SCRUB\n\
         before the provider argv, for every provider.\n",
    );
    log.push_str(&format!("MUST_SCRUB = {MUST_SCRUB:?}\n\n"));
    let mut failures = Vec::new();
    for provider in ALL_PROVIDERS {
        for existed in [false, true] {
            let sentinel = format!("rce-provider-sentinel-{}", super::provider_command_name(provider));
            let plan = managed_plan(
                provider,
                &workspace,
                vec![sentinel.clone(), "--flag".into()],
                super::leader_env_for_identity(provider, &identity_for(&workspace)),
            );
            let (argv, command) = recorded_leader_spawn(&plan, existed);
            let missing = missing_unsets(&command, &sentinel);
            let mode = if existed { "spawn_into" } else { "spawn_first" };
            log.push_str(&format!(
                "--- provider={provider:?} mode={mode}\nargv[..len-1]={:?}\nwrapper={command}\nexpected_unset={MUST_SCRUB:?}\nactual_missing={missing:?}\n\n",
                &argv[..argv.len().saturating_sub(1)]
            ));
            if !missing.is_empty() {
                failures.push(format!("{provider:?}/{mode}: missing unset {missing:?}"));
            }
        }
    }
    log.push_str(&format!("RESULT: {} failing combinations\n{}\n", failures.len(), failures.join("\n")));
    transcript("RT-1", &log);
    assert!(failures.is_empty(), "RT-1 FAILED (expected: none missing; actual):\n{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------
// RT-2 (RED at baseline): incident-shaped pi Leader first spawn.
// ---------------------------------------------------------------------------

#[test]
fn rt2_incident_pi_leader_first_spawn_scrubs_before_identity_and_never_reexports_stale_tuple() {
    let workspace = PathBuf::from("/private/tmp/rce-rt2").join(NON_ASCII_WORKSPACE_NAME);
    let identity = identity_for(&workspace);
    let mut leader_env = super::leader_env_for_identity(Provider::Pi, &identity);
    let clean_leader_env_has_caller = leader_env.keys().any(|k| MUST_SCRUB.contains(&k.as_str()));
    // A merged/inherited env map (ExecProvider-style) must not re-export the stale tuple.
    for (key, value) in stale_env() {
        leader_env.entry(key.to_string()).or_insert_with(|| value.to_string());
    }
    let sentinel = "rce-provider-sentinel-pi";
    let plan = managed_plan(Provider::Pi, &workspace, vec![sentinel.into()], leader_env);
    let (argv, command) = recorded_leader_spawn(&plan, false);
    let missing = missing_unsets(&command, sentinel);
    let reexported = reassigned(&command);
    let identity_export_at = command.find(" TEAM_AGENT_LEADER_PROVIDER=pi ");
    let late_unsets: Vec<&str> = MUST_SCRUB
        .iter()
        .copied()
        .filter(|key| {
            let at = command.find(&format!(" unset {key} "));
            matches!((at, identity_export_at), (Some(u), Some(e)) if u > e)
        })
        .collect();
    let log = format!(
        "RT-2 incident replay (argv level)\nworkspace={}\nstale tuple injected into leader env map: {:?}\n\
         precondition leader_env_for_identity(Pi) carries no MUST_SCRUB key: {}\n\
         tmux argv[..len-1]={:?}\nwrapper={command}\n\n\
         expected: all MUST_SCRUB unset before provider; none re-exported; unsets precede TEAM_AGENT_LEADER_PROVIDER export\n\
         actual_missing_unset={missing:?}\nactual_reexported={reexported:?}\nactual_unset_after_identity_export={late_unsets:?}\n",
        workspace.display(),
        stale_env(),
        !clean_leader_env_has_caller,
        &argv[..argv.len().saturating_sub(1)],
    );
    transcript("RT-2", &log);
    assert!(!clean_leader_env_has_caller, "RT-2 PRECONDITION: leader_env_for_identity must not carry caller/worker keys");
    assert!(command.contains(&format!("cd '{}'", workspace.display())) || command.contains(&format!("cd {}", workspace.display())),
        "RT-2 PRECONDITION: wrapper must cd into the non-ASCII workspace verbatim");
    assert!(
        missing.is_empty() && reexported.is_empty() && late_unsets.is_empty(),
        "RT-2 FAILED: missing_unset={missing:?} reexported={reexported:?} late_unsets={late_unsets:?}"
    );
}

// ---------------------------------------------------------------------------
// RT-3 (RED at baseline): in-tmux ExecProvider Leader, real child process env.
// The stale keys reach the child through genuine process inheritance
// (merged_exec_env seeds from std::env::vars()) in a re-exec'd test child.
// ---------------------------------------------------------------------------

const RT3_CHILD: &str = "TA_RCE_RT3_CHILD_PROVIDER";
const RT3_OUT: &str = "TA_RCE_RT3_OUT";
const RT3_WS: &str = "TA_RCE_RT3_WS";

#[test]
fn rt3_exec_provider_leader_child_env_drops_inherited_caller_tuple() {
    if let Ok(wire) = std::env::var(RT3_CHILD) {
        // Child: no TMUX/TMUX_PANE (startup-prompt handler stays inert), stale env inherited.
        let provider = crate::provider::wire::parse_provider(&wire).expect("rt3 provider");
        let workspace = PathBuf::from(std::env::var(RT3_WS).unwrap());
        let identity = identity_for(&workspace);
        let merged = super::merged_exec_env(&super::leader_env_for_identity(provider, &identity));
        let mut plan = managed_plan(provider, &workspace, vec![], merged.clone());
        plan.mode = LeaderStartMode::ExecProvider;
        plan.session_name = None;
        plan.leader_window = None;
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), format!("env > \"${RT3_OUT}\"")];
        let exit = super::run_leader_argv(&argv, &merged, &plan, &workspace, None)
            .unwrap_or_else(|error| panic!("rt3 child run_leader_argv failed: {error:?}"));
        assert_eq!(exit.status.code(), Some(0), "rt3 child launcher exit");
        return;
    }
    let test_name = format!(
        "{}::rt3_exec_provider_leader_child_env_drops_inherited_caller_tuple",
        module_path!().split_once("::").map(|(_, rest)| rest).unwrap()
    );
    let root = short_root("rt3");
    let workspace = root.join(NON_ASCII_WORKSPACE_NAME);
    std::fs::create_dir_all(&workspace).unwrap();
    let mut log = format!("RT-3 ExecProvider child env\nworkspace={}\nstale parent env={:?}\n\n", workspace.display(), stale_env());
    let mut failures = Vec::new();
    for provider in [Provider::Pi, Provider::ClaudeCode] {
        let wire = crate::provider::wire::provider_wire(provider);
        let out = root.join(format!("{wire}.env"));
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", &test_name, "--nocapture", "--test-threads=1"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("HOME", &root)
            .env(RT3_CHILD, wire)
            .env(RT3_OUT, &out)
            .env(RT3_WS, &workspace);
        for (key, value) in stale_env() {
            child.env(key, value);
        }
        let output = child.output().unwrap();
        let env_text = std::fs::read_to_string(&out).unwrap_or_default();
        let leaked: Vec<String> = env_text
            .lines()
            .filter(|line| MUST_SCRUB.iter().any(|key| line.starts_with(&format!("{key}="))))
            .map(str::to_string)
            .collect();
        let leader_marker = env_text.contains(&format!("TEAM_AGENT_LEADER_PROVIDER={wire}"));
        log.push_str(&format!(
            "--- provider={wire} child_status={:?}\nchild_stdout={}\nchild_stderr={}\n\
             precondition child env carries TEAM_AGENT_LEADER_PROVIDER={wire}: {leader_marker}\n\
             expected leaked=[]\nactual leaked={leaked:?}\n\n",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
        assert!(output.status.success() && leader_marker, "RT-3 PRECONDITION failed for {wire}; see transcript");
        if !leaked.is_empty() {
            failures.push(format!("{wire}: {leaked:?}"));
        }
    }
    transcript("RT-3", &log);
    let _ = std::fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "RT-3 FAILED (expected no inherited caller/worker keys):\n{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------
// Physical tmux harness (RT-4 RED, RT-8 GREEN guard).
// ---------------------------------------------------------------------------

struct OwnedTmux {
    root: PathBuf,
    socket: PathBuf,
}

impl Drop for OwnedTmux {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .env_remove("TMUX")
            .output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct PhysicalLeader {
    workspace: PathBuf,
    pane_env: BTreeMap<String, String>,
    pane_pwd: String,
    server_global_env: String,
    start_command: String,
}

/// Start a test-owned tmux server whose GLOBAL environment is exactly `seed`
/// (the incident mechanism), then spawn a managed Leader pane into it through the
/// real TmuxBackend and the private `ensure_managed_leader_pane` seam.
fn physical_leader(tag: &str, provider: Provider, seed: &[(&str, &str)]) -> (OwnedTmux, PhysicalLeader) {
    let root = short_root(tag);
    let workspace = root.join(NON_ASCII_WORKSPACE_NAME);
    std::fs::create_dir_all(&workspace).unwrap();
    let socket = root.join("s.sock");
    let out = root.join("pane.env");
    let guard = OwnedTmux { root: root.clone(), socket: socket.clone() };
    let mut seed_cmd = Command::new("tmux");
    seed_cmd
        .args(["-S"])
        .arg(&socket)
        .args(["-u", "-f", "/dev/null", "new-session", "-d", "-s", "seed", "-c"])
        .arg(&workspace)
        .arg("exec sleep 300")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &root)
        .env("LANG", "en_US.UTF-8");
    for (key, value) in seed {
        seed_cmd.env(key, value);
    }
    let seeded = seed_cmd.output().unwrap();
    assert!(seeded.status.success(), "{tag} PRECONDITION: seed tmux server failed: {seeded:?}");
    let server_global_env = String::from_utf8_lossy(
        &Command::new("tmux").arg("-S").arg(&socket).args(["show-environment", "-g"])
            .env_remove("TMUX").output().unwrap().stdout,
    )
    .lines()
    .filter(|line| line.starts_with("TEAM_AGENT_"))
    .collect::<Vec<_>>()
    .join("\n");

    let mut leader_env = super::leader_env_for_identity(provider, &identity_for(&workspace));
    leader_env.insert("RCE_PANE_OUT".into(), out.to_string_lossy().into_owned());
    let plan = managed_plan(
        provider,
        &workspace,
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "env > \"$RCE_PANE_OUT.tmp\"; pwd -P > \"$RCE_PANE_OUT.pwd\"; mv \"$RCE_PANE_OUT.tmp\" \"$RCE_PANE_OUT\"".into(),
        ],
        leader_env,
    );
    let backend = TmuxBackend::for_tmux_endpoint(&socket.to_string_lossy());
    let spawned = super::ensure_managed_leader_pane(
        &backend,
        plan.session_name.as_ref().unwrap(),
        plan.leader_window.as_ref().unwrap(),
        &plan,
        &workspace,
        false,
    )
    .unwrap_or_else(|error| panic!("{tag}: physical leader spawn failed: {error:?}"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !out.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let env_text = std::fs::read_to_string(&out)
        .unwrap_or_else(|_| panic!("{tag} PRECONDITION: leader pane never wrote its env"));
    let pane_env = env_text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let pane_pwd = std::fs::read_to_string(root.join("pane.env.pwd")).unwrap_or_default().trim().to_string();
    let start_command = String::from_utf8_lossy(
        &Command::new("tmux").arg("-S").arg(&socket)
            .args(["display-message", "-p", "-t", spawned.pane_id.as_str(), "#{pane_start_command}"])
            .env_remove("TMUX").output().unwrap().stdout,
    )
    .trim()
    .to_string();
    (guard, PhysicalLeader { workspace, pane_env, pane_pwd, server_global_env, start_command })
}

#[test]
fn rt4_physical_leader_pane_env_has_no_stale_caller_tuple_from_server_global_env() {
    let (_guard, leader) = physical_leader("rt4", Provider::Pi, &stale_env());
    let leaked: BTreeMap<&str, &String> = MUST_SCRUB
        .iter()
        .filter_map(|key| leader.pane_env.get(*key).map(|value| (*key, value)))
        .collect();
    let log = format!(
        "RT-4 physical tmux (real TmuxBackend, real tmux server, non-ASCII workspace)\n\
         workspace={}\nserver global env (TEAM_AGENT_*):\n{}\n\npane_start_command={}\n\n\
         leader pane TMUX_PANE={:?}\nexpected: none of {MUST_SCRUB:?} in leader pane env\nactual leaked={leaked:?}\n",
        leader.workspace.display(),
        leader.server_global_env,
        leader.start_command,
        leader.pane_env.get("TMUX_PANE"),
    );
    transcript("RT-4", &log);
    assert!(
        leader.server_global_env.contains(&format!("{CALLER_PANE_ENV}={STALE_CALLER_PANE}")),
        "RT-4 PRECONDITION: seed server global env must hold the stale caller tuple"
    );
    assert!(leaked.is_empty(), "RT-4 FAILED: leader pane inherited {leaked:?}");
}

// ---------------------------------------------------------------------------
// RT-5 (GREEN guard): the fail-closed caller gate must stay fail-closed.
// ---------------------------------------------------------------------------

#[test]
fn rt5_guard_incident_caller_tuple_remains_invalid_never_downgraded() {
    let current_endpoint = "/private/tmp/tmux-501/ta-9cdfefa9385e";
    let incident = caller_provider_resolution_from_values(
        Some(STALE_CALLER_PROVIDER),
        Some(STALE_CALLER_PANE),
        Some(STALE_CALLER_ENDPOINT),
        Some("pi"),
        None,
        Some("%0"),
        Some(current_endpoint),
        Some(current_endpoint),
    );
    // Same tuple but matching pane/endpoint: provider conflict alone must still refuse.
    let provider_conflict = caller_provider_resolution_from_values(
        Some(STALE_CALLER_PROVIDER),
        Some("%0"),
        Some(current_endpoint),
        Some("pi"),
        None,
        Some("%0"),
        Some(current_endpoint),
        Some(current_endpoint),
    );
    let log = format!(
        "RT-5 guard (pure resolution)\ncaller tuple: provider={STALE_CALLER_PROVIDER} pane={STALE_CALLER_PANE} endpoint={STALE_CALLER_ENDPOINT}\n\
         explicit TEAM_AGENT_LEADER_PROVIDER=pi current pane=%0 current endpoint={current_endpoint}\n\
         expected incident=Invalid actual={incident:?}\nexpected provider_conflict=Invalid actual={provider_conflict:?}\n"
    );
    transcript("RT-5", &log);
    assert_eq!(incident, CallerProviderResolution::Invalid, "RT-5: incident tuple must stay Invalid");
    assert_eq!(provider_conflict, CallerProviderResolution::Invalid, "RT-5: provider conflict must stay Invalid");
}

// ---------------------------------------------------------------------------
// RT-8 (GREEN guard): non-ASCII workspace is not a cause.
// ---------------------------------------------------------------------------

#[test]
fn rt8_guard_non_ascii_workspace_survives_leader_launch_and_clean_tuple_is_absent() {
    let incident_bytes = DOUBLE_ENCODED_WORKSPACE.chars()
        .map(|c| u8::try_from(c as u32).unwrap()).collect();
    assert_eq!(
        String::from_utf8(incident_bytes).unwrap(),
        format!("/private/tmp/rce-incident/{NON_ASCII_WORKSPACE_NAME}"),
        "the synthetic fixture must preserve the incident's Latin-1/UTF-8 corruption",
    );
    // (a) A clean caller context in a non-ASCII workspace is Absent, never Invalid.
    let endpoint = "/private/tmp/tmux-501/ta-9cdfefa9385e";
    let clean = caller_provider_resolution_from_values(
        None, None, None, Some("pi"), None, Some("%0"), Some(endpoint), Some(endpoint),
    );
    // (b) Physical: server global env carries the incident's double-encoded
    // TEAM_AGENT_WORKSPACE; the Leader pane must still see the exact UTF-8 path.
    let (_guard, leader) = physical_leader(
        "rt8",
        Provider::Pi,
        &[("TEAM_AGENT_WORKSPACE", DOUBLE_ENCODED_WORKSPACE)],
    );
    let expected_ws = leader.workspace.to_string_lossy().into_owned();
    let actual_ws = leader.pane_env.get("TEAM_AGENT_WORKSPACE").cloned().unwrap_or_default();
    let log = format!(
        "RT-8 guard\n(a) expected clean resolution=Absent actual={clean:?}\n\
         (b) server global env:\n{}\npane_start_command={}\n\
         expected TEAM_AGENT_WORKSPACE={expected_ws}\nactual   TEAM_AGENT_WORKSPACE={actual_ws}\n\
         expected pwd={expected_ws}\nactual   pwd={}\n",
        leader.server_global_env, leader.start_command, leader.pane_pwd,
    );
    transcript("RT-8", &log);
    assert_eq!(clean, CallerProviderResolution::Absent, "RT-8(a)");
    assert_eq!(actual_ws, expected_ws, "RT-8(b): Leader TEAM_AGENT_WORKSPACE must be the exact UTF-8 path");
    assert_eq!(leader.pane_pwd, expected_ws, "RT-8(b): Leader cwd must be the exact UTF-8 path");
}
