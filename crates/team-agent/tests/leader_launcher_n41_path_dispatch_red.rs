//! RED contract for REQUIREMENTS N41 managed-launcher path dispatch.
//!
//! User-visible contract:
//! - outside tmux, `team-agent <provider>` creates the managed leader and
//!   attaches the caller with the workspace-derived tmux endpoint;
//! - inside that same tmux server, the launcher switches the existing client
//!   to the managed leader instead of running the provider in the caller pane
//!   or attempting a nested attach;
//! - inside a different tmux server, the default is an N38 explainable
//!   refusal, while the explicit `--allow-nested-attach` opt-in permits the
//!   nested attach;
//! - a stale pane id from the real incident does not collapse the different-
//!   server branch into the generic ambient-pane-observation refusal.
//!
//! The public CLI is executed by absolute Cargo binary path. No real provider
//! or tmux server is used: a hermetic PATH shim records the physical route,
//! and all fixture sockets live below an explicit test root or Cargo's
//! package-owned target tmpdir.
//!
//! ---
//! purpose: Exercise the five N41 managed-launcher route contracts through hermetic tmux and provider recorders.
//! contract:
//!   depends:
//!     - team-agent launcher route behavior
//!     - hermetic tmux and provider shims
//! boundary: Test-only route contracts; no launcher, runner, or product socket behavior is changed.
//! maturity: experimental
//! arch:
//!   allowed_dependencies: [std, libc, serde_json, serial_test, team_agent]
//! ---

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
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::Transport;

const AMBIENT_PANE: &str = "%ambient-n41";
const SUN_LEN: usize = 104;
const SOCKET_PATH_BUDGET: usize = 100;
static SOCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservedRoute {
    DirectProvider,
    AttachSession,
    SwitchClient,
    Refused,
    Unknown,
}

#[test]
fn route_observer_canary_distinguishes_all_n41_outcomes() {
    assert_eq!(
        observed_route("", "launch canary\n", true),
        ObservedRoute::DirectProvider
    );
    assert_eq!(
        observed_route("-L ta-canary attach-session -t leader:codex\n", "", true),
        ObservedRoute::AttachSession
    );
    assert_eq!(
        observed_route("-L ta-canary switch-client -t leader:codex\n", "", true),
        ObservedRoute::SwitchClient
    );
    assert_eq!(observed_route("", "", false), ObservedRoute::Refused);
    assert_eq!(observed_route("", "", true), ObservedRoute::Unknown);
}

#[test]
#[serial(env)]
fn n41_no_tmux_uses_standard_attach_and_not_an_in_tmux_route() {
    let case = Case::new("n41-no-tmux");
    let output = case.run_in_pty(
        &["codex", "--json", "--", "--n41-contract-canary"],
        Ambient::None,
    );
    let value = json_stdout("N41 no-tmux standard attach", &output);
    let log = case.tmux_log();

    assert_eq!(value["ok"], json!(true), "N41 no-tmux output={value}");
    assert_eq!(
        observed_route(&log, &case.provider_log(), output.status.success()),
        ObservedRoute::AttachSession,
        "N41_NO_TMUX_ROUTE: no TMUX must use standard attach-session; tmux_log={log:?} \
         provider_log={:?} output={value}",
        case.provider_log()
    );
    assert!(
        has_operation(&log, "new-session"),
        "anti-vacuous control: standard attach must first create a managed leader pane; \
         tmux_log={log:?}"
    );
    assert!(
        !has_operation(&log, "switch-client") && case.provider_launches() == 0,
        "no-tmux branch must neither switch an existing client nor directly run the provider; \
         tmux_log={log:?} provider_log={:?}",
        case.provider_log()
    );
}

#[test]
#[serial(env)]
fn n41_same_server_switches_client_instead_of_running_provider_in_caller_pane() {
    let case = Case::new("n41-same-server");
    let output = case.run_in_pty(
        &["codex", "--json", "--", "--n41-contract-canary"],
        Ambient::SameServer,
    );
    let value = json_stdout("N41 same-server switch-client", &output);
    let log = case.tmux_log();
    let route = observed_route(&log, &case.provider_log(), output.status.success());

    assert_eq!(
        route,
        ObservedRoute::SwitchClient,
        "N41_SAME_SERVER_DISPATCH: a caller already in the workspace tmux server must \
         switch-client, not run the provider in the caller pane or nested-attach; \
         observed_route={route:?} tmux_log={log:?} provider_log={:?} output={value}",
        case.provider_log()
    );
    assert!(
        has_operation(&log, "new-session"),
        "anti-vacuous control: same-server dispatch must create the managed leader pane; \
         tmux_log={log:?}"
    );
    assert!(
        !has_operation(&log, "attach-session") && case.provider_launches() == 0,
        "same-server dispatch must not nested-attach or directly launch the provider; \
         tmux_log={log:?} provider_log={:?}",
        case.provider_log()
    );
}

