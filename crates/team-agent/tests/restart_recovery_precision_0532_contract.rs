//! 0.5.32 RED contract: restart recovery must not surface stale activity, and
//! old-endpoint-live claim convergence must preserve observed provenance.
//!
//! References:
//! - `.team/artifacts/restart-resumed-stale-activity-locate.md` §5 / §8.
//! - `.team/artifacts/c2-command-internalization-deletion-design.md` §10.
//!
//! User-visible contract:
//! - A new worker process cohort clears old activity/health/current-task facts.
//! - JSONL activity is fresh only if the transcript is newer than `spawned_at`.
//! - Unknown after clearing is not idle; fake READY is not busy; fresh post-spawn activity still works.
//! - A caller observed on the current `$TMUX` endpoint is not a fallback source.
//!
//! Real-machine R1 shape:
//! - A fake team starts with stale `working` / `BUSY` / `agent_health.WORKING`
//!   and an old current task id.
//! - `shutdown --keep-logs` followed by `restart` creates a fresh worker cohort.
//! - `status --json --detail` and human status must not re-surface the old task,
//!   must not classify the fake worker startup READY line as busy, and must not
//!   write `activity.rationale="recent_provider_output"` solely from startup output.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::status::agent_summary_counts;
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;
use team_agent::model::enums::Provider;
use team_agent::provider::{get_adapter, ProviderAdapter};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const TEAM: &str = "current";
const WORKER: &str = "helper";
const CALLER_PANE: &str = "%0";
const WORKER_PANE: &str = "%532";
const OLD_SOCKET: &str = "/private/tmp/tmux-501/ta-0532-old-live";
const TEAM_SESSION: &str = "team-0532-current";
const LEADER_SESSION: &str = "leader-0532";
const LIVE_LEADER_PID: u32 = 53_200;
const WORKER_PID: u32 = 53_201;
const OLD_MESSAGE_ID: &str = "msg_old_0532";

static CASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[serial(env)]
fn restart_clears_stale_working_activity_and_health_after_shutdown_keep_logs() {
    let case = CliRestartCase::new("r1-stale-working");
    case.write_fake_tmux();
    case.write_team_spec();
    case.seed_running_state_with_stale_activity();
    case.seed_agent_health("WORKING", Some(OLD_MESSAGE_ID));

    let shutdown = case.run_ta(&[
        "shutdown",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--keep-logs",
        "--json",
    ]);
    assert!(
        shutdown.status.success() || output_text(&shutdown).contains("session_killed"),
        "R1 setup: shutdown --keep-logs must reach the clean shutdown path before restart; output={}; tmux_log={}",
        output_text(&shutdown),
        case.tmux_log()
    );

    case.seed_healthy_coordinator();
    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, "R1 restart");
    assert!(
        restart.status.success()
            || matches!(
                restart_json.get("status").and_then(Value::as_str),
                Some("restarted" | "started" | "partial")
            ),
        "R1 setup: restart must create a fresh fake-worker cohort before stale fact assertions; output={}; tmux_log={}",
        output_text(&restart),
        case.tmux_log()
    );

    let status = case.run_ta(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
        "--detail",
    ]);
    let status_json = json_output(&status, "R1 status");
    let worker = status_json
        .pointer("/agents/helper")
        .unwrap_or_else(|| panic!("R1 setup: status must include helper; json={status_json}"));

    assert_ne!(
        worker.pointer("/activity/status").and_then(Value::as_str),
        Some("working"),
        "R1: fresh restart must not surface pre-shutdown activity.status=working; worker={worker}; state={}",
        case.read_state()
    );
    assert_ne!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "R1: fresh restart must clear pre-shutdown worker_state=BUSY; worker={worker}; state={}",
        case.read_state()
    );
    assert_ne!(
        worker.get("current_turn_message_id").and_then(Value::as_str),
        Some(OLD_MESSAGE_ID),
        "R1: fresh restart must clear stale current_turn_message_id/current task identity; worker={worker}; state={}",
        case.read_state()
    );

    let health = case.agent_health_row();
    assert!(
        health.as_ref().and_then(|row| row.status.as_deref()) != Some("WORKING")
            && health.as_ref().and_then(|row| row.current_task_id.as_deref()) != Some(OLD_MESSAGE_ID),
        "R1: matching agent_health observation must be cleared on the same spawn boundary; health={health:?}; status_json={status_json}"
    );
    let counts = agent_summary_counts(
        status_json.get("agents").unwrap_or(&Value::Null),
        status_json.get("agent_health").unwrap_or(&Value::Null),
    );
    assert_eq!(
        counts.busy, 0,
        "R1: five-line status summary must not count stale WORKING health as busy after restart; counts={counts:?}; status_json={status_json}"
    );
    assert_eq!(
        worker.get("status").and_then(Value::as_str),
        Some("running"),
        "R1: clearing stale activity must not make the freshly restarted fake worker stopped; worker={worker}; restart={restart_json}"
    );
}

