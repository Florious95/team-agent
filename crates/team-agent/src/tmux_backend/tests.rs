//! TMUX-BACKEND RED — every `Transport` method is `unimplemented!()` today, so these PANIC (RED)
//! until the porter wires the bodies + `RealCommandRunner`. The OS edge is mocked by
//! `MockCommandRunner` (records each argv; returns canned `CommandOutput`/io::Error you stage).
//! Each test asserts (1) the recorded argv == the golden-locked `transport::tmux_*_argv` builder
//! (or the golden command form for builder-less ops) and (2) the parsed typed return. Golden:
//! runtime.py (has-session/spawn/kill), leader/__init__.py:335 (set-environment), state.py:341
//! (_tmux_pane_liveness three-state, §bug-085 unknown != dead), transport.rs argv-builders.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{ worker_shell_wrapper_command, CommandOutput, CommandRunner, RealCommandRunner, TmuxBackend, PANE_BINDING_NONCE_METADATA_KEY };
use crate::layout::worker_env::{
    caller_provider_resolution_from_values, inject_current_caller_provider,
    isolate_worker_spawn_env, worker_spawn_env, CallerProviderResolution, CALLER_ENDPOINT_ENV,
    CALLER_PANE_ENV, CALLER_PROVIDER_ENV,
};
use crate::model::enums::{PaneLiveness, Provider};
use crate::transport::{ normalize_capture, tmux_capture_argv, tmux_query_argv, tmux_send_keys_argv, tmux_spawn_argv, CaptureRange, InjectStage, Key, PaneField, PaneId, SessionName, Target, Transport, TransportError, WindowName };

type RecordedArgv = Arc<Mutex<Vec<Vec<String>>>>;
type RecordedStdin = Arc<Mutex<Vec<String>>>;

/// A staged runner response: a canned `CommandOutput`, or an io::Error (kind) for the error path.
#[derive(Clone)]
enum MockResp {
    Out(CommandOutput),
    Io(std::io::ErrorKind),
}

/// Records every argv it is asked to run; replays staged responses (then a default).
struct MockCommandRunner {
    recorded: RecordedArgv,
    stdin_recorded: RecordedStdin,
    queue: Mutex<VecDeque<MockResp>>,
    default: MockResp,
}

/// Models the real build-before-destroy overlap: the spawn command creates `%new`, while a
/// name-based lookup still resolves the same-named old window `%old`.
struct SameNameSpawnRunner {
    recorded: RecordedArgv,
}

impl CommandRunner for SameNameSpawnRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        let output = match argv.get(1).map(String::as_str) {
            Some("new-window") => ok("%new\n"),
            Some("display-message") => ok("%old\n"),
            Some("list-panes") => ok(
                "%old\tteamsess\t0\tdeveloper\t0\t/dev/ttys001\tnode\t1\t/work/dir\t1\t0\t101\n\
                 %new\tteamsess\t1\tdeveloper\t0\t/dev/ttys002\tnode\t1\t/work/dir\t1\t0\t202\n",
            ),
            _ => fail(64, "unexpected scripted tmux command"),
        };
        Ok(output)
    }


}

impl CommandRunner for MockCommandRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        let resp = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.default.clone());
        match resp {
            MockResp::Out(o) => Ok(o),
            MockResp::Io(kind) => Err(std::io::Error::new(kind, "mock runner io error")),
        }
    }


}

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}
fn fail(code: i32, stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        code: Some(code),
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

fn spawned_caller_env() -> (BTreeMap<String, String>, Vec<String>) {
    let mut env = worker_spawn_env(
        vec![
            (CALLER_PROVIDER_ENV.to_string(), "claude".to_string()),
            (CALLER_PANE_ENV.to_string(), "%old".to_string()),
            (CALLER_ENDPOINT_ENV.to_string(), "/tmp/old.sock".to_string()),
            (
                "TEAM_AGENT_LEADER_PROVIDER".to_string(),
                "claude".to_string(),
            ),
            ("TMUX".to_string(), "/tmp/parent.sock,1,0".to_string()),
            ("TMUX_PANE".to_string(), "%parent".to_string()),
        ],
        Path::new("/tmp/worker"),
        "child",
        Some("team"),
        None,
    );
    assert!(
        !env.keys().any(|key| key.starts_with("TEAM_AGENT_CALLER_")),
        "inherited caller namespace must be stripped before inject: {env:?}"
    );
    assert!(!env.contains_key("TEAM_AGENT_LEADER_PROVIDER"));
    let env_unset = isolate_worker_spawn_env(Provider::Pi, &mut env, Vec::<String>::new());
    inject_current_caller_provider(&mut env, Provider::Pi);
    (env, env_unset)
}

fn assert_caller_unset_and_assignment_once(command: &str) {
    assert_eq!(command.matches("unset TEAM_AGENT_CALLER_PROVIDER").count(), 1);
    assert_eq!(command.matches("unset TEAM_AGENT_CALLER_PANE_ID").count(), 1);
    assert_eq!(
        command
            .matches("unset TEAM_AGENT_CALLER_TMUX_ENDPOINT")
            .count(),
        1
    );
    assert_eq!(command.matches("TEAM_AGENT_CALLER_PROVIDER=pi").count(), 1);
    assert_eq!(command.matches("TEAM_AGENT_CALLER_PANE_ID=").count(), 1);
    assert_eq!(
        command.matches("TEAM_AGENT_CALLER_TMUX_ENDPOINT=").count(),
        1
    );
    assert!(command.contains("TEAM_AGENT_CALLER_PANE_ID=\"${TMUX_PANE-}\""));
    assert!(command.contains("TEAM_AGENT_CALLER_TMUX_ENDPOINT=\"${TMUX%%,*}\""));
}

fn assignment_token<'a>(command: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key}=");
    command
        .split_whitespace()
        .find(|token| token.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing {key} assignment in {command}"))
}

fn expand_caller_allowlist(command: &str, tmux: &str, pane: &str) -> (String, String, String) {
    let script = format!(
        "{}; {}; {}; printf '%s\\n%s\\n%s\\n' \"${{{}}}\" \"${{{}}}\" \"${{{}}}\"",
        assignment_token(command, CALLER_PROVIDER_ENV),
        assignment_token(command, CALLER_PANE_ENV),
        assignment_token(command, CALLER_ENDPOINT_ENV),
        CALLER_PROVIDER_ENV,
        CALLER_PANE_ENV,
        CALLER_ENDPOINT_ENV,
    );
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env("TMUX", tmux)
        .env("TMUX_PANE", pane)
        .env_remove(CALLER_PROVIDER_ENV)
        .env_remove(CALLER_PANE_ENV)
        .env_remove(CALLER_ENDPOINT_ENV)
        .output()
        .expect("expand caller allowlist through sh");
    assert!(
        output.status.success(),
        "sh expand failed: status={:?} stderr={} script={script}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("caller allowlist stdout utf8");
    let mut lines = stdout.lines();
    let provider = lines.next().unwrap_or("").to_string();
    let pane = lines.next().unwrap_or("").to_string();
    let endpoint = lines.next().unwrap_or("").to_string();
    (provider, pane, endpoint)
}

fn resolve_expanded_caller(
    command: &str,
    tmux: &str,
    pane: &str,
    scoped: Option<&str>,
) -> CallerProviderResolution {
    let (provider, context_pane, context_endpoint) = expand_caller_allowlist(command, tmux, pane);
    let current_endpoint = tmux.split(',').next().unwrap_or(tmux);
    caller_provider_resolution_from_values(
        Some(provider.as_str()),
        Some(context_pane.as_str()),
        Some(context_endpoint.as_str()),
        None,
        None,
        Some(pane),
        Some(current_endpoint),
        scoped.or(Some(current_endpoint)),
    )
}

#[test]
fn worker_caller_provider_is_generated_and_scoped_to_provider_invocation() {
    let (env, env_unset) = spawned_caller_env();
    let argv = ["pi".to_string(), "--help".to_string()];
    let cwd = Path::new("/tmp/worker");
    let wrapper = worker_shell_wrapper_command(&argv, cwd, &env, &env_unset, "pi");
    let cold_start = super::shell_command(&argv, cwd, &env, &env_unset);
    assert_eq!(env.get(CALLER_PROVIDER_ENV).map(String::as_str), Some("pi"));
    assert!(!env.contains_key(CALLER_PANE_ENV));
    assert!(!env.contains_key(CALLER_ENDPOINT_ENV));
    assert_caller_unset_and_assignment_once(&wrapper);
    assert_caller_unset_and_assignment_once(&cold_start);
    assert!(
        cold_start.contains("exec"),
        "cold-start shell_command must exec the provider: {cold_start}"
    );
    let wrapper_tail = wrapper.split_once("; rc=$?").map(|(_, tail)| tail).unwrap_or("");
    assert!(
        !wrapper_tail.contains("TEAM_AGENT_CALLER_"),
        "provider-exit tail must not retain caller context: {wrapper_tail}"
    );
    assert!(
        !cold_start.contains("; rc=$?"),
        "cold-start exec path has no inert tail: {cold_start}"
    );
    let tmux = "/tmp/tmux.sock,123,0";
    let pane = "%1";
    assert_eq!(
        resolve_expanded_caller(&wrapper, tmux, pane, Some("/tmp/tmux.sock")),
        CallerProviderResolution::Valid(Provider::Pi)
    );
    assert_eq!(
        resolve_expanded_caller(&cold_start, tmux, pane, Some("/tmp/tmux.sock")),
        CallerProviderResolution::Valid(Provider::Pi)
    );
    // Preserve the context generated in pane %1; only the caller moves to %2.
    let (provider, context_pane, context_endpoint) = expand_caller_allowlist(&wrapper, tmux, pane);
    assert_eq!(context_pane, "%1");
    assert_eq!(
        caller_provider_resolution_from_values(
            Some(&provider),
            Some(&context_pane),
            Some(&context_endpoint),
            None,
            None,
            Some("%2"),
            Some("/tmp/tmux.sock"),
            Some("/tmp/tmux.sock"),
        ),
        CallerProviderResolution::Invalid
    );
}

#[test]
fn worker_caller_context_is_rewritten_per_spawn_and_not_inherited() {
    let (parent_env, parent_unset) = spawned_caller_env();
    let parent_cmd = worker_shell_wrapper_command(
        &["pi".to_string()],
        Path::new("/tmp/parent-worker"),
        &parent_env,
        &parent_unset,
        "pi",
    );
    assert_eq!(
        resolve_expanded_caller(&parent_cmd, "/tmp/current.sock,1,0", "%4", None),
        CallerProviderResolution::Valid(Provider::Pi)
    );

    let mut child_parent = parent_env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    child_parent.push((CALLER_PROVIDER_ENV.to_string(), "pi".to_string()));
    child_parent.push((CALLER_PANE_ENV.to_string(), "%4".to_string()));
    child_parent.push((
        CALLER_ENDPOINT_ENV.to_string(),
        "/tmp/current.sock".to_string(),
    ));
    let mut child_env = worker_spawn_env(
        child_parent,
        Path::new("/tmp/child-worker"),
        "grandchild",
        Some("child-team"),
        None,
    );
    assert!(
        !child_env
            .keys()
            .any(|key| key.starts_with("TEAM_AGENT_CALLER_")),
        "child spawn must strip the current worker caller namespace: {child_env:?}"
    );
    let child_unset = isolate_worker_spawn_env(Provider::Codex, &mut child_env, Vec::<String>::new());
    inject_current_caller_provider(&mut child_env, Provider::Codex);
    let child_cmd = worker_shell_wrapper_command(
        &["codex".to_string()],
        Path::new("/tmp/child-worker"),
        &child_env,
        &child_unset,
        "codex",
    );
    assert!(child_cmd.contains("TEAM_AGENT_CALLER_PROVIDER=codex"));
    assert!(!child_cmd.contains("TEAM_AGENT_CALLER_PROVIDER=pi"));
    assert_eq!(
        resolve_expanded_caller(&child_cmd, "/tmp/child.sock,1,0", "%8", None),
        CallerProviderResolution::Valid(Provider::Codex)
    );
}

/// Build a backend over a mock runner: `default` answers every un-queued call; `queued` is drained
/// first. Returns the backend + the shared recorded-argv handle (read AFTER the call).
fn backend_with(default: MockResp, queued: Vec<MockResp>) -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stdin_recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = MockCommandRunner {
        recorded: Arc::clone(&recorded),
        stdin_recorded,
        queue: Mutex::new(queued.into_iter().collect()),
        default,
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}

#[test]
fn pane_dead_observation_requires_native_boolean_receipt() {
    for (text, expected) in [("1\n", Some(true)), ("0\n", Some(false)), ("", None), ("dead", None), ("2", None)] {
        let (backend, recorded) = backend_with(MockResp::Out(ok(text)), Vec::new());
        assert_eq!(backend.pane_is_dead(&PaneId::new("%47")).unwrap(), expected);
        assert_eq!(recorded.lock().unwrap()[0], svec(&[
            "tmux", "display-message", "-p", "-t", "%47", "#{pane_dead}",
        ]));
    }
    let (backend, _) = backend_with(MockResp::Out(fail(1, "pane unavailable")), Vec::new());
    assert_eq!(backend.pane_is_dead(&PaneId::new("%47")).unwrap(), None);
    let (backend, _) = backend_with(MockResp::Io(std::io::ErrorKind::TimedOut), Vec::new());
    assert!(backend.pane_is_dead(&PaneId::new("%47")).is_err());
}

#[test]
fn provider_exit_receipt_accepts_only_shell_exit_status() {
    for (text, expected) in [
        ("", None),
        ("running", None),
        ("[team-agent] pi exited with 0", None),
        ("256", None),
        ("0\n", Some(0)),
        ("17\n", Some(17)),
        ("143\n", Some(143)),
    ] {
        let (backend, recorded) = backend_with(MockResp::Out(ok(text)), Vec::new());
        assert_eq!(
            backend.provider_exit_status(&PaneId::new("%47")).unwrap(),
            expected
        );
        let calls = recorded.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][4], "%47");
        assert_eq!(calls[0][5], "#{@team-agent-provider-exit-status}");
    }
    let (backend, _) = backend_with(MockResp::Out(fail(1, "pane unavailable")), Vec::new());
    assert_eq!(
        backend.provider_exit_status(&PaneId::new("%47")).unwrap(),
        None
    );
}