#[test]
#[serial(env)]
fn n41_different_server_default_refuses_with_n38_and_zero_launch() {
    let case = Case::new("n41-different-server-live");
    let output = case.run_in_pty(
        &["codex", "--", "--n41-contract-canary"],
        Ambient::DifferentServerLivePane,
    );

    assert_different_server_default_refusal(&case, &output, "N41_DIFFERENT_SERVER_DEFAULT");
}

#[test]
#[serial(env)]
fn n41_user_sample_stale_pane_still_takes_different_server_branch() {
    let case = Case::new("n41-different-server-stale-pane");
    let output = case.run_in_pty(
        &["codex", "--", "--n41-user-sample-canary"],
        Ambient::DifferentServerMissingPane,
    );

    assert_different_server_default_refusal(&case, &output, "N41_USER_SAMPLE_PANE_NOT_FOUND");
}

#[test]
#[serial(env)]
fn n41_different_server_explicit_opt_in_allows_nested_attach() {
    let case = Case::new("n41-different-server-opt-in");
    let output = case.run_in_pty(
        &[
            "codex",
            "--allow-nested-attach",
            "--json",
            "--",
            "--n41-contract-canary",
        ],
        Ambient::DifferentServerLivePane,
    );
    let value = json_stdout("N41 different-server explicit opt-in", &output);
    let log = case.tmux_log();
    let route = observed_route(&log, &case.provider_log(), output.status.success());

    assert_eq!(
        route,
        ObservedRoute::AttachSession,
        "N41_DIFFERENT_SERVER_OPT_IN: --allow-nested-attach must select the managed \
         nested attach path instead of being passed through to a direct provider; \
         observed_route={route:?} tmux_log={log:?} provider_log={:?} output={value}",
        case.provider_log()
    );
    assert!(
        has_operation(&log, "new-session"),
        "anti-vacuous control: opt-in must create the managed leader pane before attach; \
         tmux_log={log:?}"
    );
    assert!(
        !has_operation(&log, "switch-client") && case.provider_launches() == 0,
        "different-server opt-in is nested attach, not same-server switch or direct provider; \
         tmux_log={log:?} provider_log={:?}",
        case.provider_log()
    );
}

fn assert_different_server_default_refusal(case: &Case, output: &Output, signature: &str) {
    let text = output_text(output);
    let lower = text.to_ascii_lowercase();
    let log = case.tmux_log();
    let route = observed_route(&log, &case.provider_log(), output.status.success());

    assert_eq!(
        route,
        ObservedRoute::Refused,
        "{signature}: a different tmux server must be refused by default before provider or \
         managed-client launch; observed_route={route:?} status={} tmux_log={log:?} \
         provider_log={:?} output={text:?}",
        output.status,
        case.provider_log()
    );
    for marker in ["error", "action", "log"] {
        assert!(
            lower
                .lines()
                .any(|line| line.trim_start().starts_with(marker)),
            "{signature}: N38 refusal must contain a {marker:?} line; output={text:?}"
        );
    }
    assert!(
        text.contains(case.source_endpoint_str()) && text.contains(case.target_socket_name()),
        "{signature}: refusal must identify the observed and requested tmux server facts; \
         source_endpoint={:?} target_socket_name={:?} output={text:?}",
        case.source_endpoint_str(),
        case.target_socket_name()
    );
    assert!(
        case.provider_launches() == 0
            && !has_operation(&log, "new-session")
            && !has_operation(&log, "attach-session")
            && !has_operation(&log, "switch-client"),
        "{signature}: default refusal must be pre-launch; tmux_log={log:?} \
         provider_log={:?}",
        case.provider_log()
    );
}

#[derive(Clone, Copy)]
enum Ambient {
    None,
    SameServer,
    DifferentServerLivePane,
    DifferentServerMissingPane,
}

impl Ambient {
    fn pane_present(self) -> bool {
        !matches!(self, Self::DifferentServerMissingPane)
    }
}

struct Case {
    _target_socket: UnixListener,
    _source_socket: UnixListener,
    _socket_root: ShortSocketRoot,
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    fake_bin: PathBuf,
    tmpdir: PathBuf,
    target_socket_name: String,
    target_endpoint: PathBuf,
    source_endpoint: PathBuf,
    tmux_log_path: PathBuf,
    provider_log_path: PathBuf,
    spawn_session_path: PathBuf,
}

struct ShortSocketRoot(PathBuf);

