//! 0.5.35 RED contract: re-running a managed leader provider in its own pane
//! must not overwrite the worker team session identity.
//!
//! References:
//! - `.team/artifacts/managed-leader-provider-reentry-locate.md` §5 / §6 / §8.
//!
//! User-visible contract:
//! - After a managed `team-agent claude` leader provider exits back to a shell,
//!   running `team-agent claude` again in that same tmux pane relaunches the
//!   provider without turning the leader launcher session into the team session.
//! - The same physical pane remains the owner even if the process UUID changes.
//! - A different pane may not silently steal the canonical leader binding.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

const TEAM: &str = "current";
const WORKER: &str = "worker";
const RENDERER_WORKER: &str = "helper";
const TEAM_SESSION: &str = "team-current";
const LEADER_SESSION: &str = "team-agent-leader-claude_code-ws-nonce";
const LEADER_PANE: &str = "%42";
const OTHER_PANE: &str = "%77";
const TMUX_SOCKET: &str = "/Volumes/nvme/tmp/ta-0535-managed-reentry.sock";
const OWNER_EPOCH: u64 = 7;

static CASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[serial(env)]
fn managed_leader_reentry_preserves_worker_session_identity() {
    let case = ReentryCase::new("same-pane");
    case.seed_managed_leader_state("old-managed-uuid");

    let out = case.run_leader_provider(LEADER_PANE, None);
    assert!(
        out.status.success(),
        "RED1 setup: same-pane provider re-entry should be accepted; output={}; tmux_log={}",
        output_text(&out),
        case.tmux_log()
    );

    let state = case.read_state();
    assert_worker_session_not_polluted(&state, "RED1");
    assert_eq!(
        state
            .pointer(&format!("/teams/{TEAM}/leader_receiver/session_name"))
            .and_then(Value::as_str),
        Some(LEADER_SESSION),
        "RED1: leader launcher session belongs only under canonical leader_receiver; state={state}"
    );
    assert_epoch_non_regressive(&state, "RED1");
    assert_stage3_root_owner_absent(&state, "RED1");
}

#[test]
#[serial(env)]
fn same_physical_pane_with_new_uuid_is_still_owner() {
    let case = ReentryCase::new("same-pane-new-uuid");
    case.seed_managed_leader_state("old-managed-uuid");

    let out = case.run_leader_provider(LEADER_PANE, Some("new-provider-process-uuid"));
    let text = output_text(&out);
    assert!(
        out.status.success(),
        "RED2: same physical pane must not be refused solely because the provider process UUID changed; output={text}; tmux_log={}",
        case.tmux_log()
    );
    assert!(
        !text.contains("leader_session_uuid_mismatch")
            && !text.contains("team_owner_mismatch")
            && !text.contains("owner_takeover_required"),
        "RED2: same pane UUID drift is metadata, not authority; output={text}"
    );

    let state = case.read_state();
    assert_worker_session_not_polluted(&state, "RED2");
    assert_eq!(
        state
            .pointer(&format!("/teams/{TEAM}/leader_receiver/pane_id"))
            .and_then(Value::as_str),
        Some(LEADER_PANE),
        "RED2: same physical pane remains the canonical receiver; state={state}"
    );
    assert_epoch_non_regressive(&state, "RED2");
    assert_stage3_root_owner_absent(&state, "RED2");
}

#[test]
#[serial(env)]
fn different_pane_does_not_silently_steal_managed_leader_binding() {
    let case = ReentryCase::new("different-pane");
    case.seed_managed_leader_state("old-managed-uuid");

    let _out = case.run_leader_provider(OTHER_PANE, Some("other-pane-uuid"));
    let state = case.read_state();
    assert_eq!(
        state.pointer(&format!("/teams/{TEAM}/leader_receiver/pane_id"))
            .and_then(Value::as_str),
        Some(LEADER_PANE),
        "RED3: running `team-agent claude` from a different pane must not silently rewrite canonical leader_receiver to {OTHER_PANE}; state={state}; tmux_log={}",
        case.tmux_log()
    );
    assert_worker_session_not_polluted(&state, "RED3");
    assert_stage3_root_owner_absent(&state, "RED3");
}