fn backend_with_stdin(
    default: MockResp,
    queued: Vec<MockResp>,
) -> (TmuxBackend, RecordedArgv, RecordedStdin) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let stdin_recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = MockCommandRunner {
        recorded: Arc::clone(&recorded),
        stdin_recorded: Arc::clone(&stdin_recorded),
        queue: Mutex::new(queued.into_iter().collect()),
        default,
    };
    (
        TmuxBackend::with_runner(Box::new(runner)),
        recorded,
        stdin_recorded,
    )
}

#[test]
fn a21_spawn_command_too_long_is_structured_before_transport() {
    let (backend, recorded) = backend_with(MockResp::Out(ok("%0\n")), Vec::new());
    let session = SessionName::new("a21-session");
    let window = WindowName::new("worker-long-role");
    let argv = vec!["codex".to_string(), "x".repeat(20_000)];

    let error = backend
        .spawn_first(
            &session,
            &window,
            &argv,
            Path::new("/tmp/a21-workspace"),
            &BTreeMap::new(),
        )
        .expect_err("oversized spawn must fail before tmux transport");
    let text = error.to_string();
    assert!(
        text.contains("command_too_long"),
        "structured error: {text}"
    );
    assert!(
        text.contains("worker-long-role"),
        "seat must be named: {text}"
    );
    assert!(
        text.contains("actual_bytes="),
        "actual length must be visible: {text}"
    );
    assert!(
        text.contains("limit_bytes="),
        "limit must be visible: {text}"
    );
    assert!(
        recorded.lock().unwrap().is_empty(),
        "tmux must not be called: {text}"
    );
}

