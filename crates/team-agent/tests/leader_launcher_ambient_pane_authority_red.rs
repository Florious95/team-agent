//! 0.5.62 P0 RED — an ambient tmux tuple is observation, not leader authority.
//!
//! User-visible contract:
//! - no ambient tuple selects the workspace-derived managed leader path;
//! - a complete tuple may select the direct provider path only when the caller
//!   tty, live pane and requested workspace agree;
//! - a live but foreign historical pane must fail loudly with a typed reason,
//!   without starting a provider, switching to managed mode, or changing
//!   canonical state/leader registry;
//! - the same typed mismatch and a copyable recovery action reach `send` and
//!   `diagnose`;
//! - messages that failed in the attach window are either physically retried
//!   after attach or remain explicitly visible as attach-window debt.
//!
//! No real provider is started. The public CLI process runs against a PATH
//! shim, a deterministic pane/tty fixture, and a hermetic HOME.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::ffi::CStr;
use std::fs::{self, File};
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::save_runtime_state;
use team_agent::transport::Transport;

const TEAM: &str = "pane-authority";
const AMBIENT_PANE: &str = "%ambient";
const GOOD_PANE: &str = "%good";

#[test]
#[serial(env)]
fn a1_no_ambient_tuple_uses_workspace_derived_managed_path() {
    let case = Case::new("a1-no-ambient");
    case.set_mode("managed");

    let output = case.run(&["codex", "--json", "--", "--contract-canary"], None);
    let value = json_stdout("A1 managed launch", &output);
    let tmux_log = case.tmux_log();
    let expected_endpoint = team_agent::tmux_backend::TmuxBackend::for_workspace(&case.workspace)
        .tmux_endpoint()
        .expect("workspace tmux endpoint");

    assert_eq!(value["ok"], json!(true), "A1 output={value}");
    assert_eq!(
        value["mode"],
        json!("managed_tmux_client"),
        "ambient absence must select the managed path; output={value}"
    );
    assert!(
        tmux_log.contains(&expected_endpoint),
        "managed path must use the product's workspace-derived endpoint; \
         expected={expected_endpoint:?} tmux_log={tmux_log:?}"
    );
    assert!(
        contains_tmux_operation(&tmux_log, "new-session"),
        "positive control: managed path must really create its test-owned leader pane; \
         tmux_log={tmux_log:?}"
    );
}

#[test]
#[serial(env)]
fn a2_complete_matching_tuple_allows_direct_provider_path() {
    let case = Case::new("a2-matching");
    case.set_mode("matching");

    let output = case.run_with_controlling_tty(
        &["codex", "--json", "--", "--contract-canary"],
        AMBIENT_PANE,
    );
    let value = json_stdout("A2 matching ambient launch", &output);

    assert_eq!(value["ok"], json!(true), "A2 output={value}");
    assert_eq!(
        value["mode"],
        json!("exec_provider"),
        "a live pane whose tty and workspace match remains a legal direct-provider launch; \
         output={value}"
    );
    assert_eq!(
        case.provider_launches(),
        1,
        "positive control: the matching direct-provider path must actually reach the shim"
    );
    let state = fs::read_to_string(case.state_path()).expect("matching branch writes state");
    assert!(
        state.contains(AMBIENT_PANE),
        "matching branch must bind the verified ambient pane; state={state}"
    );
}

#[test]
#[serial(env)]
fn a3_foreign_live_tuple_fails_with_typed_reason_and_copyable_action() {
    let case = Case::new("a3-typed");
    case.set_mode("foreign");

    let output = case.run(
        &["codex", "--json", "--", "--contract-canary"],
        Some(AMBIENT_PANE),
    );
    let value = json_stdout_even_on_error("A3 foreign ambient refusal", &output);

    assert!(
        !output.status.success() && value["ok"] == json!(false),
        "A3 RED signature: a present but foreign/unverifiable ambient tuple must fail loud; \
         status={} output={value}",
        output.status
    );
    assert_eq!(
        value["reason"],
        json!("PaneWorkspaceMismatch"),
        "A3 RED signature: refusal must carry the existing machine-readable workspace-mismatch \
         reason, not a generic launcher string; output={value}"
    );
    assert!(
        has_copyable_recovery_action(&value),
        "A3 RED signature: typed refusal must carry attach-leader/takeover/clean-terminal \
         recovery guidance without echoing the rejected tuple; output={value}"
    );
}

