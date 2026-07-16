//! P0 RED contract: host co-tenant death / worker cohort identity.
//!
//! Source: `.team/artifacts/host-cotenant-death-p0-architecture-locate.md` §8.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_pid_path, Coordinator, ErrorLists, MetadataSource, Pid, ProviderRegistry,
    WorkspacePath,
};
use team_agent::lifecycle::{
    reset_agent_with_transport, restart_with_transport_with_readiness_deadline,
    start_agent_with_transport, LifecycleError, RestartReport,
};
use team_agent::message_store::MessageStore;
use team_agent::model::enums::{PaneLiveness, Provider};
use team_agent::model::ids::AgentId;
use team_agent::model::paths::{runtime_dir, runtime_spec_path};
use team_agent::provider::{get_adapter, CaptureSessionContext, ProviderAdapter};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const TEAM: &str = "current";
const SESSION: &str = "team-current";
const TMUX_ENDPOINT: &str = "/private/tmp/ta-p0-cohort.sock";
const POLLUTED_CALLER_AUTHORITY_ENVS: &[&str] = &[
    "TEAM_AGENT_LEADER_PANE_ID",
    "TEAM_AGENT_LEADER_SESSION_UUID",
    "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
    "TEAM_AGENT_LEADER_SESSION_NAME",
    "TEAM_AGENT_LEADER_PROVIDER",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
    "TEAM_AGENT_WORKSPACE",
    "TEAM_AGENT_TEAM_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
    "TEAM_AGENT_ACTIVE_TEAM",
    "TEAM_AGENT_ID",
    "TEAM_AGENT_AGENT_ID",
    "TEAM_AGENT_AUTH_MODE",
    "TEAM_AGENT_LEADER_BYPASS",
    "TEAM_AGENT_LEADER_BYPASS_SOURCE",
    "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
    "TEAM_AGENT_LEADER_BYPASS_FLAG",
    "TEAM_AGENT_MCP_AUTO_APPROVE",
    "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
];

#[test]
#[serial(env)]
fn polluted_caller_env_never_authorizes_teardown_to_kill_foreign_tmux() {
    let env = hermetic_guard::HermeticTestEnv::enter("p0-caller-env-sentinel");
    let shim_dir = env.root().join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let recorder = hermetic_guard::short_tmux_socket("p0-caller-env-recorder")
        .with_extension("log");
    let _ = fs::remove_file(&recorder);
    let sentinel = env.root().join("foreign-sentinel.sock");
    let polluted_tmux = format!("{},4242,0", sentinel.display());

    let real_path = std::env::var_os("PATH").unwrap_or_default();
    let real_tmux = std::env::split_paths(&real_path)
        .map(|dir| dir.join("tmux"))
        .find(|path| path.is_file())
        .expect("real tmux binary on PATH");
    let shim = shim_dir.join("tmux");
    fs::write(
        &shim,
        r#"#!/bin/sh
{
  printf 'TMUX=%s' "${TMUX-}"
  for arg in "$@"; do printf '\t%s' "$arg"; done
  printf '\n'
} >> "$TEAM_AGENT_TEST_TMUX_RECORDER_LOG"
case " $* " in
  *" kill-session "*|*" kill-server "*) exit 0 ;;
esac
exec "$TEAM_AGENT_TEST_REAL_TMUX" "$@"
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shim, permissions).unwrap();

    let mut polluted_env = vec![
        env.with_env(
            "PATH",
            &format!("{}:{}", shim_dir.display(), real_path.to_string_lossy()),
        ),
        env.with_env(
            "TEAM_AGENT_TEST_TMUX_RECORDER_LOG",
            recorder.to_str().unwrap(),
        ),
        env.with_env("TEAM_AGENT_TEST_REAL_TMUX", real_tmux.to_str().unwrap()),
        env.with_env("TMUX", &polluted_tmux),
        env.with_env("TMUX_PANE", "%foreign-sentinel"),
    ];
    for key in POLLUTED_CALLER_AUTHORITY_ENVS {
        polluted_env.push(env.with_env(key, "foreign-caller-authority"));
    }

    let missing_scrub_keys = POLLUTED_CALLER_AUTHORITY_ENVS
        .iter()
        .filter(|key| !hermetic_guard::CALLER_IDENTITY_ENVS.contains(key))
        .copied()
        .collect::<Vec<_>>();

    let _registration = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.register_owned_tmux_socket(&sentinel);
    }));
    drop(env);

    let recorded = fs::read_to_string(&recorder).unwrap_or_default();
    let destructive_hits = recorded
        .lines()
        .filter(|line| line.contains("\tkill-session") || line.contains("\tkill-server"))
        .filter(|line| {
            let explicit_endpoint = line.contains("\t-S\t") || line.contains("\t-L\t");
            let targets_sentinel = line.split('\t').any(|arg| arg == sentinel.to_str().unwrap());
            let inherits_sentinel = line.starts_with(&format!("TMUX={polluted_tmux}\t"));
            targets_sentinel || (inherits_sentinel && !explicit_endpoint)
        })
        .collect::<Vec<_>>();
    let _ = fs::remove_file(&recorder);

    assert!(
        missing_scrub_keys.is_empty() && destructive_hits.is_empty(),
        "P0 caller-env boundary: polluted caller identity must never become teardown authority; missing_scrub_keys={missing_scrub_keys:?}; destructive tmux argv targeting ambient sentinel={destructive_hits:?}; all recorded argv={recorded:?}"
    );
    drop(polluted_env);
}