fn svec(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn apply(vars: &[(&str, Option<&str>)]) -> Self {
        let saved = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

#[test]
#[serial_test::serial(env)]
fn socket_name_resolution_uses_native_tmpdir_without_foreign_root_fallback() {
    let root = std::env::temp_dir().join(format!("ta-socket-root-{}-{}",
        std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir(&root).unwrap();
    let native = root.join("native");
    let ordinary = root.join("ordinary");
    let uid = unsafe { libc::geteuid() };
    let name = root.file_name().unwrap().to_str().unwrap();
    let expected = native.join(format!("tmux-{uid}")).join(name);
    let foreign = ordinary.join(format!("tmux-{uid}")).join(name);
    std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    std::fs::write(&foreign, b"").unwrap();
    let _env = EnvGuard::apply(&[
        ("TMUX_TMPDIR", native.to_str()), ("TMPDIR", ordinary.to_str()),
    ]);
    let expected = expected.parent().unwrap().canonicalize().unwrap().join(name);
    assert_eq!(super::socket_path_for_name(name), Some(expected.clone()));
    assert_eq!(super::existing_socket_path_for_name(name), None);
    assert!(super::tmux_socket_roots().contains(&native.join(format!("tmux-{uid}"))));
    std::fs::write(&expected, b"").unwrap();
    assert_eq!(super::existing_socket_path_for_name(name), Some(expected));
    for value in [None, Some("")] {
        let _default = EnvGuard::apply(&[("TMUX_TMPDIR", value)]);
        let expected = Path::new("/tmp").canonicalize().unwrap().join(format!("tmux-{uid}")).join(name);
        assert_eq!(super::socket_path_for_name(name), Some(expected));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[serial_test::serial(env)]
fn leader_receiver_endpoint_from_tmux_env_preserves_full_socket_path() {
    let leader_socket = "/tmp/ta-leader-root/tmux-501/dl2f";
    let _env = EnvGuard::apply(&[
        ("TMUX", Some("/tmp/ta-leader-root/tmux-501/dl2f,12345,0")),
        ("TMUX_TMPDIR", Some("/tmp/ta-coordinator-root")),
    ]);

    assert_eq!(
        super::socket_name_from_tmux_env().as_deref(),
        Some(leader_socket),
        "leader receivers must persist the exact tmux endpoint from $TMUX; a short -L socket \
             name is re-rooted under the coordinator's TMUX_TMPDIR and cannot reach an external \
             leader pane"
    );
}

#[test]
#[serial_test::serial(env)]
fn leader_receiver_endpoint_from_tmux_env_rejects_short_socket_name() {
    let _env = EnvGuard::apply(&[
        ("TMUX", Some("dl9aa40c88,12345,0")),
        ("TMUX_TMPDIR", Some("/tmp/ta-coordinator-root")),
    ]);

    assert_eq!(
            super::socket_name_from_tmux_env(),
            None,
            "leader_receiver.tmux_socket is a durable physical endpoint: a short socket name from \
             $TMUX must not be persisted because tmux -L <short> is re-rooted under the coordinator"
        );
}

#[test]
fn leader_receiver_delivery_uses_full_socket_endpoint_not_short_l_reconstruction() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let delivery = std::fs::read_to_string(manifest.join("src/messaging/delivery.rs")).unwrap();
    let leader_receiver =
        std::fs::read_to_string(manifest.join("src/messaging/leader_receiver.rs")).unwrap();
    let tmux_backend = std::fs::read_to_string(manifest.join("src/tmux_backend.rs")).unwrap();

    assert!(
            tmux_backend.contains("\"-S\""),
            "tmux backend must support `tmux -S <full-socket-path>` for persisted external leader \
             endpoints; `-L <short-name>` is not enough when leader and coordinator TMUX_TMPDIR differ"
        );
    assert!(
        !delivery.contains("TmuxBackend::for_socket_name(socket)"),
        "worker->leader delivery must not reconstruct an external leader endpoint with \
             `tmux -L <short-name>`; it must use the persisted full socket path endpoint"
    );
    assert!(
        !leader_receiver.contains("TmuxBackend::for_socket_name(socket)"),
        "leader_receiver live checks must verify the same full socket endpoint used by delivery, \
            not a short socket name resolved under the coordinator's socket root"
    );
}

#[test]
fn leader_receiver_full_endpoint_liveness_list_and_inject_use_s_path_command_shape() {
    let endpoint = "/private/tmp/tmux-501/default";
    let stdout = "%7\tteam-x\t0\tleader\t0\t/dev/ttys003\tbash\t1\t/Users/me/work\t1\t0\n";
    let (be, rec, _stdin) = {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdin_recorded = Arc::new(Mutex::new(Vec::new()));
        let runner = MockCommandRunner {
            recorded: Arc::clone(&recorded),
            stdin_recorded: Arc::clone(&stdin_recorded),
            queue: Mutex::new(
                vec![
                    MockResp::Out(ok(stdout)),
                    MockResp::Out(ok("%7\n")),
                    MockResp::Out(ok("")),
                    MockResp::Out(ok("")),
                    MockResp::Out(ok("")),
                ]
                .into_iter()
                .collect(),
            ),
            default: MockResp::Out(ok("")),
        };
        (
            TmuxBackend::with_runner_for_tmux_endpoint(Box::new(runner), endpoint),
            recorded,
            stdin_recorded,
        )
    };

    let _ = be.list_targets().expect("list_targets via endpoint");
    let _ = be
        .liveness(&PaneId::new("%7"))
        .expect("liveness via endpoint");
    be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::Enter])
        .expect("send_keys via endpoint");

    let calls = rec.lock().unwrap().clone();
    assert!(
            calls.len() >= 3,
            "fixture must exercise list-panes, display-message, and send-keys; got {calls:?}"
        );
    for call in &calls {
        assert!(
                call.starts_with(&["tmux".to_string(), "-S".to_string(), endpoint.to_string()]),
                "leader receiver list/liveness/send_keys must use tmux -S <full socket path>; got {call:?}"
            );
        assert!(
            !call
                .windows(2)
                .any(|w| w == ["-L".to_string(), endpoint.to_string()]),
            "leader receiver full endpoint must never be reconstructed with -L; got {call:?}"
        );
    }
    assert!(
        calls
            .iter()
            .any(|call| call.iter().any(|arg| arg == "list-panes"))
            && calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "display-message"))

            && calls
                .iter()
                .any(|call| call.iter().any(|arg| arg == "send-keys")),
        "contract must cover liveness/list/inject command shapes; got {calls:?}"
    );
}

#[test]
#[serial_test::serial(env)]
fn leader_receiver_short_endpoint_must_not_reconstruct_tmux_l_socket() {
    let endpoint = "dl9aa40c88";
    let uid = unsafe { libc::geteuid() };
    let tmp = std::env::temp_dir().join(format!("ta-tmux-short-endpoint-{}", std::process::id()));
    let root = tmp.join(format!("tmux-{uid}"));
    std::fs::create_dir_all(&root).unwrap();
    let socket_path = root.join(endpoint);
    let _listener = UnixListener::bind(&socket_path).unwrap();
    let socket_path = socket_path.canonicalize().unwrap();
    let _env = EnvGuard::apply(&[("TMUX_TMPDIR", Some(tmp.to_str().unwrap()))]);
    let (be, rec) = {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let runner = MockCommandRunner {
            recorded: Arc::clone(&recorded),
            stdin_recorded: Arc::new(Mutex::new(Vec::new())),
            queue: Mutex::new(vec![MockResp::Out(ok(""))].into_iter().collect()),
            default: MockResp::Out(ok("")),
        };
        (
            TmuxBackend::with_runner_for_tmux_endpoint(Box::new(runner), endpoint),
            recorded,
        )
    };

    let _ = be
        .list_targets()
        .expect("short endpoint should not become -L");

    let calls = rec.lock().unwrap().clone();
    assert!(
        calls.iter().all(|call| !call
            .windows(2)
            .any(|w| w == ["-L".to_string(), endpoint.to_string()])),
        "non-canonical leader endpoints must be rejected or left unbound, never reconstructed as \
             tmux -L <short> under the coordinator socket root; calls={calls:?}"
    );
    assert!(
            calls.iter().all(|call| call.starts_with(&[
                "tmux".to_string(),
                "-S".to_string(),
                socket_path.to_string_lossy().to_string()
            ])),
            "short leader endpoints must resolve to the existing physical socket path with -S; calls={calls:?}"
        );
}

// ── 1. has_session: exit 0 -> true, exit 1 -> false; argv = `tmux has-session -t <s>` ──────────
#[test]
fn has_session_argv_and_exit_code_maps_to_bool() {
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    assert!(
        be.has_session(&SessionName::new("sess"))
            .expect("has_session"),
        "exit 0 -> true"
    );
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&["tmux", "has-session", "-t", "sess"])
    );

    let (be, rec) = backend_with(MockResp::Out(fail(1, "can't find session: sess")), vec![]);
    assert!(
        !be.has_session(&SessionName::new("sess"))
            .expect("has_session"),
        "exit 1 -> false"
    );
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&["tmux", "has-session", "-t", "sess"])
    );
}

// ── 2. spawn_first / spawn_into frame via tmux_spawn_argv; canned output parses pane id ────────
#[test]
fn spawn_first_frames_via_new_session_builder_and_parses_pane_id() {
    let pane_inventory = "%3\tteamsess\t0\tw1\t0\t/dev/ttys003\tnode\t1\t/work/dir\t1\t0\t123\n";
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![MockResp::Out(ok("%3\n")), MockResp::Out(ok(pane_inventory))],
    );
    let s = SessionName::new("teamsess");
    let w = WindowName::new("w1");
    let env = BTreeMap::from([("TEAM_AGENT_ID".to_string(), "w1".to_string())]);
    let result = be
        .spawn_first(
            &s,
            &w,
            &svec(&["provider-bin", "--flag"]),
            Path::new("/work/dir"),
            &env,
        )
        .expect("spawn_first");
    let calls = rec.lock().unwrap().clone();
    let argv = calls[0].clone();
    let cmd = argv.last().expect("the sh -lc command string").clone();
    assert_eq!(
        argv,
        tmux_spawn_argv(&s, &w, &cmd, true),
        "spawn_first must frame via tmux_spawn_argv (new-session -d -s <s> -n <w> sh -lc <cmd>)"
    );
    assert!(
        cmd.contains("provider-bin"),
        "the provider argv must be in the sh -lc command; got {cmd}"
    );
    assert_eq!(
        result.pane_id.as_str(),
        "%3",
        "SpawnResult.pane_id must parse from the tmux output"
    );
    assert_eq!(result.child_pid, Some(123));
    assert!(
        calls
            .iter()
            .all(|call| call.get(1).is_none_or(|arg| arg != "display-message")),
        "spawn_first identity must come from new-session stdout, never a display-message lookup; calls={calls:?}"
    );
}

#[test]
fn spawn_into_frames_via_new_window_builder() {
    let pane_inventory = "%4\tteamsess\t1\tw2\t0\t/dev/ttys004\tnode\t1\t/work/dir\t1\t0\t124\n";
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![MockResp::Out(ok("%4\n")), MockResp::Out(ok(pane_inventory))],
    );
    let s = SessionName::new("teamsess");
    let w = WindowName::new("w2");
    let result = be
        .spawn_into(
            &s,
            &w,
            &svec(&["provider-bin"]),
            Path::new("/work/dir"),
            &BTreeMap::new(),
        )
        .expect("spawn_into");
    let calls = rec.lock().unwrap().clone();
    let argv = calls[0].clone();
    let cmd = argv.last().expect("the sh -lc command string").clone();
    assert_eq!(
            argv,
            tmux_spawn_argv(&s, &w, &cmd, false),
            "spawn_into must frame via tmux_spawn_argv first=false (new-window -t <s> -n <w> sh -lc <cmd>)"
        );
    assert_eq!(result.pane_id.as_str(), "%4");
    assert!(
        calls
            .iter()
            .all(|call| call.get(1).is_none_or(|arg| arg != "display-message")),
        "spawn_into identity must come from new-window stdout, never a display-message lookup; calls={calls:?}"
    );
}

#[test]
fn spawn_into_same_named_replacement_returns_identity_created_by_spawn_command() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let be = TmuxBackend::with_runner(Box::new(SameNameSpawnRunner {
        recorded: Arc::clone(&recorded),
    }));

    let result = be
        .spawn_into(
            &SessionName::new("teamsess"),
            &WindowName::new("developer"),
            &svec(&["provider-bin"]),
            Path::new("/work/dir"),
            &BTreeMap::new(),
        )
        .expect("same-name replacement spawn");
    let calls = recorded.lock().unwrap().clone();
    let name_based_identity_lookup = calls.iter().any(|call| {
        call.get(1).is_some_and(|arg| arg == "display-message")
            && call
                .iter()
                .position(|arg| arg == "-t")
                .and_then(|index| call.get(index + 1))
                .is_some_and(|target| target == "teamsess:developer")
    });

    assert_eq!(
        result.pane_id.as_str(),
        "%new",
        "spawn_into must return the pane identity atomically emitted by this new-window command, not the same-named old window; calls={calls:?}"
    );
    assert!(
        !name_based_identity_lookup,
        "spawn identity must not be re-derived through ambiguous session:window display-message; calls={calls:?}"
    );
}

#[test]
fn spawn_with_command_refuses_and_rolls_back_spawn_pane_owned_by_other_window() {
    let pane_inventory = "%5\tteamsess\t1\tw2\t0\t/dev/ttys005\tnode\t1\t/work/dir\t1\t0\t125\n";
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![MockResp::Out(ok("%5\n")), MockResp::Out(ok(pane_inventory))],
    );
    let err = be
        .spawn_into(
            &SessionName::new("teamsess"),
            &WindowName::new("w1"),
            &svec(&["provider-bin"]),
            Path::new("/work/dir"),
            &BTreeMap::new(),
        )
        .expect_err("spawn stdout pane owned by w2 must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("requested=teamsess:w1")
            && msg.contains("observed_pane=%5")
            && msg.contains("observed=teamsess:w2"),
        "error must include requested/observed ownership evidence, got {msg}"
    );
    let calls = rec.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|call| call == &svec(&["tmux", "kill-pane", "-t", "%5"])),
        "identity mismatch must roll back the exact pane created by this spawn; calls={calls:?}"
    );
}

