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
//! - The accepted same-pane fixture has a real controlling tty that matches the
//!   live pane; missing/mismatched tty or foreign workspace stays refused with
//!   the catalog reason and fact set that describes what was actually observed.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use serial_test::serial;
use sha2::{Digest, Sha256};
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;
use team_agent::model::pane_authority_refusal as refusal_catalog;
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
    assert_eq!(
        case.provider_launches(),
        1,
        "RED1 positive control: verified same-pane re-entry must reach the provider shim"
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
    assert_eq!(
        case.provider_launches(),
        1,
        "RED2 positive control: verified same-pane UUID drift must still reach the provider shim"
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
fn ambient_pane_without_controlling_tty_is_refused() {
    let case = ReentryCase::new("ambient-without-controlling-tty");
    case.seed_managed_leader_state("old-managed-uuid");
    case.seed_refusal_durable_canaries();
    let durable_before = case.refusal_durable_snapshot();

    let out = case.run_leader_provider_without_controlling_tty(LEADER_PANE);

    assert_catalog_refusal(
        &case,
        &out,
        refusal_catalog::PaneAuthorityRefusalReason::CallerControllingTtyUnavailable,
        Some((
            refusal_catalog::PaneAuthorityRefusalField::CallerControllingTty,
            refusal_catalog::CallerControllingTtyUnavailableCause::NoControllingTty.as_str(),
        )),
        "NEG1 tty unverifiable",
    );
    case.assert_refusal_durable_unchanged(&durable_before, "NEG1 tty unverifiable");
}

#[test]
#[serial(env)]
fn controlling_tty_not_matching_ambient_pane_is_refused() {
    let case = ReentryCase::new("ambient-mismatched-controlling-tty");
    case.seed_managed_leader_state("old-managed-uuid");
    case.seed_refusal_durable_canaries();
    let durable_before = case.refusal_durable_snapshot();

    let out = case.run_leader_provider_with_mismatched_tty(LEADER_PANE);

    assert_catalog_refusal(
        &case,
        &out,
        refusal_catalog::PaneAuthorityRefusalReason::PaneTtyMismatch,
        None,
        "NEG2 tty/pane mismatch",
    );
    assert_tty_mismatch_facts(&out, "NEG2 tty/pane mismatch");
    case.assert_refusal_durable_unchanged(&durable_before, "NEG2 tty/pane mismatch");
}

#[test]
#[serial(env)]
fn matching_tty_in_foreign_workspace_is_refused() {
    let case = ReentryCase::new("ambient-foreign-workspace");
    case.seed_managed_leader_state("old-managed-uuid");
    case.seed_refusal_durable_canaries();
    let durable_before = case.refusal_durable_snapshot();

    let out = case.run_leader_provider_from_foreign_workspace(LEADER_PANE);

    assert_catalog_refusal(
        &case,
        &out,
        refusal_catalog::PaneAuthorityRefusalReason::PaneWorkspaceMismatch,
        None,
        "NEG3 workspace mismatch",
    );
    assert_workspace_mismatch_facts(&out, "NEG3 workspace mismatch");
    case.assert_refusal_durable_unchanged(&durable_before, "NEG3 workspace mismatch");
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
    foreign_workspace: PathBuf,
    fake_bin: PathBuf,
}

impl ReentryCase {
    fn new(tag: &str) -> Self {
        let id = CASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let env = hermetic_guard::HermeticTestEnv::enter(&format!("0535-{tag}-{id}"));
        let workspace = env.workspace(tag);
        let foreign_workspace = env.workspace(&format!("{tag}-foreign"));
        let fake_bin = workspace.join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        let case = Self {
            _env: env,
            workspace,
            foreign_workspace,
            fake_bin,
        };
        case.write_fake_provider();
        case.write_fake_tmux(LEADER_PANE, "/dev/ttys0535", &case.workspace);
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

    fn seed_refusal_durable_canaries(&self) {
        let store = MessageStore::open(&self.workspace).expect("seed refusal message store");
        store
            .create_message(
                None,
                "existing-worker",
                "leader",
                "reentry refusal durable canary",
                None,
                false,
                Some(TEAM),
            )
            .expect("seed refusal durable message row");
        let registry_path = self
            ._env
            .home()
            .join(".team-agent/leaders/reentry-canary.json");
        fs::write(
            registry_path,
            serde_json::to_vec_pretty(&json!({
                "team_key": TEAM,
                "workspace": self.workspace,
                "pane_id": LEADER_PANE,
            }))
            .expect("serialize refusal registry canary"),
        )
        .expect("seed refusal registry canary");
    }

    fn refusal_durable_snapshot(&self) -> RefusalDurableSnapshot {
        RefusalDurableSnapshot::capture(
            &self.workspace.join(".team"),
            &self._env.home().join(".team-agent/leaders"),
            &self.workspace.join(".team/runtime/team.db"),
        )
    }

    fn assert_refusal_durable_unchanged(&self, before: &RefusalDurableSnapshot, label: &str) {
        let after = self.refusal_durable_snapshot();
        assert_eq!(
            &after, before,
            "{label}: refusal must leave state, DB, registry and every durable byte/mtime/inode \
             unchanged; before={before:#?} after={after:#?}"
        );
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
        self.run_ta_with_controlling_tty(
            &["claude", "--json"],
            pane,
            uuid_override,
            None,
            &self.workspace,
        )
    }

    fn run_leader_provider_without_controlling_tty(&self, pane: &str) -> Output {
        self.write_fake_tmux(pane, "/dev/ttys0535", &self.workspace);
        let (stdin_master, stdin_slave, _stdin_tty) =
            open_pty().expect("allocate redirected stdin tty");
        let stdin_rdev =
            fd_rdev(stdin_slave.as_raw_fd()).expect("measure no-control stdin slave rdev");
        let measurement_path = self._env.root().join("no-controlling-tty-measurement");
        let measurement_file =
            measurement_file(&measurement_path).expect("create no-control measurement file");
        let measurement_fd = measurement_file.as_raw_fd();
        let mut command = self.command(&["claude", "--json"], pane, None);
        command
            .stdin(Stdio::from(stdin_slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                write_final_process_tty_measurement(measurement_fd, u64::MAX, stdin_rdev)?;
                Ok(())
            });
        }
        let child = command
            .spawn()
            .expect("spawn team-agent CLI without controlling tty");
        let output = child
            .wait_with_output()
            .expect("wait for team-agent CLI without controlling tty");
        drop((stdin_master, measurement_file));
        read_tty_measurement(&measurement_path)
            .assert_no_controlling_tty("NEG1 final product-caller process");
        output
    }

    fn run_leader_provider_with_mismatched_tty(&self, pane: &str) -> Output {
        let (_other_master, _other_slave, other_tty) =
            open_pty().expect("allocate distinct pane tty");
        self.run_ta_with_controlling_tty(
            &["claude", "--json"],
            pane,
            None,
            Some(&other_tty),
            &self.workspace,
        )
    }

    fn run_leader_provider_from_foreign_workspace(&self, pane: &str) -> Output {
        self.run_ta_with_controlling_tty(
            &["claude", "--json"],
            pane,
            None,
            None,
            &self.foreign_workspace,
        )
    }

    fn run_ta(&self, args: &[&str], pane: &str, uuid_override: Option<&str>) -> Output {
        self.write_fake_tmux(pane, "/dev/ttys0535", &self.workspace);
        self.command(args, pane, uuid_override)
            .output()
            .expect("run team-agent CLI")
    }

    fn run_ta_with_controlling_tty(
        &self,
        args: &[&str],
        pane: &str,
        uuid_override: Option<&str>,
        observed_pane_tty: Option<&str>,
        observed_workspace: &Path,
    ) -> Output {
        let (master, slave, caller_tty) = open_pty().expect("allocate controlling tty");
        let expected_tdev = fd_rdev(slave.as_raw_fd()).expect("measure controlling slave rdev");
        let measurement_path = self._env.root().join("controlling-tty-measurement");
        let measurement_file =
            measurement_file(&measurement_path).expect("create controlling-tty measurement file");
        let measurement_fd = measurement_file.as_raw_fd();
        self.write_fake_tmux(
            pane,
            observed_pane_tty.unwrap_or(&caller_tty),
            observed_workspace,
        );
        let mut command = self.command(args, pane, uuid_override);
        command
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                write_final_process_tty_measurement(measurement_fd, expected_tdev, expected_tdev)?;
                Ok(())
            });
        }
        let child = command
            .spawn()
            .expect("spawn team-agent CLI with controlling tty");
        let output = child
            .wait_with_output()
            .expect("wait for team-agent CLI with controlling tty");
        drop((master, measurement_file));
        read_tty_measurement(&measurement_path).assert_controlling_tty(
            expected_tdev,
            expected_tdev,
            "reentry matching/mismatch final product-caller process",
        );
        output
    }

    fn command(&self, args: &[&str], pane: &str, uuid_override: Option<&str>) -> Command {
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
        command
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

    fn write_fake_tmux(&self, caller_pane: &str, caller_tty: &str, caller_workspace: &Path) {
        let leader_line = pane_line(
            LEADER_PANE,
            LEADER_SESSION,
            "claude_code",
            "zsh",
            if caller_pane == LEADER_PANE {
                caller_workspace
            } else {
                &self.workspace
            },
            53_542,
            if caller_pane == LEADER_PANE {
                caller_tty
            } else {
                "/dev/ttys0535"
            },
        );
        let other_line = pane_line(
            OTHER_PANE,
            "user-shell",
            "shell",
            "zsh",
            if caller_pane == OTHER_PANE {
                caller_workspace
            } else {
                &self.workspace
            },
            53_577,
            if caller_pane == OTHER_PANE {
                caller_tty
            } else {
                "/dev/ttys0577"
            },
        );
        let worker_line = pane_line(
            "%9",
            TEAM_SESSION,
            WORKER,
            "team-agent",
            &self.workspace,
            53_509,
            "/dev/ttys0509",
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

    fn provider_launches(&self) -> usize {
        fs::read_to_string(self.workspace.join("fake-claude.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.starts_with("--version"))
            .count()
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

fn assert_catalog_refusal(
    case: &ReentryCase,
    out: &Output,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
    unavailable: Option<(refusal_catalog::PaneAuthorityRefusalField, &str)>,
    label: &str,
) {
    let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|error| {
        panic!(
            "{label}: refusal stdout must remain structured JSON: {error}; output={}",
            output_text(out)
        )
    });
    assert!(
        !out.status.success(),
        "{label}: an unverifiable ambient authority must fail closed; output={value}"
    );
    assert_eq!(
        value["ok"],
        json!(false),
        "{label}: refusal must be a typed negative result; output={value}"
    );
    assert_eq!(
        value["status"],
        json!("not_started"),
        "{label}: refusal must happen before provider launch; output={value}"
    );
    let object = find_reason_object(&value, reason).unwrap_or_else(|| {
        panic!(
            "{label}: refusal must retain catalog reason {}; output={value}",
            reason.as_str()
        )
    });
    assert_catalog_fact_set(object, reason, unavailable, label);
    assert_recovery_action(object, label);
    assert_eq!(
        case.provider_launches(),
        0,
        "{label}: typed refusal must not reach the provider shim"
    );
    assert!(
        case.tmux_log().contains("list-panes"),
        "{label}: anti-vacuous canary — the public CLI must inspect the ambient pane before refusing; tmux_log={}",
        case.tmux_log()
    );
    for forbidden in [
        "new-session",
        "new-window",
        "attach-session",
        "switch-client",
    ] {
        assert!(
            !contains_tmux_operation(&case.tmux_log(), forbidden),
            "{label}: typed refusal must not silently fall back to Managed; \
             forbidden={forbidden} tmux_log={}",
            case.tmux_log()
        );
    }
}

fn find_reason_object(
    value: &Value,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
) -> Option<&serde_json::Map<String, Value>> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_reason_object(value, reason)),
        Value::Object(object) => {
            if object
                .get(refusal_catalog::REASON_FIELD)
                .and_then(Value::as_str)
                == Some(reason.as_str())
            {
                Some(object)
            } else {
                object
                    .values()
                    .find_map(|value| find_reason_object(value, reason))
            }
        }
        _ => None,
    }
}

fn assert_catalog_fact_set(
    object: &serde_json::Map<String, Value>,
    reason: refusal_catalog::PaneAuthorityRefusalReason,
    unavailable: Option<(refusal_catalog::PaneAuthorityRefusalField, &str)>,
    label: &str,
) {
    assert!(
        refusal_catalog::PaneAuthorityRefusalReason::ALL.contains(&reason),
        "{label}: reason identity must come from the product catalog"
    );
    let mut expected = reason
        .required_fact_fields()
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    let mut all_fields = refusal_catalog::PaneAuthorityRefusalReason::ALL
        .iter()
        .flat_map(|reason| reason.required_fact_fields())
        .map(|field| field.as_str())
        .collect::<Vec<_>>();
    all_fields.sort_unstable();
    all_fields.dedup();
    let mut actual = object
        .keys()
        .filter_map(|key| all_fields.contains(&key.as_str()).then_some(key.as_str()))
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(
        actual,
        expected,
        "{label}: reason→required-field identity must be projected from the one product catalog; \
         reason={} output={}",
        reason.as_str(),
        Value::Object(object.clone())
    );
    for field in reason.required_fact_fields() {
        let value = object.get(field.as_str()).unwrap_or_else(|| {
            panic!(
                "{label}: required field {} missing for {}; output={}",
                field.as_str(),
                reason.as_str(),
                Value::Object(object.clone())
            )
        });
        if let Some((unavailable_field, expected_cause)) = unavailable
            .as_ref()
            .filter(|(unavailable_field, _)| unavailable_field == field)
        {
            let typed = value.as_object().unwrap_or_else(|| {
                panic!(
                    "{label}: unavailable field {} must be availability+cause; value={value}",
                    unavailable_field.as_str()
                )
            });
            assert_eq!(
                typed
                    .get(refusal_catalog::AVAILABILITY_FIELD)
                    .and_then(Value::as_str),
                Some(refusal_catalog::PaneAuthorityFactAvailability::Unavailable.as_str()),
                "{label}: availability identity must come from catalog; value={value}"
            );
            assert_eq!(
                typed
                    .get(refusal_catalog::CAUSE_FIELD)
                    .and_then(Value::as_str),
                Some(*expected_cause),
                "{label}: unavailable cause must identify why observation failed; value={value}"
            );
            assert!(
                typed.values().all(|value| {
                    value
                        .as_str()
                        .is_none_or(|value| !value.trim().is_empty() && value != "unknown")
                }),
                "{label}: unavailable fact must never collapse to empty/unknown; value={value}"
            );
        } else {
            let legal = match value {
                Value::String(value) => !value.trim().is_empty() && value != "unknown",
                Value::Number(_) => true,
                _ => false,
            };
            assert!(
                legal,
                "{label}: available field {} must use a legal scalar shape; value={value}",
                field.as_str()
            );
        }
    }
}

fn assert_workspace_mismatch_facts(out: &Output, label: &str) {
    use refusal_catalog::{
        PaneAuthorityRefusalField as Field, PaneAuthorityRefusalReason as Reason,
    };

    let value: Value = serde_json::from_slice(&out.stdout).expect("workspace refusal JSON");
    let object = find_reason_object(&value, Reason::PaneWorkspaceMismatch)
        .unwrap_or_else(|| panic!("{label}: workspace mismatch missing; output={value}"));
    let requested = object[Field::RequestedWorkspace.as_str()]
        .as_str()
        .expect("requested workspace");
    let observed = object[Field::ObservedPaneWorkspace.as_str()]
        .as_str()
        .expect("observed pane workspace");
    assert!(
        Path::new(requested).is_absolute() && Path::new(observed).is_absolute(),
        "{label}: workspace facts must be self-locating absolute paths; output={value}"
    );
    assert_ne!(
        requested, observed,
        "{label}: mismatch reason is legal only for two observed, different workspace identities; \
         output={value}"
    );
}

fn assert_tty_mismatch_facts(out: &Output, label: &str) {
    use refusal_catalog::{
        PaneAuthorityRefusalField as Field, PaneAuthorityRefusalReason as Reason,
    };

    let value: Value = serde_json::from_slice(&out.stdout).expect("tty refusal JSON");
    let object = find_reason_object(&value, Reason::PaneTtyMismatch)
        .unwrap_or_else(|| panic!("{label}: tty mismatch missing; output={value}"));
    let caller = object[Field::CallerControllingTty.as_str()]
        .as_u64()
        .expect("caller tty identity");
    let pane = object[Field::ObservedPaneTty.as_str()]
        .as_u64()
        .expect("observed pane tty identity");
    assert_ne!(
        caller, pane,
        "{label}: tty mismatch reason requires two measured, different device identities; \
         output={value}"
    );
}

fn assert_recovery_action(object: &serde_json::Map<String, Value>, label: &str) {
    assert_eq!(
        object
            .get(refusal_catalog::ACTION_REQUIRED_FIELD)
            .and_then(Value::as_bool),
        Some(refusal_catalog::PaneAuthorityRecovery::REQUIRED.action_required),
        "{label}: action_required must come from the catalog"
    );
    let hint = object
        .get(refusal_catalog::HINT_ACTION_FIELD)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        hint.split_whitespace()
            .collect::<Vec<_>>()
            .starts_with(&["team-agent", "attach-leader"]),
        "{label}: recovery hint must be a directly executable catalog action; output={}",
        Value::Object(object.clone())
    );
    let action = object
        .get(refusal_catalog::ACTION_FIELD)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        action.contains("terminal")
            && action.contains("outside")
            && (action.contains("tmux") || action.contains("pane")),
        "{label}: clean-terminal guidance must state that the new terminal is outside the current \
         tmux/pane; output={}",
        Value::Object(object.clone())
    );
}

