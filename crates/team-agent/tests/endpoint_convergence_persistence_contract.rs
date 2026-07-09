//! 0.5.16 RED contract: endpoint convergence must be a durable, monotonic
//! owner-epoch fact.
//!
//! References:
//! - `.team/artifacts/claim-endpoint-nonconvergence-locate-2.md` §9 RED1-RED3.
//!
//! User-visible contract:
//! - If `claim-leader` says endpoint topology converged, disk state already proves
//!   root/team endpoint/socket/source/marker/epoch are the new endpoint.
//! - A live coordinator that saves a stale pre-claim team snapshot may persist its
//!   own health/capture observations, but must not roll endpoint convergence back.
//! - Dirty-topology restart refusal must not create a coordinator or team session.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;

const TEAM: &str = "current";
const WORKER: &str = "fetcher";
const CALLER_PANE: &str = "%0";
const STALE_OWNER_PANE: &str = "%9";
const LIVE_LEADER_PID: u32 = 14_663;
const STALE_OWNER_PID: u32 = 47_641;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[serial(env)]
fn red1_claim_convergence_is_persisted_before_converged_response() {
    let case = PersistenceCase::new("red1-persist-before-response");

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED1 claim-leader");
    assert_binding_ok(&claim, &claim_json, "RED1 claim-leader");
    assert_eq!(
        claim_json
            .pointer("/topology_convergence/status")
            .and_then(Value::as_str),
        Some("converged"),
        "RED1 setup: claim-leader must exercise the converged branch; json={claim_json}"
    );
    let owner_epoch = claim_json
        .get("owner_epoch")
        .and_then(Value::as_u64)
        .expect("RED1 setup: claim response must carry owner_epoch");
    assert_persisted_convergence(&case.read_state(), &case, owner_epoch, "RED1");
    assert_convergence_event_proves_persisted(&case, owner_epoch);
}

#[test]
#[serial(env)]
fn red2_coordinator_stale_save_cannot_rollback_claim_endpoint_convergence() {
    let case = PersistenceCase::new("red2-stale-coordinator-save");
    let mut stale_team = case.team_state_snapshot();
    stale_team["coordinator"] = json!({
        "health": "stale-writer-survived",
        "tick_id": "coordinator-stale-save-red2"
    });
    stale_team["agents"][WORKER]["capture_state"] = json!({
        "last_output_at": "2026-07-09T00:00:00Z",
        "source": "coordinator-stale-save-red2"
    });

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED2 claim-leader");
    assert_binding_ok(&claim, &claim_json, "RED2 claim-leader");
    let owner_epoch = claim_json
        .get("owner_epoch")
        .and_then(Value::as_u64)
        .expect("RED2 setup: claim response must carry owner_epoch");
    assert_persisted_convergence(&case.read_state(), &case, owner_epoch, "RED2 setup");

    team_agent::state::projection::save_team_scoped_state(case.path(), &stale_team).expect(
        "RED2 setup: stale coordinator writer must use the real save_team_scoped_state path",
    );

    let state = case.read_state();
    assert_persisted_convergence(&state, &case, owner_epoch, "RED2");
    assert_eq!(
        state.pointer("/teams/current/coordinator/health")
            .and_then(Value::as_str),
        Some("stale-writer-survived"),
        "RED2: coordinator-owned health delta should survive while endpoint fields stay monotonic; state={state}"
    );
    assert_eq!(
        state.pointer("/teams/current/agents/fetcher/capture_state/source")
            .and_then(Value::as_str),
        Some("coordinator-stale-save-red2"),
        "RED2: coordinator-owned capture delta should survive while endpoint fields stay monotonic; state={state}"
    );
}