#[test]
#[serial(env)]
fn a3_foreign_live_tuple_spawns_neither_provider_nor_managed_leader() {
    let case = Case::new("a3-zero-spawn");
    case.set_mode("foreign");

    let _ = case.run(
        &["codex", "--json", "--", "--contract-canary"],
        Some(AMBIENT_PANE),
    );
    let tmux_log = case.tmux_log();

    assert_eq!(
        case.provider_launches(),
        0,
        "A3 RED signature: authority refusal must happen before provider spawn"
    );
    for forbidden in [
        "new-session",
        "new-window",
        "attach-session",
        "switch-client",
    ] {
        assert!(
            !contains_tmux_operation(&tmux_log, forbidden),
            "A3 RED signature: a foreign tuple must not silently switch to Managed; \
             forbidden={forbidden} tmux_log={tmux_log:?}"
        );
    }
}

#[test]
#[serial(env)]
fn a3_foreign_live_tuple_leaves_state_and_leader_registry_byte_stable() {
    let case = Case::new("a3-zero-state");
    case.seed_preexisting_state_and_registry();
    case.set_mode("foreign");
    let state_before = fs::read(case.state_path()).expect("positive control: preexisting state");
    let registry_before = case.env.registry_entries();
    assert!(
        !state_before.is_empty() && !registry_before.is_empty(),
        "positive control: zero-write check must begin with both state and registry inventory"
    );

    let _ = case.run(
        &["codex", "--json", "--", "--contract-canary"],
        Some(AMBIENT_PANE),
    );

    let state_changed = fs::read(case.state_path()).ok().as_deref() != Some(&state_before);
    let registry_changed = case.env.registry_entries() != registry_before;
    assert!(
        !state_changed && !registry_changed,
        "A3 RED signature: failed ambient authority must leave both .team/runtime/state.json \
         and the complete host leader registry byte-stable; \
         state_changed={state_changed} registry_changed={registry_changed}"
    );
}

