//! Sixth-case RED contract: legacy bare worker pane records must not deadlock
//! explicit leader recovery.
//!
//! References:
//! - `.team/artifacts/legacy-bare-pane-claim-deadlock-locate.md` §8 RED 1/2
//!   and §10 deadlock exit principle.
//! - `.team/artifacts/claim-endpoint-nonconvergence-locate.md` §10 RED 5:
//!   repair next-actions must be restart-after-action exits.
//!
//! User-visible contract:
//! - A stale legacy worker row that only matches the caller by bare `%pane_id`
//!   must not make `claim-leader`/`takeover` refuse `caller_not_leader_shaped`.
//! - A truly live worker tuple must still be refused, so the anti-self-promotion
//!   guard is preserved.
//! - `refused_dirty_topology.next_actions` must point to executable commands
//!   that do not route the operator back into the same stale bare-pane refusal.
//! - In an old-endpoint-dead fixture, repair next-actions must also converge
//!   endpoint state so the next `restart` succeeds instead of repeating the
//!   same dirty-topology refusal.

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
const OLD_SOCKET: &str = "/private/tmp/tmux-501/ta-old";
const NEW_SOCKET: &str = "/private/tmp/tmux-501/ta-new";
const TEAM_SESSION: &str = "team-video-workflow";
const LEADER_SESSION: &str = "team-agent-leader-claude_code-workspace-deadlock-probe";
const LIVE_LEADER_PID: u32 = 14663;
const DEAD_WORKER_PID: u32 = 47641;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[serial(env)]
fn red1_stale_legacy_bare_worker_pane_must_not_block_claim_leader() {
    let case = LegacyPaneCase::new("red1-stale-claim", PaneShape::StaleLegacyWorker);
    let output = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let value = json_output(&output, "RED1 claim-leader stale legacy worker");
    assert_not_caller_not_leader_shaped(
        &case.workspace,
        &output,
        &value,
        "RED1: stale legacy worker pane_id=%0 with dead pane_pid and no tuple proof must not block claim-leader; the lease guard must use endpoint/session/window/pane/pid tuple evidence, not bare pane_id",
    );
    assert!(
        output.status.success() && value.get("ok").and_then(Value::as_bool) == Some(true),
        "RED1: claim-leader should be executable recovery under the stale legacy bare-pane fixture; expected ok=true/claimed or already_bound, got code={:?} value={value} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial(env)]
fn red2_live_worker_tuple_still_refuses_self_promotion() {
    let case = LegacyPaneCase::new("red2-live-worker", PaneShape::LiveWorkerTuple);
    let output = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let value = json_output(&output, "RED2 live worker tuple claim-leader");
    assert_eq!(
        value.get("status").and_then(Value::as_str),
        Some("refused"),
        "RED2: a caller that is the live worker tuple must still be refused, not silently promoted; value={value}"
    );
    assert_eq!(
        value.get("reason").and_then(Value::as_str),
        Some("caller_not_leader_shaped"),
        "RED2: anti-self-promotion guard must remain for endpoint/session/window/pane/pid validated workers; value={value}"
    );
    assert!(
        value
            .get("action")
            .and_then(Value::as_str)
            .is_some_and(|action| action.contains("not a worker pane")),
        "RED2: refusal must tell the operator not to claim from a worker pane; value={value}"
    );
}

#[test]
#[serial(env)]
fn red3_refused_dirty_topology_next_actions_are_executable_deadlock_exits() {
    let case = LegacyPaneCase::new("red3-next-actions", PaneShape::StaleLegacyWorker);
    let restart = case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
    let restart_json = json_output(&restart, "RED3 restart dirty topology");
    assert_eq!(
        restart_json.get("status").and_then(Value::as_str),
        Some("refused_dirty_topology"),
        "RED3 setup: restart must fail closed on the socket split fixture before validating next_actions; value={restart_json}"
    );
    let actions = restart_json
        .get("next_actions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "RED3: refused_dirty_topology must expose executable next_actions; value={restart_json}"
            )
        });
    assert!(
        !actions.is_empty(),
        "RED3: refused_dirty_topology must not leave the operator without a recovery command; value={restart_json}"
    );
    assert!(
        actions.iter().any(|action| {
            action
                .as_str()
                .is_some_and(|text| contains_rebind_command(text))
        }),
        "RED3: next_actions must include an explicit rebind/claim/takeover command, not only prose or diagnostics; actions={actions:?}"
    );

    for action in actions {
        let text = action.as_str().unwrap_or_else(|| {
            panic!("RED3: next_actions entries must be strings; action={action}")
        });
        let argv = executable_team_agent_argv(text).unwrap_or_else(|| {
            panic!(
                "RED3: every refused_dirty_topology next_action must be an executable `team-agent ...` command under the same dirty fixture; got {text:?}"
            )
        });
        let output = case.run_ta_vec(&argv);
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !combined.contains("caller_not_leader_shaped"),
            "RED3: next_action `{text}` executed under the same dirty/stale fixture must not deadlock by returning caller_not_leader_shaped again; code={:?} output={combined}",
            output.status.code()
        );
        if contains_rebind_command(text) {
            let restart_after_action =
                case.run_ta(&["restart", case.workspace_str(), "--team", TEAM, "--json"]);
            let restart_after_json = json_output(
                &restart_after_action,
                "RED3 restart after repair next_action",
            );
            assert_eq!(
                restart_after_json.get("status").and_then(Value::as_str),
                Some("restarted"),
                "RED3/RED5: repair next_action `{text}` must be a true deadlock exit in the old-endpoint-dead fixture; after it succeeds, restart must not repeat the same tmux_endpoint_socket_conflict/leader_receiver_socket_mismatch loop. code={:?} json={restart_after_json} stdout={} stderr={}",
                restart_after_action.status.code(),
                String::from_utf8_lossy(&restart_after_action.stdout),
                String::from_utf8_lossy(&restart_after_action.stderr)
            );
            assert_no_dirty_topology_issue(
                &restart_after_json,
                "tmux_endpoint_socket_conflict",
                "RED3/RED5: restart after repair next_action must not repeat tmux_endpoint_socket_conflict",
            );
            assert_no_dirty_topology_issue(
                &restart_after_json,
                "leader_receiver_socket_mismatch",
                "RED3/RED5: restart after repair next_action must not repeat leader_receiver_socket_mismatch",
            );
        }
    }
}