#[test]
#[serial(env)]
fn restart_allow_fresh_does_not_leave_same_role_host_cotenants() {
    let mut failures = Vec::new();
    let (env, workspace) = team_case("p0-restart", &["developer", "test-engineer", "arch-reviewer"]);
    let shim_dir = env.root().join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let recorder = env.root().join("ps-recorder.log");
    let real_path = std::env::var_os("PATH").unwrap_or_default();
    let real_ps = std::env::split_paths(&real_path)
        .map(|dir| dir.join("ps"))
        .find(|path| path.is_file())
        .expect("real ps binary on PATH");
    let shim = shim_dir.join("ps");
    fs::write(
        &shim,
        r#"#!/bin/sh
{
  first=1
  for arg in "$@"; do
    if [ "$first" -eq 0 ]; then printf '\t'; fi
    printf '%s' "$arg"
    first=0
  done
  printf '\n'
} >> "$TEAM_AGENT_TEST_PS_RECORDER_LOG"
if [ "$#" -eq 4 ] && [ "$1" = "-p" ] && [ "$2" = "$TEAM_AGENT_TEST_SELF_PID" ] && [ "$3" = "-o" ] && [ "$4" = "stat=" ]; then
  printf 'S\n'
  exit 0
fi
exec "$TEAM_AGENT_TEST_REAL_PS" "$@"
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shim, permissions).unwrap();
    let self_pid = std::process::id().to_string();
    let _ps_env = [
        env.with_env(
            "PATH",
            &format!("{}:{}", shim_dir.display(), real_path.to_string_lossy()),
        ),
        env.with_env(
            "TEAM_AGENT_TEST_PS_RECORDER_LOG",
            recorder.to_str().unwrap(),
        ),
        env.with_env("TEAM_AGENT_TEST_REAL_PS", real_ps.to_str().unwrap()),
        env.with_env("TEAM_AGENT_TEST_SELF_PID", &self_pid),
    ];
    let transport = CohortTransport::with_old_panes(&["developer", "test-engineer", "arch-reviewer"]);
    let before = load_runtime_state(&workspace).unwrap();
    let restart = restart_with_transport_with_readiness_deadline(
        &workspace,
        true,
        Some(TEAM),
        &transport,
        Some(1_000),
    );
    if !matches!(&restart, Ok(RestartReport::Restarted { .. })) {
        failures.push(format!(
            "restart --allow-fresh: report must be Restarted, never readiness timeout; report={restart:?}"
        ));
    }
    collect_lifecycle_cohort_result(
        &mut failures,
        "restart --allow-fresh",
        restart.map(|_| ()),
        &transport,
    );
    let restart_coordinator_pid = fs::read_to_string(coordinator_pid_path(&WorkspacePath::new(
        workspace.clone(),
    )))
    .unwrap_or_default();
    if restart_coordinator_pid.trim() != self_pid {
        failures.push(format!(
            "restart --allow-fresh: seeded coordinator pid must remain the integration-test self pid; expected={self_pid}; actual={restart_coordinator_pid:?}"
        ));
    }
    let after = load_runtime_state(&workspace).unwrap();
    for worker in ["developer", "test-engineer", "arch-reviewer"] {
        let live = transport.live_panes_for_window(worker);
        if live.len() != 1 {
            failures.push(format!(
                "restart --allow-fresh: {worker} must have one live tuple before state comparison; live={live:?}"
            ));
            continue;
        }
        let live = &live[0];
        let saved = &after["agents"][worker];
        let prior = &before["agents"][worker];
        let saved_tuple = (
            saved.get("window").and_then(Value::as_str),
            saved.get("pane_id").and_then(Value::as_str),
            saved.get("pane_pid").and_then(Value::as_u64),
        );
        let live_tuple = (
            live.window_name.as_ref().map(WindowName::as_str),
            Some(live.pane_id.as_str()),
            live.pane_pid.map(u64::from),
        );
        if saved_tuple != live_tuple {
            failures.push(format!(
                "restart --allow-fresh: {worker} saved state tuple must equal its sole live tuple; saved={saved_tuple:?}; live={live_tuple:?}"
            ));
        }
        let prior_pane = prior.get("pane_id").and_then(Value::as_str);
        if saved.get("pane_id").and_then(Value::as_str) == prior_pane {
            failures.push(format!(
                "restart --allow-fresh: {worker} state must not remain bound to retired pane {prior_pane:?}"
            ));
        }
        let prior_epoch = prior.get("spawn_epoch").and_then(Value::as_u64).unwrap_or(0);
        let saved_epoch = saved.get("spawn_epoch").and_then(Value::as_u64).unwrap_or(0);
        if saved_epoch <= prior_epoch {
            failures.push(format!(
                "restart --allow-fresh: {worker} spawn_epoch must increase monotonically; before={prior_epoch}; after={saved_epoch}"
            ));
        }
    }
    let (_env2, workspace2) = team_case("p0-start-force", &["developer"]);
    let transport2 = CohortTransport::with_old_panes(&["developer"]);
    collect_lifecycle_cohort_result(
        &mut failures,
        "start-agent --force",
        start_agent_with_transport(&workspace2, &AgentId::new("developer"), true, false, true, Some(TEAM), &transport2).map(|_| ()),
        &transport2,
    );
    let start_force_coordinator_pid = fs::read_to_string(coordinator_pid_path(
        &WorkspacePath::new(workspace2.clone()),
    ))
    .unwrap_or_default();
    if start_force_coordinator_pid.trim() != self_pid {
        failures.push(format!(
            "start-agent --force: seeded coordinator pid must remain the integration-test self pid; expected={self_pid}; actual={start_force_coordinator_pid:?}"
        ));
    }
    let recorded_ps = fs::read_to_string(&recorder).unwrap_or_default();
    let expected_probe = format!("-p\t{self_pid}\t-o\tstat=");
    if !recorded_ps.lines().any(|line| line == expected_probe) {
        failures.push(format!(
            "fixture ps shim must observe the exact self-pid coordinator health probe {expected_probe:?}; recorded={recorded_ps:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "P0 fixture1: lifecycle fresh/force paths must retire/swap/refuse so each (team_key, agent_id) has exactly one live tuple:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn restart_bad_spawn_identity_fails_closed_before_stale_binding_and_new_orphan() {
    let (_env, workspace) = team_case("p0-bad-spawn-identity", &["developer"]);
    let before = load_runtime_state(&workspace).unwrap();
    let old_pane = before["agents"]["developer"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let old_epoch = before["agents"]["developer"]["spawn_epoch"]
        .as_u64()
        .unwrap();
    let transport = CohortTransport::with_old_panes(&["developer"]).with_bad_spawn_identity();

    let result = restart_with_transport_with_readiness_deadline(
        &workspace,
        true,
        Some(TEAM),
        &transport,
        Some(0),
    );
    let report = format!("{result:?}");
    let report_lower = report.to_ascii_lowercase();
    let refused = !matches!(
        &result,
        Ok(RestartReport::Restarted { .. } | RestartReport::Partial { .. })
    );
    let named_identity_mismatch = report_lower.contains("spawn")
        && (report_lower.contains("identity") || report_lower.contains("binding"))
        && report_lower.contains("mismatch");
    let after = load_runtime_state(&workspace).unwrap();
    let saved = &after["agents"]["developer"];
    let saved_pane = saved.get("pane_id").and_then(Value::as_str);
    let saved_epoch = saved.get("spawn_epoch").and_then(Value::as_u64).unwrap_or(0);
    let old_live = transport
        .has_pane(&PaneId::new(old_pane.as_str()))
        .unwrap()
        .unwrap_or(false);
    let new_live = transport
        .live_panes_for_window("developer")
        .into_iter()
        .filter(|pane| pane.pane_id.as_str().starts_with("%new-"))
        .map(|pane| pane.pane_id.as_str().to_string())
        .collect::<Vec<_>>();
    let stale_third_state = !old_live && saved_pane == Some(old_pane.as_str()) && !new_live.is_empty();
    let old_redeclared_as_new = saved_pane == Some(old_pane.as_str()) && saved_epoch > old_epoch;

    assert!(
        refused && named_identity_mismatch && !stale_third_state && !old_redeclared_as_new,
        "P0 bad-spawn-identity must fail closed with a spawn identity/binding mismatch and must never reach retired-old + state-bound-old + live-new-orphan third state; report={report}; old_live={old_live}; saved_pane={saved_pane:?}; epochs={old_epoch}->{saved_epoch}; new_live={new_live:?}; panes={:?}",
        transport.pane_summary()
    );
}

#[test]
#[serial(env)]
fn codex_capture_rejects_old_rollout_with_new_mtime_and_uses_typed_claim_keys() {
    let env = hermetic_guard::HermeticTestEnv::enter("p0-capture");
    let spawn_cwd = env.workspace("spawn-cwd");
    let rollout = write_old_codex_rollout(env.home(), &spawn_cwd);
    fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap()
        .write_all(b"{\"type\":\"event\",\"payload\":{\"append\":\"after spawn\"}}\n")
        .unwrap();

    let context = CaptureSessionContext {
        agent_id: "developer".to_string(),
        spawn_cwd: spawn_cwd.clone(),
        pane_id: Some("%42".to_string()),
        pane_pid: Some(4242),
        spawned_at: Some("2026-01-02T00:00:00+00:00".to_string()),
        expected_session_id: None,
        provider_projects_root: None,
    };
    let candidates = get_adapter(Provider::Codex)
        .capture_session_candidates(&context, 0)
        .expect("scan codex candidates");

    let capture_src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/provider/session/capture.rs"
    ));
    let mut failures = Vec::new();
    if candidates.iter().any(|candidate| {
        candidate
            .captured
            .rollout_path
            .as_ref()
            .is_some_and(|path| path.as_path() == rollout)
    }) {
        failures.push(format!(
            "old rollout was accepted as a fresh candidate: filename/created_at predate spawn, only mutable mtime is new; candidates={candidates:?}"
        ));
    }
    if !capture_src.contains("SessionAttributionKey")
        || capture_src.contains("format!(\"session:{}\"")
        || capture_src.contains("format!(\"rollout:{}\"")
    {
        failures.push(
            "claimed/captured keys must be typed by provider+team_key+agent_id(+session/rollout), not bare session:/rollout: strings".to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "P0 capture contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn reset_stopped_true_still_rechecks_same_role_residue_before_start() {
    let (_env, workspace) = team_case("p0-reset", &["developer"]);
    let transport = CohortTransport::with_old_panes(&["developer"]);

    let result = reset_agent_with_transport(&workspace, &AgentId::new("developer"), true, false, Some(TEAM), &transport);
    let error_text = result.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(
        error_text.contains("stop_not_proven")
            || error_text.contains("residue")
            || error_text.contains("same-role"),
        "P0 reset: stop.stopped=true is not proof when the same-role pane remains visible; reset must return a named residue proof error (stop_not_proven/residue/same-role) before spawning; result={result:?} error={error_text:?} panes={:?}",
        transport.pane_summary()
    );
}

#[test]
fn mcp_reset_output_exposes_capture_outcome_and_weak_reset_proof() {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/mcp_server/lifecycle_tools/agent_ops.rs"
    ));
    let reset = source_section(src, "pub(crate) fn reset_agent", "pub(crate) fn fork_agent");
    let json_object = reset_success_json_object(reset);
    let missing = ["\"capture_state\":", "\"reset_proof\":", "\"transcript_missing\"", "\"attribution_ambiguous\"", "\"weak\""]
        .into_iter().filter(|needle| !json_object.contains(needle)).collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "P0 MCP reset: ok=true/status=reset must expose additive capture_state and reset_proof=weak inside the ResetAgentOutcome::Reset serde_json::json! object; missing={missing:?}; reset_json={json_object}"
    );
}

#[test]
#[serial(env)]
fn abnormal_shell_pane_with_live_delivery_is_not_dead_and_dedupe_ignores_size() {
    let env = hermetic_guard::HermeticTestEnv::enter("p0-abnormal");
    let workspace = env.workspace("abnormal");
    let rollout = workspace.join("architect-rollout.jsonl");
    fs::write(&rollout, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-arch\",\"cwd\":\"/tmp\"}}\n").unwrap();
    seed_healthy_coordinator(&workspace);
    save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": TEAM,
            "session_name": SESSION,
            "agents": {
                "architect": {
                    "status": "running",
                    "provider": "codex",
                    "agent_id": "architect",
                    "window": "architect",
                    "pane_id": "%old-architect",
                    "session_id": "sess-arch",
                    "rollout_path": rollout.to_string_lossy(),
                    "spawn_epoch": 7,
                    "spawn_cwd": workspace.to_string_lossy()
                }
            }
        }),
    )
    .unwrap();
    let transport = CohortTransport::with_old_panes(&["architect"]).with_current_command("bash");
    transport
        .inject(
            &Target::Pane(PaneId::new("%old-architect")),
            &InjectPayload::Text("ping live tuple".to_string()),
            Key::Enter,
            false,
        )
        .expect("fixture proves the same physical tuple can receive a message");
    let coord = Coordinator::new(
        WorkspacePath::new(workspace.clone()),
        Box::new(RealAdapterRegistry),
        Box::new(transport.clone()),
    );
    let tick = coord.tick();

    let state = load_runtime_state(&workspace).unwrap();
    let watch = state
        .pointer("/coordinator/abnormal_exit_watch/architect")
        .cloned()
        .unwrap_or(Value::Null);
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/coordinator/steps/abnormal.rs"
    ));
    let mut failures = Vec::new();
    if let Err(error) = tick { failures.push(format!("coordinator tick failed before abnormal fixture could be evaluated: {error}")); }
    if watch.is_null() { failures.push("coordinator tick did not write abnormal_exit_watch for the live shell fixture".to_string()); }
    if watch.pointer("/last_liveness").and_then(Value::as_str) == Some("dead")
        || watch
            .pointer("/last_liveness_detail")
            .and_then(Value::as_str)
            .is_some_and(|detail| detail.contains("provider_not_foreground:bash"))
    {
        failures.push(format!(
            "live wrapper shell with no exit marker was classified dead; watch={watch}"
        ));
    }
    for needle in [
        "fn abnormal_dedupe_key(\n    agent: &AbnormalWatchAgent,\n    fact: &crate::provider::FaultFact,\n    size: u64",
        "fn abnormal_suppression_key(\n    agent: &AbnormalWatchAgent,\n    liveness: &ProcessCheck,\n    reason: &str,\n    size: u64",
        "fn abnormal_check_key(\n    agent: &AbnormalWatchAgent,\n    liveness: &ProcessCheck,\n    fact: Option<&crate::provider::FaultFact>,\n    error_recency: ErrorRecency,\n    error_observation_key: Option<&str>,\n    size: u64",
        "unwrap_or_else(|| size.to_string())",
    ] {
        if src.contains(needle) {
            failures.push(format!("abnormal dedupe/check identity still includes mutable rollout size marker `{needle}`"));
        }
    }
    assert!(
        failures.is_empty(),
        "P0 abnormal contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn status_model_uses_effective_codex_source_not_stale_claude_state() {
    let (_env, workspace) = team_case_with_model("p0-status", &[("developer", "gpt-5.6-sol")]);
    let mut state = load_runtime_state(&workspace).unwrap();
    state["agents"]["developer"]["provider"] = json!("codex");
    state["agents"]["developer"]["model"] = json!("claude-opus-4-7");
    save_runtime_state(&workspace, &state).unwrap();

    let status = team_agent::cli::status_port::status(&workspace, false, true)
        .expect("status --json --detail");
    let agent = status.pointer("/agents/developer").unwrap_or(&Value::Null);
    let model = agent.get("model").and_then(Value::as_str);
    let model_stale = agent.get("model_stale").and_then(Value::as_bool) == Some(true);
    let model_source = agent.get("model_source").and_then(Value::as_str);
    assert!(
        model == Some("gpt-5.6-sol") || model_stale || model_source == Some("role"),
        "P0 provider/model projection: Codex seat must not render stale Claude model as truth; agent={agent} status={status}"
    );
}