#[test]
fn spawn_with_command_retries_until_spawn_pane_is_visible() {
    let pane_inventory = "%6\tteamsess\t1\tw1\t0\t/dev/ttys006\tnode\t1\t/work/dir\t1\t0\t126\n";
    let (be, rec) = backend_with(
        MockResp::Out(ok(pane_inventory)),
        vec![MockResp::Out(ok("%6\n")), MockResp::Out(ok(""))],
    );
    let result = be
        .spawn_into(
            &SessionName::new("teamsess"),
            &WindowName::new("w1"),
            &svec(&["provider-bin"]),
            Path::new("/work/dir"),
            &BTreeMap::new(),
        )
        .expect("spawn pane must tolerate bounded tmux inventory lag");
    let calls = rec.lock().unwrap().clone();

    assert_eq!(result.pane_id.as_str(), "%6");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.get(1).is_some_and(|arg| arg == "list-panes"))
            .count(),
        2,
        "spawn identity must retry the same pane id until it becomes visible; calls={calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| call.get(1).is_none_or(|arg| arg != "kill-pane")),
        "a delayed but valid identity must not be rolled back; calls={calls:?}"
    );
}

#[test]
fn spawn_with_command_empty_stdout_fails_closed_without_identity_fallback() {
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![MockResp::Out(ok(""))]);
    let err = be
        .spawn_into(
            &SessionName::new("teamsess"),
            &WindowName::new("developer"),
            &svec(&["provider-bin"]),
            Path::new("/work/dir"),
            &BTreeMap::new(),
        )
        .expect_err("empty new-window stdout must fail closed");
    let msg = err.to_string().to_ascii_lowercase();
    let calls = rec.lock().unwrap().clone();
    let used_identity_fallback = calls
        .iter()
        .any(|call| call.get(1).is_some_and(|arg| arg == "display-message"));

    assert!(
        msg.contains("spawn") && (msg.contains("empty") || msg.contains("no pane id")),
        "empty spawn stdout error must name the missing spawn identity; error={err}; calls={calls:?}"
    );
    assert!(
        !used_identity_fallback,
        "empty spawn stdout must not fall back to ambiguous display-message identity lookup; calls={calls:?}"
    );
}

#[test]
fn spawn_split_selects_even_horizontal_not_tiled() {
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![MockResp::Out(ok("%5")), MockResp::Out(ok(""))],
    );
    let s = SessionName::new("teamsess");
    let w = WindowName::new("team-w1");
    let result = be
        .spawn_split_with_env_unset(
            &s,
            &w,
            &svec(&["provider-bin"]),
            Path::new("/work/dir"),
            &BTreeMap::new(),
            &[],
        )
        .expect("spawn_split");
    assert_eq!(result.pane_id.as_str(), "%5");
    let calls = rec.lock().unwrap().clone();
    // E53 (0.3.26): split-window now carries `-d` (no focus steal).
    assert_eq!(
        calls[0][0..5],
        svec(&["tmux", "split-window", "-d", "-t", "teamsess:team-w1"])
    );
    assert_eq!(
        calls[1],
        svec(&[
            "tmux",
            "select-layout",
            "-t",
            "teamsess:team-w1",
            "even-horizontal",
        ])
    );
    assert!(
        !calls.iter().flatten().any(|arg| arg == "tiled"),
        "adaptive split must not leave tmux tiled layout in the command stream: {calls:?}"
    );
}

#[test]
fn configure_adaptive_pane_title_sets_border_and_pane_title() {
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    be.configure_adaptive_pane_title(
        &SessionName::new("teamsess"),
        &WindowName::new("team-w1"),
        &PaneId::new("%7"),
        "builder",
    )
    .expect("configure title");
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            svec(&[
                "tmux",
                "set-window-option",
                "-t",
                "teamsess:team-w1",
                "pane-border-status",
                "bottom",
            ]),
            svec(&[
                "tmux",
                "set-window-option",
                "-t",
                "teamsess:team-w1",
                "pane-border-format",
                " #{pane_title} ",
            ]),
            svec(&["tmux", "select-pane", "-t", "%7", "-T", "builder"]),
        ]
    );
}

// ── 3. set_session_env: argv = `tmux set-environment -t <s> <k> <v>`; success -> Applied ───────

// ── 4. capture: argv = tmux_capture_argv; canned scrollback -> normalize_capture -> CapturedText ─
#[test]
fn capture_argv_and_normalizes_scrollback() {
    let scroll = "line one  \nbusy\u{a0}marker   \n  \n";
    let (be, rec) = backend_with(MockResp::Out(ok(scroll)), vec![]);
    let pane = PaneId::new("%7");
    let captured = be
        .capture(&Target::Pane(pane.clone()), CaptureRange::Tail(40))
        .expect("capture");
    assert_eq!(
        rec.lock().unwrap()[0],
        tmux_capture_argv(&pane, CaptureRange::Tail(40))
    );
    assert_eq!(
        captured.text,
        normalize_capture(scroll),
        "capture output must be normalize_capture'd"
    );
    assert_eq!(captured.range, CaptureRange::Tail(40));
}

// ── 5a. send_keys: argv = tmux_send_keys_argv ──────────────────────────────────────────────────
#[test]
fn send_keys_argv_matches_builder() {
    let (be, rec) = backend_with(MockResp::Out(ok("0\n")), vec![]);
    let pane = PaneId::new("%7");
    be.send_keys(&Target::Pane(pane.clone()), &[Key::Enter])
        .expect("send_keys");
    let calls = rec.lock().unwrap().clone();
    let submit = calls.iter().find(|argv| {
        argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "Enter")
            && !argv.iter().any(|a| a == "-l")
    });
    assert_eq!(
        submit.cloned().as_ref(),
        Some(&tmux_send_keys_argv(&pane, &[Key::Enter])),
        "Enter 键组仍走 tmux_send_keys_argv；prepare 只允许前缀 mode-query/201~; calls={calls:?}"
    );
}

// ── 5b. inject (text): set/load-buffer(text) -> paste-buffer -p -> submit send-keys; report Submit ─


// U1 #7 (RED→GREEN) — pre-submit pane readback: a token payload whose token is NOT
// visible in the pane before submit (paste silently dropped) must report
// CaptureMissingToken, not the static false-positive CaptureContainsToken.


struct InjectProbeRunner {
    recorded: RecordedArgv,
    capture: MockResp,
    command: String,
}

impl CommandRunner for InjectProbeRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        if argv.iter().any(|arg| arg.contains("#{pane_current_command}")) {
            return Ok(ok(&format!("{}\n", self.command)));
        }
        if argv.iter().any(|arg| arg.contains("#{pane_id}")) {
            return Ok(ok("%9\n"));
        }
        if argv.get(1).map(String::as_str) == Some("capture-pane") {
            return match &self.capture {
                MockResp::Out(output) => Ok(output.clone()),
                MockResp::Io(kind) => Err(std::io::Error::new(*kind, "capture unavailable")),
            };
        }
        Ok(ok(""))
    }


}

fn inject_probe_backend(
    capture: MockResp,
    command: &str,
) -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = InjectProbeRunner {
        recorded: Arc::clone(&recorded),
        capture,
        command: command.to_string(),
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}










// ═════════════════════════════════════════════════════════════════════════════
// E46 0.3.24 task#327 P0 — submit verification false-positive / false-negative.
//
// Architect + Workflow C1: do NOT flip `EnterSentWithoutPlaceholderCheck =>
// false` (breaks MUST-10 in provider_submit_verification_red.rs:113-159).
// Instead introduce SubmitConsumptionUnverified and only emit it from the
// bracketed-paste / Enter path when post-Enter input consumption is NOT
// observed within a bounded resend cap.
//
// Real-machine truth source (macmini): a fresh claude TUI's bracketed
// paste swallows the framework's Enter as paste content; the framework
// must (a) send Escape first to exit the paste bracket and (b) verify
// post-Enter consumption by structural input-empty probe (token marker
// no longer in the bottom 5 lines of the pane = composer cleared).
// ═════════════════════════════════════════════════════════════════════════════










// ═════════════════════════════════════════════════════════════════════════
// E50 PR-1 (0.3.24 P0, pasted-prompt 假阴诊断) — InjectReport.submit_diagnostics
// is populated for paste-prompt loops, and `pasted_prompt_match` / scrubbing
// helpers are pure functions safe to test deterministically.
//
// Architect verdict (Workflow forensic): the pre-fix
// `capture_has_pasted_content_prompt` matched ANY `pasted content` /
// `pasted text` token in the full Tail(80), including scrolled-off
// submitted blocks. PR-1 just adds the diagnostics; PR-2 will USE
// `where_in_tail` to fix the criterion. These tests pin the data shape.
// ═════════════════════════════════════════════════════════════════════════





#[test]
fn send_keys_cancel_mode_queries_mode_and_dispatches_cancel_argv() {
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![MockResp::Out(ok("tree-mode\n")), MockResp::Out(ok(""))],
    );
    be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::CancelMode])
        .expect("cancel mode");

    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        calls[0],
        svec(&["tmux", "display-message", "-p", "-t", "%7", "#{pane_mode}"])
    );
    assert_eq!(calls[1], svec(&["tmux", "send-keys", "-t", "%7", "q"]));
}

#[test]
fn cancel_mode_numeric_zero_is_input_ready_and_does_not_send_cancel() {
    // Golden /tmp/transport_golden_probe.py:
    // `_normalize_pane_mode("0") == ""`; `_prepare_tmux_pane_for_input` returns
    // pane_input_ready and does NOT call `_pane_mode_cancel`.
    // RED: pane_mode_from_raw("0") maps to Unknown, so Rust sends `-X cancel`.
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![MockResp::Out(ok("0\n"))]);
    be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::CancelMode])
        .expect("cancel mode input-ready no-op");

    let calls = rec.lock().unwrap().clone();
    assert_eq!(
            calls,
            vec![svec(&["tmux", "display-message", "-p", "-t", "%7", "#{pane_mode}"])],
            "pane_mode='0' is Python input-ready; CancelMode must stop after the mode query, got {calls:?}"
        );
}