fn contains_tmux_operation(log: &str, operation: &str) -> bool {
    log.lines()
        .any(|line| line.split_whitespace().any(|part| part == operation))
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct FinalProcessTtyMeasurement {
    dev_tty_fd: i32,
    dev_tty_errno: i32,
    proc_pidinfo_bytes: i32,
    e_tdev: u32,
    expected_tdev: u64,
    stdin_rdev: u64,
}

impl FinalProcessTtyMeasurement {
    fn assert_controlling_tty(&self, expected_tdev: u64, stdin_rdev: u64, label: &str) {
        assert!(
            self.dev_tty_fd >= 0,
            "{label}: final product-caller process must open /dev/tty after TIOCSCTTY; \
             measurement={self:?}"
        );
        assert_eq!(
            (self.expected_tdev, self.stdin_rdev),
            (expected_tdev, stdin_rdev)
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                self.proc_pidinfo_bytes as usize,
                std::mem::size_of::<libc::proc_bsdinfo>(),
                "{label}: proc_pidinfo(PROC_PIDTBSDINFO) must read a complete measurement; \
                 measurement={self:?}"
            );
            assert_eq!(
                u64::from(self.e_tdev),
                expected_tdev,
                "{label}: TIOCSCTTY positive control must report the stdin slave rdev, not the \
                 generic /dev/tty alias; measurement={self:?}"
            );
        }
    }

    fn assert_no_controlling_tty(&self, label: &str) {
        assert_eq!(
            (self.dev_tty_fd, self.dev_tty_errno),
            (-1, libc::ENXIO),
            "{label}: final product-caller process must observe open(/dev/tty)=ENXIO; \
             measurement={self:?}"
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                self.proc_pidinfo_bytes as usize,
                std::mem::size_of::<libc::proc_bsdinfo>(),
                "{label}: negative measurement must prove proc_pidinfo works on the same \
                 process; measurement={self:?}"
            );
            assert_eq!(
                self.e_tdev,
                u32::MAX,
                "{label}: setsid without TIOCSCTTY must retain NODEV; measurement={self:?}"
            );
        }
    }
}

