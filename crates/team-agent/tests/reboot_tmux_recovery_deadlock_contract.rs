//! 0.5.29 RED contract: reboot / tmux-server-death recovery must not deadlock.
//!
//! References:
//! - `.team/artifacts/reboot-tmux-recovery-deadlock-locate.md` §10.1 RED 1-8.
//! - Non-goals / hard lines: locate §7 and §11. Restart dirty-topology gates
//!   stay fail-closed; recovery exits must make claim/takeover/status truthful.
//!
//! User-visible contract:
//! - A live old tmux server is not proof that the old team is still live.
//! - `claim-leader` uses the endpoint that actually observed the caller pane.
//! - Explicit retained-team recovery remains reachable after every old pane is
//!   dead.
//! - `status` must not count stale post-reboot workers as running.

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
use team_agent::cli::status::agent_summary_counts;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::topology::{endpoint_convergence_decision, EndpointConvergenceDecision};

const TEAM: &str = "current";
const RETAINED_TEAM: &str = "research";
const WORKER: &str = "worker";
const CALLER_PANE: &str = "%0";
const OLD_WORKER_PANE: &str = "%8";
const OLD_SOCKET: &str = "/private/tmp/tmux-501/ta-0529-old";
const DEAD_ENV_SOCKET: &str = "/private/tmp/tmux-501/ta-0529-env-dead";
const TEAM_SESSION: &str = "team-0529-current";
const RETAINED_SESSION: &str = "team-0529-research";
const LEADER_SESSION: &str = "leader-0529-recovery";
const LIVE_LEADER_PID: u32 = 52_900;
const OLD_WORKER_PID: u32 = 52_908;

#[test]
#[serial(env)]
fn endpoint_convergence_allows_old_server_live_without_team_session() {
    let case = RecoveryCase::new("old-server-unrelated")
        .with_old_server_live(true)
        .with_old_team_session(false)
        .with_old_team_tuple(false);
    let _path = case.enter_fake_tmux_path();

    let decision =
        endpoint_convergence_decision(&case.single_team_state(), TEAM, case.new_socket());
    let debug = format!("{decision:?}");
    assert!(
        matches!(decision, EndpointConvergenceDecision::Converge { .. })
            && debug.contains("old_team_session_absent_on_live_endpoint"),
        "RED1: old endpoint server live with no {TEAM_SESSION} session and no matching team tuple must converge with reason=old_team_session_absent_on_live_endpoint, not treat the whole tmux server as authoritative. decision={debug} log={}",
        case.tmux_log()
    );
}

#[test]
#[serial(env)]
fn endpoint_convergence_refuses_old_endpoint_with_target_session() {
    let case = RecoveryCase::new("old-session-live")
        .with_old_server_live(true)
        .with_old_team_session(true)
        .with_old_team_tuple(false);
    let _path = case.enter_fake_tmux_path();

    let decision =
        endpoint_convergence_decision(&case.single_team_state(), TEAM, case.new_socket());
    let debug = format!("{decision:?}");
    assert!(
        matches!(
            decision,
            EndpointConvergenceDecision::RefuseLiveOldEndpoint { .. }
        ) && (debug.contains("old_team_session_live")
            || debug.contains("target_session")
            || debug.contains(TEAM_SESSION)),
        "RED2 guard: if the old endpoint still has this team's session, convergence must refuse and name the team-session reason rather than telling the user to clean a whole unrelated socket. decision={debug} log={}",
        case.tmux_log()
    );
}

#[test]
#[serial(env)]
fn endpoint_convergence_refuses_old_endpoint_with_live_team_tuple() {
    let case = RecoveryCase::new("old-tuple-live")
        .with_old_server_live(true)
        .with_old_team_session(false)
        .with_old_team_tuple(true);
    let _path = case.enter_fake_tmux_path();

    let decision =
        endpoint_convergence_decision(&case.single_team_state(), TEAM, case.new_socket());
    let debug = format!("{decision:?}");
    assert!(
        matches!(
            decision,
            EndpointConvergenceDecision::RefuseLiveOldEndpoint { .. }
        ) && (debug.contains("old_team_tuple_live")
            || debug.contains(OLD_WORKER_PANE)
            || debug.contains(&OLD_WORKER_PID.to_string())),
        "RED3 guard: even if has-session is false, a live endpoint/session/window/pane/pid tuple for this team must still refuse convergence; no server-wide blanket allow. decision={debug} log={}",
        case.tmux_log()
    );
}