fn team_case(tag: &str, workers: &[&str]) -> (hermetic_guard::HermeticTestEnv, PathBuf) {
    let pairs = workers.iter().map(|worker| (*worker, "gpt-5")).collect::<Vec<_>>();
    team_case_with_model(tag, &pairs)
}

fn team_case_with_model(
    tag: &str,
    workers: &[(&str, &str)],
) -> (hermetic_guard::HermeticTestEnv, PathBuf) {
    let env = hermetic_guard::HermeticTestEnv::enter(tag);
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace(tag);
    write_team_docs(&workspace, workers);
    write_runtime_spec(&workspace);
    seed_state(&workspace, workers);
    seed_healthy_coordinator(&workspace);
    (env, workspace)
}

fn write_team_docs(workspace: &Path, workers: &[(&str, &str)]) {
    fs::create_dir_all(workspace.join("agents")).unwrap();
    fs::write(workspace.join("TEAM.md"), "---\nname: current\nobjective: P0 host co-tenant contract.\nprovider: codex\n---\n").unwrap();
    for (worker, model) in workers {
        let role = format!("---\nname: {worker}\nrole: {worker}\nprovider: codex\nmodel: {model}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{worker}.\n");
        fs::write(workspace.join("agents").join(format!("{worker}.md")), role).unwrap();
    }
}

