//! Seventh-case RED contract: explicit leader binding recovery must converge
//! tmux endpoint state, not merely return a successful ownership bind.
//!
//! References:
//! - `.team/artifacts/claim-endpoint-nonconvergence-locate.md` §10 RED 1-4.
//! - Leader verdict for §7.2 live-old-endpoint shape: binding remains ok:true,
//!   with `topology_convergence.status="not_converged_old_endpoint_live"`.
//!
//! User-visible contract:
//! - `restart` may fail closed on split endpoint state.
//! - The advertised `claim-leader` / `takeover` repair commands must remove the
//!   stale endpoint refusal when the old endpoint is dead.
//! - If the old endpoint is still live, binding must not silently overwrite it;
//!   the response must name that live old endpoint and leave restart refused.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;

const TEAM: &str = "current";
const WORKER: &str = "fetcher";
const CALLER_PANE: &str = "%0";
const STALE_OWNER_PANE: &str = "%9";
const OLD_SOCKET: &str = "/private/tmp/tmux-501/ta-old";
const NEW_SOCKET: &str = "/private/tmp/tmux-501/ta-new";
const TEAM_SESSION: &str = "team-video-workflow";
const LEADER_SESSION: &str = "team-agent-leader-claude_code-workspace-convergence-probe";
const LIVE_LEADER_PID: u32 = 14663;
const STALE_OWNER_PID: u32 = 47641;
const OLD_LIVE_PID: u32 = 22441;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[serial(env)]
fn red1_claim_converges_dead_old_endpoint_then_restart_succeeds() {
    let case = EndpointCase::new(
        "red1-claim-converges",
        ReceiverShape::DifferentDeadPane,
        OldEndpoint::Dead,
    );
    assert_initial_restart_refuses_split(&case, "RED1 setup");

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
    assert_state_converged(
        &case.read_state(),
        "RED1: claim-leader must converge root and teams.current endpoint fields when the old endpoint is dead",
    );
    assert_convergence_metadata(
        &case,
        &claim_json,
        "claim-leader",
        "RED1: claim-leader must emit or return explicit topology convergence metadata",
    );
    assert_restart_succeeds(
        &case,
        "RED1: after claim-leader converges endpoint state, restart must exit the same refused_dirty_topology loop",
    );
}

#[test]
#[serial(env)]
fn red2_takeover_inherits_dead_old_endpoint_convergence() {
    let case = EndpointCase::new(
        "red2-takeover-converges",
        ReceiverShape::DifferentDeadPane,
        OldEndpoint::Dead,
    );
    assert_initial_restart_refuses_split(&case, "RED2 setup");

    let takeover = case.run_ta(&[
        "takeover",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let takeover_json = json_output(&takeover, "RED2 takeover");
    assert_binding_ok(&takeover, &takeover_json, "RED2 takeover");
    assert_state_converged(
        &case.read_state(),
        "RED2: takeover shares the claim binding path and must converge endpoint state when the old endpoint is dead",
    );
    assert_convergence_metadata(
        &case,
        &takeover_json,
        "takeover",
        "RED2: takeover must emit or return explicit topology convergence metadata",
    );
    assert_restart_succeeds(
        &case,
        "RED2: after takeover converges endpoint state, restart must exit the same refused_dirty_topology loop",
    );
}

#[test]
#[serial(env)]
fn red3_already_bound_branch_still_converges_dead_old_endpoint() {
    let case = EndpointCase::new(
        "red3-already-bound-converges",
        ReceiverShape::AlreadyBoundCaller,
        OldEndpoint::Dead,
    );
    assert_initial_restart_refuses_split(&case, "RED3 setup");

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED3 already-bound claim-leader");
    assert_binding_ok(&claim, &claim_json, "RED3 already-bound claim-leader");
    assert_eq!(
        claim_json.get("status").and_then(Value::as_str),
        Some("already_bound"),
        "RED3 setup: this fixture must exercise the already-bound success branch; json={claim_json}"
    );
    assert_state_converged(
        &case.read_state(),
        "RED3: already-bound must not be a no-op when endpoint fields are still split; it must converge the dead old endpoint",
    );
    assert_restart_succeeds(
        &case,
        "RED3: after already-bound convergence, restart must exit the same refused_dirty_topology loop",
    );
}

#[test]
#[serial(env)]
fn red4_live_old_endpoint_is_not_silently_overwritten() {
    let case = EndpointCase::new(
        "red4-live-old-refuses-convergence",
        ReceiverShape::DifferentDeadPane,
        OldEndpoint::Live,
    );
    assert_initial_restart_refuses_split(&case, "RED4 setup");

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "RED4 live old endpoint claim-leader");
    assert_binding_ok(&claim, &claim_json, "RED4 live old endpoint claim-leader");
    assert_state_still_split(
        &case.read_state(),
        "RED4: live old endpoint must not be silently overwritten by claim-leader",
    );
    assert_eq!(
        claim_json.pointer("/topology_convergence/status").and_then(Value::as_str),
        Some("not_converged_old_endpoint_live"),
        "RED4: live-old-endpoint branch must keep binding ok:true but report topology_convergence.status=not_converged_old_endpoint_live; json={claim_json}"
    );
    let output_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&claim.stdout),
        String::from_utf8_lossy(&claim.stderr)
    );
    assert!(
        output_text.contains(OLD_SOCKET),
        "RED4: response/action must name the live old endpoint so the operator knows what to clean up; output={output_text}"
    );
    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, "RED4 restart after live old endpoint claim");
    assert_eq!(
        restart_json.get("status").and_then(Value::as_str),
        Some("refused_dirty_topology"),
        "RED4: restart must remain fail-closed while the live old endpoint conflict remains; json={restart_json}"
    );
}

