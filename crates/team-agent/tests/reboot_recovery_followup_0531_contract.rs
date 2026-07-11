//! 0.5.31 RED contract: follow-up locks for reboot tmux recovery.
//!
//! References:
//! - `.team/artifacts/0529-casea-restart-converge-triage.md` §6.1-§6.3.
//! - Gate evidence: 0.5.29 Case A physical rerun and Case C human status gap.
//!
//! User-visible contract:
//! - Claim uses the current caller tmux endpoint as an observed candidate, not
//!   a fallback.
//! - Fake/no-spawn restart bypasses are test-only and must not look physical.
//! - Human status renders the same restart hint JSON status already exposes.
//! - Restart after endpoint convergence creates the team on the selected socket.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

const TEAM: &str = "current";
const WORKER: &str = "worker";
const CALLER_PANE: &str = "%0";
const SPAWNED_WORKER_PANE: &str = "%31";
const OLD_SOCKET: &str = "/private/tmp/tmux-501/ta-0531-old";
const TEAM_SESSION: &str = "team-0531-current";
const LEADER_SESSION: &str = "leader-0531-recovery";
const LIVE_LEADER_PID: u32 = 53_100;
const OLD_WORKER_PID: u32 = 53_108;

#[test]
#[serial(env)]
fn claim_uses_current_tmux_endpoint_as_observed_candidate() {
    let case = Recovery0531Case::new("r1-observed-caller", ProviderShape::Fake)
        .with_team_session_mode(TeamSessionMode::Missing);
    case.seed_stale_old_endpoint_state("fake");
    case.write_team_spec("fake", "fake");

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "R1 claim-leader");
    let state = case.read_state();
    let observed = case.new_socket();

    assert_eq!(
        claim_json
            .pointer("/topology_convergence/new_tmux_endpoint")
            .and_then(Value::as_str),
        Some(observed.as_str()),
        "R1: claim must converge to the current $TMUX endpoint that actually hosts the caller pane; json={claim_json} state={state} tmux_log={}",
        case.tmux_log()
    );
    assert_eq!(
        claim_json
            .pointer("/topology_convergence/candidate_source")
            .and_then(Value::as_str),
        Some("observed_target_endpoint"),
        "R1: current $TMUX endpoint that actually lists the caller pane must be a first-class observed_target_endpoint, not fallback_tmux_env; json={claim_json} tmux_log={}",
        case.tmux_log()
    );
    assert_eq!(
        state.pointer("/tmux_endpoint").and_then(Value::as_str),
        Some(observed.as_str()),
        "R1: root tmux_endpoint must persist the observed caller endpoint; state={state}"
    );
    assert_eq!(
        state
            .pointer("/teams/current/leader_receiver/tmux_socket")
            .and_then(Value::as_str),
        Some(observed.as_str()),
        "R1: canonical nested leader_receiver must persist the observed caller endpoint; state={state}"
    );
    assert!(
        state.pointer("/leader_receiver").is_none(),
        "R1 guard: Stage3 canonical-only save must not reintroduce raw root leader_receiver; state={state}"
    );
}

#[test]
#[serial(env)]
fn fake_no_spawn_restart_bypass_requires_explicit_test_gate() {
    let case = Recovery0531Case::new("r2-fake-bypass", ProviderShape::Fake)
        .with_team_session_mode(TeamSessionMode::PresentWithoutSpawn)
        .without_test_env_for_child();
    case.seed_converged_state("fake");
    case.write_team_spec("fake", "fake");
    case.seed_healthy_coordinator();

    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let events = restart_spawn_events(&case);
    let fake_events = events
        .iter()
        .filter(|event| {
            event.get("tmux_start_mode").and_then(Value::as_str) == Some("fake_harness")
                || event
                    .get("argv")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        })
        .collect::<Vec<_>>();

    assert!(
        fake_events.is_empty(),
        "R2: production-like restart with convergence marker + fake provider must not silently take fake no-spawn bypass without an explicit test-only gate; restart={}; fake_events={fake_events:?}; state={}; tmux_log={}",
        output_text(&restart),
        case.read_state(),
        case.tmux_log()
    );
    let state = case.read_state();
    assert!(
        !agent_pane_id(&state).is_some_and(|pane| pane.starts_with("__team_agent_fake_harness_")),
        "R2: readiness must not record a harness sentinel as a physical addressable worker pane in production-like env; state={state}; restart={}",
        output_text(&restart)
    );
}