fn write_runtime_spec(workspace: &Path) {
    let spec = team_agent::compiler::compile_team(workspace).unwrap();
    let spec_text = team_agent::model::yaml::dumps(&spec);
    fs::write(workspace.join("team.spec.yaml"), &spec_text).unwrap();
    let path = runtime_spec_path(workspace, TEAM);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, spec_text).unwrap();
}

fn seed_state(workspace: &Path, workers: &[(&str, &str)]) {
    let agents = workers
        .iter()
        .map(|(worker, model)| ((*worker).to_string(), json!({
            "status": "running", "provider": "codex", "agent_id": worker, "role": worker,
            "model": model, "auth_mode": "subscription", "tools": ["mcp_team"],
            "window": worker, "pane_id": format!("%old-{worker}"), "pane_pid": 10_000,
            "owner_team_id": TEAM,
            "session_id": format!("sess-{worker}"),
            "rollout_path": workspace.join(format!("{worker}.jsonl")).to_string_lossy(),
            "spawn_epoch": 1, "spawn_cwd": workspace.to_string_lossy()
        })))
        .collect::<serde_json::Map<_, _>>();
    for (worker, _) in workers {
        fs::write(workspace.join(format!("{worker}.jsonl")), "{}\n").unwrap();
    }
    save_runtime_state(workspace, &json!({
        "active_team_key": TEAM, "team_dir": workspace.to_string_lossy(),
        "spec_path": workspace.join("team.spec.yaml").to_string_lossy(),
        "session_name": SESSION, "agents": agents
    })).unwrap();
}