#[test]
#[serial(env)]
fn claim_uses_observed_target_endpoint_not_state_or_tmux_env() {
    let case = RecoveryCase::new("claim-observed-endpoint")
        .with_old_server_live(false)
        .with_tmux_env_socket(DEAD_ENV_SOCKET);
    case.seed_split_state(TEAM, TEAM_SESSION, OLD_SOCKET, OLD_SOCKET, true);
    case.write_minimal_team_spec(TEAM, TEAM_SESSION);

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED4 claim-leader");
    let observed = case.observed_caller_endpoint();
    let state = case.read_state();

    assert_eq!(
        claim_json.pointer("/topology_convergence/new_tmux_endpoint").and_then(Value::as_str),
        Some(observed.as_str()),
        "RED4: claim-leader must use the endpoint whose list-targets snapshot observed the caller pane, not stale state.tmux_socket or $TMUX. observed={observed} json={claim_json} state={state} log={}",
        case.tmux_log()
    );
    assert_eq!(
        state.pointer("/leader_receiver/tmux_socket").and_then(Value::as_str),
        Some(observed.as_str()),
        "RED4: persisted leader_receiver.tmux_socket must be the observed target endpoint. observed={observed} state={state}"
    );
    assert_ne!(
        observed, OLD_SOCKET,
        "RED4 setup guard: observed target endpoint must differ from stale state endpoint"
    );
    assert_ne!(
        observed, DEAD_ENV_SOCKET,
        "RED4 setup guard: observed target endpoint must differ from stale $TMUX endpoint"
    );
}

#[test]
#[serial(env)]
fn scoped_claim_persists_convergence_to_restart_selected_state() {
    let case = RecoveryCase::new("scoped-claim-persist")
        .with_old_server_live(false)
        .with_tmux_env_socket(case_new_env_socket());
    case.seed_multi_team_state();
    case.write_minimal_team_spec(RETAINED_TEAM, RETAINED_SESSION);

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        RETAINED_TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED5 claim retained team");
    let state = case.read_state();
    let observed = case.observed_caller_endpoint();

    assert_eq!(
        claim_json.pointer("/ok").and_then(Value::as_bool),
        Some(true),
        "RED5 setup: explicit non-active retained-team claim should complete before persistence assertions; json={claim_json} stdout={} stderr={}",
        text(&claim.stdout),
        text(&claim.stderr)
    );
    assert_eq!(
        claim_json.pointer("/topology_convergence/status").and_then(Value::as_str),
        Some("converged"),
        "RED5: scoped claim must report converged, not persistence_conflict; json={claim_json} state={state}"
    );
    for pointer in [
        "/tmux_endpoint",
        "/tmux_socket",
        "/teams/research/tmux_endpoint",
        "/teams/research/tmux_socket",
        "/topology_convergence/new_tmux_endpoint",
        "/teams/research/topology_convergence/new_tmux_endpoint",
    ] {
        assert_eq!(
            state.pointer(pointer).and_then(Value::as_str),
            Some(observed.as_str()),
            "RED5: explicit non-active claim must persist the same new endpoint to root and teams.research; pointer={pointer} observed={observed} state={state}"
        );
    }
}

#[test]
#[serial(env)]
fn claim_leader_explicit_team_resolves_shutdown_or_all_dead_team() {
    let case = RecoveryCase::new("all-dead-retained-alias")
        .with_old_server_live(false)
        .with_tmux_env_socket(case_new_env_socket());
    case.seed_all_dead_retained_alias_state();
    case.write_minimal_team_spec(RETAINED_TEAM, RETAINED_SESSION);

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        "retained-research-alias",
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED6 claim retained alias");
    assert_ne!(
        claim_json.get("reason").and_then(Value::as_str),
        Some("team_target_unresolved"),
        "RED6: explicit --team that uniquely aliases a retained all-dead team must reach the recovery bind path, not die in the CLI exact-only resolver. json={claim_json}"
    );
    assert!(
        claim_json.pointer("/resolved_team").and_then(Value::as_str) == Some(RETAINED_TEAM)
            || claim_json.pointer("/team").and_then(Value::as_str) == Some(RETAINED_TEAM)
            || claim_json.pointer("/owner_team_id").and_then(Value::as_str) == Some(RETAINED_TEAM)
            || claim_json.pointer("/ok").and_then(Value::as_bool) == Some(true),
        "RED6: response should make the retained canonical team visible after alias resolution; json={claim_json}"
    );
}