#[test]
#[serial(env)]
fn coordinator_tick_ignores_working_transcript_older_than_spawned_at() {
    let case = TickCase::new("r2-stale-jsonl");
    let rollout = case.workspace.join("stale-rollout.jsonl");
    fs::write(&rollout, codex_open_turn()).expect("write stale codex transcript");
    set_mtime_before_spawned_at(&rollout);
    case.seed_tick_state(json!({
        "status": "running",
        "provider": "codex",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": WORKER_PANE,
        "session_id": "0532-stale-session",
        "rollout_path": rollout.to_string_lossy(),
        "spawned_at": "2026-07-12T12:00:00+00:00",
        "spawn_cwd": case.workspace.to_string_lossy(),
        "process_liveness": "alive"
    }));

    case.coordinator(PrecisionTransport::empty_capture())
        .tick()
        .expect("coordinator tick");
    let state = case.read_state();
    let worker = &state["agents"][WORKER];

    assert_ne!(
        worker.pointer("/activity/status").and_then(Value::as_str),
        Some("working"),
        "R2: a transcript whose mtime is older than spawned_at must not replay Working into a fresh cohort; worker={worker}; state={state}"
    );
    assert_ne!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "R2: stale pre-spawn JSONL must not repopulate worker_state=BUSY; worker={worker}; state={state}"
    );
}

#[test]
#[serial(env)]
fn cleared_activity_stays_unknown_not_idle_until_post_spawn_evidence_exists() {
    let case = TickCase::new("r3-unknown-not-idle");
    case.seed_tick_state(json!({
        "status": "running",
        "provider": "codex",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": WORKER_PANE,
        "session_id": "0532-unknown-session",
        "spawned_at": "2026-07-12T12:00:00+00:00",
        "spawn_cwd": case.workspace.to_string_lossy(),
        "process_liveness": "alive"
    }));

    case.coordinator(PrecisionTransport::empty_capture())
        .tick()
        .expect("coordinator tick");
    let state = case.read_state();
    let worker = &state["agents"][WORKER];
    assert!(
        !matches!(
            worker.get("worker_state").and_then(Value::as_str),
            Some("PROBABLY_IDLE" | "IDLE" | "idle" | "probably_idle")
        ),
        "R3 guard: absence of post-spawn evidence is UNKNOWN/empty, not synthesized idle; worker={worker}; state={state}"
    );
}

#[test]
#[serial(env)]
fn fake_ready_startup_capture_does_not_recreate_busy_after_restart_clear() {
    let case = TickCase::new("r1-ready-capture");
    case.seed_tick_state(fresh_respawned_worker("fake"));

    case.coordinator(PrecisionTransport::with_capture_text(
        "TEAM_AGENT_FAKE_READY agent=helper\n",
    ))
    .tick()
    .expect("coordinator tick");
    let state = case.read_state();
    let worker = &state["agents"][WORKER];
    let health = case.agent_health_row();

    assert_not_busy_from_pane_startup(
        worker,
        health.as_ref(),
        "R1 READY capture: fake worker startup READY is a structural non-busy marker, not recent_provider_output",
        &state,
    );
}

#[test]
#[serial(env)]
fn non_structural_startup_capture_stays_unknown_not_busy_or_idle() {
    let case = TickCase::new("r1-startup-banner");
    case.seed_tick_state(fresh_respawned_worker("codex"));

    case.coordinator(PrecisionTransport::with_capture_text(
        "provider startup banner\n",
    ))
    .tick()
    .expect("coordinator tick");
    let state = case.read_state();
    let worker = &state["agents"][WORKER];
    let health = case.agent_health_row();

    assert_not_busy_from_pane_startup(
        worker,
        health.as_ref(),
        "R1 startup banner: non-structural first capture must stay unknown, not recent_provider_output",
        &state,
    );
    assert!(
        !matches!(
            worker.get("worker_state").and_then(Value::as_str),
            Some("PROBABLY_IDLE" | "IDLE" | "idle" | "probably_idle")
        ),
        "R1 startup banner guard: generic no-signal output is UNKNOWN/empty, not synthesized idle; worker={worker}; state={state}"
    );
}