fn seed_healthy_coordinator(workspace: &Path) {
    fs::create_dir_all(runtime_dir(workspace)).unwrap();
    let _ = MessageStore::open(workspace).unwrap();
    let workspace = WorkspacePath::new(workspace.to_path_buf());
    let pid = Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(&workspace, pid, MetadataSource::Boot)
        .unwrap();
    fs::write(coordinator_pid_path(&workspace), pid.to_string()).unwrap();
}

fn write_old_codex_rollout(home: &Path, spawn_cwd: &Path) -> PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&dir).unwrap();
    let rollout = dir.join("rollout-2026-01-01T00-00-00-old.jsonl");
    let meta = json!({"type": "session_meta", "payload": {
        "id": "old-session-before-spawn", "cwd": spawn_cwd.to_string_lossy(),
        "created_at": "2026-01-01T00:00:00+00:00"
    }});
    fs::write(&rollout, format!("{meta}\n{}\n", json!({"type": "event", "payload": {"message": "old rollout"}}))).unwrap();
    rollout
}

#[derive(Clone)]
struct CohortTransport {
    state: Arc<Mutex<TransportState>>,
    bad_spawn_identity: bool,
}

struct TransportState {
    panes: Vec<PaneInfo>,
    killed: BTreeSet<String>,
    next: usize,
    current_command: String,
}