// ── 6. liveness three-state (§bug-085): exit 0 -> Live; "can't find …" -> Dead; else -> Unknown ─
#[test]
fn liveness_is_three_state_unknown_is_not_dead() {
    let (be, rec) = backend_with(MockResp::Out(ok("%7")), vec![]);
    assert_eq!(
        be.liveness(&PaneId::new("%7")).expect("liveness"),
        PaneLiveness::Live
    );
    let argv0 = rec.lock().unwrap()[0].clone();
    assert!(
        argv0.contains(&"display-message".to_string())
            && argv0.iter().any(|x| x.contains("#{pane_id}"))
            && argv0.contains(&"%7".to_string()),
        "liveness must probe the pane via display-message #{{pane_id}}; got {argv0:?}"
    );

    let (be, _r) = backend_with(MockResp::Out(fail(1, "can't find pane %7")), vec![]);
    assert_eq!(
        be.liveness(&PaneId::new("%7")).expect("liveness"),
        PaneLiveness::Dead,
        "a 'can't find pane' failure -> Dead"
    );

    let (be, _r) = backend_with(
        MockResp::Out(fail(
            1,
            "error connecting to server: No such file or directory",
        )),
        vec![],
    );
    assert_eq!(
        be.liveness(&PaneId::new("%7")).expect("liveness"),
        PaneLiveness::Unknown,
        "a NON-'can't find' failure is UNKNOWN, not DEAD (§bug-085 three-state)"
    );
}

#[test]
fn has_pane_is_direct_existence_probe_not_liveness_guess() {
    let (be, rec) = backend_with(MockResp::Out(ok("%7")), vec![]);
    assert_eq!(
        be.has_pane(&PaneId::new("%7")).expect("has_pane"),
        Some(true)
    );
    let argv0 = rec.lock().unwrap()[0].clone();
    assert!(
        argv0.contains(&"display-message".to_string())
            && argv0.iter().any(|x| x.contains("#{pane_id}"))
            && argv0.contains(&"%7".to_string()),
        "has_pane must use the cheap display-message #{{pane_id}} probe; got {argv0:?}"
    );

    let (be, _r) = backend_with(MockResp::Out(ok("")), vec![]);
    assert_eq!(
        be.has_pane(&PaneId::new("%9999")).expect("has_pane"),
        Some(false),
        "real tmux can report a missing pane as exit 0 with empty stdout"
    );

    let (be, _r) = backend_with(MockResp::Out(fail(1, "can't find pane: %9999")), vec![]);
    assert_eq!(
        be.has_pane(&PaneId::new("%9999")).expect("has_pane"),
        Some(false)
    );

    let (be, _r) = backend_with(MockResp::Out(ok("%8")), vec![]);
    assert_eq!(
        be.has_pane(&PaneId::new("%7")).expect("has_pane"),
        None,
        "a successful but mismatched pane id is not proof that the requested pane exists"
    );

    let (be, _r) = backend_with(MockResp::Out(ok("not-a-pane")), vec![]);
    assert_eq!(
        be.has_pane(&PaneId::new("%7")).expect("has_pane"),
        None,
        "a successful but invalid pane id stays Unknown"
    );

    let (be, _r) = backend_with(
        MockResp::Out(fail(
            1,
            "error connecting to server: No such file or directory",
        )),
        vec![],
    );
    assert_eq!(
        be.has_pane(&PaneId::new("%7")).expect("has_pane"),
        None,
        "server/probe errors remain Unknown, not absent"
    );
}

// ── CP-1: per-team socket — for_workspace injects `-L ta-<hash>` at the run chokepoint; new() does NOT ─
#[test]
fn for_workspace_backend_injects_per_team_socket_but_default_backend_does_not() {
    use super::socket_name_for_workspace;
    let ws = Path::new("/tmp/ta-cp1-socket-test-ws");
    let socket = socket_name_for_workspace(ws);
    assert!(
        socket.starts_with("ta-") && socket.len() == 15,
        "socket name must be short + deterministic `ta-<12 hex>`; got {socket:?}"
    );
    // deterministic: the SAME workspace path always derives the SAME socket (CLI == daemon == ops).
    assert_eq!(
        socket,
        socket_name_for_workspace(ws),
        "socket derivation must be deterministic"
    );

    // workspace-bound backend: every executed `tmux` argv gets `-L <socket>` after the leading token.
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = MockCommandRunner {
        recorded: Arc::clone(&recorded),
        stdin_recorded: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(VecDeque::new()),
        default: MockResp::Out(ok("")),
    };
    let be = TmuxBackend::with_runner_for_workspace(Box::new(runner), ws);
    be.has_session(&SessionName::new("sess"))
        .expect("has_session");
    let argv = recorded.lock().unwrap()[0].clone();
    assert_eq!(
        argv,
        svec(&["tmux", "-L", &socket, "has-session", "-t", "sess"]),
        "for_workspace backend must inject `-L <socket>` right after `tmux`; got {argv:?}"
    );

    // default backend (new()/with_runner): NO `-L` — argv stays the golden-locked builder form.
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    be.has_session(&SessionName::new("sess"))
        .expect("has_session");
    assert_eq!(
            rec.lock().unwrap()[0],
            svec(&["tmux", "has-session", "-t", "sess"]),
            "the default-socket backend must NOT inject `-L` (existing tests + non-team callers unaffected)"
        );
}

// ── 7. kill_session / kill_window: golden argv; success -> Ok(()) ───────────────────────────────
#[test]
fn kill_session_and_kill_window_argv() {
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    be.kill_session(&SessionName::new("sess"))
        .expect("kill_session");
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&["tmux", "kill-session", "-t", "sess"])
    );

    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    be.kill_window(&Target::Pane(PaneId::new("%7")))
        .expect("kill_window");
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&["tmux", "kill-window", "-t", "%7"])
    );
}

#[test]
fn issue336_session_listing_accepts_tab_and_literal_escape_without_rewriting_names() {
    for separator in ["\t", "\\t"] {
        let (backend, calls) = backend_with(
            MockResp::Out(ok("")),
            vec![
                MockResp::Out(ok(&format!("%7{separator}worker\\tname\n"))),
                MockResp::Out(ok("%8\n")),
            ],
        );
        backend.preserve_session_before_stop(
            &SessionName::new("sess"),
            &[Target::SessionWindow {
                session: SessionName::new("sess"),
                window: WindowName::new("worker\\tname"),
            }],
        ).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 4, "separator={separator:?}");
    }
}

#[test]
fn issue336_escaped_listing_keeps_survivors_and_malformed_rows_fail_closed() {
    let (backend, calls) = backend_with(MockResp::Out(ok("%7\\tworker\n%8\\tleader\n")), vec![]);
    backend.preserve_session_before_stop(
        &SessionName::new("sess"), &[Target::Pane(PaneId::new("%7"))],
    ).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);
    for row in ["%7 worker", "%7", ""] {
        let (backend, calls) = backend_with(MockResp::Out(ok(row)), vec![]);
        assert!(backend.preserve_session_before_stop(
            &SessionName::new("sess"), &[Target::Pane(PaneId::new("%7"))],
        ).is_err());
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn stop_session_preservation_creates_processless_anchor_by_exact_pane_id() {
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![MockResp::Out(ok("%7\tworker\n")), MockResp::Out(ok("%8\n"))],
    );
    be.preserve_session_before_stop(
        &SessionName::new("sess"),
        &[Target::Pane(PaneId::new("%7"))],
    ).unwrap();
    let calls = rec.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0], svec(&[
        "tmux", "list-panes", "-s", "-t", "=sess", "-F", "#{pane_id}\t#{window_name}",
    ]));
    assert_eq!(calls[1], tmux_spawn_argv(
        &SessionName::new("=sess"), &WindowName::new("[team-agent keepalive]"),
        "exec sleep 86400", false,
    ));
    assert_eq!(calls[2], svec(&[
        "tmux", "set-window-option", "-t", "%8", "remain-on-exit", "on",
    ]));
    assert_eq!(calls[3], svec(&["tmux", "respawn-pane", "-k", "-t", "%8", "exit 0"]));
}

#[test]
fn stop_session_preservation_skips_surviving_workers_leaders_and_anchors() {
    for survivor in ["other-worker", "leader", "[team-agent keepalive]", "worker"] {
        let (be, rec) = backend_with(
            MockResp::Out(ok(&format!("%7\tworker\n%8\t{survivor}\n"))), vec![],
        );
        be.preserve_session_before_stop(
            &SessionName::new("sess"), &[Target::Pane(PaneId::new("%7"))],
        ).unwrap();
        assert_eq!(rec.lock().unwrap().len(), 1, "survivor={survivor}");
    }
}

#[test]
fn stop_session_preservation_covers_duplicate_panes_and_legacy_window_targets() {
    for targets in [
        vec![Target::Pane(PaneId::new("%7")), Target::Pane(PaneId::new("%8"))],
        vec![Target::SessionWindow { session: SessionName::new("sess"), window: WindowName::new("worker") }],
    ] {
        let (be, rec) = backend_with(MockResp::Out(ok("")), vec![
            MockResp::Out(ok("%7\tworker\n%8\tworker\n")), MockResp::Out(ok("%9\n")),
        ]);
        be.preserve_session_before_stop(&SessionName::new("sess"), &targets).unwrap();
        assert_eq!(rec.lock().unwrap().len(), 4);
    }
}

#[test]
fn stop_session_preservation_refuses_unknown_listing_and_rolls_back_failed_anchor() {
    for response in [fail(1, "listing failed"), ok(""), ok("malformed")] {
        let (be, rec) = backend_with(MockResp::Out(response), vec![]);
        assert!(be.preserve_session_before_stop(
            &SessionName::new("sess"), &[Target::Pane(PaneId::new("%7"))],
        ).is_err());
        assert_eq!(rec.lock().unwrap().len(), 1);
    }
    for failed_step in ["new-window", "set-window-option", "respawn-pane"] {
        let mut queued = vec![MockResp::Out(ok("%7\tworker\n"))];
        if failed_step != "new-window" { queued.push(MockResp::Out(ok("%8\n"))); }
        if failed_step == "respawn-pane" { queued.push(MockResp::Out(ok(""))); }
        queued.push(MockResp::Out(fail(1, "anchor failed")));
        let (be, rec) = backend_with(MockResp::Out(ok("")), queued);
        assert!(be.preserve_session_before_stop(
            &SessionName::new("sess"), &[Target::Pane(PaneId::new("%7"))],
        ).is_err());
        let calls = rec.lock().unwrap();
        assert!(!calls.iter().any(|argv| argv.get(1).is_some_and(|op| op == "kill-window")));
        if failed_step != "new-window" {
            assert_eq!(calls.last().unwrap(), &svec(&["tmux", "kill-pane", "-t", "%8"]));
        }
        assert!(!calls.iter().any(|argv| argv.get(1).is_some_and(|op| op == "kill-pane") && argv.contains(&"%7".to_string())));
    }
}