#[test]
#[serial(env)]
fn structural_working_capture_still_classifies_busy() {
    let case = TickCase::new("r1-structural-working");
    case.seed_tick_state(fresh_respawned_worker("codex"));

    case.coordinator(PrecisionTransport::with_capture_text(
        "tool output\n• Working (5s · esc to interrupt)\n",
    ))
    .tick()
    .expect("coordinator tick");
    let state = case.read_state();
    let worker = &state["agents"][WORKER];
    let health = case.agent_health_row();

    assert_eq!(
        worker.pointer("/activity/status").and_then(Value::as_str),
        Some("working"),
        "R1 guard: structural pane Working signal must still classify as working; worker={worker}; state={state}"
    );
    assert_eq!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "R1 guard: structural pane Working signal must still map to worker_state=BUSY; worker={worker}; state={state}"
    );
    assert_eq!(
        health.as_ref().and_then(|row| row.status.as_deref()),
        Some("WORKING"),
        "R1 guard: structural pane Working signal must still update agent_health.WORKING; health={health:?}; state={state}"
    );
}

#[test]
#[serial(env)]
fn fresh_post_spawn_working_transcript_still_classifies_busy() {
    let case = TickCase::new("r3-fresh-working");
    let rollout = case.workspace.join("fresh-rollout.jsonl");
    fs::write(&rollout, codex_open_turn()).expect("write fresh codex transcript");
    set_mtime_after_spawned_at(&rollout);
    case.seed_tick_state(json!({
        "status": "running",
        "provider": "codex",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": WORKER_PANE,
        "session_id": "0532-fresh-session",
        "rollout_path": rollout.to_string_lossy(),
        "spawned_at": "2026-07-12T12:00:00+00:00",
        "spawn_cwd": case.workspace.to_string_lossy(),
        "process_liveness": "alive"
    }));

    case.coordinator(PrecisionTransport::empty_capture())
        .tick()
        .expect("coordinator tick");
    let state = case.read_state();
    let worker = &state["agents"][WORKER];

    assert_eq!(
        worker.pointer("/activity/status").and_then(Value::as_str),
        Some("working"),
        "R3 guard: fresh post-spawn JSONL Working remains authoritative; worker={worker}; state={state}"
    );
    assert_eq!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "R3 guard: fresh post-spawn Working still maps to worker_state=BUSY; worker={worker}; state={state}"
    );
}

#[test]
#[serial(env)]
fn claim_old_endpoint_live_uses_current_tmux_observed_candidate_source() {
    let case = CliRestartCase::new("r4-old-live-candidate");
    case.write_fake_tmux();
    case.write_team_spec();
    case.seed_old_endpoint_state_with_unrelated_live_server();

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "R4 claim-leader");
    let state = case.read_state();

    assert_eq!(
        claim_json
            .pointer("/topology_convergence/new_tmux_endpoint")
            .and_then(Value::as_str),
        Some(case.new_socket().as_str()),
        "R4 setup: claim must converge endpoint value to the caller's current $TMUX socket; json={claim_json}; state={state}; tmux_log={}",
        case.tmux_log()
    );
    assert_eq!(
        claim_json
            .pointer("/topology_convergence/candidate_source")
            .and_then(Value::as_str),
        Some("observed_target_endpoint"),
        "R4: old endpoint live with only unrelated sessions must still treat the current $TMUX caller pane as an observed_target_endpoint, not fallback_tmux_env; json={claim_json}; tmux_log={}",
        case.tmux_log()
    );
    assert_eq!(
        state
            .pointer("/topology_convergence/candidate_source")
            .and_then(Value::as_str),
        Some("observed_target_endpoint"),
        "R4: persisted topology convergence provenance must match the response; state={state}"
    );
}

struct CliRestartCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
}