fn assert_initial_restart_refuses_split(case: &EndpointCase, label: &str) {
    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, label);
    assert_eq!(
        restart_json.get("status").and_then(Value::as_str),
        Some("refused_dirty_topology"),
        "{label}: restart must initially fail closed on split endpoint state; json={restart_json}"
    );
    assert_issue(
        &restart_json,
        "tmux_endpoint_socket_conflict",
        "{label}: initial restart refusal must name tmux_endpoint_socket_conflict",
    );
    assert_issue(
        &restart_json,
        "leader_receiver_socket_mismatch",
        "{label}: initial restart refusal must name leader_receiver_socket_mismatch",
    );
}

fn assert_binding_ok(output: &Output, value: &Value, label: &str) {
    assert!(
        output.status.success() && value.get("ok").and_then(Value::as_bool) == Some(true),
        "{label}: binding command must return ok:true before topology convergence is evaluated; code={:?} json={value} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_convergence_metadata(case: &EndpointCase, value: &Value, source: &str, message: &str) {
    let response_converged = value
        .pointer("/topology_convergence/status")
        .and_then(Value::as_str)
        == Some("converged");
    let event_converged = events(&case.workspace)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|event| {
            event.get("event").and_then(Value::as_str)
                == Some("leader_receiver.tmux_endpoint_converged")
                && event.get("source").and_then(Value::as_str) == Some(source)
                && event.get("old_tmux_endpoint").and_then(Value::as_str) == Some(OLD_SOCKET)
                && event.get("new_tmux_endpoint").and_then(Value::as_str) == Some(NEW_SOCKET)
        });
    assert!(
        response_converged || event_converged,
        "{message}; expected response topology_convergence.status=converged or event leader_receiver.tmux_endpoint_converged with old/new endpoints; json={value}; events={}",
        events(&case.workspace)
    );
}

fn assert_state_converged(state: &Value, message: &str) {
    assert_eq!(
        state.get("tmux_endpoint").and_then(Value::as_str),
        Some(NEW_SOCKET),
        "{message}: root tmux_endpoint must be rewritten to the live caller endpoint; state={state}"
    );
    assert_eq!(
        state.get("tmux_socket").and_then(Value::as_str),
        Some(NEW_SOCKET),
        "{message}: root tmux_socket must remain the live caller endpoint; state={state}"
    );
    assert_eq!(
        state
            .pointer("/teams/current/tmux_endpoint")
            .and_then(Value::as_str),
        Some(NEW_SOCKET),
        "{message}: teams.current.tmux_endpoint must converge with root; state={state}"
    );
    assert_eq!(
        state
            .pointer("/teams/current/tmux_socket")
            .and_then(Value::as_str),
        Some(NEW_SOCKET),
        "{message}: teams.current.tmux_socket must remain the live caller endpoint; state={state}"
    );
}