#[test]
#[ignore = "isolated native tmux: executed explicitly on the remote test host"]
fn stop_session_preservation_native_tmux_stop_start_and_shutdown() {
    let tmp = std::env::temp_dir().join(format!(
        "ta-preserve-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap(),
    ));
    std::fs::create_dir(&tmp).unwrap();
    let endpoint = tmp.join("tmux.sock");
    let be = TmuxBackend::with_runner_for_tmux_endpoint(Box::new(RealCommandRunner), endpoint.to_str().unwrap());
    let session = SessionName::new("preserve-test");
    struct Cleanup<'a>(&'a TmuxBackend, &'a SessionName, &'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill_session(self.1);
            let _ = std::fs::remove_dir_all(self.2);
        }
    }
    let _cleanup = Cleanup(&be, &session, &tmp);
    let argv = svec(&["sleep", "86400"]);
    let env = BTreeMap::new();
    let mut worker = be.spawn_first(&session, &WindowName::new("worker"), &argv, &tmp, &env).unwrap();
    for _ in 0..3 {
        be.preserve_session_before_stop(&session, &[Target::Pane(worker.pane_id.clone())]).unwrap();
        be.kill_pane(&worker.pane_id).unwrap();
        assert!(be.has_session(&session).unwrap());
        assert_eq!(be.has_pane(&worker.pane_id).unwrap(), Some(false));
        assert_eq!(be.list_windows(&session).unwrap(), vec![WindowName::new("[team-agent keepalive]")]);
        let anchor = be.list_targets().unwrap().into_iter().find(|pane| pane.session == session).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let dead = be.runner.run(&be.tmux_argv(&svec(&[
                "tmux", "display-message", "-p", "-t", anchor.pane_id.as_str(), "#{pane_dead}",
            ]))).unwrap();
            if dead.success && dead.stdout.trim() == "1" { break; }
            assert!(Instant::now() < deadline, "keepalive must have no running process: {dead:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        worker = be.spawn_into(&session, &WindowName::new("worker"), &argv, &tmp, &env).unwrap();
    }
    be.kill_session(&session).unwrap();
    assert!(!be.has_session(&session).unwrap());
}

// ── 8. ERROR MAPPING: non-zero tmux exit -> TransportError::Subprocess; runner io::Error -> Err ──
#[test]
fn error_paths_map_to_transport_error_not_panic() {
    // tmux cli non-zero exit (the Subprocess variant's documented purpose).
    let (be, _r) = backend_with(
        MockResp::Out(fail(1, "no server running on /tmp/tmux-x/default")),
        vec![],
    );
    let err = be
        .kill_session(&SessionName::new("sess"))
        .expect_err("kill_session must error on non-zero exit");
    assert!(
        matches!(err, TransportError::Subprocess { code: Some(1), .. }),
        "a non-zero tmux exit must map to TransportError::Subprocess{{code,stderr}}; got {err:?}"
    );

    // a runner io::Error (e.g. tmux not on PATH) must surface as a TransportError, never a panic.
    let (be, _r) = backend_with(MockResp::Io(std::io::ErrorKind::NotFound), vec![]);
    let err = be
        .capture(&Target::Pane(PaneId::new("%7")), CaptureRange::Full)
        .expect_err("capture must surface the runner io error");
    assert!(
        matches!(err, TransportError::Capture { .. } | TransportError::Io(_)),
        "a runner io error must map to a TransportError (not panic); got {err:?}"
    );
}

// ── 9. RealCommandRunner GOLDEN 5s TIMEOUT (rt-host-b transient-session race) ────────────────────
// GOLDEN: terminal.py:12-13 `run_cmd(args, timeout=timeout, check=False)`; runtime.py:1010-1014
// `_tmux_session_exists` runs `tmux has-session -t <s>` with timeout=5. A has-session that outlives
// 5s raises `subprocess.TimeoutExpired`, which the coordinator daemon CATCHES
// (coordinator/__main__.py:60-90 `except Exception`) and treats as a TOLERATED transient
// (exponential backoff + retry next tick) — it is NEVER read as a definitive "session gone".
// The 5s subprocess timeout is golden's ONLY tolerance for a slow/hung probe.
//
// RUST GAP (THE BUG): `RealCommandRunner::run` (tmux_backend.rs:52) calls
// `std::process::Command::output()` with NO timeout, so a slow/hung tmux blocks indefinitely.
// On the (slow) mac mini this is the ~17% single-round-trip flake: a transient slow has-session
// tears down a healthy team. This is rt-host-b's deterministic 5/5 anchor — `run` on a HUNG
// command must abandon at the golden 5s and surface `Err(TimedOut)`, NOT block on the full
// subprocess.
//
// RED today: there is no timeout, so `run(["sleep","30"])` blocks ~30s and the `< 6s` bound fails.
// #[ignore] real-machine: this is the only test here that spawns a real subprocess.
// PORTER SEAM: add a 5s timeout inside `RealCommandRunner::run` (spawn child + wait-with-timeout
// via a thread/channel + kill the child on expiry), returning `Err(io::Error, kind TimedOut)` —
// NO new crate dependency. Keep the existing `CommandRunner::run(&[String]) -> Result<…, io::Error>`
// signature (the timeout is internal; do not add a parameter).
#[test]
#[ignore = "real-machine: spawns a real sleeping subprocess; asserts RealCommandRunner enforces \
                the golden 5s timeout (terminal.py run_cmd timeout / runtime.py:1013 \
                _tmux_session_exists timeout=5)"]
fn real_command_runner_enforces_golden_5s_timeout_on_hang() {
    use std::time::{Duration, Instant};
    let runner = RealCommandRunner;
    let started = Instant::now();
    let result = runner.run(&svec(&["sleep", "30"]));
    let elapsed = started.elapsed();
    assert!(
            elapsed < Duration::from_secs(6),
            "RealCommandRunner::run must abandon a hung command at the golden 5s timeout, not block on \
             the full subprocess (terminal.py run_cmd timeout / runtime.py:1013 timeout=5); blocked {elapsed:?}"
        );
    let err = result.expect_err(
            "a command outliving the 5s timeout must surface as Err (subprocess.TimeoutExpired analog) so \
             the daemon backoff path tolerates it, instead of yielding a bogus has-session bool",
        );
    assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the timeout must be io::ErrorKind::TimedOut (golden: TimeoutExpired -> daemon except -> backoff/retry)"
        );
}

// ── 10. query (TRANSPORT TRIO) — single-field display-message; nonzero -> None ──────────────────
// Golden _legacy_pane_discovery.py:35-39 _tmux_pane_info: `tmux display-message -p -t <target> -F
// <fmt>` (returncode != 0 -> None), single-field reads at state.py:346 (#{pane_id}) / delivery.py:34
// (#{pane_width}). The argv is exactly `transport::tmux_query_argv(pane, field)` (the golden-locked
// builder). RED today: `query` is unimplemented!() -> PANIC. Porter: pane_from_target(target) ->
// tmux_query_argv -> run; success => Some(stdout.trim()); nonzero => None (never Err).
#[test]
fn query_single_field_argv_and_nonzero_maps_to_none() {
    // Current command field: argv == the golden builder; present value parsed (trimmed) into Some.
    let (be, rec) = backend_with(MockResp::Out(ok("codex\n")), vec![]);
    let got = be
        .query(&Target::Pane(PaneId::new("%7")), PaneField::PaneCurrentCommand)
        .expect("query ok");
    assert_eq!(
        rec.lock().unwrap()[0],
        tmux_query_argv(&PaneId::new("%7"), PaneField::PaneCurrentCommand),
        "query must build the golden single-field `display-message -p -t <t> -F #{{pane_current_command}}` argv"
    );
    assert_eq!(
        got,
        Some("codex".to_string()),
        "a present field value is parsed (stripped) into Some"
    );

    // PaneMode omits -F; lock argv and the parsed mode string.
    let (be, rec) = backend_with(MockResp::Out(ok("1\n")), vec![]);
    let got = be
        .query(&Target::Pane(PaneId::new("%7")), PaneField::PaneMode)
        .expect("query ok");
    assert_eq!(
        rec.lock().unwrap()[0],
        tmux_query_argv(&PaneId::new("%7"), PaneField::PaneMode)
    );
    assert_eq!(got, Some("1".to_string()));

    // nonzero exit (pane gone) -> None, NOT an Err (golden _tmux_pane_info: returncode != 0 -> None).
    let (be, _r) = backend_with(MockResp::Out(fail(1, "can't find pane %7")), vec![]);
    assert_eq!(
        be.query(&Target::Pane(PaneId::new("%7")), PaneField::PaneCurrentCommand)
            .expect("query ok on nonzero"),
        None,
        "a nonzero / pane-gone query must map to None (not Err)"
    );
}