impl CohortTransport {
    fn with_old_panes(workers: &[&str]) -> Self {
        let panes = workers.iter().map(|w| pane_info(w, &format!("%old-{w}"), "team-agent-worker")).collect();
        Self {
            state: Arc::new(Mutex::new(TransportState { panes, killed: BTreeSet::new(), next: 1, current_command: "team-agent-worker".to_string() })),
            bad_spawn_identity: false,
        }
    }

    fn with_bad_spawn_identity(mut self) -> Self {
        self.bad_spawn_identity = true;
        self
    }

    fn with_current_command(self, command: &str) -> Self {
        let mut state = self.state.lock().unwrap();
        state.current_command = command.to_string();
        for pane in &mut state.panes { pane.current_command = Some(command.to_string()); }
        drop(state);
        self
    }

    fn spawn(&self, session: &SessionName, window: &WindowName) -> SpawnResult {
        let mut state = self.state.lock().unwrap();
        let old_identity = state.panes.iter().find(|pane| {
            pane.window_name.as_ref().is_some_and(|name| name == window)
                && pane.pane_id.as_str().starts_with("%old-")
                && !state.killed.contains(pane.pane_id.as_str())
        }).map(|pane| (pane.pane_id.clone(), pane.pane_pid));
        let pane_id = PaneId::new(format!("%new-{}-{}", window.as_str(), state.next));
        let pane_pid = 20_000 + u32::try_from(state.next).unwrap();
        state.next += 1;
        let current_command = state.current_command.clone();
        state.panes.push(pane_info_with_pid(
            window.as_str(),
            pane_id.as_str(),
            current_command.as_str(),
            pane_pid,
        ));
        if self.bad_spawn_identity {
            let (old_pane, old_pid) = old_identity.expect("bad identity fixture needs old pane");
            SpawnResult { pane_id: old_pane, session: session.clone(), window: window.clone(), child_pid: old_pid }
        } else {
            SpawnResult { pane_id, session: session.clone(), window: window.clone(), child_pid: Some(pane_pid) }
        }
    }