fn measurement_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)
}

fn fd_rdev(fd: i32) -> io::Result<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() }.st_rdev as u64)
}

fn write_final_process_tty_measurement(
    output_fd: i32,
    expected_tdev: u64,
    stdin_rdev: u64,
) -> io::Result<()> {
    let dev_tty_fd = unsafe {
        libc::open(
            b"/dev/tty\0".as_ptr().cast(),
            libc::O_RDONLY | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    let dev_tty_errno = if dev_tty_fd == -1 {
        last_errno()
    } else {
        unsafe {
            libc::close(dev_tty_fd);
        }
        0
    };
    let (proc_pidinfo_bytes, e_tdev) = final_process_bsd_tty();
    let measurement = FinalProcessTtyMeasurement {
        dev_tty_fd,
        dev_tty_errno,
        proc_pidinfo_bytes,
        e_tdev,
        expected_tdev,
        stdin_rdev,
    };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!(measurement).cast::<u8>(),
            std::mem::size_of::<FinalProcessTtyMeasurement>(),
        )
    };
    let written = unsafe {
        libc::pwrite(
            output_fd,
            bytes.as_ptr().cast(),
            bytes.len(),
            0 as libc::off_t,
        )
    };
    if written != bytes.len() as isize {
        return Err(if written == -1 {
            io::Error::last_os_error()
        } else {
            io::Error::new(io::ErrorKind::WriteZero, "short tty measurement write")
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn final_process_bsd_tty() -> (i32, u32) {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    let e_tdev = if read == size as i32 {
        unsafe { info.assume_init() }.e_tdev
    } else {
        u32::MAX
    };
    (read, e_tdev)
}

#[cfg(not(target_os = "macos"))]
fn final_process_bsd_tty() -> (i32, u32) {
    (-1, u32::MAX)
}

#[cfg(target_os = "macos")]
fn last_errno() -> i32 {
    unsafe { *libc::__error() }
}

#[cfg(not(target_os = "macos"))]
fn last_errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

fn read_tty_measurement(path: &Path) -> FinalProcessTtyMeasurement {
    let bytes = fs::read(path).expect("read final-process tty measurement");
    assert_eq!(
        bytes.len(),
        std::mem::size_of::<FinalProcessTtyMeasurement>(),
        "final-process tty measurement must be complete; path={} bytes={}",
        path.display(),
        bytes.len()
    );
    unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<FinalProcessTtyMeasurement>()) }
}

#[derive(Debug, PartialEq, Eq)]
struct RefusalDurableSnapshot {
    team_tree: Vec<DurableEntry>,
    leader_registry_tree: Vec<DurableEntry>,
    db: Option<ImmutableDbFacts>,
}

impl RefusalDurableSnapshot {
    fn capture(team_root: &Path, registry_root: &Path, db_path: &Path) -> Self {
        Self {
            team_tree: durable_tree(team_root),
            leader_registry_tree: durable_tree(registry_root),
            db: db_path.exists().then(|| ImmutableDbFacts::capture(db_path)),
        }
    }
}

#[derive(PartialEq, Eq)]
struct DurableEntry {
    relative_path: PathBuf,
    kind: &'static str,
    bytes: Option<Vec<u8>>,
    sha256: Option<[u8; 32]>,
    inode: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl std::fmt::Debug for DurableEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableEntry")
            .field("relative_path", &self.relative_path)
            .field("kind", &self.kind)
            .field("bytes_len", &self.bytes.as_ref().map(Vec::len))
            .field("sha256", &self.sha256)
            .field("inode", &self.inode)
            .field("mtime", &self.mtime)
            .field("ctime", &self.ctime)
            .finish()
    }
}

fn durable_tree(root: &Path) -> Vec<DurableEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<DurableEntry>) {
        let metadata = fs::symlink_metadata(path).expect("snapshot durable metadata");
        let file_type = metadata.file_type();
        let (kind, bytes) = if file_type.is_dir() {
            ("directory", None)
        } else if file_type.is_file() {
            (
                "file",
                Some(fs::read(path).expect("snapshot durable file bytes")),
            )
        } else if file_type.is_symlink() {
            (
                "symlink",
                Some(
                    fs::read_link(path)
                        .expect("snapshot durable symlink")
                        .to_string_lossy()
                        .as_bytes()
                        .to_vec(),
                ),
            )
        } else {
            ("other", None)
        };
        let sha256 = bytes
            .as_ref()
            .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
        entries.push(DurableEntry {
            relative_path: path
                .strip_prefix(root)
                .expect("durable path must stay under root")
                .to_path_buf(),
            kind,
            bytes,
            sha256,
            inode: metadata.ino(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        });
        if file_type.is_dir() {
            let mut children = fs::read_dir(path)
                .expect("enumerate durable tree")
                .map(|entry| entry.expect("read durable tree entry").path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
        }
    }

    let mut entries = Vec::new();
    if root.exists() {
        visit(root, root, &mut entries);
    }
    entries
}

#[derive(Debug, PartialEq, Eq)]
struct ImmutableDbFacts {
    user_version: i64,
    schema: Vec<(String, String, String, Option<String>)>,
    row_counts: Vec<(String, i64)>,
}

impl ImmutableDbFacts {
    fn capture(path: &Path) -> Self {
        let uri = format!("file:{}?immutable=1", path.to_string_lossy());
        let connection = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .expect("open refusal DB immutable");
        let user_version = connection
            .query_row("pragma user_version", [], |row| row.get(0))
            .expect("read refusal DB user_version");
        let mut statement = connection
            .prepare(
                "select type, name, tbl_name, sql
                 from sqlite_schema
                 order by type, name, tbl_name",
            )
            .expect("prepare refusal DB schema");
        let schema = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .expect("query refusal DB schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect refusal DB schema");
        let row_counts = schema
            .iter()
            .filter_map(|(kind, name, _, _)| (kind == "table").then_some(name.clone()))
            .map(|table| {
                let quoted = table.replace('"', "\"\"");
                let count = connection
                    .query_row(&format!("select count(*) from \"{quoted}\""), [], |row| {
                        row.get(0)
                    })
                    .unwrap_or_else(|error| panic!("count rows in {table}: {error}"));
                (table, count)
            })
            .collect();
        Self {
            user_version,
            schema,
            row_counts,
        }
    }
}

fn pane_line(
    pane: &str,
    session: &str,
    window: &str,
    command: &str,
    cwd: &Path,
    pid: u32,
    tty: &str,
) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t{tty}\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn open_pty() -> io::Result<(File, File, String)> {
    let mut master = -1;
    let mut slave = -1;
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0_i8; 256];
    let name_result = unsafe { libc::ttyname_r(slave, name.as_mut_ptr(), name.len()) };
    if name_result != 0 {
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        return Err(io::Error::from_raw_os_error(name_result));
    }
    let tty = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    Ok((master, slave, tty))
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