#[test]
#[serial(env)]
fn red1_takeover_inherits_stale_legacy_bare_worker_fix() {
    let case = LegacyPaneCase::new("red1-stale-takeover", PaneShape::StaleLegacyWorker);
    let output = case.run_ta(&[
        "takeover",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--confirm",
        "--json",
    ]);
    let value = json_output(&output, "RED1 takeover stale legacy worker");
    assert_not_caller_not_leader_shaped(
        &case.workspace,
        &output,
        &value,
        "RED1/takeover: takeover shares the claim lease path and must not be blocked by a dead legacy worker row that only matches the caller by bare pane_id",
    );
}

fn assert_not_caller_not_leader_shaped(
    workspace: &Path,
    output: &Output,
    value: &Value,
    message: &str,
) {
    assert_ne!(
        value.get("reason").and_then(Value::as_str),
        Some("caller_not_leader_shaped"),
        "{message}; value={value}; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !events(workspace).contains("caller_not_leader_shaped"),
        "{message}; event log must not emit caller_not_leader_shaped for stale legacy worker matches; value={value}"
    );
}

fn contains_rebind_command(text: &str) -> bool {
    ["claim-leader", "takeover", "attach-leader"]
        .iter()
        .any(|needle| text.split_whitespace().any(|token| token == *needle))
}

fn executable_team_agent_argv(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    let command = trimmed.strip_prefix("team-agent ")?;
    if command.contains(';') || command.contains('|') || command.contains("&&") {
        return None;
    }
    Some(command.split_whitespace().map(str::to_string).collect())
}