#[test]
#[serial(env)]
fn red3_restart_refused_dirty_topology_does_not_boot_coordinator() {
    let case = PersistenceCase::new("red3-refused-no-coordinator");
    let runtime = case.path().join(".team/runtime");
    let _ = std::fs::remove_file(runtime.join("coordinator.pid"));
    let _ = std::fs::remove_file(runtime.join("coordinator.json"));
    case.clear_tmux_log();

    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, "RED3 restart");
    assert_eq!(
        restart_json.get("status").and_then(Value::as_str),
        Some("refused_dirty_topology"),
        "RED3 setup: restart must refuse dirty split topology; json={restart_json}"
    );
    assert!(
        events(case.path()).contains("\"restart.refused_dirty_topology\""),
        "RED3: refused restart must emit restart.refused_dirty_topology; events={}",
        events(case.path())
    );
    assert!(
        !events(case.path()).contains("\"coordinator.boot\""),
        "RED3: dirty-topology refusal must not boot a new coordinator; events={}",
        events(case.path())
    );
    assert!(
        !runtime.join("coordinator.pid").exists() && !runtime.join("coordinator.json").exists(),
        "RED3: refused restart must not write coordinator.pid/coordinator.json"
    );
    let log = case.tmux_log();
    assert!(
        !log.lines().any(|line| {
            line.contains(case.team_session())
                && (line.contains(" new-session ") || line.contains(" new-window "))
        }),
        "RED3: refused restart must not create a team session on old or new endpoint; fake tmux log={log}"
    );
}

struct PersistenceCase {
    workspace: PathBuf,
    fake_bin: PathBuf,
    new_socket: String,
    team_session: String,
}

impl PersistenceCase {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let run_id = N.fetch_add(1, Ordering::Relaxed);
        let workspace = tmp_dir(tag, run_id);
        std::fs::create_dir_all(workspace.join("home")).expect("create isolated home");
        let old_socket = format!("/private/tmp/tmux-501/ta-0516-persist-old-{run_id}");
        let new_socket = format!("/private/tmp/tmux-501/ta-0516-persist-new-{run_id}");
        let team_session = format!("team-0516-persist-{run_id}");
        let leader_session = format!("leader-0516-persist-{run_id}");
        let fake_bin = fake_tmux_bin(
            &workspace,
            &old_socket,
            &new_socket,
            &team_session,
            &leader_session,
        );
        seed_split_state(
            &workspace,
            &old_socket,
            &new_socket,
            &team_session,
            &leader_session,
        );
        write_minimal_team_spec(&workspace, &team_session);
        Self {
            workspace,
            fake_bin,
            new_socket,
            team_session,
        }
    }

    fn path(&self) -> &Path {
        &self.workspace
    }

    fn workspace_str(&self) -> &str {
        self.workspace
            .to_str()
            .expect("workspace path must be utf8")
    }

    fn team_session(&self) -> &str {
        &self.team_session
    }

    fn read_state(&self) -> Value {
        let path = self.workspace.join(".team/runtime/state.json");
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read state.json"))
            .expect("parse state.json")
    }

    fn team_state_snapshot(&self) -> Value {
        self.read_state()
            .pointer("/teams/current")
            .cloned()
            .expect("teams.current exists")
    }

    fn clear_tmux_log(&self) {
        let _ = std::fs::remove_file(self.tmux_log_path());
    }

    fn tmux_log(&self) -> String {
        std::fs::read_to_string(self.tmux_log_path()).unwrap_or_default()
    }

    fn tmux_log_path(&self) -> PathBuf {
        self.workspace.join("fake-tmux.log")
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(bin());
        command
            .args(args)
            .current_dir(&self.workspace)
            .env(
                "TEAM_AGENT_TEST_ENDPOINT_CONVERGENCE_HARNESS_SPEC_FALLBACK",
                "1",
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{},12345,0", self.new_socket))
            .env("TMUX_PANE", CALLER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-persistence-red")
            .env("HOME", self.workspace.join("home"))
            .env("USER", "te-red");
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_OWNER_TEAM_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent test binary")
    }
}

impl Drop for PersistenceCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

fn seed_split_state(
    workspace: &Path,
    old_socket: &str,
    new_socket: &str,
    team_session: &str,
    leader_session: &str,
) {
    let worker = json!({
        "id": WORKER,
        "name": WORKER,
        "provider": "fake",
        "window": WORKER,
        "status": "stopped"
    });
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": "codex",
        "pane_id": STALE_OWNER_PANE,
        "pane_pid": STALE_OWNER_PID,
        "session_name": leader_session,
        "window_name": "claude_code",
        "tmux_socket": new_socket,
        "leader_session_uuid": "endpoint-persistence-leader",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    });
    let owner = json!({
        "pane_id": CALLER_PANE,
        "provider": "codex",
        "pane_pid": LIVE_LEADER_PID,
        "tmux_socket": new_socket,
        "leader_session_uuid": "endpoint-persistence-leader",
        "machine_fingerprint": "machine-persistence-red",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    });
    team_agent::state::persist::save_runtime_state(
        workspace,
        &json!({
            "active_team_key": TEAM,
            "session_name": team_session,
            "team_dir": workspace.to_string_lossy().to_string(),
            "tmux_endpoint": old_socket,
            "tmux_socket": new_socket,
            "tmux_socket_source": "leader_env",
            "agents": { WORKER: worker.clone() },
            "leader_receiver": receiver.clone(),
            "team_owner": owner.clone(),
            "owner_epoch": 7,
            "teams": {
                TEAM: {
                    "team_key": TEAM,
                    "session_name": team_session,
                    "team_dir": workspace.to_string_lossy().to_string(),
                    "tmux_endpoint": old_socket,
                    "tmux_socket": new_socket,
                    "tmux_socket_source": "leader_env",
                    "agents": { WORKER: worker },
                    "leader_receiver": receiver,
                    "team_owner": owner,
                    "owner_epoch": 7
                }
            }
        }),
    )
    .expect("seed runtime state");
}