impl CliRestartCase {
    fn new(tag: &str) -> Self {
        let seq = CASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(&format!("{tag}-{seq}"));
        fs::create_dir_all(workspace.join(".team/runtime")).expect("create runtime dir");
        fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        let fake_bin = workspace.join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake bin dir");
        Self {
            env,
            workspace,
            fake_bin,
        }
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn new_socket(&self) -> String {
        self.workspace
            .join("ta-0532-new.sock")
            .to_string_lossy()
            .to_string()
    }

    fn write_team_spec(&self) {
        fs::write(
            self.workspace.join("TEAM.md"),
            format!(
                "---\nname: {TEAM}\nobjective: 0.5.32 restart recovery precision.\nprovider: fake\ndisplay_backend: none\n---\n\nTeam.\n"
            ),
        )
        .expect("write TEAM.md");
        fs::write(
            self.workspace.join("agents").join(format!("{WORKER}.md")),
            role_doc(),
        )
        .expect("write role doc");
        let spec = format!(
            r#"version: 1
team:
  name: "{team}"
  mode: "supervisor_worker"
  objective: "0.5.32 restart recovery precision"
  workspace: "{workspace}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  model: null
  tools: []
agents:
  - id: "{worker}"
    role: "helper"
    provider: "fake"
    model: "fake"
    auth_mode: "subscription"
    working_directory: "{workspace}"
    system_prompt:
      inline: "helper"
      file: null
    tools: []
    permission_mode: "restricted"
    preferred_for: []
    avoid_for: []
    output_contract:
      format: "result_envelope_v1"
      required_fields: []
routing:
  default_assignee: "{worker}"
  rules: []
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  ack_timeout_sec: 60
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
    mirror_files: ".team/messages"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "{session}"
  auto_launch: true
  require_user_approval_before_launch: false
  max_active_agents: 1
  startup_order:
    - "{worker}"
  dangerous_auto_approve: false
  fast: false
  tick_interval_sec: 2
  push_min_interval_sec: 60
  stuck_timeout_sec: 300
context:
  state_file: "team_state.md"
  artifact_dir: ".team/artifacts"
  log_dir: ".team/logs"
tasks: []
"#,
            team = TEAM,
            workspace = self.workspace.display(),
            worker = WORKER,
            session = TEAM_SESSION
        );
        fs::write(self.workspace.join("team.spec.yaml"), &spec).expect("write root spec");
        let runtime_spec_dir = self.workspace.join(".team/runtime").join(TEAM);
        fs::create_dir_all(&runtime_spec_dir).expect("create runtime spec dir");
        fs::write(runtime_spec_dir.join("team.spec.yaml"), spec).expect("write runtime spec");
    }

    fn seed_running_state_with_stale_activity(&self) {
        let state = base_state(&self.workspace, &self.new_socket(), stale_busy_worker());
        save_runtime_state(&self.workspace, &state).expect("seed stale activity state");
    }

    fn seed_old_endpoint_state_with_unrelated_live_server(&self) {
        let state = base_state(&self.workspace, OLD_SOCKET, running_worker());
        save_runtime_state(&self.workspace, &state).expect("seed old endpoint state");
    }

    fn seed_agent_health(&self, status: &str, current_task_id: Option<&str>) {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = open_db(store.db_path()).expect("open team db");
        conn.execute(
            "insert or replace into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                TEAM,
                WORKER,
                status,
                "2026-07-12T10:00:00Z",
                42_i64,
                current_task_id,
                "2026-07-12T10:00:00Z"
            ],
        )
        .expect("seed agent_health");
    }

    fn seed_healthy_coordinator(&self) {
        let workspace = team_agent::coordinator::WorkspacePath::new(self.workspace.clone());
        let _ = MessageStore::open(&self.workspace).expect("initialize message store");
        let identity = json!({
            "binary_path": cli_binary_path(),
            "binary_version": env!("CARGO_PKG_VERSION")
        })
        .to_string();
        let _identity = self
            .env
            .with_env("TEAM_AGENT_TEST_CALLER_BINARY_IDENTITY", &identity);
        let pid = team_agent::coordinator::Pid::new(std::process::id());
        team_agent::coordinator::write_coordinator_metadata(
            &workspace,
            pid,
            team_agent::coordinator::MetadataSource::Boot,
        )
        .expect("write coordinator metadata");
        fs::write(
            team_agent::coordinator::coordinator_pid_path(&workspace),
            pid.to_string(),
        )
        .expect("write coordinator pid");
    }