#[test]
#[serial(env)]
fn status_human_and_summary_render_canonical_unknown_over_legacy_working_health() {
    let case = ReentryCase::new("status-unknown-renderer");
    case.seed_status_unknown_renderer_state();
    case.seed_agent_health(RENDERER_WORKER, "WORKING");

    let workspace = case.workspace_str();
    let human = case.run_ta(
        &[
            "status",
            "--workspace",
            &workspace,
            "--team",
            TEAM,
            "--detail",
        ],
        LEADER_PANE,
        None,
    );
    let human_text = output_text(&human);
    assert!(
        human.status.success(),
        "R4 setup: status --detail must render the fixture; output={human_text}; state={}",
        case.read_state()
    );
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    let mut failures = Vec::new();
    if !human_stdout.contains("helper,未知") {
        failures.push(format!(
            "human status must render helper,未知 when canonical worker_state=UNKNOWN/activity=uncertain; output={human_text}"
        ));
    }
    if human_stdout.contains("helper,工作") || human_stdout.contains("helper,空闲") {
        failures.push(format!(
            "human status must not render helper,工作 or helper,空闲 from legacy agent_health=WORKING; output={human_text}"
        ));
    }

    let summary = case.run_ta(
        &[
            "status",
            "--workspace",
            &workspace,
            "--team",
            TEAM,
            "--summary",
        ],
        LEADER_PANE,
        None,
    );
    let summary_text = output_text(&summary);
    assert!(
        summary.status.success(),
        "R4 setup: status --summary must render the fixture; output={summary_text}; state={}",
        case.read_state()
    );
    if !String::from_utf8_lossy(&summary.stdout)
        .contains("agents: 1 — running=0 busy=0 idle=0 stopped=0 failed=0 unknown=1")
    {
        failures.push(format!(
            "summary must count the conflict fixture as unknown=1 busy=0 idle=0; output={summary_text}"
        ));
    }
    assert!(
        failures.is_empty(),
        "R4: status renderer must prefer canonical UNKNOWN/uncertain over legacy working health.\n{}\nstate={}",
        failures.join("\n"),
        case.read_state()
    );
}

struct ReentryCase {
    _env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
}

impl ReentryCase {
    fn new(tag: &str) -> Self {
        let id = CASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let env = hermetic_guard::HermeticTestEnv::enter(&format!("0535-{tag}-{id}"));
        let workspace = env.workspace(tag);
        let fake_bin = workspace.join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        let case = Self {
            _env: env,
            workspace,
            fake_bin,
        };
        case.write_fake_provider();
        case.write_fake_tmux();
        case
    }