fn assert_binding_ok(output: &Output, value: &Value, label: &str) {
    assert!(
        output.status.success() && value.get("ok").and_then(Value::as_bool) == Some(true),
        "{label}: binding command must return ok:true; code={:?} json={value} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_persisted_convergence(
    state: &Value,
    case: &PersistenceCase,
    owner_epoch: u64,
    label: &str,
) {
    for pointer in [
        "/tmux_endpoint",
        "/tmux_socket",
        "/teams/current/tmux_endpoint",
        "/teams/current/tmux_socket",
    ] {
        assert_eq!(
            state.pointer(pointer).and_then(Value::as_str),
            Some(case.new_socket.as_str()),
            "{label}: {pointer} must persist the new endpoint before convergence is advertised; state={state}"
        );
    }
    for pointer in ["/tmux_socket_source", "/teams/current/tmux_socket_source"] {
        assert_eq!(
            state.pointer(pointer).and_then(Value::as_str),
            Some("leader_env"),
            "{label}: {pointer} must persist source=leader_env; state={state}"
        );
    }
    for pointer in [
        "/topology_convergence",
        "/teams/current/topology_convergence",
    ] {
        let marker = state.pointer(pointer).unwrap_or_else(|| {
            panic!("{label}: missing convergence marker {pointer}; state={state}")
        });
        assert_eq!(
            marker.get("status").and_then(Value::as_str),
            Some("converged"),
            "{label}: {pointer}.status must be converged; marker={marker}; state={state}"
        );
        assert_eq!(
            marker.get("new_tmux_endpoint").and_then(Value::as_str),
            Some(case.new_socket.as_str()),
            "{label}: {pointer}.new_tmux_endpoint must be the new endpoint; marker={marker}"
        );
        assert_eq!(
            marker.get("owner_epoch").and_then(Value::as_u64),
            Some(owner_epoch),
            "{label}: {pointer}.owner_epoch must match the response epoch; marker={marker}"
        );
    }
    assert!(
        state
            .pointer("/teams/current/owner_epoch")
            .and_then(Value::as_u64)
            .is_some_and(|epoch| epoch >= owner_epoch),
        "{label}: teams.current.owner_epoch must not regress below response epoch={owner_epoch}; state={state}"
    );
}

fn assert_convergence_event_proves_persisted(case: &PersistenceCase, owner_epoch: u64) {
    let event = events(case.path())
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event.get("event").and_then(Value::as_str)
                == Some("leader_receiver.tmux_endpoint_converged")
        })
        .unwrap_or_else(|| {
            panic!(
                "RED1: claim convergence must emit leader_receiver.tmux_endpoint_converged after persistence proof; events={}",
                events(case.path())
            )
        });
    assert_eq!(
        event.get("persisted").and_then(Value::as_bool),
        Some(true),
        "RED1: convergence event may be emitted only after persisted readback proof; it must carry persisted=true and checked paths; event={event}"
    );
    assert_eq!(
        event.get("owner_epoch").and_then(Value::as_u64),
        Some(owner_epoch),
        "RED1: convergence event owner_epoch must match persisted response epoch; event={event}"
    );
    assert!(
        event
            .get("checked_paths")
            .and_then(Value::as_array)
            .is_some_and(|paths| paths.iter().any(|p| p == "/teams/current/tmux_endpoint")),
        "RED1: convergence event must name the persisted root/team paths it verified; event={event}"
    );
}