#[test]
#[serial(env)]
fn human_status_detail_renders_session_missing_restart_hint() {
    let case = Recovery0531Case::new("r3-human-status-hint", ProviderShape::Fake)
        .with_team_session_mode(TeamSessionMode::Missing);
    case.seed_stale_old_endpoint_state("fake");
    case.write_team_spec("fake", "fake");

    let status = case.run_ta(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--detail",
    ]);
    let text = output_text(&status);

    assert!(
        text.contains("tmux session missing") && text.contains("team-agent restart"),
        "R3: human `status --detail` must render the same runtime.hint JSON exposes when the team tmux session is missing; output={text}; tmux_log={}",
        case.tmux_log()
    );
}

#[test]
#[serial(env)]
fn restart_after_convergence_uses_physical_spawn_on_selected_endpoint() {
    let case = Recovery0531Case::new("r4-physical-spawn", ProviderShape::Codex)
        .with_team_session_mode(TeamSessionMode::SpawnTracked);
    case.seed_converged_state("codex");
    case.write_team_spec("codex", "gpt-5");
    case.seed_healthy_coordinator();

    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, "R4 restart");
    let events = restart_spawn_events(&case);
    let spawn_event = events
        .iter()
        .find(|event| event.get("agent_id").and_then(Value::as_str) == Some(WORKER))
        .unwrap_or_else(|| {
            panic!(
                "R4: missing provider.worker.spawn_argv for {WORKER}; events={events:?}; output={}",
                output_text(&restart)
            )
        });

    assert_eq!(
        restart_json.get("status").and_then(Value::as_str),
        Some("restarted"),
        "R4 setup: restart must complete before physical endpoint assertions; output={}; tmux_log={}",
        output_text(&restart),
        case.tmux_log()
    );
    assert!(
        !spawn_event
            .get("argv")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "R4: physical restart spawn event must carry non-empty provider argv; event={spawn_event}; events={events:?}"
    );
    assert_ne!(
        spawn_event.get("tmux_start_mode").and_then(Value::as_str),
        Some("fake_harness"),
        "R4: physical restart must not regress to fake_harness mode; event={spawn_event}"
    );
    assert_eq!(
        spawn_event.get("tmux_endpoint").and_then(Value::as_str),
        Some(case.new_socket().as_str()),
        "R4: spawn metadata must name the selected converged endpoint; event={spawn_event}"
    );
    assert!(
        case.tmux_log()
            .lines()
            .any(|line| line.starts_with(&format!("{}\t", case.new_socket()))
                && line.contains(" new-session ")
                && line.contains(TEAM_SESSION)),
        "R4: restart must physically create the team session on the new selected socket; tmux_log={}",
        case.tmux_log()
    );
    assert!(
        !case
            .tmux_log()
            .lines()
            .any(|line| line.starts_with(&format!("{OLD_SOCKET}\t"))
                && (line.contains(" new-session ") || line.contains(" new-window "))
                && line.contains(TEAM_SESSION)),
        "R4: restart must not create the team session on the stale old socket; tmux_log={}",
        case.tmux_log()
    );
    assert_attach_commands_use_new_endpoint(&restart_json, &case);
}

#[derive(Clone, Copy)]
enum ProviderShape {
    Fake,
    Codex,
}

#[derive(Clone, Copy)]
enum TeamSessionMode {
    Missing,
    PresentWithoutSpawn,
    SpawnTracked,
}

struct Recovery0531Case {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
    team_session_mode: TeamSessionMode,
    provider_shape: ProviderShape,
    inherit_test_env: bool,
}