impl ShortSocketRoot {
    fn new(path: PathBuf) -> Self {
        assert!(
            path.is_absolute() && path != Path::new("/") && path.components().count() >= 3,
            "refuse unsafe socket fixture root: {}",
            path.display()
        );
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create short socket fixture root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ShortSocketRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Case {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace("requested");
        let fake_bin = env.root().join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("create fake bin");
        write_executable(&fake_bin.join("tmux"), TMUX_SHIM);
        write_executable(&fake_bin.join("codex"), PROVIDER_SHIM);

        let short_root_base = if cfg!(target_os = "macos") {
            PathBuf::from("/private/tmp")
        } else if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let owned_socket_root = ShortSocketRoot::new(short_root_base.join(format!(
            "ta-n41-{}-{}",
            std::process::id(),
            SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed)
        )));
        let tmpdir = owned_socket_root.path().to_path_buf();
        let tmux_socket_dir = tmpdir.join(format!("tmux-{}", unsafe { libc::geteuid() }));
        fs::create_dir_all(&tmux_socket_dir).expect("create isolated tmux socket root");
        let target_socket_name = TmuxBackend::for_workspace(&workspace)
            .tmux_endpoint()
            .expect("workspace tmux endpoint name");
        let target_endpoint = tmux_socket_dir.join(&target_socket_name);
        let source_endpoint = tmux_socket_dir.join("foreign-source.sock");

        assert!(
            target_endpoint.starts_with(owned_socket_root.path())
                && source_endpoint.starts_with(owned_socket_root.path()),
            "fixture sockets must remain inside the process-owned short root"
        );
        assert!(
            target_endpoint.as_os_str().as_encoded_bytes().len() < SOCKET_PATH_BUDGET
                && source_endpoint.as_os_str().as_encoded_bytes().len() < SOCKET_PATH_BUDGET,
            "fixture socket paths must stay below the portable Unix sun_path budget of {SUN_LEN}"
        );
        assert!(
            target_endpoint.as_os_str().as_encoded_bytes().len() + 1 <= SUN_LEN
                && source_endpoint.as_os_str().as_encoded_bytes().len() + 1 <= SUN_LEN,
            "fixture socket paths including the terminating NUL must fit SUN_LEN={SUN_LEN}"
        );
        let target_socket =
            UnixListener::bind(&target_endpoint).expect("bind target fixture socket");
        let source_socket =
            UnixListener::bind(&source_endpoint).expect("bind source fixture socket");

        Self {
            _target_socket: target_socket,
            _source_socket: source_socket,
            _socket_root: owned_socket_root,
            workspace,
            fake_bin,
            tmpdir,
            target_socket_name,
            target_endpoint,
            source_endpoint,
            tmux_log_path: env.root().join("tmux.log"),
            provider_log_path: env.root().join("provider.log"),
            spawn_session_path: env.root().join("spawn-session"),
            env,
        }
    }

    fn run_in_pty(&self, args: &[&str], ambient: Ambient) -> Output {
        let (master, slave, tty) = open_pty().expect("allocate isolated controlling tty");
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", self.env.home())
            .env("TMPDIR", &self.tmpdir)
            .env("PATH", self.test_path())
            .env("TEAM_AGENT_TEST_TMUX_LOG", &self.tmux_log_path)
            .env("TEAM_AGENT_TEST_PROVIDER_LOG", &self.provider_log_path)
            .env("TEAM_AGENT_TEST_SPAWN_SESSION", &self.spawn_session_path)
            .env(
                "TEAM_AGENT_TEST_TARGET_SOCKET_NAME",
                &self.target_socket_name,
            )
            .env("TEAM_AGENT_TEST_TARGET_ENDPOINT", &self.target_endpoint)
            .env("TEAM_AGENT_TEST_SOURCE_ENDPOINT", &self.source_endpoint)
            .env("TEAM_AGENT_TEST_REQUESTED_WORKSPACE", &self.workspace)
            .env("TEAM_AGENT_TEST_PANE_TTY", &tty)
            .env(
                "TEAM_AGENT_TEST_SOURCE_PANE_PRESENT",
                if ambient.pane_present() { "1" } else { "0" },
            )
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in hermetic_guard::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        match ambient {
            Ambient::None => {
                command
                    .env_remove("TMUX")
                    .env("TMUX_PANE", "%stray-without-tmux");
            }
            Ambient::SameServer => {
                command
                    .env("TMUX", self.tmux_tuple(&self.target_endpoint))
                    .env("TMUX_PANE", AMBIENT_PANE);
            }
            Ambient::DifferentServerLivePane | Ambient::DifferentServerMissingPane => {
                command
                    .env("TMUX", self.tmux_tuple(&self.source_endpoint))
                    .env("TMUX_PANE", AMBIENT_PANE);
            }
        }
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
        let child = command.spawn().expect("spawn public team-agent CLI in pty");
        let output = child.wait_with_output().expect("wait for public CLI");
        drop(master);
        output
    }