// ── 11. list_targets (TRANSPORT TRIO) — `list-panes -a -F TMUX_PANE_FORMAT` + per-line parse ────
// Golden _legacy_pane_discovery.py:29-33 _tmux_list_panes: `tmux list-panes -a -F <TMUX_PANE_FORMAT>`.
// The Rust backend uses the printable separator locked below so the 12-field frame stays explicit
// in argv/log evidence, while retaining the golden nonzero -> empty inventory behavior. P5 (C-P5-3) appends
// `#{pane_pid}` as field 12 so pane pids ride the single list-panes call (the per-pane
// display-message N+1 fallback is gone). leader_env stays the reverse-env real-machine bit.
#[test]
fn list_targets_argv_and_parses_tmux_pane_format() {
    const FMT: &str = "#{pane_id}__TA_FIELD__#{session_name}__TA_FIELD__#{window_index}__TA_FIELD__#{window_name}__TA_FIELD__#{pane_index}__TA_FIELD__#{pane_tty}__TA_FIELD__#{pane_current_command}__TA_FIELD__#{pane_active}__TA_FIELD__#{pane_current_path}__TA_FIELD__#{session_attached}__TA_FIELD__#{pane_in_mode}__TA_FIELD__#{pane_pid}__TA_FIELD__#{@team_agent_pane_binding_nonce}";
    let stdout = "%7__TA_FIELD__team-x__TA_FIELD__0__TA_FIELD__win0__TA_FIELD__0__TA_FIELD__/dev/ttys003__TA_FIELD__codex__TA_FIELD__1__TA_FIELD__/Users/me/work__TA_FIELD__1__TA_FIELD__0__TA_FIELD__41001__TA_FIELD__nonce-7\n\
                      %8__TA_FIELD__team-x__TA_FIELD__1__TA_FIELD__win1__TA_FIELD__0__TA_FIELD__/dev/ttys004__TA_FIELD__node__TA_FIELD__0__TA_FIELD__/Users/me/other__TA_FIELD__0__TA_FIELD__0__TA_FIELD__41002__TA_FIELD__\n";
    let (be, rec) = backend_with(MockResp::Out(ok(stdout)), vec![]);
    let panes = be.list_targets().expect("list_targets ok");
    assert_eq!(
            rec.lock().unwrap()[0],
            svec(&["tmux", "list-panes", "-a", "-F", FMT]),
            "list_targets must run `tmux list-panes -a -F <TMUX_PANE_FORMAT>` (golden _legacy_pane_discovery.py:29)"
        );
    assert_eq!(panes.len(), 2, "one PaneInfo per output line");
    let p = &panes[0];
    assert_eq!(p.pane_id.as_str(), "%7", "field[0] -> pane_id");
    assert_eq!(p.session.as_str(), "team-x", "field[1] -> session_name");
    assert_eq!(
        p.window_index,
        Some(0),
        "field[2] -> window_index (parsed u32)"
    );
    assert_eq!(
        p.window_name.as_ref().map(|w| w.as_str().to_string()),
        Some("win0".to_string()),
        "field[3] -> window_name"
    );
    assert_eq!(p.pane_index, Some(0), "field[4] -> pane_index (parsed u32)");
    assert_eq!(
        p.tty.as_deref(),
        Some("/dev/ttys003"),
        "field[5] -> pane_tty"
    );
    assert_eq!(
        p.current_command.as_deref(),
        Some("codex"),
        "field[6] -> pane_current_command"
    );
    assert!(p.active, "field[7] pane_active='1' -> active=true");
    assert_eq!(
        p.current_path
            .as_ref()
            .map(|x| x.to_string_lossy().to_string()),
        Some("/Users/me/work".to_string()),
        "field[8] -> pane_current_path"
    );
    assert!(!panes[1].active, "field[7] pane_active='0' -> active=false");
    assert_eq!(
        p.pane_pid,
        Some(41001),
        "field[11] -> pane_pid (P5 C-P5-3, no N+1 fallback)"
    );
    assert_eq!(
        panes[1].pane_pid,
        Some(41002),
        "field[11] -> pane_pid (second pane)"
    );
    assert_eq!(
        p.leader_env
            .get(PANE_BINDING_NONCE_METADATA_KEY)
            .map(String::as_str),
        Some("nonce-7"),
        "field[12] pane option -> pane-instance binding metadata"
    );
    assert!(
        !panes[1]
            .leader_env
            .contains_key(PANE_BINDING_NONCE_METADATA_KEY),
        "an empty pane option must remain absent"
    );

    // nonzero exit -> empty vec (golden returncode != 0 -> []).
    let (be, _r) = backend_with(
        MockResp::Out(fail(1, "no server running on /tmp/tmux-x/default")),
        vec![],
    );
    assert!(
        be.list_targets()
            .expect("list_targets ok on nonzero")
            .is_empty(),
        "a nonzero list-panes must map to an EMPTY Vec (not Err)"
    );
}

#[test]
fn set_pane_binding_nonce_is_socket_scoped_and_pane_local() {
    let rec = Arc::new(Mutex::new(Vec::new()));
    let runner = MockCommandRunner {
        recorded: Arc::clone(&rec),
        stdin_recorded: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(VecDeque::new()),
        default: MockResp::Out(ok("")),
    };
    let be =
        TmuxBackend::with_runner_for_tmux_endpoint(Box::new(runner), "/tmp/ta-pane-binding.sock");
    be.set_pane_binding_nonce_if_unset(&PaneId::new("%7"), "nonce-7")
        .expect("set pane binding nonce");
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&[
            "tmux",
            "-S",
            "/tmp/ta-pane-binding.sock",
            "set-option",
            "-p",
            "-o",
            "-t",
            "%7",
            "@team_agent_pane_binding_nonce",
            "nonce-7",
        ])
    );
}

#[test]
fn set_pane_binding_nonce_if_unset_reads_existing_winner() {
    let rec = Arc::new(Mutex::new(Vec::new()));
    let runner = MockCommandRunner {
        recorded: Arc::clone(&rec),
        stdin_recorded: Arc::new(Mutex::new(Vec::new())),
        queue: Mutex::new(
            vec![
                MockResp::Out(fail(1, "already set: @team_agent_pane_binding_nonce")),
                MockResp::Out(ok("winner-nonce\n")),
            ]
            .into_iter()
            .collect(),
        ),
        default: MockResp::Out(ok("")),
    };
    let be =
        TmuxBackend::with_runner_for_tmux_endpoint(Box::new(runner), "/tmp/ta-pane-binding.sock");
    assert_eq!(
        be.set_pane_binding_nonce_if_unset(&PaneId::new("%7"), "loser-nonce")
            .expect("read the conditional-write winner"),
        "winner-nonce"
    );
    assert_eq!(
        rec.lock().unwrap().as_slice(),
        vec![
            svec(&[
                "tmux",
                "-S",
                "/tmp/ta-pane-binding.sock",
                "set-option",
                "-p",
                "-o",
                "-t",
                "%7",
                "@team_agent_pane_binding_nonce",
                "loser-nonce",
            ]),
            svec(&[
                "tmux",
                "-S",
                "/tmp/ta-pane-binding.sock",
                "show-options",
                "-p",
                "-v",
                "-t",
                "%7",
                "@team_agent_pane_binding_nonce",
            ]),
        ]
    );
}

#[test]
fn set_pane_binding_nonce_if_unset_rejects_write_or_read_failure() {
    let (be, _rec) = backend_with(
        MockResp::Out(fail(1, "already set: @team_agent_pane_binding_nonce")),
        vec![MockResp::Out(fail(1, "pane disappeared"))],
    );
    assert!(be
        .set_pane_binding_nonce_if_unset(&PaneId::new("%7"), "nonce-7")
        .is_err());
}

// ── 12. attach_session (TRANSPORT TRIO) — `tmux attach-session -t <s>` -> Attached ──────────────
// Golden tmux attach is `tmux attach-session -t <session>`; a successful attach -> AttachOutcome::
// Attached. RED today: attach_session is unimplemented!() -> PANIC. The in-process lock asserts the
// argv + outcome via the recording runner; the REAL attach is interactive (takes over the terminal)
// — that is the real-machine boundary, not unit-testable.

// ── 13. TARGET-SCAN WIRING (a): list_targets is the LIVE pane-discovery primitive ───────────────
// WAVE-2 Lane C. `list_targets` (the `tmux list-panes -a` scan, locked argv/parse in test #11) has
// ZERO production callers today — it is dead code. Golden wires pane discovery on top of it: status
// (_capture_missing_sessions / _tmux_session_exists, queries.py:46,52) and doctor (coordinator_health)
// consume the live scan. The in-process wiring obligation is exercised at the status level by
// cli::tests::status_tmux_session_present_uses_live_tmux_probe_not_is_some (RED). This #[ignore]
// real-machine seam locks that a LIVE `list_targets` actually enumerates the running panes, proving
// the primitive is usable by the status/doctor discovery the porter must wire.
#[test]
#[ignore = "real-machine: needs a live tmux server+session; asserts list_targets() (the dangling \
                pane-discovery primitive, zero production callers) enumerates live panes so status/doctor \
                discovery can consume it (golden _legacy_pane_discovery list-panes -a)"]
fn list_targets_is_live_pane_discovery_primitive_for_status_doctor() {
    let be = TmuxBackend::with_runner(Box::new(RealCommandRunner));
    let panes = be.list_targets().expect("live list_targets must not error");
    assert!(
        !panes.is_empty(),
        "a live `tmux list-panes -a` must surface the running panes; status/doctor pane discovery \
             is wired on top of this scan (currently dead code — zero production callers)"
    );
}

// ── 14. TARGET-SCAN WIRING (b): R1 — caller_target.uuid is FIRST leader_session_uuid precedence ──
// WAVE-2 Lane C / wave2-laneB-rereview PROBE-D. When the caller-target scan lands, golden
// claim_lease_no_incident threads `_target_leader_session_uuid(caller_target)` as the FIRST
// leader_session_uuid precedence (leader/__init__.py:679-684): caller_target.uuid BEFORE
// owner.uuid / receiver.uuid / derived. A DIFFERENT live pane reclaiming a DEAD owner must persist
// the CALLER's uuid, not the dead owner's (PROBE-D: PY "NEWUUID" / RUST persists "OLD"). The
// caller-target uuid is read from the caller pane's INJECTED TEAM_AGENT_LEADER_SESSION_UUID via a
// per-pane env query (NOT a TMUX_PANE_FORMAT field), so the live scan is the dependency this seam
// marks. SCOPE NOTE: the decisive IN-PROCESS claim-path R1 RED belongs in leader/tests.rs, which is
// outside this task's (cli + tmux_backend) editor scope — flagged to the leader for the
// leader-contracts agent to graduate R1 to its own claim-path RED.
#[test]
#[ignore = "real-machine + SCOPE: R1 (PROBE-D) caller_target.uuid is FIRST leader_session_uuid \
                precedence (leader/__init__.py:679-684); the in-process claim-path assertion lives in \
                leader/tests.rs (out of cli+tmux_backend scope) — this seam marks the live caller-target \
                env-scan dependency"]