    fn agent_health_row(&self) -> Option<HealthRow> {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = open_db(store.db_path()).expect("open team db");
        conn.query_row(
            "select status, current_task_id from agent_health where owner_team_id = ?1 and agent_id = ?2",
            params![TEAM, WORKER],
            |row| {
                Ok(HealthRow {
                    status: row.get(0)?,
                    current_task_id: row.get(1)?,
                })
            },
        )
        .ok()
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self.env.home())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{},12345,0", self.new_socket()))
            .env("TMUX_PANE", CALLER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0532-red");
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        let output = command.output().expect("run team-agent");
        eprintln!(
            "$ team-agent {}\nexit={}\nstdout={}\nstderr={}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            text(&output.stdout),
            text(&output.stderr)
        );
        output
    }

    fn write_fake_tmux(&self) {
        let tmux = self.fake_bin.join("tmux");
        let new_socket = self.new_socket();
        let caller_line = pane_line(
            CALLER_PANE,
            LEADER_SESSION,
            "leader",
            "codex",
            &self.workspace,
            LIVE_LEADER_PID,
        );
        let worker_line = pane_line(
            WORKER_PANE,
            TEAM_SESSION,
            WORKER,
            "team-agent",
            &self.workspace,
            WORKER_PID,
        );
        let script = format!(
            r#"#!/bin/sh
endpoint="default"
target=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "-S" ] || [ "$previous" = "-L" ]; then
    endpoint="$arg"
  fi
  if [ "$previous" = "-t" ]; then
    target="$arg"
  fi
  previous="$arg"
done
printf '%s	%s\n' "$endpoint" "$*" >> '{log_path}'
is_new=false
case "$endpoint" in
  "{new_socket}") is_new=true ;;
esac
is_old=false
case "$endpoint" in
  "{old_socket}") is_old=true ;;
esac
killed=false
spawned=false
if grep -q " kill-session .*{team_session}" '{log_path}' 2>/dev/null; then
  killed=true
fi
if grep -q " new-session .*{team_session}" '{log_path}' 2>/dev/null || grep -q " new-window .*{team_session}" '{log_path}' 2>/dev/null; then
  spawned=true
fi
session_present=true
if [ "$killed" = "true" ] && [ "$spawned" != "true" ]; then
  session_present=false
fi
case " $* " in
  *" list-sessions "*)
    if [ "$is_old" = "true" ]; then
      printf '%s\n' 'unrelated-session: 1 windows'
      exit 0
    fi
    printf '%s\n' '{leader_session}: 1 windows'
    if [ "$session_present" = "true" ]; then
      printf '%s\n' '{team_session}: 1 windows'
    fi
    exit 0
    ;;
  *" has-session "*)
    if [ "$is_old" = "true" ]; then
      exit 1
    fi
    if [ "$session_present" = "true" ]; then
      exit 0
    fi
    exit 1
    ;;
  *" list-windows "*)
    if [ "$session_present" = "true" ]; then
      printf '%s\n' '0: {worker}'
      exit 0
    fi
    exit 1
    ;;
  *" list-panes "*)
    if [ "$is_old" = "true" ]; then
      exit 0
    fi
    if [ "$is_new" = "true" ]; then
      printf '%s' '{caller_line}'
      if [ "$session_present" = "true" ]; then
        printf '%s' '{worker_line}'
      fi
    fi
    exit 0
    ;;
  *" kill-session "*)
    exit 0
    ;;
  *" new-session "*|*" new-window "*)
    exit 0
    ;;
  *" display-message "*)
    case "$target" in
      %*) printf '%s\n' "$target"; exit 0 ;;
      *"{worker}"*) printf '%s\n' '{worker_pane}'; exit 0 ;;
      *) printf '%s\n' '{caller_pane}'; exit 0 ;;
    esac
    ;;
  *)
    exit 0
    ;;
esac
"#,
            log_path = shell_single_quoted_payload(&self.tmux_log_path().to_string_lossy()),
            new_socket = new_socket,
            old_socket = OLD_SOCKET,
            team_session = TEAM_SESSION,
            leader_session = LEADER_SESSION,
            worker = WORKER,
            caller_line = shell_single_quoted_payload(&caller_line),
            worker_line = shell_single_quoted_payload(&worker_line),
            worker_pane = WORKER_PANE,
            caller_pane = CALLER_PANE,
        );
        fs::write(&tmux, script).expect("write fake tmux");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).expect("chmod tmux");
        }
    }

    fn read_state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("read runtime state")
    }

    fn tmux_log_path(&self) -> PathBuf {
        self.workspace.join("fake-tmux.log")
    }

    fn tmux_log(&self) -> String {
        fs::read_to_string(self.tmux_log_path()).unwrap_or_default()
    }
}