#[test]
#[serial(env)]
fn b1_send_surfaces_typed_workspace_mismatch_and_recovery_action() {
    let case = Case::new("b1-send");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");

    let output = case.run(
        &[
            "send",
            "leader",
            "pane authority send canary",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        None,
    );
    let value = json_stdout_even_on_error("B1 send refusal", &output);

    assert!(
        has_typed_reason(&value, "PaneWorkspaceMismatch"),
        "B1 RED signature: send must preserve the known typed cause instead of collapsing it \
         to leader_not_attached; output={value}"
    );
    assert!(
        has_copyable_recovery_action(&value),
        "B1 RED signature: send must carry a copyable attach-leader/takeover action; output={value}"
    );
}

#[test]
#[serial(env)]
fn b2_diagnose_checks_live_workspace_even_when_state_says_attached() {
    let case = Case::new("b2-diagnose");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");

    let output = case.run(
        &[
            "diagnose",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        None,
    );
    let value = json_stdout_even_on_error("B2 diagnose", &output);

    assert!(
        json_contains_string(&value, "PaneWorkspaceMismatch"),
        "B2 RED signature: diagnose must not trust status=attached when the live pane belongs \
         to another workspace; output={value}"
    );
    assert!(
        has_copyable_recovery_action(&value),
        "B2 RED signature: diagnose mismatch must include an executable recovery action; \
         output={value}"
    );
}

#[test]
#[serial(env)]
fn b3_recovery_action_removes_the_same_typed_error() {
    let case = Case::new("b3-action");
    case.seed_foreign_attached_state();
    case.set_mode("foreign");
    let before = case.send_canary("before recovery");
    assert!(
        has_copyable_recovery_action(&before),
        "positive-control precondition: mismatch output must advertise a supported recovery action; \
         output={before}"
    );
    let recovery_argv = copyable_recovery_command(&before).unwrap_or_else(|| {
        panic!(
            "B3 RED signature: recovery guidance must contain a directly executable \
             team-agent attach-leader/takeover command; output={before}"
        )
    });
    let recovery_args = recovery_argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .collect::<Vec<_>>();

    case.set_mode("recovery");
    let attach = case.run(&recovery_args, Some(GOOD_PANE));
    assert!(
        attach.status.success(),
        "B3 RED signature: copying the advertised recovery command in a corrected leader \
         terminal must succeed; command={recovery_argv:?} status={} stdout={} stderr={}",
        attach.status,
        String::from_utf8_lossy(&attach.stdout),
        String::from_utf8_lossy(&attach.stderr)
    );

    let diagnose = case.run(
        &[
            "diagnose",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        Some(GOOD_PANE),
    );
    let diagnose_value = json_stdout_even_on_error("B3 post-action diagnose", &diagnose);
    assert!(
        !json_contains_string(&diagnose_value, "PaneWorkspaceMismatch"),
        "copying the suggested action must remove the original typed error; \
         diagnose={diagnose_value}"
    );
}

#[test]
#[serial(env)]
fn c_attach_window_failures_are_retried_or_remain_user_visible() {
    let case = Case::new("c-attach-window");
    case.seed_foreign_attached_state();
    let message_ids = case.seed_attach_window_failures();
    let attempts_before = message_ids
        .iter()
        .map(|id| case.message_attempts(id))
        .collect::<Vec<_>>();

    case.set_mode("recovery");
    let attach = case.run(
        &[
            "attach-leader",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--pane",
            GOOD_PANE,
            "--provider",
            "codex",
            "--confirm",
            "--json",
        ],
        Some(GOOD_PANE),
    );
    let attach_value = json_stdout_even_on_error("C attach", &attach);
    assert_eq!(
        attach_value["ok"],
        json!(true),
        "C precondition: leader attach must complete; output={attach_value}"
    );

    let physically_retried = message_ids.iter().enumerate().all(|(index, id)| {
        let row = case.message_row(id);
        row.attempts > attempts_before[index]
            && matches!(
                row.status.as_str(),
                "submitted_pending_acceptance"
                    | "submitted_awaiting_receipt"
                    | "submitted_unverified"
                    | "visible"
                    | "delivered"
                    | "acknowledged"
            )
    });
    let status = case.run(
        &[
            "status",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
            "--detail",
        ],
        Some(GOOD_PANE),
    );
    let diagnose = case.run(
        &[
            "diagnose",
            "--workspace",
            case.workspace_str(),
            "--team",
            TEAM,
            "--json",
        ],
        Some(GOOD_PANE),
    );
    let status_value = json_stdout_even_on_error("C status", &status);
    let diagnose_value = json_stdout_even_on_error("C diagnose", &diagnose);
    let visible_debt = attach_window_debt_visible(
        &[&attach_value, &status_value, &diagnose_value],
        &message_ids,
    );

    assert!(
        physically_retried || visible_debt,
        "C RED signature: after attach, messages that failed in the attach window must either \
         cross a new physical-attempt boundary or remain user-visible as N=2 attach-window debt; \
         rows={:?} attach={attach_value} status={status_value} diagnose={diagnose_value}",
        message_ids
            .iter()
            .map(|id| case.message_row(id))
            .collect::<Vec<_>>()
    );
}

struct Case {
    _endpoint_fixture: UnixSocketFixture,
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    foreign_workspace: PathBuf,
    fake_bin: PathBuf,
    endpoint: PathBuf,
    mode_path: PathBuf,
    tmux_log_path: PathBuf,
    provider_launch_log_path: PathBuf,
    pane_capture_path: PathBuf,
    workspace_string: String,
}

impl Case {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace("requested");
        let foreign_workspace = env.workspace("historical-foreign");
        let fake_bin = env.root().join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        let endpoint = hermetic_guard::short_tmux_socket(tag);
        let endpoint_fixture = UnixSocketFixture::bind(&endpoint);
        let mode_path = env.root().join("pane-mode");
        let tmux_log_path = env.root().join("tmux.log");
        let provider_launch_log_path = env.root().join("provider-launch.log");
        let pane_capture_path = env.root().join("pane-capture");
        let workspace_string = workspace.to_string_lossy().into_owned();

        write_executable(&fake_bin.join("tmux"), TMUX_SHIM);
        write_executable(&fake_bin.join("codex"), PROVIDER_SHIM);
        Self {
            _endpoint_fixture: endpoint_fixture,
            env,
            workspace,
            foreign_workspace,
            fake_bin,
            endpoint,
            mode_path,
            tmux_log_path,
            provider_launch_log_path,
            pane_capture_path,
            workspace_string,
        }
    }

    fn workspace_str(&self) -> &str {
        &self.workspace_string
    }

    fn state_path(&self) -> PathBuf {
        self.workspace.join(".team/runtime/state.json")
    }

    fn spawn_session_path(&self) -> PathBuf {
        self.env.root().join("spawn-session")
    }

    fn set_mode(&self, mode: &str) {
        fs::write(&self.mode_path, mode).expect("set pane fixture mode");
    }

    fn run(&self, args: &[&str], ambient_pane: Option<&str>) -> Output {
        let mut command = self.command(args, ambient_pane);
        command.output().expect("run team-agent CLI")
    }

    fn run_with_controlling_tty(&self, args: &[&str], ambient_pane: &str) -> Output {
        let (master, slave, tty) = open_pty().expect("allocate controlling tty");
        let mut command = self.command(args, Some(ambient_pane));
        command
            .env("TEAM_AGENT_TEST_PANE_TTY", tty)
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn team-agent in pty");
        drop(master);
        child.wait_with_output().expect("wait for pty child")
    }

    fn command(&self, args: &[&str], ambient_pane: Option<&str>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self.env.home())
            .env("PATH", self.test_path())
            .env("TEAM_AGENT_TEST_TMUX_LOG", &self.tmux_log_path)
            .env(
                "TEAM_AGENT_TEST_PROVIDER_LAUNCH_LOG",
                &self.provider_launch_log_path,
            )
            .env("TEAM_AGENT_TEST_PANE_MODE_FILE", &self.mode_path)
            .env("TEAM_AGENT_TEST_REQUESTED_WORKSPACE", &self.workspace)
            .env("TEAM_AGENT_TEST_FOREIGN_WORKSPACE", &self.foreign_workspace)
            .env("TEAM_AGENT_TEST_PANE_CAPTURE", &self.pane_capture_path)
            .env("TEAM_AGENT_TEST_SPAWN_SESSION", self.spawn_session_path())
            .env("TEAM_AGENT_TEST_PANE_TTY", "/dev/ttys-good");
        for key in hermetic_guard::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        if let Some(pane) = ambient_pane {
            command
                .env(
                    "TMUX",
                    format!("{},4242,0", self.endpoint.to_string_lossy()),
                )
                .env("TMUX_PANE", pane);
        }
        command
    }

    fn test_path(&self) -> String {
        format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn tmux_log(&self) -> String {
        fs::read_to_string(&self.tmux_log_path).unwrap_or_default()
    }

    fn provider_launches(&self) -> usize {
        fs::read_to_string(&self.provider_launch_log_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "launch")
            .count()
    }

    fn seed_foreign_attached_state(&self) {
        let receiver = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": AMBIENT_PANE,
            "pane": AMBIENT_PANE,
            "tmux_socket": self.endpoint,
            "session_name": "historical-foreign-leader",
            "pane_tty": "/dev/ttys-historical",
            "owner_epoch": 7
        });
        let owner = json!({
            "pane_id": AMBIENT_PANE,
            "provider": "codex",
            "owner_epoch": 7,
            "claimed_via": "attach-leader"
        });
        let team = json!({
            "team_key": TEAM,
            "workspace": self.workspace,
            "session_name": "team-pane-authority",
            "tmux_endpoint": self.endpoint,
            "tmux_socket": self.endpoint,
            "leader_receiver": receiver,
            "team_owner": owner,
            "owner_epoch": 7,
            "agents": {}
        });
        let state = json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "workspace": self.workspace,
            "session_name": "team-pane-authority",
            "tmux_endpoint": self.endpoint,
            "tmux_socket": self.endpoint,
            "teams": {TEAM: team},
            "agents": {}
        });
        save_runtime_state(&self.workspace, &state).expect("seed foreign attached state");
        MessageStore::open(&self.workspace).expect("initialize message store");
    }

    fn seed_preexisting_state_and_registry(&self) {
        let state = json!({
            "active_team_key": "preexisting",
            "teams": {
                "preexisting": {
                    "team_key": "preexisting",
                    "workspace": self.workspace,
                    "agents": {}
                }
            },
            "agents": {}
        });
        save_runtime_state(&self.workspace, &state).expect("seed preexisting state inventory");
        let registry_path = self.env.home().join(".team-agent/leaders/preexisting.json");
        fs::write(
            registry_path,
            serde_json::to_vec_pretty(&json!({
                "team_key": "preexisting",
                "workspace": self.workspace,
                "pane_id": "%preexisting"
            }))
            .expect("serialize preexisting registry"),
        )
        .expect("seed preexisting registry inventory");
    }

    fn send_canary(&self, suffix: &str) -> Value {
        let output = self.run(
            &[
                "send",
                "leader",
                suffix,
                "--workspace",
                self.workspace_str(),
                "--team",
                TEAM,
                "--json",
            ],
            None,
        );
        json_stdout_even_on_error("send canary", &output)
    }

    fn seed_attach_window_failures(&self) -> Vec<String> {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        [2_i64, 4_i64]
            .into_iter()
            .enumerate()
            .map(|(index, attempts)| {
                let id = store
                    .create_message(
                        None,
                        "worker",
                        "leader",
                        &format!("attach-window canary {index}"),
                        None,
                        false,
                        Some(TEAM),
                    )
                    .expect("create attach-window message");
                let conn = team_agent::db::schema::open_db(store.db_path()).expect("open team db");
                conn.execute(
                    "update messages
                     set status = 'failed',
                         error = 'leader_not_attached',
                         delivery_attempts = ?2,
                         created_at = ?3,
                         updated_at = ?3
                     where message_id = ?1",
                    params![
                        id,
                        attempts,
                        if index == 0 {
                            "2026-07-27T14:35:09+08:00"
                        } else {
                            "2026-07-27T14:35:22+08:00"
                        }
                    ],
                )
                .expect("shape attach-window failure");
                id
            })
            .collect()
    }

    fn message_attempts(&self, message_id: &str) -> i64 {
        self.message_row(message_id).attempts
    }

    fn message_row(&self, message_id: &str) -> MessageRow {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = team_agent::db::schema::open_db(store.db_path()).expect("open team db");
        conn.query_row(
            "select status, delivery_attempts, error from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(MessageRow {
                    status: row.get(0)?,
                    attempts: row.get(1)?,
                    error: row.get(2)?,
                })
            },
        )
        .expect("read message row")
    }
}