fn assert_no_dirty_topology_issue(value: &Value, issue_id: &str, message: &str) {
    let ids = value
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
        .collect::<Vec<_>>();
    assert!(
        !ids.iter().any(|id| id == issue_id),
        "{message}; issues={ids:?}; json={value}"
    );
}

#[derive(Clone, Copy)]
enum PaneShape {
    StaleLegacyWorker,
    LiveWorkerTuple,
}

struct LegacyPaneCase {
    workspace: PathBuf,
    fake_bin: PathBuf,
}

impl LegacyPaneCase {
    fn new(tag: &str, shape: PaneShape) -> Self {
        let workspace = tmp_dir(tag);
        std::fs::create_dir_all(workspace.join("home")).expect("create isolated home");
        let fake_bin = fake_tmux_bin(&workspace, shape);
        seed_dirty_state(&workspace, shape);
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

    fn run_ta(&self, args: &[&str]) -> Output {
        self.run_ta_vec(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
    }

    fn run_ta_vec(&self, args: &[String]) -> Output {
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
            .env("TMUX_PANE", CALLER_PANE)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-deadlock-red")
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

impl Drop for LegacyPaneCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

fn seed_dirty_state(workspace: &Path, shape: PaneShape) {
    let (window, pane_pid, status) = match shape {
        PaneShape::StaleLegacyWorker => (WORKER, DEAD_WORKER_PID, "stopped"),
        PaneShape::LiveWorkerTuple => (WORKER, DEAD_WORKER_PID, "running"),
    };
    let worker = json!({
        "id": WORKER,
        "name": WORKER,
        "provider": "codex",
        "window": window,
        "pane_id": CALLER_PANE,
        "pane_pid": pane_pid,
        "status": status
    });
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": "codex",
        "pane_id": CALLER_PANE,
        "pane_pid": LIVE_LEADER_PID,
        "session_name": LEADER_SESSION,
        "window_name": "claude_code",
        "tmux_socket": NEW_SOCKET,
        "leader_session_uuid": "legacy-deadlock-leader",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    });
    let owner = json!({
        "pane_id": CALLER_PANE,
        "provider": "codex",
        "pane_pid": LIVE_LEADER_PID,
        "tmux_socket": NEW_SOCKET,
        "leader_session_uuid": "legacy-deadlock-leader",
        "machine_fingerprint": "machine-deadlock-red",
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
  objective: "legacy bare pane claim deadlock contract"
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

fn fake_tmux_bin(workspace: &Path, shape: PaneShape) -> PathBuf {
    let bin_dir = workspace.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    let tmux = bin_dir.join("tmux");
    let line = match shape {
        PaneShape::StaleLegacyWorker => pane_line(
            CALLER_PANE,
            LEADER_SESSION,
            "claude_code",
            "codex",
            workspace,
            LIVE_LEADER_PID,
        ),
        PaneShape::LiveWorkerTuple => pane_line(
            CALLER_PANE,
            TEAM_SESSION,
            WORKER,
            "codex",
            workspace,
            DEAD_WORKER_PID,
        ),
    };
    let old_endpoint_dead = matches!(shape, PaneShape::StaleLegacyWorker);
    let script = format!(
        r#"#!/bin/sh
case " $* " in
  *" -S {old_socket} "*)
    if [ "{old_endpoint_dead}" = "true" ]; then
      echo "no server running on {old_socket}" >&2
      exit 1
    fi
    ;;
esac
case " $* " in
  *" list-panes "*)
    printf '%s' '{line}'
    exit 0
    ;;
  *" display-message "*)
    printf '%s\n' '{pid}'
    exit 0
    ;;
  *" has-session "*)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
        line = shell_single_quoted_payload(&line),
        pid = LIVE_LEADER_PID,
        old_socket = OLD_SOCKET,
        old_endpoint_dead = if old_endpoint_dead { "true" } else { "false" }
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
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0513\t{command}\t1\t{}\t1\t0\t{pid}\n",
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
        "ta-0513-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    std::fs::canonicalize(dir).expect("canonicalize temp workspace")
}