    fn pane_summary(&self) -> Vec<String> {
        self.state.lock().unwrap().panes.iter().map(|p| {
            format!("{}:{}", p.window_name.as_ref().map(WindowName::as_str).unwrap_or("-"), p.pane_id.as_str())
        }).collect()
    }

    fn window_counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        let state = self.state.lock().unwrap();
        for pane in state.panes.iter().filter(|pane| !state.killed.contains(pane.pane_id.as_str())) {
            if let Some(window) = pane.window_name.as_ref() { *counts.entry(window.as_str().to_string()).or_insert(0) += 1; }
        }
        counts
    }

    fn live_panes_for_window(&self, window: &str) -> Vec<PaneInfo> {
        let state = self.state.lock().unwrap();
        state.panes.iter().filter(|pane| {
            !state.killed.contains(pane.pane_id.as_str())
                && pane.window_name.as_ref().is_some_and(|name| name.as_str() == window)
        }).cloned().collect()
    }
}

fn collect_lifecycle_cohort_result(
    failures: &mut Vec<String>,
    label: &str,
    result: Result<(), LifecycleError>,
    transport: &CohortTransport,
) {
    let spawned = transport.pane_summary().into_iter().filter(|pane| pane.contains(":%new-")).collect::<Vec<_>>();
    if let Err(error) = result {
        let text = error.to_string();
        if spawned.is_empty()
            && (text.contains("cohort") || text.contains("same-role") || text.contains("duplicate"))
        {
            return;
        }
        failures.push(if spawned.is_empty() {
            format!("{label}: refusal must name cohort/duplicate proof; error={text}")
        } else {
            format!("{label}: refusal happened after spawning {spawned:?}, so it is not a safe pre-spawn cohort/duplicate refusal; error={text}; panes={:?}", transport.pane_summary())
        });
        return;
    }
    let duplicates = transport.window_counts().into_iter().filter(|(_, count)| *count != 1).map(|(window, count)| format!("{window}={count}")).collect::<Vec<_>>();
    if !duplicates.is_empty() {
        failures.push(format!(
            "{label}: duplicate live tuple(s) {duplicates:?}; panes={:?}",
            transport.pane_summary()
        ));
    }
}