    fn seed_managed_leader_state(&self, uuid: &str) {
        let receiver = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "claude_code",
            "pane_id": LEADER_PANE,
            "pane": LEADER_PANE,
            "pane_pid": 53_542,
            "session_name": LEADER_SESSION,
            "window_name": "claude_code",
            "tmux_socket": TMUX_SOCKET,
            "leader_session_uuid": uuid,
            "owner_epoch": OWNER_EPOCH,
            "attached_at": "2026-07-13T00:00:00Z",
            "discovery": "managed_leader"
        });
        let owner = json!({
            "pane_id": LEADER_PANE,
            "provider": "claude_code",
            "pane_pid": 53_542,
            "tmux_socket": TMUX_SOCKET,
            "leader_session_uuid": uuid,
            "machine_fingerprint": "machine-0535",
            "owner_epoch": OWNER_EPOCH,
            "claimed_at": "2026-07-13T00:00:00Z",
            "claimed_via": "claim-leader",
            "os_user": "tester"
        });
        let state = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": self.workspace_str(),
            "spec_path": self.workspace.join("team.spec.yaml").to_string_lossy(),
            "tmux_endpoint": TMUX_SOCKET,
            "tmux_socket": TMUX_SOCKET,
            "leader": { "id": "leader", "provider": "fake" },
            "agents": {
                WORKER: {
                    "id": WORKER,
                    "provider": "fake",
                    "model": "fake",
                    "window": WORKER,
                    "status": "running",
                    "pane_id": "%9",
                    "pane_pid": 53_509,
                    "owner_team_id": TEAM
                }
            },
            "teams": {
                TEAM: {
                    "active_team_key": TEAM,
                    "team_key": TEAM,
                    "session_name": TEAM_SESSION,
                    "team_dir": self.workspace_str(),
                    "spec_path": self.workspace.join("team.spec.yaml").to_string_lossy(),
                    "tmux_endpoint": TMUX_SOCKET,
                    "tmux_socket": TMUX_SOCKET,
                    "leader": { "id": "leader", "provider": "fake" },
                    "agents": {
                        WORKER: {
                            "id": WORKER,
                            "provider": "fake",
                            "model": "fake",
                            "window": WORKER,
                            "status": "running",
                            "pane_id": "%9",
                            "pane_pid": 53_509,
                            "owner_team_id": TEAM
                        }
                    },
                    "leader_receiver": receiver,
                    "team_owner": owner,
                    "owner_epoch": OWNER_EPOCH
                }
            }
        });
        fs::write(
            self.workspace.join("team.spec.yaml"),
            format!(
                "version: 1\nteam:\n  id: {TEAM}\n  name: {TEAM}\n  session_name: {TEAM_SESSION}\n  workspace: \"{}\"\nleader:\n  provider: fake\nagents:\n  - id: {WORKER}\n    provider: fake\n    model: fake\n    role: Worker\n    window: {WORKER}\ntasks: []\n",
                self.workspace.display()
            ),
        )
        .expect("write team spec");
        save_runtime_state(&self.workspace, &state).expect("seed runtime state");
    }

    fn seed_status_unknown_renderer_state(&self) {
        let agent = json!({
            "agent_id": RENDERER_WORKER,
            "id": RENDERER_WORKER,
            "provider": "fake",
            "model": "fake",
            "window": RENDERER_WORKER,
            "status": "running",
            "worker_state": "UNKNOWN",
            "activity": {
                "status": "uncertain",
                "confidence": 0.6,
                "rationale": "fake_ready_structural"
            },
            "pane_id": "%9",
            "pane_pid": 53_509,
            "owner_team_id": TEAM,
            "spawn_cwd": self.workspace_str(),
            "spawned_at": "2026-07-13T00:00:00Z"
        });
        let state = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": TEAM_SESSION,
            "team_dir": self.workspace_str(),
            "spec_path": self.workspace.join("team.spec.yaml").to_string_lossy(),
            "tmux_endpoint": TMUX_SOCKET,
            "tmux_socket": TMUX_SOCKET,
            "leader": { "id": "leader", "provider": "fake" },
            "agents": {
                RENDERER_WORKER: agent
            },
            "teams": {
                TEAM: {
                    "active_team_key": TEAM,
                    "team_key": TEAM,
                    "session_name": TEAM_SESSION,
                    "team_dir": self.workspace_str(),
                    "spec_path": self.workspace.join("team.spec.yaml").to_string_lossy(),
                    "tmux_endpoint": TMUX_SOCKET,
                    "tmux_socket": TMUX_SOCKET,
                    "leader": { "id": "leader", "provider": "fake" },
                    "agents": {
                        RENDERER_WORKER: agent
                    },
                    "owner_epoch": OWNER_EPOCH
                }
            }
        });
        fs::write(
            self.workspace.join("team.spec.yaml"),
            format!(
                "version: 1\nteam:\n  id: {TEAM}\n  name: {TEAM}\n  session_name: {TEAM_SESSION}\n  workspace: \"{}\"\nleader:\n  provider: fake\nagents:\n  - id: {RENDERER_WORKER}\n    provider: fake\n    model: fake\n    role: Helper\n    window: {RENDERER_WORKER}\ntasks: []\n",
                self.workspace.display()
            ),
        )
        .expect("write renderer team spec");
        save_runtime_state(&self.workspace, &state).expect("seed renderer runtime state");
    }

    fn seed_agent_health(&self, agent_id: &str, status: &str) {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = open_db(store.db_path()).expect("open team db");
        conn.execute(
            "insert or replace into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                TEAM,
                agent_id,
                status,
                "2026-07-13T00:00:00Z",
                42_i64,
                Option::<String>::None,
                "2026-07-13T00:00:00Z"
            ],
        )
        .expect("seed agent_health");
    }

    fn run_leader_provider(&self, pane: &str, uuid_override: Option<&str>) -> Output {
        self.run_ta(&["claude", "--json"], pane, uuid_override)
    }

    fn run_ta(&self, args: &[&str], pane: &str, uuid_override: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self._env.home())
            .env("TEAM_AGENT_TEST_TMP", self._env.root())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{TMUX_SOCKET},12345,0"))
            .env("TMUX_PANE", pane)
            .env("TEAM_AGENT_LEADER_PROVIDER", "claude_code")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0535");
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
        if let Some(uuid) = uuid_override {
            command.env("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", uuid);
        }
        command.output().expect("run team-agent claude")
    }

    fn write_fake_provider(&self) {
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "--version" ]; then
  printf 'Claude Code 2.1.181\n'
  exit 0