#[test]
#[serial(env)]
fn status_session_missing_downgrades_running_agents() {
    let case = RecoveryCase::new("status-stale-running")
        .with_status_has_session(false)
        .with_old_server_live(true);
    case.seed_status_state();

    let status = case.run_ta(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
        "--detail",
    ]);
    let status_json = json_output(&status, "RED7 status --json --detail");
    let worker = status_json
        .pointer("/agents/worker")
        .unwrap_or_else(|| panic!("RED7 setup: status must include worker; json={status_json}"));

    assert_eq!(
        status_json
            .get("tmux_session_present")
            .and_then(Value::as_bool),
        Some(false),
        "RED7 setup: fake transport must model a missing tmux session; json={status_json} log={}",
        case.tmux_log()
    );
    assert_eq!(
        worker.get("stale").and_then(Value::as_bool),
        Some(true),
        "RED7 setup: stale marker should still be present; worker={worker}"
    );
    assert_ne!(
        worker.get("status").and_then(Value::as_str),
        Some("running"),
        "RED7: session-missing worker with cached pane/process facts must be product-visible non-running, not raw running plus a diagnostic footnote. worker={worker} json={status_json}"
    );
    assert!(
        !matches!(
            worker.get("worker_state").and_then(Value::as_str),
            Some("RUNNING" | "BUSY" | "PROBABLY_IDLE" | "running" | "busy")
        ),
        "RED7: worker_state must also degrade when the tmux session is missing; worker={worker}"
    );
}

#[test]
fn status_summary_counts_ignore_stale_running() {
    let agents = json!({
        "worker": {
            "status": "running",
            "worker_state": "PROBABLY_IDLE",
            "stale": true,
            "stale_reason": "tmux_session_missing"
        }
    });
    let counts = agent_summary_counts(&agents, &json!({}));
    assert_eq!(
        counts.running, 0,
        "RED8: summary counts must consume stale/stale_reason first, so stale running agents do not inflate running count. counts={counts:?}"
    );
    assert!(
        counts.stopped + counts.unknown >= 1,
        "RED8: stale running must be counted as stopped or unknown, never running. counts={counts:?}"
    );
}

struct RecoveryCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
    old_server_live: bool,
    old_team_session: bool,
    old_team_tuple: bool,
    status_has_session: bool,
    tmux_env_socket: String,
}

impl RecoveryCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        fs::create_dir_all(workspace.join(".team/runtime")).expect("create runtime dir");
        let fake_bin = workspace.join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake-bin");
        Self {
            env,
            workspace,
            fake_bin,
            old_server_live: true,
            old_team_session: false,
            old_team_tuple: false,
            status_has_session: true,
            tmux_env_socket: OLD_SOCKET.to_string(),
        }
    }

    fn with_old_server_live(mut self, live: bool) -> Self {
        self.old_server_live = live;
        self
    }

    fn with_old_team_session(mut self, live: bool) -> Self {
        self.old_team_session = live;
        self
    }

    fn with_old_team_tuple(mut self, live: bool) -> Self {
        self.old_team_tuple = live;
        self
    }

    fn with_status_has_session(mut self, present: bool) -> Self {
        self.status_has_session = present;
        self
    }

    fn with_tmux_env_socket(mut self, socket: &str) -> Self {
        self.tmux_env_socket = socket.to_string();
        self
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace path is utf8")
    }

    fn new_socket(&self) -> &str {
        "ta-0529-new-live"
    }

    fn fake_tmux(&self) -> PathBuf {
        let tmux = self.fake_bin.join("tmux");
        let old_tuple_line = if self.old_team_tuple {
            pane_line(
                OLD_WORKER_PANE,
                TEAM_SESSION,
                WORKER,
                "codex",
                &self.workspace,
                OLD_WORKER_PID,
            )
        } else {
            String::new()
        };
        let caller_line = pane_line(
            CALLER_PANE,
            LEADER_SESSION,
            "leader",
            "codex",
            &self.workspace,
            LIVE_LEADER_PID,
        );
        let script = format!(
            r#"#!/bin/sh
endpoint="default"
previous=""
for arg in "$@"; do
  if [ "$previous" = "-S" ] || [ "$previous" = "-L" ]; then
    endpoint="$arg"
  fi
  previous="$arg"
done
printf '%s	%s\n' "$endpoint" "$*" >> '{log_path}'
is_old=false
case "$endpoint" in
  "{old_socket}") is_old=true ;;
esac
is_new=false
case "$endpoint" in
  "{new_socket}"|*/"{new_socket}"|ta-*) is_new=true ;;