#[derive(Debug)]
struct HealthRow {
    status: Option<String>,
    current_task_id: Option<String>,
}

struct TickCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
}

impl TickCase {
    fn new(tag: &str) -> Self {
        let seq = CASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(&format!("{tag}-{seq}"));
        fs::create_dir_all(workspace.join(".team/runtime")).expect("create runtime dir");
        Self { env, workspace }
    }

    fn seed_tick_state(&self, agent: Value) {
        let _ = &self.env;
        let _ = MessageStore::open(&self.workspace).expect("create message store");
        save_runtime_state(
            &self.workspace,
            &json!({
                "session_name": TEAM_SESSION,
                "active_team_key": TEAM,
                "team_key": TEAM,
                "team_dir": self.workspace.to_string_lossy(),
                "agents": {
                    WORKER: agent
                }
            }),
        )
        .expect("seed tick state");
    }

    fn coordinator(&self, transport: PrecisionTransport) -> Coordinator {
        Coordinator::new(
            WorkspacePath::new(self.workspace.clone()),
            Box::new(RealAdapterRegistry),
            Box::new(transport),
        )
    }

    fn read_state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("read runtime state")
    }

    fn agent_health_row(&self) -> Option<HealthRow> {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = open_db(store.db_path()).expect("open team db");
        conn.query_row(
            "select status, current_task_id from agent_health where owner_team_id = ?1 and agent_id = ?2",
            params![TEAM, WORKER],
            |row| {
                Ok(HealthRow {
                    status: row.get(0)?,
                    current_task_id: row.get(1)?,
                })
            },
        )
        .ok()
    }
}

struct RealAdapterRegistry;

impl ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists {
            whitelist: Vec::new(),
            blacklist: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct PrecisionTransport {
    capture_text: String,
}

impl PrecisionTransport {
    fn empty_capture() -> Self {
        Self {
            capture_text: String::new(),
        }
    }

    fn with_capture_text(text: &str) -> Self {
        Self {
            capture_text: text.to_string(),
        }
    }
}

impl Transport for PrecisionTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult {
            pane_id: PaneId::new(WORKER_PANE),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(WORKER_PID),
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: self.capture_text.clone(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new(WORKER)])
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn base_state(workspace: &Path, endpoint: &str, worker: Value) -> Value {
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": "codex",
        "pane_id": CALLER_PANE,
        "pane_pid": LIVE_LEADER_PID,
        "session_name": LEADER_SESSION,
        "window_name": "leader",
        "tmux_socket": endpoint,
        "leader_session_uuid": "0532-leader",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    });
    json!({
        "active_team_key": TEAM,
        "team_key": TEAM,
        "session_name": TEAM_SESSION,
        "team_dir": workspace.to_string_lossy(),
        "tmux_endpoint": endpoint,
        "tmux_socket": endpoint,
        "tmux_socket_source": "seed",
        "agents": {
            WORKER: worker.clone()
        },
        "leader_receiver": receiver.clone(),
        "teams": {
            TEAM: {
                "active_team_key": TEAM,
                "team_key": TEAM,
                "session_name": TEAM_SESSION,
                "team_dir": workspace.to_string_lossy(),
                "tmux_endpoint": endpoint,
                "tmux_socket": endpoint,
                "tmux_socket_source": "seed",
                "agents": {
                    WORKER: worker
                },
                "leader_receiver": receiver
            }
        }
    })
}

fn stale_busy_worker() -> Value {
    let mut worker = running_worker();
    worker["activity"] = json!({
        "status": "working",
        "confidence": 0.95,
        "rationale": "pre_restart_fixture"
    });
    worker["worker_state"] = json!("BUSY");
    worker["last_output_at"] = json!("2026-07-12T10:00:00Z");
    worker["last_output_hash"] = json!("old-output-hash");
    worker["current_turn_message_id"] = json!(OLD_MESSAGE_ID);
    worker["current_task_id"] = json!(OLD_MESSAGE_ID);
    worker["task_id"] = json!(OLD_MESSAGE_ID);
    worker["coordinator_idle_capture_next_at"] = json!("2026-07-12T10:01:00Z");
    worker["spawn_epoch"] = json!(1);
    worker
}