fi
exit 0
"#,
            self.workspace.join("fake-claude.log").display()
        );
        write_executable(&self.fake_bin.join("claude"), &script);
    }

    fn write_fake_tmux(&self) {
        let leader_line = pane_line(
            LEADER_PANE,
            LEADER_SESSION,
            "claude_code",
            "zsh",
            &self.workspace,
            53_542,
        );
        let other_line = pane_line(
            OTHER_PANE,
            "user-shell",
            "shell",
            "zsh",
            &self.workspace,
            53_577,
        );
        let worker_line = pane_line(
            "%9",
            TEAM_SESSION,
            WORKER,
            "team-agent",
            &self.workspace,
            53_509,
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
case " $* " in
  *" list-panes "*)
    printf '%s' '{leader_line}'
    printf '%s' '{other_line}'
    printf '%s' '{worker_line}'
    exit 0
    ;;
  *" display-message "*)
    case "$target" in
      "{leader_pane}") printf '%s\n' '53542'; exit 0 ;;
      "{other_pane}") printf '%s\n' '53577'; exit 0 ;;
      "%9") printf '%s\n' '53509'; exit 0 ;;
      *) printf '%s\n' '{leader_pane}'; exit 0 ;;
    esac
    ;;
  *" has-session "*)
    case "$target" in
      "{team_session}"|"{leader_session}"|"user-shell") exit 0 ;;
      *) exit 1 ;;
    esac
    ;;
  *" list-sessions "*)
    printf '%s\n' '{team_session}: 1 windows'
    printf '%s\n' '{leader_session}: 1 windows'
    printf '%s\n' 'user-shell: 1 windows'
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
            log_path = shell_single_quoted_payload(&self.tmux_log_path().to_string_lossy()),
            leader_line = shell_single_quoted_payload(&leader_line),
            other_line = shell_single_quoted_payload(&other_line),
            worker_line = shell_single_quoted_payload(&worker_line),
            leader_pane = LEADER_PANE,
            other_pane = OTHER_PANE,
            team_session = TEAM_SESSION,
            leader_session = LEADER_SESSION,
        );
        write_executable(&self.fake_bin.join("tmux"), &script);
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

    fn workspace_str(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }
}

fn assert_worker_session_not_polluted(state: &Value, label: &str) {
    assert_eq!(
        state.get("session_name").and_then(Value::as_str),
        Some(TEAM_SESSION),
        "{label}: root session_name is worker team identity and must not become a leader launcher session; state={state}"
    );
    assert_eq!(
        state.pointer(&format!("/teams/{TEAM}/session_name"))
            .and_then(Value::as_str),
        Some(TEAM_SESSION),
        "{label}: teams.{TEAM}.session_name is worker team identity and must not become a leader launcher session; state={state}"
    );
    assert!(
        !state
            .pointer(&format!("/teams/{TEAM}/session_name"))
            .and_then(Value::as_str)
            .is_some_and(|session| session.starts_with("team-agent-leader-")),
        "{label}: leader-prefixed session must never be written as worker session; state={state}"
    );
}

fn assert_epoch_non_regressive(state: &Value, label: &str) {
    let epoch = state
        .pointer(&format!("/teams/{TEAM}/owner_epoch"))
        .and_then(Value::as_u64)
        .or_else(|| {
            state
                .pointer(&format!("/teams/{TEAM}/leader_receiver/owner_epoch"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    assert!(
        epoch >= OWNER_EPOCH,
        "{label}: same-pane provider re-entry must not reset owner_epoch below {OWNER_EPOCH}; state={state}"
    );
}

fn assert_stage3_root_owner_absent(state: &Value, label: &str) {
    assert!(
        state.get("team_owner").is_none()
            && state.get("leader_receiver").is_none()
            && state.get("owner_epoch").is_none(),
        "{label}: Stage3 canonical-only save must not reintroduce raw root owner/receiver fields; state={state}"
    );
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
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0535\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn output_text(out: &Output) -> String {
    format!(
        "status={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod executable");
    }
}

fn shell_single_quoted_payload(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('\'', "'\"'\"'")
}