impl Recovery0531Case {
    fn new(tag: &str, provider_shape: ProviderShape) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        fs::create_dir_all(workspace.join(".team/runtime")).expect("create runtime dir");
        fs::create_dir_all(workspace.join("home")).expect("create home dir");
        let fake_bin = workspace.join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake-bin dir");
        Self {
            env,
            workspace,
            fake_bin,
            team_session_mode: TeamSessionMode::Missing,
            provider_shape,
            inherit_test_env: true,
        }
    }

    fn with_team_session_mode(mut self, mode: TeamSessionMode) -> Self {
        self.team_session_mode = mode;
        self
    }

    fn without_test_env_for_child(mut self) -> Self {
        self.inherit_test_env = false;
        self
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace path is utf8")
    }

    fn new_socket(&self) -> String {
        self.workspace
            .join("ta-0531-new.sock")
            .to_string_lossy()
            .to_string()
    }

    fn seed_stale_old_endpoint_state(&self, provider: &str) {
        self.seed_state(provider, OLD_SOCKET, None);
    }

    fn seed_converged_state(&self, provider: &str) {
        self.seed_state(
            provider,
            &self.new_socket(),
            Some(json!({
                "status": "converged",
                "reason": "old_team_session_absent_on_live_endpoint",
                "old_tmux_endpoint": OLD_SOCKET,
                "new_tmux_endpoint": self.new_socket(),
                "candidate_source": "observed_target_endpoint",
                "persisted": true
            })),
        );
    }

    fn seed_state(&self, provider: &str, endpoint: &str, convergence: Option<Value>) {
        let worker = json!({
            "id": WORKER,
            "name": WORKER,
            "provider": provider,
            "model": if provider == "fake" { "fake" } else { "gpt-5" },
            "window": WORKER,
            "status": "stopped",
            "worker_state": "DEAD",
            "pane_id": "",
            "process_started": false
        });
        let receiver = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": CALLER_PANE,
            "pane_pid": LIVE_LEADER_PID,
            "session_name": LEADER_SESSION,
            "window_name": "leader",
            "tmux_socket": endpoint,
            "leader_session_uuid": "0531-recovery-leader",
            "owner_epoch": 7,
            "claimed_via": "claim-leader"
        });
        let mut state = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": self.workspace_str(),
            "tmux_endpoint": endpoint,
            "tmux_socket": endpoint,
            "tmux_socket_source": "seed",
            "agents": {
                WORKER: worker.clone()
            },
            "teams": {
                TEAM: {
                    "active_team_key": TEAM,
                    "team_key": TEAM,
                    "session_name": TEAM_SESSION,
                    "team_dir": self.workspace_str(),
                    "tmux_endpoint": endpoint,
                    "tmux_socket": endpoint,
                    "tmux_socket_source": "seed",
                    "agents": {
                        WORKER: worker
                    },
                    "leader_receiver": receiver.clone()
                }
            }
        });
        if let Some(convergence) = convergence {
            state["topology_convergence"] = convergence.clone();
            state["teams"][TEAM]["topology_convergence"] = convergence;
        }
        save_runtime_state(&self.workspace, &state).expect("seed runtime state");
    }

    fn write_team_spec(&self, provider: &str, model: &str) {
        fs::write(
            self.workspace.join("TEAM.md"),
            format!(
                "---\nname: {team}\nobjective: 0.5.31 reboot recovery follow-up contract.\nprovider: {provider}\ndisplay_backend: none\n---\n\nTeam.\n",
                team = TEAM,
                provider = provider
            ),
        )
        .expect("write source TEAM.md");
        let agents_dir = self.workspace.join("agents");
        fs::create_dir_all(&agents_dir).expect("create source agents dir");
        fs::write(
            agents_dir.join(format!("{WORKER}.md")),
            role_doc(WORKER, provider, model),
        )
        .expect("write source worker role");

        let spec = format!(
            r#"version: 1
team:
  name: "{team}"
  mode: "supervisor_worker"
  objective: "0.5.31 reboot recovery follow-up contract"
  workspace: "{workspace}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  model: null
  tools: []
agents:
  - id: "{worker}"
    role: "worker"
    provider: "{provider}"
    model: "{model}"
    auth_mode: "subscription"
    working_directory: "{workspace}"
    system_prompt:
      inline: "worker"
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
            provider = provider,
            model = model,
            session = TEAM_SESSION
        );
        fs::write(self.workspace.join("team.spec.yaml"), &spec).expect("write root spec");
        let runtime_spec_dir = self.workspace.join(".team/runtime").join(TEAM);
        fs::create_dir_all(&runtime_spec_dir).expect("create runtime spec dir");
        fs::write(runtime_spec_dir.join("team.spec.yaml"), spec).expect("write runtime spec");
    }

    fn seed_healthy_coordinator(&self) {
        let workspace = team_agent::coordinator::WorkspacePath::new(self.workspace.clone());
        let _ = team_agent::message_store::MessageStore::open(&self.workspace)
            .expect("initialize message store");
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

    fn run_ta(&self, args: &[&str]) -> Output {
        self.write_fake_tmux();
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
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0531-red");
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
        if !self.inherit_test_env {
            for key in [
                "TEAM_AGENT_TEST_TMP",
                "TEAM_AGENT_KEEP_TEST_TMP",
                "TEAM_AGENT_TEST_ENDPOINT_CONVERGENCE_HARNESS_SPEC_FALLBACK",
            ] {
                command.env_remove(key);
            }
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
        let log_path = self.tmux_log_path();
        let caller_line = pane_line(
            CALLER_PANE,
            LEADER_SESSION,
            "leader",
            "codex",
            &self.workspace,
            LIVE_LEADER_PID,
        );
        let worker_line = pane_line(
            SPAWNED_WORKER_PANE,
            TEAM_SESSION,
            WORKER,
            provider_command(self.provider_shape),
            &self.workspace,
            OLD_WORKER_PID,
        );
        let team_session_mode = match self.team_session_mode {
            TeamSessionMode::Missing => "missing",
            TeamSessionMode::PresentWithoutSpawn => "present",
            TeamSessionMode::SpawnTracked => "spawn_tracked",
        };
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
spawned=false
if grep -q " new-session .*{team_session}" '{log_path}' 2>/dev/null || grep -q " new-window .*{team_session}" '{log_path}' 2>/dev/null; then
  spawned=true
fi
is_new=false
case "$endpoint" in
  "{new_socket}") is_new=true ;;
esac
is_old=false
case "$endpoint" in
  "{old_socket}") is_old=true ;;
esac
case " $* " in
  *" list-panes "*)
    if [ "$is_new" = "true" ]; then
      printf '%s' '{caller_line}'
      if [ "$spawned" = "true" ]; then
        printf '%s' '{worker_line}'
      fi
    fi
    exit 0
    ;;
  *" list-sessions "*)
    if [ "$is_old" = "true" ]; then
      echo "no server running on old endpoint" >&2
      exit 1
    fi
    if [ "$is_new" = "true" ]; then
      printf '%s\n' '{leader_session}: 1 windows'
      if [ "{team_session_mode}" = "present" ] || [ "$spawned" = "true" ]; then
        printf '%s\n' '{team_session}: 1 windows'
      fi
      exit 0
    fi
    exit 1
    ;;
  *" list-windows "*)
    if [ "{team_session_mode}" = "present" ] || [ "$spawned" = "true" ]; then
      printf '%s\n' '0: {worker}'
      exit 0
    fi
    exit 1
    ;;
  *" has-session "*)
    if [ "$is_old" = "true" ]; then
      exit 1
    fi
    if [ "{team_session_mode}" = "present" ] || [ "$spawned" = "true" ]; then
      exit 0
    fi
    exit 1
    ;;
  *" new-session "*|*" new-window "*)
    exit 0
    ;;
  *" display-message "*)
    case "$target" in
      %*) printf '%s\n' "$target"; exit 0 ;;
      *"{worker}"*) printf '%s\n' '{spawned_worker_pane}'; exit 0 ;;
      *) printf '%s\n' '{caller_pane}'; exit 0 ;;
    esac
    ;;
  *)
    exit 0
    ;;