esac
case " $* " in
  *" list-sessions "*)
    if [ "$is_old" = "true" ]; then
      if [ "{old_server_live}" = "true" ]; then
        printf '%s\n' 'unrelated-session: 1 windows'
        exit 0
      fi
      echo "no server running on old endpoint" >&2
      exit 1
    fi
    printf '%s\n' '{leader_session}: 1 windows'
    exit 0
    ;;
  *" has-session "*)
    if [ "$is_old" = "true" ]; then
      if [ "{old_team_session}" = "true" ]; then
        exit 0
      fi
      exit 1
    fi
    if [ "{status_has_session}" = "true" ]; then
      exit 0
    fi
    exit 1
    ;;
  *" list-panes "*)
    if [ "$is_old" = "true" ]; then
      printf '%s' '{old_tuple_line}'
      exit 0
    fi
    if [ "$is_new" = "true" ] || [ "$endpoint" = "default" ]; then
      printf '%s' '{caller_line}'
      exit 0
    fi
    printf '%s' '{caller_line}'
    exit 0
    ;;
  *" display-message "*)
    printf '%s\n' '{leader_pid}'
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
            log_path = shell_single_quoted_payload(&self.tmux_log_path().to_string_lossy()),
            old_socket = OLD_SOCKET,
            new_socket = self.new_socket(),
            old_server_live = bool_word(self.old_server_live),
            old_team_session = bool_word(self.old_team_session),
            status_has_session = bool_word(self.status_has_session),
            old_tuple_line = shell_single_quoted_payload(&old_tuple_line),
            caller_line = shell_single_quoted_payload(&caller_line),
            leader_session = LEADER_SESSION,
            leader_pid = LIVE_LEADER_PID,
        );
        fs::write(&tmux, script).expect("write fake tmux");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).expect("chmod fake tmux");
        }
        tmux
    }

    fn enter_fake_tmux_path(&self) -> EnvGuard {
        let _ = self.fake_tmux();
        EnvGuard::set(
            "PATH",
            format!(
                "{}:{}",
                self.fake_bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
    }

    fn single_team_state(&self) -> Value {
        json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": self.workspace_str(),
            "tmux_endpoint": OLD_SOCKET,
            "tmux_socket": self.new_socket(),
            "agents": {
                WORKER: running_worker()
            },
            "teams": {
                TEAM: {
                    "team_key": TEAM,
                    "session_name": TEAM_SESSION,
                    "team_dir": self.workspace_str(),
                    "tmux_endpoint": OLD_SOCKET,
                    "tmux_socket": self.new_socket(),
                    "agents": {
                        WORKER: running_worker()
                    }
                }
            }
        })
    }

    fn seed_split_state(
        &self,
        team: &str,
        session: &str,
        root_endpoint: &str,
        root_socket: &str,
        include_owner: bool,
    ) {
        let receiver = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%9",
            "pane_pid": 9_999,
            "session_name": LEADER_SESSION,
            "window_name": "leader",
            "tmux_socket": root_socket,
            "leader_session_uuid": "0529-stale-owner",
            "owner_epoch": 7,
            "claimed_via": "claim-leader"
        });
        let owner = json!({
            "pane_id": "%9",
            "provider": "codex",
            "pane_pid": 9_999,
            "tmux_socket": root_socket,
            "leader_session_uuid": "0529-stale-owner",
            "machine_fingerprint": "machine-0529-stale",
            "owner_epoch": 7,
            "claimed_via": "claim-leader"
        });
        let mut state = json!({
            "active_team_key": team,
            "team_key": team,
            "session_name": session,
            "team_dir": self.workspace_str(),
            "tmux_endpoint": root_endpoint,
            "tmux_socket": root_socket,
            "tmux_socket_source": "stale_state",
            "agents": {
                WORKER: running_worker()
            },
            "teams": {
                team: {
                    "active_team_key": team,
                    "team_key": team,
                    "session_name": session,
                    "team_dir": self.workspace_str(),
                    "tmux_endpoint": root_endpoint,
                    "tmux_socket": root_socket,
                    "tmux_socket_source": "stale_state",
                    "agents": {
                        WORKER: running_worker()
                    }
                }
            }
        });
        if include_owner {
            state["leader_receiver"] = receiver.clone();
            state["team_owner"] = owner.clone();
            state["teams"][team]["leader_receiver"] = receiver;
            state["teams"][team]["team_owner"] = owner;
        }
        save_runtime_state(&self.workspace, &state).expect("seed runtime state");
    }

    fn seed_multi_team_state(&self) {
        let current = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": self.workspace_str(),
            "status": "shutdown",
            "tmux_endpoint": OLD_SOCKET,
            "tmux_socket": OLD_SOCKET,
            "agents": { WORKER: stopped_worker() }
        });
        let research = json!({
            "active_team_key": RETAINED_TEAM,
            "team_key": RETAINED_TEAM,
            "team": {"name": "retained-research-alias"},
            "session_name": RETAINED_SESSION,
            "team_dir": self.workspace_str(),
            "status": "shutdown",
            "tmux_endpoint": OLD_SOCKET,
            "tmux_socket": OLD_SOCKET,
            "agents": { WORKER: stopped_worker() }
        });
        let state = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": self.workspace_str(),
            "status": "shutdown",
            "tmux_endpoint": OLD_SOCKET,
            "tmux_socket": OLD_SOCKET,
            "agents": { WORKER: stopped_worker() },
            "teams": {
                TEAM: current,
                RETAINED_TEAM: research
            }
        });
        save_runtime_state(&self.workspace, &state).expect("seed multi-team state");
    }

    fn seed_all_dead_retained_alias_state(&self) {
        self.seed_multi_team_state();
    }

    fn seed_status_state(&self) {
        self.seed_split_state(TEAM, TEAM_SESSION, OLD_SOCKET, OLD_SOCKET, true);
    }

    fn write_minimal_team_spec(&self, team: &str, session: &str) {
        let spec = format!(
            r#"version: 1
team:
  id: {team}
  name: {team}
  session_name: {session}
  workspace: "{workspace}"
agents:
  - id: {worker}
    role: Worker
    provider: fake
    model: fake
    window: {worker}
tasks: []
"#,
            team = team,
            session = session,
            workspace = self.workspace.display(),
            worker = WORKER
        );
        fs::write(self.workspace.join("team.spec.yaml"), &spec).expect("write team.spec.yaml");
        let runtime_spec_dir = self.workspace.join(".team/runtime").join(team);
        fs::create_dir_all(&runtime_spec_dir).expect("create runtime spec dir");
        fs::write(runtime_spec_dir.join("team.spec.yaml"), spec)
            .expect("write runtime team.spec.yaml");
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let _ = self.fake_tmux();
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
            .env("TMUX", format!("{},12345,0", self.tmux_env_socket))
            .env("TMUX_PANE", CALLER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0529-red");
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn read_state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("read runtime state")
    }

    fn observed_caller_endpoint(&self) -> String {
        self.tmux_log()
            .lines()
            .filter(|line| line.contains(" list-panes "))
            .filter_map(|line| line.split('\t').next())
            .find(|endpoint| {
                *endpoint != "default"
                    && *endpoint != OLD_SOCKET
                    && *endpoint != DEAD_ENV_SOCKET
                    && *endpoint != self.tmux_env_socket
            })
            .map(str::to_string)
            .unwrap_or_else(|| {
                panic!(
                    "no observed caller endpoint in fake tmux log: {}",
                    self.tmux_log()
                )
            })
    }

    fn tmux_log_path(&self) -> PathBuf {
        self.workspace.join("fake-tmux.log")
    }

    fn tmux_log(&self) -> String {
        fs::read_to_string(self.tmux_log_path()).unwrap_or_default()
    }
}

fn running_worker() -> Value {
    json!({
        "id": WORKER,
        "name": WORKER,
        "provider": "fake",
        "window": WORKER,
        "status": "running",
        "worker_state": "PROBABLY_IDLE",
        "pane_id": OLD_WORKER_PANE,
        "pane_pid": OLD_WORKER_PID,
        "pid": OLD_WORKER_PID,
        "process_started": true
    })
}

fn stopped_worker() -> Value {
    json!({
        "id": WORKER,
        "name": WORKER,
        "provider": "fake",
        "window": WORKER,
        "status": "stopped",
        "worker_state": "DEAD"
    })
}

fn case_new_env_socket() -> &'static str {
    "ta-0529-env-live"
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
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0529\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn bool_word(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: String) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}