struct UnixSocketFixture {
    _listener: UnixListener,
    path: PathBuf,
}

impl UnixSocketFixture {
    fn bind(path: &Path) -> Self {
        let listener = UnixListener::bind(path).expect("bind live historical tmux endpoint");
        Self {
            _listener: listener,
            path: path.to_path_buf(),
        }
    }
}

impl Drop for UnixSocketFixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct MessageRow {
    status: String,
    attempts: i64,
    #[allow(dead_code)]
    error: Option<String>,
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

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fixture");
}

fn json_stdout(label: &str, output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{label} must exit zero; status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    json_stdout_even_on_error(label, output)
}

fn json_stdout_even_on_error(label: &str, output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout must be JSON: {error}; status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn contains_tmux_operation(log: &str, operation: &str) -> bool {
    log.lines()
        .any(|line| line.split_whitespace().any(|part| part == operation))
}

fn has_copyable_recovery_action(value: &Value) -> bool {
    let text = value.to_string().to_ascii_lowercase();
    text.contains("team-agent attach-leader")
        || text.contains("team-agent takeover")
        || text.contains("clean terminal")
        || text.contains("unset tmux")
}

fn copyable_recovery_command(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(text) => text
            .split('`')
            .enumerate()
            .filter_map(|(index, candidate)| (index % 2 == 1).then_some(candidate))
            .find_map(parse_supported_recovery_command),
        Value::Array(values) => values.iter().find_map(copyable_recovery_command),
        Value::Object(values) => values.values().find_map(copyable_recovery_command),
        _ => None,
    }
}