esac
"#,
            log_path = shell_single_quoted_payload(&log_path.to_string_lossy()),
            new_socket = new_socket,
            old_socket = OLD_SOCKET,
            caller_line = shell_single_quoted_payload(&caller_line),
            worker_line = shell_single_quoted_payload(&worker_line),
            leader_session = LEADER_SESSION,
            team_session = TEAM_SESSION,
            team_session_mode = team_session_mode,
            worker = WORKER,
            spawned_worker_pane = SPAWNED_WORKER_PANE,
            caller_pane = CALLER_PANE,
        );
        fs::write(&tmux, script).expect("write fake tmux");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).expect("chmod fake tmux");
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

fn provider_command(provider: ProviderShape) -> &'static str {
    match provider {
        ProviderShape::Fake => "fake",
        ProviderShape::Codex => "codex",
    }
}

fn role_doc(id: &str, provider: &str, model: &str) -> String {
    format!(
        "---\nname: {id}\nrole: Worker {id}\nprovider: {provider}\nmodel: {model}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker {id}.\n"
    )
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
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0531\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn restart_spawn_events(case: &Recovery0531Case) -> Vec<Value> {
    events(&case.workspace)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event.get("event").and_then(Value::as_str) == Some("provider.worker.spawn_argv")
                && event.get("source").and_then(Value::as_str) == Some("restart")
        })
        .collect()
}

fn assert_attach_commands_use_new_endpoint(value: &Value, case: &Recovery0531Case) {
    let commands = value
        .get("attach_commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        commands.iter().any(|command| command.contains(&case.new_socket())),
        "R4: attach_commands must point at the converged new endpoint; commands={commands:?}; json={value}"
    );
    assert!(
        commands.iter().all(|command| !command.contains(OLD_SOCKET)),
        "R4: attach_commands must not point at OLD_SOCKET={OLD_SOCKET}; commands={commands:?}; json={value}"
    );
}

fn agent_pane_id(state: &Value) -> Option<&str> {
    state
        .pointer("/agents/worker/pane_id")
        .and_then(Value::as_str)
        .or_else(|| {
            state
                .pointer("/teams/current/agents/worker/pane_id")
                .and_then(Value::as_str)
        })
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

fn events(workspace: &Path) -> String {
    fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}