fn write_minimal_team_spec(workspace: &Path, team_session: &str) {
    let spec = format!(
        r#"version: 1
team:
  name: "{team}"
  mode: "supervisor_worker"
  objective: "endpoint persistence contract"
  workspace: "{workspace}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  model: null
  tools: []
agents:
  - id: "{worker}"
    role: "fetcher"
    provider: "fake"
    model: "fake"
    auth_mode: "subscription"
    working_directory: "{workspace}"
    system_prompt:
      inline: "fake worker"
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
  summarization:
    worker_full_logs: "retain_outside_leader_context"
    state_update: "after_each_result"
tasks: []
"#,
        team = TEAM,
        worker = WORKER,
        session = team_session,
        workspace = workspace.display()
    );
    std::fs::write(workspace.join("team.spec.yaml"), &spec).expect("write root team.spec.yaml");
    let runtime_spec_dir = workspace.join(".team/runtime").join(TEAM);
    std::fs::create_dir_all(&runtime_spec_dir).expect("create runtime spec dir");
    std::fs::write(runtime_spec_dir.join("team.spec.yaml"), spec)
        .expect("write runtime team.spec.yaml");
}

fn fake_tmux_bin(
    workspace: &Path,
    old_socket: &str,
    new_socket: &str,
    team_session: &str,
    leader_session: &str,
) -> PathBuf {
    let bin_dir = workspace.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    let tmux = bin_dir.join("tmux");
    let log_path = workspace.join("fake-tmux.log");
    let new_line = pane_line(
        CALLER_PANE,
        leader_session,
        "claude_code",
        workspace,
        LIVE_LEADER_PID,
    );
    let script = format!(
        r#"#!/bin/sh
endpoint="default"
previous=""
for arg in "$@"; do
  if [ "$previous" = "-S" ]; then
    endpoint="$arg"
  fi
  previous="$arg"
done
printf '%s\t%s\n' "$endpoint" "$*" >> '{log_path}'
case "$endpoint" in
  "{old_socket}")
    echo "no server running on {old_socket}" >&2
    exit 1
    ;;
  "{new_socket}"|*)
    case " $* " in
      *" list-panes "*) printf '%s' '{new_line}'; exit 0 ;;
      *" list-sessions "*) printf '%s\n' '{leader_session}: 1 windows'; exit 0 ;;
      *" display-message "*) printf '%s\n' '{leader_pid}'; exit 0 ;;
      *" has-session "*)
        case " $* " in
          *" {team_session}"*) exit 1 ;;
          *) exit 0 ;;
        esac ;;
      *" new-session "*|*" new-window "*) exit 0 ;;
      *) exit 0 ;;
    esac
    ;;
esac
"#,
        log_path = shell_single_quoted_payload(&log_path.to_string_lossy()),
        old_socket = old_socket,
        new_socket = new_socket,
        new_line = shell_single_quoted_payload(&new_line),
        team_session = team_session,
        leader_session = leader_session,
        leader_pid = LIVE_LEADER_PID,
    );
    std::fs::write(&tmux, script).expect("write fake tmux");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake tmux");
    }
    bin_dir
}

fn pane_line(pane: &str, session: &str, window: &str, cwd: &Path, pid: u32) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0516\tcodex\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}

fn json_output(output: &Output, label: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "{label}: stdout must contain JSON object; code={:?} stdout={stdout:?} stderr={:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let end = stdout.rfind('}').expect("stdout JSON object end");
    serde_json::from_str(&stdout[start..=end]).unwrap_or_else(|error| {
        panic!(
            "{label}: parse JSON failed: {error}; stdout={stdout:?} stderr={:?}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn tmp_dir(tag: &str, run_id: u64) -> PathBuf {
    let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).expect("create TEAM_AGENT_TEST_TMP root");
    let dir = root.join(format!(
        "ta-0516-persistence-{tag}-{}-{run_id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    std::fs::canonicalize(dir).expect("canonicalize temp workspace")
}