fn r1_caller_target_uuid_is_first_leader_session_uuid_precedence_seam() {
    // The caller-target scan (reading the caller pane's injected TEAM_AGENT_LEADER_SESSION_UUID)
    // is the live precursor to R1's uuid precedence. The full uuid-persistence assertion is the
    // leader claim path's obligation (see report). Here we only confirm the scan is reachable.
    let be = TmuxBackend::with_runner(Box::new(RealCommandRunner));
    let _panes = be
        .list_targets()
        .expect("live list_targets (caller-target scan precursor)");
}

// ═════════════════════════════════════════════════════════════════════════
// pasted-signal: 折叠占位符正信号 + token NeverSeen 三态
// ═════════════════════════════════════════════════════════════════════════

struct FoldedPasteRunner {
    recorded: RecordedArgv,
    pre_submit: String,
    post_submit: String,
    submitted: AtomicBool,
}

impl CommandRunner for FoldedPasteRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        // 投递提交键是 C-m（tmux_send_submit_argv），不是字面量 Enter。
        if argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "C-m" || a == "Enter")
        {
            self.submitted.store(true, Ordering::SeqCst);
        }
        let stdout = match argv.get(1).map(String::as_str) {
            Some("capture-pane") => {
                if self.submitted.load(Ordering::SeqCst) {
                    self.post_submit.clone()
                } else {
                    self.pre_submit.clone()
                }
            }
            Some("display-message") => "0\n".to_string(),
            _ => String::new(),
        };
        Ok(ok(&stdout))
    }


}

fn backend_folded(pre_submit: &str, post_submit: &str) -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = FoldedPasteRunner {
        recorded: Arc::clone(&recorded),
        pre_submit: pre_submit.to_string(),
        post_submit: post_submit.to_string(),
        submitted: AtomicBool::new(false),
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}

/// 修复前尺子：token 不在底部 15 行 ⇒ consumed。折叠占位符会打中这条恒真。







fn count_submit_enters(calls: &[Vec<String>]) -> usize {
    calls
        .iter()
        .filter(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.iter().any(|a| a == "C-m" || a == "Enter")
        })
        .count()
}

fn count_paste_stage(calls: &[Vec<String>]) -> usize {
    calls
        .iter()
        .filter(|argv| {
            matches!(
                argv.get(1).map(String::as_str),
                Some("load-buffer") | Some("set-buffer") | Some("paste-buffer")
            )
        })
        .count()
}

/// Inject runner that reports a fixed `#{pane_mode}` and a token that disappears after C-m.
struct ModePasteRunner {
    recorded: RecordedArgv,
    pre_submit: String,
    post_submit: String,
    submitted: AtomicBool,
    pane_mode: String,
}

impl CommandRunner for ModePasteRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        if argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "C-m" || a == "Enter")
        {
            self.submitted.store(true, Ordering::SeqCst);
        }
        let stdout = match argv.get(1).map(String::as_str) {
            Some("capture-pane") => {
                if self.submitted.load(Ordering::SeqCst) {
                    self.post_submit.clone()
                } else {
                    self.pre_submit.clone()
                }
            }
            Some("display-message") => format!("{}\n", self.pane_mode),
            _ => String::new(),
        };
        Ok(ok(&stdout))
    }


}

fn backend_mode(pane_mode: &str, token_text: &str) -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = ModePasteRunner {
        recorded: Arc::clone(&recorded),
        pre_submit: token_text.to_string(),
        post_submit: "❯ \n".to_string(),
        submitted: AtomicBool::new(false),
        pane_mode: pane_mode.to_string(),
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}

fn first_submit_index(calls: &[Vec<String>]) -> Option<usize> {
    calls.iter().position(|argv| {
        argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "C-m" || a == "Enter")
    })
}

fn inject_send_keys_before_first_enter(calls: &[Vec<String>]) -> Vec<Vec<String>> {
    let Some(i) = first_submit_index(calls) else {
        return Vec::new();
    };
    calls[..i]
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("send-keys"))
        .cloned()
        .collect()
}

fn assert_no_e55_keys(calls: &[Vec<String>]) {
    for argv in calls {
        if argv.get(1).map(String::as_str) != Some("send-keys") {
            continue;
        }
        assert!(
            !argv
                .iter()
                .any(|a| a == "Escape" || a == "C-c" || a == "C-C"),
            "E55: inject must not send Escape/C-c; argv={argv:?}"
        );
    }
}

fn inject_token_for(tag: &str) -> String {
    format!("ping [team-agent-token:msg_{tag}]")
}









/// A6 send_keys([Enter]) 与 C 块 8 个调用方：含 Enter 的键组先 prepare。
#[test]
fn send_keys_enter_cancels_tree_mode_before_enter() {
    let (be, rec) = backend_mode("tree-mode", "");
    be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::Enter])
        .expect("send_keys");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.last().map(String::as_str) == Some("q")),
        "send_keys(Enter) must cancel tree-mode first; before={before:?} calls={calls:?}"
    );
}

#[test]
fn send_keys_down_enter_cancels_tree_mode_before_group() {
    let (be, rec) = backend_mode("tree-mode", "");
    be.send_keys(&Target::Pane(PaneId::new("%7")), &[Key::Down, Key::Enter])
        .expect("send_keys");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.last().map(String::as_str) == Some("q")),
        "Down+Enter must cancel tree-mode before the key group; before={before:?}"
    );
}

/// 外部队 2026-08-19 屏面原文，一字不改。
const GROK_INCIDENT_LINE: &str = "│ ❯ [Pasted: 42 lines]           │";

fn grok_raw_unfolded_paste(marker: &str) -> String {
    let mut lines = Vec::new();
    for i in 0..20 {
        lines.push(format!("payload line {i}"));
    }
    lines.push(format!("token {marker}"));
    lines.join("\n")
}

struct RawThenFoldRunner {
    recorded: RecordedArgv,
    raw: String,
    folded: String,
    after_submit: String,
    captures: Mutex<u32>,
    fold_after: u32,
    submitted: AtomicBool,
}

impl CommandRunner for RawThenFoldRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        if argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "C-m" || a == "Enter")
        {
            self.submitted.store(true, Ordering::SeqCst);
        }
        let stdout = match argv.get(1).map(String::as_str) {
            Some("capture-pane") => {
                if self.submitted.load(Ordering::SeqCst) {
                    self.after_submit.clone()
                } else {
                    let mut n = self.captures.lock().unwrap();
                    *n = n.saturating_add(1);
                    if *n > self.fold_after {
                        self.folded.clone()
                    } else {
                        self.raw.clone()
                    }
                }
            }
            Some("display-message") => "0\n".to_string(),
            _ => String::new(),
        };
        Ok(ok(&stdout))
    }


}

fn backend_raw_then_fold(
    raw: &str,
    folded: &str,
    after_submit: &str,
    fold_after: u32,
) -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = RawThenFoldRunner {
        recorded: Arc::clone(&recorded),
        raw: raw.to_string(),
        folded: folded.to_string(),
        after_submit: after_submit.to_string(),
        captures: Mutex::new(0),
        fold_after,
        submitted: AtomicBool::new(false),
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}




/// 2026-08-21 真机屏尾：折叠占位符还在 transcript，Working 已开。
/// 非 cursor 路径会把 pasted #N 当成未消费而连按 Enter。
const CURSOR_BUSY_AFTER_SUBMIT: &str = "\
  [Pasted text #1 +46 lines]\n\
\n\
  T126S-34701-OK\n\
\n\
 ⠰⠰ Working  37 tokens\n\
    Tip: Use /plan to plan execution and reach the right outcome faster.\n\
\n\
  → Add a follow-up                                             ctrl+c to stop\n\
\n\
  Cursor Grok 4.6 Extra High                                    Run Everything\n";











const G4_TOKEN: &str = "[team-agent-token:msg_g4]";




/// capture-pane 一律失败；其它 tmux 命令成功。用于「读数拿不到」而不是「读数不一样」。
struct CapturePaneFailRunner {
    recorded: RecordedArgv,
}

impl CommandRunner for CapturePaneFailRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        if argv.get(1).map(String::as_str) == Some("capture-pane") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "mock capture missing",
            ));
        }
        Ok(ok(""))
    }


}

fn backend_capture_pane_fail() -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = CapturePaneFailRunner {
        recorded: Arc::clone(&recorded),
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}



// ═════════════════════════════════════════════════════════════════════════
// Unverified 之后补一颗 C-m（TUI composer 残留 / 折行），busy-shell 不补
// ═════════════════════════════════════════════════════════════════════════

struct NthEnterClearRunner {
    recorded: RecordedArgv,
    pending: String,
    cleared: String,
    enters: std::sync::atomic::AtomicU32,
    clear_after: u32,
}

impl CommandRunner for NthEnterClearRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        if argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "C-m" || a == "Enter")
        {
            self.enters.fetch_add(1, Ordering::SeqCst);
        }
        let stdout = match argv.get(1).map(String::as_str) {
            Some("capture-pane") => {
                if self.enters.load(Ordering::SeqCst) >= self.clear_after {
                    self.cleared.clone()
                } else {
                    self.pending.clone()
                }
            }
            Some("display-message") => "0\n".to_string(),
            _ => String::new(),
        };
        Ok(ok(&stdout))
    }


}

fn backend_clears_after_n_enters(
    pending: &str,
    cleared: &str,
    clear_after: u32,
) -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = NthEnterClearRunner {
        recorded: Arc::clone(&recorded),
        pending: pending.to_string(),
        cleared: cleared.to_string(),
        enters: std::sync::atomic::AtomicU32::new(0),
        clear_after,
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}

fn wrapped_identity_with_skin(marker: &str, skin: &str) -> String {
    // tmux 80 列折行：单行不含完整 token，拼接后才含。
    let split_at = marker.len() / 2;
    format!("{skin}\n{}\n{}\n", &marker[..split_at], &marker[split_at..])
}

fn wrapped_unverified_composer(marker: &str) -> String {
    wrapped_identity_with_skin(marker, "tui-ready composer>")
}