fn assert_state_still_split(state: &Value, message: &str) {
    assert_eq!(
        state.get("tmux_endpoint").and_then(Value::as_str),
        Some(OLD_SOCKET),
        "{message}: root tmux_endpoint should still name the live old endpoint; state={state}"
    );
    assert_eq!(
        state.pointer("/teams/current/tmux_endpoint").and_then(Value::as_str),
        Some(OLD_SOCKET),
        "{message}: teams.current.tmux_endpoint should still name the live old endpoint; state={state}"
    );
    assert_eq!(
        state.get("tmux_socket").and_then(Value::as_str),
        Some(NEW_SOCKET),
        "{message}: root tmux_socket remains the caller endpoint; state={state}"
    );
}

fn assert_restart_succeeds(case: &EndpointCase, message: &str) {
    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, message);
    assert_eq!(
        restart_json.get("status").and_then(Value::as_str),
        Some("restarted"),
        "{message}; expected status=restarted, got json={restart_json} stdout={} stderr={}",
        String::from_utf8_lossy(&restart.stdout),
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_no_issue(
        &restart_json,
        "tmux_endpoint_socket_conflict",
        "{message}: restart after repair must not repeat tmux_endpoint_socket_conflict",
    );
    assert_no_issue(
        &restart_json,
        "leader_receiver_socket_mismatch",
        "{message}: restart after repair must not repeat leader_receiver_socket_mismatch",
    );
}

fn assert_issue(value: &Value, issue_id: &str, message: &str) {
    assert!(
        issue_ids(value).iter().any(|id| id == issue_id),
        "{message}; issues={:?}; json={value}",
        issue_ids(value)
    );
}

fn assert_no_issue(value: &Value, issue_id: &str, message: &str) {
    assert!(
        !issue_ids(value).iter().any(|id| id == issue_id),
        "{message}; issues={:?}; json={value}",
        issue_ids(value)
    );
}