    fn tmux_tuple(&self, endpoint: &Path) -> String {
        format!("{},4242,0", endpoint.to_string_lossy())
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

    fn provider_log(&self) -> String {
        fs::read_to_string(&self.provider_log_path).unwrap_or_default()
    }

    fn provider_launches(&self) -> usize {
        self.provider_log()
            .lines()
            .filter(|line| line.starts_with("launch "))
            .count()
    }

    fn target_socket_name(&self) -> &str {
        &self.target_socket_name
    }

    fn source_endpoint_str(&self) -> &str {
        self.source_endpoint
            .to_str()
            .expect("fixture source endpoint is UTF-8")
    }
}

fn observed_route(tmux_log: &str, provider_log: &str, success: bool) -> ObservedRoute {
    if provider_log.lines().any(|line| line.starts_with("launch ")) {
        ObservedRoute::DirectProvider
    } else if has_operation(tmux_log, "switch-client") {
        ObservedRoute::SwitchClient
    } else if has_operation(tmux_log, "attach-session") {
        ObservedRoute::AttachSession
    } else if !success {
        ObservedRoute::Refused
    } else {
        ObservedRoute::Unknown
    }
}

fn has_operation(log: &str, operation: &str) -> bool {
    log.lines()
        .any(|line| line.split_whitespace().any(|token| token == operation))
}

fn json_stdout(label: &str, output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{label} must exit zero; status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout must be JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fixture");
}

const PROVIDER_SHIM: &str = r#"#!/bin/sh
if [ "${1-}" = "--version" ]; then
  printf 'codex 1.0.0\n'
  exit 0
fi
printf 'launch %s\n' "$*" >> "$TEAM_AGENT_TEST_PROVIDER_LOG"
exit 0
"#;

const TMUX_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$TEAM_AGENT_TEST_TMUX_LOG"
selector=
previous=
target=
last=
spawn_session=
for arg in "$@"; do
  if [ "$previous" = "-S" ]; then selector="$arg"; fi
  if [ "$previous" = "-L" ]; then selector="$arg"; fi
  if [ "$previous" = "-t" ]; then target="$arg"; fi
  if [ "$previous" = "-s" ]; then spawn_session="$arg"; fi
  previous="$arg"
  last="$arg"
done

if [ "${1-}" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi

case " $* " in
  *" has-session "*) exit 1 ;;
  *" new-session "*|*" new-window "*)
    if [ -n "$spawn_session" ]; then
      printf '%s\n' "$spawn_session" > "$TEAM_AGENT_TEST_SPAWN_SESSION"
    fi
    printf '%%managed-n41\n'
    exit 0
    ;;
  *" list-panes "*)
    if [ "$selector" = "$TEAM_AGENT_TEST_SOURCE_ENDPOINT" ] && \
       [ "$TEAM_AGENT_TEST_SOURCE_PANE_PRESENT" = "1" ]; then
      printf '%%ambient-n41\tforeign-source\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4101\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    fi
    if [ "$selector" = "$TEAM_AGENT_TEST_TARGET_ENDPOINT" ]; then
      printf '%%ambient-n41\ttarget-source\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4102\t\n' \
        "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    fi
    if [ "$selector" = "$TEAM_AGENT_TEST_TARGET_SOCKET_NAME" ] || \
       [ "$selector" = "$TEAM_AGENT_TEST_TARGET_ENDPOINT" ]; then
      managed_session=managed-leader
      if [ -f "$TEAM_AGENT_TEST_SPAWN_SESSION" ]; then
        managed_session=$(cat "$TEAM_AGENT_TEST_SPAWN_SESSION")
      fi
      printf '%%managed-n41\t%s\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4103\t\n' \
        "$managed_session" "$TEAM_AGENT_TEST_PANE_TTY" "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE"
    fi
    exit 0
    ;;
  *" display-message "*)
    case "$last" in
      '#{pane_id}') printf '%s\n' "${target:-%managed-n41}" ;;
      '#{pane_current_command}') printf 'codex\n' ;;
      '#{pane_current_path}') printf '%s\n' "$TEAM_AGENT_TEST_REQUESTED_WORKSPACE" ;;
      '#{pane_tty}') printf '%s\n' "$TEAM_AGENT_TEST_PANE_TTY" ;;
      '#{pane_dead}') printf '0\n' ;;
      '#{session_name}') printf 'managed-leader\n' ;;
      '#{pane_width}') printf '120\n' ;;
      '#{pane_mode}') printf '0\n' ;;
      *) printf '%s\n' "${target:-%managed-n41}" ;;
    esac
    exit 0
    ;;
  *" switch-client "*|*" attach-session "*) exit 0 ;;
  *) exit 0 ;;
esac
"#;