fn parse_supported_recovery_command(command: &str) -> Option<Vec<String>> {
    let argv = command
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let supported = argv.first().is_some_and(|arg| arg == "team-agent")
        && argv
            .get(1)
            .is_some_and(|arg| arg == "attach-leader" || arg == "takeover");
    supported.then_some(argv)
}

fn has_typed_reason(value: &Value, expected: &str) -> bool {
    value.as_object().is_some_and(|object| {
        ["reason", "reason_code", "channel_reason"]
            .iter()
            .any(|key| object.get(*key).and_then(Value::as_str) == Some(expected))
    }) || value
        .as_array()
        .is_some_and(|values| values.iter().any(|value| has_typed_reason(value, expected)))
        || value.as_object().is_some_and(|object| {
            object
                .values()
                .any(|value| has_typed_reason(value, expected))
        })
}

fn json_contains_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value == expected,
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_string(value, expected)),
        Value::Object(values) => values
            .values()
            .any(|value| json_contains_string(value, expected)),
        _ => false,
    }
}

fn attach_window_debt_visible(values: &[&Value], message_ids: &[String]) -> bool {
    values
        .iter()
        .any(|value| value_names_attach_window_debt(value, message_ids.len() as u64))
}

fn value_names_attach_window_debt(value: &Value, expected_count: u64) -> bool {
    match value {
        Value::String(text) => {
            let text = text.to_ascii_lowercase();
            (text.contains("leader_not_attached") || text.contains("attach window"))
                && text.contains(&expected_count.to_string())
        }
        Value::Array(values) => values
            .iter()
            .any(|value| value_names_attach_window_debt(value, expected_count)),
        Value::Object(object) => {
            let local_text = Value::Object(object.clone())
                .to_string()
                .to_ascii_lowercase();
            let names_cause =
                local_text.contains("leader_not_attached") || local_text.contains("attach_window");
            let names_count = object.iter().any(|(key, value)| {
                (key == "count" || key == "failed_count" || key == "message_count")
                    && value.as_u64() == Some(expected_count)
            });
            (names_cause && names_count)
                || object
                    .values()
                    .any(|value| value_names_attach_window_debt(value, expected_count))
        }
        _ => false,
    }
}