fn issue_ids(value: &Value) -> Vec<String> {
    value
        .get("issues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|issue| {
            issue
                .as_str()
                .map(str::to_string)
                .or_else(|| issue.get("id").and_then(Value::as_str).map(str::to_string))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ReceiverShape {
    DifferentDeadPane,
    AlreadyBoundCaller,
}

#[derive(Clone, Copy)]
enum OldEndpoint {
    Dead,
    Live,
}

struct EndpointCase {
    workspace: PathBuf,
    fake_bin: PathBuf,
}

impl EndpointCase {
    fn new(tag: &str, receiver_shape: ReceiverShape, old_endpoint: OldEndpoint) -> Self {
        let workspace = tmp_dir(tag);
        std::fs::create_dir_all(workspace.join("home")).expect("create isolated home");
        let fake_bin = fake_tmux_bin(&workspace, old_endpoint);
        seed_split_state(&workspace, receiver_shape);
        write_minimal_team_spec(&workspace);
        Self {
            workspace,
            fake_bin,
        }
    }

    fn workspace_str(&self) -> &str {
        self.workspace
            .to_str()
            .expect("workspace path must be utf8")
    }

    fn read_state(&self) -> Value {
        let path = self.workspace.join(".team/runtime/state.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(bin());
        command
            .args(args)
            .current_dir(&self.workspace)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{NEW_SOCKET},12345,0"))
            .env("TMUX_PANE", CALLER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env(
                "TEAM_AGENT_MACHINE_FINGERPRINT",
                "machine-endpoint-convergence-red",
            )
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

impl Drop for EndpointCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

fn seed_split_state(workspace: &Path, receiver_shape: ReceiverShape) {
    let receiver_pane = match receiver_shape {
        ReceiverShape::DifferentDeadPane => STALE_OWNER_PANE,
        ReceiverShape::AlreadyBoundCaller => CALLER_PANE,
    };
    let receiver_pid = match receiver_shape {
        ReceiverShape::DifferentDeadPane => STALE_OWNER_PID,
        ReceiverShape::AlreadyBoundCaller => LIVE_LEADER_PID,
    };
    let owner_pane = CALLER_PANE;
    let owner_pid = LIVE_LEADER_PID;
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
        "pane_id": receiver_pane,
        "pane_pid": receiver_pid,
        "session_name": LEADER_SESSION,
        "window_name": "claude_code",
        "tmux_socket": NEW_SOCKET,
        "leader_session_uuid": "endpoint-convergence-leader",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    });
    let owner = json!({
        "pane_id": owner_pane,
        "provider": "codex",
        "pane_pid": owner_pid,
        "tmux_socket": NEW_SOCKET,
        "leader_session_uuid": "endpoint-convergence-leader",
        "machine_fingerprint": "machine-endpoint-convergence-red",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    });
    team_agent::state::persist::save_runtime_state(
        workspace,
        &json!({
            "active_team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": workspace.to_string_lossy().to_string(),
            "tmux_endpoint": OLD_SOCKET,
            "tmux_socket": NEW_SOCKET,
            "tmux_socket_source": "leader_env",
            "agents": {
                WORKER: worker.clone()
            },
            "leader_receiver": receiver.clone(),
            "team_owner": owner.clone(),
            "teams": {
                TEAM: {
                    "session_name": TEAM_SESSION,
                    "team_dir": workspace.to_string_lossy().to_string(),
                    "tmux_endpoint": OLD_SOCKET,
                    "tmux_socket": NEW_SOCKET,
                    "tmux_socket_source": "leader_env",
                    "agents": {
                        WORKER: worker
                    },
                    "leader_receiver": receiver,
                    "team_owner": owner
                }
            }
        }),
    )
    .expect("seed runtime state");
}

fn write_minimal_team_spec(workspace: &Path) {
    let spec = format!(
        r#"version: 1
team:
  name: "{team}"
  mode: "supervisor_worker"
  objective: "claim endpoint convergence contract"
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
        session = TEAM_SESSION,
        workspace = workspace.display()
    );
    std::fs::write(workspace.join("team.spec.yaml"), &spec).expect("write root team.spec.yaml");
    let runtime_spec_dir = workspace.join(".team/runtime").join(TEAM);
    std::fs::create_dir_all(&runtime_spec_dir).expect("create runtime spec dir");
    std::fs::write(runtime_spec_dir.join("team.spec.yaml"), spec)
        .expect("write runtime team.spec.yaml");
}

fn fake_tmux_bin(workspace: &Path, old_endpoint: OldEndpoint) -> PathBuf {
    let bin_dir = workspace.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    let tmux = bin_dir.join("tmux");
    let new_line = pane_line(
        CALLER_PANE,
        LEADER_SESSION,
        "claude_code",
        "codex",
        workspace,
        LIVE_LEADER_PID,
    );
    let old_line = pane_line("%8", TEAM_SESSION, WORKER, "codex", workspace, OLD_LIVE_PID);
    let old_live = matches!(old_endpoint, OldEndpoint::Live);
    let script = format!(
        r#"#!/bin/sh
case " $* " in
  *" -S {old_socket} "*)
    if [ "{old_live}" != "true" ]; then
      echo "no server running on {old_socket}" >&2
      exit 1
    fi
    case " $* " in
      *" list-panes "*) printf '%s' '{old_line}'; exit 0 ;;
      *" list-sessions "*) printf '%s\n' '{team_session}: 1 windows'; exit 0 ;;
      *" has-session "*) exit 0 ;;
      *) exit 0 ;;
    esac
    ;;
  *)
    case " $* " in
      *" list-panes "*) printf '%s' '{new_line}'; exit 0 ;;
      *" list-sessions "*) printf '%s\n' '{leader_session}: 1 windows'; exit 0 ;;
      *" display-message "*) printf '%s\n' '{leader_pid}'; exit 0 ;;
      *" has-session "*) exit 0 ;;
      *) exit 0 ;;
    esac
    ;;
esac
"#,
        old_socket = OLD_SOCKET,
        old_live = if old_live { "true" } else { "false" },
        old_line = shell_single_quoted_payload(&old_line),
        new_line = shell_single_quoted_payload(&new_line),
        team_session = TEAM_SESSION,
        leader_session = LEADER_SESSION,
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

fn pane_line(
    pane: &str,
    session: &str,
    window: &str,
    command: &str,
    cwd: &Path,
    pid: u32,
) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0514\t{command}\t1\t{}\t1\t0\t{pid}\n",
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

fn tmp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).expect("create TEAM_AGENT_TEST_TMP root");
    let dir = root.join(format!(
        "ta-0514-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    std::fs::canonicalize(dir).expect("canonicalize temp workspace")
}