impl Transport for CohortTransport {
    fn kind(&self) -> BackendKind { BackendKind::Tmux }

    fn tmux_endpoint(&self) -> Option<String> { Some(TMUX_ENDPOINT.to_string()) }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn(session, window))
    }

    fn spawn_into(&self, session: &SessionName, window: &WindowName, argv: &[String], cwd: &Path, env: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
    }

    fn inject(&self, _target: &Target, _payload: &InjectPayload, _submit: Key, _bracketed: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport { stage_reached: InjectStage::Submit, inject_verification: InjectVerification::CaptureContainsToken, submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck, turn_verification: TurnVerification::NotRequired, attempts: 1, submit_diagnostics: None })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> { Ok(()) }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: "wrapper shell prompt without team-agent provider exit marker".to_string(), range })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneId => Some("%query".to_string()),
            PaneField::SessionName => Some(SESSION.to_string()),
            PaneField::PaneCurrentCommand => Some(self.state.lock().unwrap().current_command.clone()),
            _ => None,
        })
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(if self.has_pane(pane)? == Some(true) { PaneLiveness::Live } else { PaneLiveness::Dead })
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let state = self.state.lock().unwrap();
        Ok(Some(!state.killed.contains(pane.as_str()) && state.panes.iter().any(|item| item.pane_id == *pane)))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> { Ok(self.state.lock().unwrap().panes.clone()) }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> { Ok(true) }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.state.lock().unwrap().panes.iter().filter_map(|pane| pane.window_name.clone()).collect::<BTreeSet<_>>().into_iter().collect())
    }

    fn set_session_env(&self, _session: &SessionName, _key: &str, _value: &str) -> Result<SetEnvOutcome, TransportError> { Ok(SetEnvOutcome::Applied) }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> { Ok(()) }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        if let Target::Pane(pane) = target { self.kill_pane(pane)?; }
        Ok(())
    }

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        self.state.lock().unwrap().killed.insert(pane.as_str().to_string());
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> { Ok(AttachOutcome::Attached) }
}

fn pane_info(window: &str, pane: &str, current_command: &str) -> PaneInfo {
    pane_info_with_pid(window, pane, current_command, 10_000)
}

fn pane_info_with_pid(window: &str, pane: &str, current_command: &str, pane_pid: u32) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane),
        session: SessionName::new(SESSION),
        window_name: Some(WindowName::new(window)),
        window_index: None,
        pane_index: None, tty: None,
        current_command: Some(current_command.to_string()),
        current_path: None, active: true, pane_pid: Some(pane_pid),
        leader_env: BTreeMap::new(),
    }
}

struct RealAdapterRegistry;

impl ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists::default()
    }
}

fn source_section<'a>(src: &'a str, start: &str, end: &str) -> &'a str { let start = src.find(start).unwrap_or(0); let rest = &src[start..]; &rest[..rest.find(end).unwrap_or(rest.len())] }

fn reset_success_json_object(reset_src: &str) -> &str {
    let success = source_section(reset_src, "ResetAgentOutcome::Reset", "ResetAgentOutcome::Refused");
    let macro_start = success.find("serde_json::json!({").expect("reset success json object"); let object_start = macro_start + success[macro_start..].find('{').unwrap(); let mut depth = 0_i32;
    for (offset, ch) in success[object_start..].char_indices() {
        if ch == '{' { depth += 1; } if ch == '}' { depth -= 1; }
        if depth == 0 { return &success[object_start..object_start + offset + 1]; }
    }
    panic!("unterminated reset success json object")
}