const PROVIDER_SHIM: &str = r#"#!/bin/sh
if [ "${1-}" = "--version" ]; then
  printf 'codex 1.0.0\n'
  exit 0
fi
printf 'launch\n' >> "$TEAM_AGENT_TEST_PROVIDER_LAUNCH_LOG"
exit 0
"#;

const TMUX_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$TEAM_AGENT_TEST_TMUX_LOG"
mode=$(cat "$TEAM_AGENT_TEST_PANE_MODE_FILE")
last=
target=
previous=
spawn_session=
for arg in "$@"; do
  if [ "$previous" = "-t" ]; then
    target="$arg"
  fi
  if [ "$previous" = "-s" ]; then
    spawn_session="$arg"
  fi
  previous="$arg"
  last="$arg"
done

if [ "${1-}" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi

case " $* " in
  *" has-session "*)
    exit 1
    ;;
  *" new-session "*|*" new-window "*)
    if [ -n "$spawn_session" ]; then
      printf '%s\n' "$spawn_session" > "$TEAM_AGENT_TEST_SPAWN_SESSION"
    fi
    printf '%%managed\n'
    exit 0
    ;;
  *" list-panes "*)
    if [ "$mode" = "matching" ]; then
      printf '%%ambient\tambient-leader\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4101\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    elif [ "$mode" = "foreign" ]; then
      printf '%%ambient\thistorical-foreign-leader\t0\tcodex\t0\t/dev/ttys-historical\tcodex\t1\t%s\t1\t0\t4102\t\n' \
        "$TEAM_AGENT_TEST_FOREIGN_WORKSPACE"
    fi
    printf '%%good\trequested-leader\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4103\t\n' \
      "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    managed_session=managed-leader
    if [ -f "$TEAM_AGENT_TEST_SPAWN_SESSION" ]; then
      managed_session=$(cat "$TEAM_AGENT_TEST_SPAWN_SESSION")
    fi
    printf '%%managed\t%s\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4104\t\n' \
      "$managed_session" "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    exit 0
    ;;
  *" display-message "*)
    case "$last" in
      '#{pane_id}') printf '%s\n' "${target:-%good}" ;;
      '#{pane_current_command}') printf 'codex\n' ;;
      '#{pane_current_path}')
        if [ "$target" = "%ambient" ] && [ "$mode" = "foreign" ]; then
          printf '%s\n' "$TEAM_AGENT_TEST_FOREIGN_WORKSPACE"
        else
          printf '%s\n' "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
        fi
        ;;
      '#{pane_tty}')
        if [ "$target" = "%ambient" ] && [ "$mode" = "foreign" ]; then
          printf '/dev/ttys-historical\n'
        else
          printf '%s\n' "$TEAM_AGENT_TEST_PANE_TTY"
        fi
        ;;
      '#{session_name}') printf 'requested-leader\n' ;;
      '#{pane_width}') printf '120\n' ;;
      '#{pane_mode}') printf '0\n' ;;
      *) printf '%s\n' "${target:-%good}" ;;
    esac
    exit 0
    ;;
  *" set-buffer "*)
    printf '%s' "$last" > "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *" load-buffer "*)
    cat > "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *" capture-pane "*)
    [ -f "$TEAM_AGENT_TEST_PANE_CAPTURE" ] && cat "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *" send-keys "*" Enter"*|*" send-keys "*" Enter")
    : > "$TEAM_AGENT_TEST_PANE_CAPTURE"
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#;