fn running_worker() -> Value {
    json!({
        "id": WORKER,
        "name": WORKER,
        "provider": "fake",
        "model": "fake",
        "window": WORKER,
        "status": "running",
        "worker_state": "PROBABLY_IDLE",
        "pane_id": WORKER_PANE,
        "pane_pid": WORKER_PID,
        "pid": WORKER_PID,
        "process_started": true,
        "spawned_at": "2026-07-12T10:00:00Z",
        "spawn_cwd": ""
    })
}

fn fresh_respawned_worker(provider: &str) -> Value {
    json!({
        "id": WORKER,
        "name": WORKER,
        "provider": provider,
        "model": provider,
        "window": WORKER,
        "status": "running",
        "pane_id": WORKER_PANE,
        "pane_pid": WORKER_PID,
        "pid": WORKER_PID,
        "process_started": true,
        "session_id": "0532-fresh-session",
        "spawned_at": "2026-07-12T12:00:00+00:00",
        "spawn_cwd": ""
    })
}

fn assert_not_busy_from_pane_startup(
    worker: &Value,
    health: Option<&HealthRow>,
    context: &str,
    state: &Value,
) {
    assert_ne!(
        worker.pointer("/activity/status").and_then(Value::as_str),
        Some("working"),
        "{context}; worker={worker}; health={health:?}; state={state}"
    );
    assert_ne!(
        worker.get("worker_state").and_then(Value::as_str),
        Some("BUSY"),
        "{context}; worker={worker}; health={health:?}; state={state}"
    );
    assert_ne!(
        worker
            .pointer("/activity/rationale")
            .and_then(Value::as_str),
        Some("recent_provider_output"),
        "{context}; worker={worker}; health={health:?}; state={state}"
    );
    assert_ne!(
        health.and_then(|row| row.status.as_deref()),
        Some("WORKING"),
        "{context}; worker={worker}; health={health:?}; state={state}"
    );
    assert!(
        worker.get("current_task_id").is_none()
            && worker.get("current_turn_message_id").is_none()
            && health.and_then(|row| row.current_task_id.as_deref()).is_none(),
        "{context}: fresh startup capture must not invent or retain current task identity; worker={worker}; health={health:?}; state={state}"
    );
}

fn role_doc() -> String {
    format!(
        "---\nname: {WORKER}\nrole: Fake helper\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nFake helper.\n"
    )
}

fn codex_open_turn() -> &'static str {
    "{\"jsonrpc\":\"2.0\",\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"ct-0532\",\"status\":\"inProgress\"}}}\n"
}

fn set_mtime_before_spawned_at(path: &Path) {
    set_mtime(path, "202607121159.00");
}

fn set_mtime_after_spawned_at(path: &Path) {
    set_mtime(path, "202607121201.00");
}

fn set_mtime(path: &Path, stamp: &str) {
    let status = Command::new("touch")
        .args(["-t", stamp])
        .arg(path)
        .status()
        .expect("run touch -t");
    assert!(
        status.success(),
        "touch -t {stamp} {} must succeed",
        path.display()
    );
}

fn cli_binary_path() -> String {
    fs::canonicalize(env!("CARGO_BIN_EXE_team-agent"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_team-agent")))
        .to_string_lossy()
        .to_string()
}

fn pane_line(
    pane: &str,
    session: &str,
    window: &str,
    command: &str,
    cwd: &Path,
    pid: u32,
) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0532\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn json_output(output: &Output, label: &str) -> Value {
    let stdout = text(&output.stdout);
    let start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "{label}: stdout must contain JSON object; code={:?} stdout={stdout:?} stderr={:?}",
            output.status.code(),
            text(&output.stderr)
        )
    });
    let end = stdout.rfind('}').expect("stdout JSON object end");
    serde_json::from_str(&stdout[start..=end]).unwrap_or_else(|error| {
        panic!(
            "{label}: parse JSON failed: {error}; stdout={stdout:?} stderr={:?}",
            text(&output.stderr)
        )
    })
}

fn output_text(output: &Output) -> String {
    format!(
        "code={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}
