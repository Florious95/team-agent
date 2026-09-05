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

use super::{
    current_paste_to_submit_floor, observe_turn_from_capture,
    sleep_remaining_paste_to_submit_floor, with_cursor_single_enter, with_paste_to_submit_floor,
    CommandOutput, CommandRunner, RealCommandRunner, TmuxBackend, PANE_BINDING_NONCE_METADATA_KEY,
};
use crate::model::enums::PaneLiveness;
use crate::transport::{
    normalize_capture, tmux_capture_argv, tmux_query_argv, tmux_send_keys_argv, tmux_spawn_argv,
    tmux_submit_key_name, AttachOutcome, CaptureRange, InjectPayload, InjectStage,
    InjectVerification, Key, PaneField, PaneId, SessionName, SetEnvOutcome, SubmitVerification,
    Target, Transport, TransportError, TurnVerification, WindowName,
};

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

    fn run_with_stdin(
        &self,
        argv: &[String],
        _stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
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

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.stdin_recorded.lock().unwrap().push(stdin.to_string());
        self.run(argv)
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
    let _ = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text("hello leader".to_string()),
            Key::Enter,
            true,
        )
        .expect("inject via endpoint");

    let calls = rec.lock().unwrap().clone();
    assert!(
            calls.len() >= 5,
            "fixture must exercise list-panes, display-message, buffer/paste, and send-keys; got {calls:?}"
        );
    for call in &calls {
        assert!(
                call.starts_with(&["tmux".to_string(), "-S".to_string(), endpoint.to_string()]),
                "leader receiver list/liveness/inject must use tmux -S <full socket path>; got {call:?}"
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
                .any(|call| call.iter().any(|arg| arg == "paste-buffer"))
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
    let _env = EnvGuard::apply(&[("TMPDIR", Some(tmp.to_str().unwrap()))]);
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
#[test]
fn set_session_env_argv_and_applied_outcome() {
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let outcome = be
        .set_session_env(&SessionName::new("sess"), "KEY", "VAL")
        .expect("set env");
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&["tmux", "set-environment", "-t", "sess", "KEY", "VAL"])
    );
    assert_eq!(
        outcome,
        SetEnvOutcome::Applied,
        "tmux set-environment success -> SetEnvOutcome::Applied"
    );
}

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
#[test]
fn inject_text_runs_buffer_paste_submit_sequence_and_reports_submit() {
    let (be, rec) = backend_with(MockResp::Out(ok("hello")), vec![]);
    let pane = PaneId::new("%7");
    let report = be
        .inject(
            &Target::Pane(pane.clone()),
            &InjectPayload::Text("hello".to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    let calls = rec.lock().unwrap().clone();
    let is = |a: &[String], sub: &str| a.get(1).map(String::as_str) == Some(sub);
    assert!(
        calls
            .iter()
            .any(|a| (is(a, "set-buffer") || is(a, "load-buffer"))
                && a.iter().any(|x| x.contains("hello"))),
        "inject must stage the text into a tmux buffer (set-buffer/load-buffer); got {calls:?}"
    );
    assert!(
        calls.iter().any(|a| is(a, "paste-buffer")
            && a.contains(&"-p".to_string())
            && a.contains(&"%7".to_string())),
        "inject must bracketed-paste (-p) the buffer to the pane; got {calls:?}"
    );
    assert!(
        calls.iter().any(
            |a| is(a, "send-keys") && a.contains(&tmux_submit_key_name(Key::Enter).to_string())
        ),
        "inject must send the submit key (Enter) last; got {calls:?}"
    );
    assert_eq!(
        report.stage_reached,
        InjectStage::Submit,
        "a fully-applied inject reaches the Submit stage"
    );
    assert_eq!(report.inject_verification, InjectVerification::NoToken);
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    assert_eq!(report.turn_verification, TurnVerification::NotYetObserved);
}

#[test]
fn inject_large_text_load_buffer_writes_stdin_and_token_report() {
    let text = format!("{}{}", "x".repeat(16 * 1024), " [team-agent-token:abc]");
    // U1 #7: the pre-submit readback captures the pane; a pane that ECHOES the token
    // back (default capture returns the text) → CaptureContainsToken (true positive).
    let (be, rec, stdin_rec) = backend_with_stdin(MockResp::Out(ok(&text)), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(text.clone()),
            Key::Down,
            true,
        )
        .expect("inject large text");

    assert_eq!(
        report.inject_verification,
        InjectVerification::CaptureContainsToken
    );
    assert_eq!(
        report.submit_verification,
        SubmitVerification::KeySentAfterVisibleToken { key: Key::Down }
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        calls[0],
        svec(&["tmux", "load-buffer", "-b", "team-agent-send-abc", "-"])
    );
    assert_eq!(stdin_rec.lock().unwrap()[0], text);
}

// U1 #7 (RED→GREEN) — pre-submit pane readback: a token payload whose token is NOT
// visible in the pane before submit (paste silently dropped) must report
// CaptureMissingToken, not the static false-positive CaptureContainsToken.
#[test]
fn inject_token_not_visible_in_pane_reports_capture_missing_token() {
    let text = format!("{}{}", "x".repeat(16 * 1024), " [team-agent-token:zzz]");
    // default capture returns empty → token not visible → readback says missing.
    let (be, _rec, _stdin) = backend_with_stdin(MockResp::Out(ok("")), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%9")),
            &InjectPayload::Text(text),
            Key::Down,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.inject_verification,
        InjectVerification::CaptureMissingToken,
        "U1 #7: a token that never appeared in the pane must read back as \
CaptureMissingToken, not the static CaptureContainsToken false-positive"
    );
}

#[test]
fn inject_waits_for_token_visibility_before_enter() {
    let text = "hello [team-agent-token:e31]".to_string();
    let marker_visible = format!("{text}\n");
    let (be, rec) = backend_with(
        MockResp::Out(ok("")),
        vec![
            MockResp::Out(ok("")),              // set-buffer
            MockResp::Out(ok("")),              // paste-buffer
            MockResp::Out(ok("")),              // delete-buffer
            MockResp::Out(ok("")),              // pasted-content prompt poll 1
            MockResp::Out(ok("")),              // pasted-content prompt poll 2
            MockResp::Out(ok("")),              // pasted-content prompt poll 3
            MockResp::Out(ok("")),              // pasted-content prompt poll 4
            MockResp::Out(ok("")),              // pasted-content prompt poll 5
            MockResp::Out(ok("")),              // token gate: not visible yet
            MockResp::Out(ok("")),              // token gate: still not visible
            MockResp::Out(ok(&marker_visible)), // token gate: visible, Enter may fire
            MockResp::Out(ok("")),              // send-keys Enter
        ],
    );

    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(text),
            Key::Enter,
            true,
        )
        .expect("inject");
    let calls = rec.lock().unwrap().clone();
    let submit_index = calls
        .iter()
        .position(|argv| argv.get(1).map(String::as_str) == Some("send-keys"))
        .expect("inject must eventually send Enter");
    let captures_before_submit = calls[..submit_index]
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("capture-pane"))
        .count();

    assert!(
        captures_before_submit >= super::PASTED_CONTENT_APPEAR_POLLS as usize + 3,
        "Enter must wait until the pasted token is visible; calls={calls:?}"
    );
    assert_eq!(
        report.inject_verification,
        InjectVerification::CaptureContainsToken
    );
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
}

#[test]
fn paste_to_submit_floor_default_is_zero_even_if_test_tmp_is_set() {
    assert_eq!(
        current_paste_to_submit_floor(),
        Duration::ZERO,
        "default floor must be ZERO; TEAM_AGENT_TEST_TMP is not this switch"
    );
}

#[test]
fn paste_to_submit_nonzero_floor_is_actually_slept() {
    let start = Instant::now();
    sleep_remaining_paste_to_submit_floor(start, Duration::from_millis(40));
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(40),
        "non-zero floor must sleep the remaining time; elapsed={elapsed:?}"
    );
}

#[test]
fn paste_to_submit_zero_floor_does_not_sleep() {
    let start = Instant::now();
    sleep_remaining_paste_to_submit_floor(start, Duration::ZERO);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(20),
        "ZERO floor must be free for claude/codex/grok; elapsed={elapsed:?}"
    );
}

#[test]
fn inject_honors_injected_nonzero_paste_to_submit_floor() {
    let (be, _) = backend_with(MockResp::Out(ok("hello")), vec![]);
    let start = Instant::now();
    with_paste_to_submit_floor(Duration::from_millis(40), || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text("hello".to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    });
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(40),
        "inject must take the injected cursor floor; elapsed={elapsed:?}"
    );
}

#[test]
fn inject_skip_consumption_payload_sends_enter_without_phase2_poll() {
    let text = "hello leader [team-agent-token:skip]";
    let (be, rec) = backend_with(MockResp::Out(ok(text)), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::TextSkipConsumptionPoll(text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    let calls = rec.lock().unwrap().clone();

    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    assert_eq!(report.attempts, 1);
    assert!(
        calls.iter().any(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.contains(&tmux_submit_key_name(Key::Enter).to_string())
        }),
        "skip-consumption payload must still submit once; calls={calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|argv| { argv == &tmux_capture_argv(&PaneId::new("%7"), CaptureRange::Tail(40)) }),
        "leader-bound skip payload must not run Phase 2 consumption polls; calls={calls:?}"
    );
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

/// ledger.inject-fix: paste landing is not submit. Token still in the
/// composer and no Working signal ⇒ SubmitConsumptionUnverified.
#[test]
fn e46_post_submit_matched_token_without_scroll_is_unverified() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_red1]";
    let (be, _rec) = backend_with(MockResp::Out(ok(token_text)), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "token still in composer and no Working signal must be unverified, \
             not EnterSentWithoutPlaceholderCheck. Got {:?}",
        report.submit_verification
    );
    assert_eq!(
        report.turn_verification,
        TurnVerification::LeaderNewTurnBoundaryMissing,
        "token still in composer and no busy must report 没开跑, not unknown/started"
    );
    let diagnostics = report.submit_diagnostics.expect("diagnostics");
    assert!(
        diagnostics.attempts_detail.iter().any(|obs| obs.matched),
        "the unverified verdict must still record that paste landed"
    );
}

#[test]
fn e46_unconsumed_token_with_live_busy_state_is_treated_as_processing() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_busy]";
    let busy_tail = format!("{token_text}\n● Working (1s · esc to interrupt)\n");
    let (be, _rec) = backend_with(MockResp::Out(ok(&busy_tail)), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "busy provider state means the turn is being processed even if \
             the token has not yet scrolled out of the bottom capture"
    );
    assert_eq!(
        report.turn_verification,
        TurnVerification::LeaderNewTurnBoundaryVerified,
        "live Working signal is 开跑; must not stay not_yet_observed"
    );
    let diagnostics = report.submit_diagnostics.expect("diagnostics");
    assert!(
        diagnostics
            .attempts_detail
            .last()
            .map(|obs| obs
                .pane_tail_excerpt
                .to_ascii_lowercase()
                .contains("working"))
            .unwrap_or(false),
        "busy-state capture should be recorded in attempts_detail: {:?}",
        diagnostics.attempts_detail
    );
}

/// NeverSeen + 无占位符：空 pane 不得再把「token 不在底部」写成 consumed。
/// 旧默认 EnterSentWithoutPlaceholderCheck 就是折叠恒真的同构。
#[test]
fn e46_inject_text_with_empty_pane_is_unverified() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_empty]";
    let (be, _rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "empty pane / token never visible / no pasted #N ⇒ unverified, \
             not EnterSentWithoutPlaceholderCheck. Got {:?}",
        report.submit_verification
    );
}

/// **E46 RED-2 (正向消费确认 → delivered)**: token-bearing Text + Enter +
/// bracketed paste. Token VISIBLE on first capture (pre/post-submit token
/// readback) BUT then GONE from the post-submit input-consumption probe
/// (composer cleared after Enter). Submit must report
/// `EnterSentWithoutPlaceholderCheck` (MUST-10 path) — delivery proceeds
/// to delivered. Guards against regression on crd's existing-worker path.
#[test]
fn e46_inject_text_with_token_consumed_after_enter_keeps_enter_sent_without_placeholder_check() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_red2]";
    // First captures (token visibility probes pre/post-Enter) return text
    // with token; the LAST capture (consumption probe) returns text WITHOUT
    // the token in the tail (composer cleared). MockCommandRunner drains
    // `queued` first then defaults — we queue the early "token visible"
    // captures, then drop to the empty default for the consumption probe.
    let visible = ok(token_text);
    let queued = vec![
        MockResp::Out(ok("")),          // set-buffer
        MockResp::Out(ok("")),          // paste-buffer
        MockResp::Out(ok("")),          // delete-buffer
        MockResp::Out(visible.clone()), // pasted-content prompt poll: none → continue
        MockResp::Out(visible.clone()), // pre-submit token visibility: visible
        MockResp::Out(ok("")),          // Escape send-keys (no stdout needed)
        MockResp::Out(ok("")),          // Enter send-keys
                                        // From here on, the default response is `ok("")` (empty pane tail) so
                                        // post-Enter consumption probe sees no token in tail → consumed=true.
    ];
    let (be, _rec) = backend_with(MockResp::Out(ok("")), queued);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "E46 RED-2: when post-Enter consumption is observed (token gone \
             from input region tail), submit must report the canonical \
             EnterSentWithoutPlaceholderCheck so MUST-10 delivery semantics \
             hold (provider_submit_verification_red.rs:113-159). Got {:?}",
        report.submit_verification
    );
}

/// **E46 RED-3 (resend-to-cap, no double-submit)**: when the FIRST Enter
/// landed on the composer but our readback was slow, the re-check BEFORE
/// resend must observe the now-empty input and STOP — must NOT fire a
/// second Enter into an empty composer (would open an empty turn).
#[test]
fn e46_inject_text_resend_rechecks_input_before_resending_to_avoid_double_submit() {
    // Same mock as RED-2: post-Enter consumption probe sees empty tail.
    // The implementation should issue only ONE Enter (no resend) once
    // consumption is observed.
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_red3]";
    let queued = vec![
        MockResp::Out(ok("")),         // set-buffer
        MockResp::Out(ok("")),         // paste-buffer
        MockResp::Out(ok("")),         // delete-buffer
        MockResp::Out(ok(token_text)), // pasted-content prompt: not matched
        MockResp::Out(ok(token_text)), // pre-submit token visibility
        MockResp::Out(ok("")),         // Escape
        MockResp::Out(ok("")),         // Enter
    ];
    let (be, rec) = backend_with(MockResp::Out(ok("")), queued);
    let _report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    let calls = rec.lock().unwrap().clone();
    // Count `send-keys ... Enter` invocations. There must be EXACTLY ONE
    // (the canonical submit). A second one indicates the resend loop
    // didn't honour C3 (re-check input before resend).
    let enter_count = calls
        .iter()
        .filter(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.contains(&tmux_submit_key_name(Key::Enter).to_string())
        })
        .count();
    assert_eq!(
        enter_count, 1,
        "E46 RED-3: when consumption is observed (or already happened \
             between Enter and probe), the resend loop must STOP and NOT \
             issue a second Enter. Got {enter_count} Enter sends. \
             calls={calls:?}"
    );
}

/// **E46 RED-5 (provider-agnostic detector)**: the consumption probe must
/// NOT hard-code provider UI strings (claude `>`, codex `╰`, ...). It uses
/// the token marker absence in the bottom of the pane — works for any
/// provider. Here we run a non-claude-shaped pane (no claude markers) and
/// confirm the same delivered semantics when consumption happens.
#[test]
fn e46_consumption_detector_works_on_codex_shaped_pane_without_provider_strings() {
    // Codex-shaped fake: prompt looks like `codex>` with no claude markers.
    // After Enter, the composer clears (empty default returns ok("")).
    let token_text = "Team Agent message from leader:\n\ncodex msg\n\n[team-agent-token:msg_red5]";
    let queued = vec![
        MockResp::Out(ok("")),         // set-buffer
        MockResp::Out(ok("")),         // paste-buffer
        MockResp::Out(ok("")),         // delete-buffer
        MockResp::Out(ok("codex>\n")), // pasted-content prompt: no match
        MockResp::Out(ok(token_text)), // pre-submit token visibility
        MockResp::Out(ok("")),         // Escape
        MockResp::Out(ok("")),         // Enter
                                       // post-Enter: default returns ok("") (composer cleared) → token gone
    ];
    let (be, _rec) = backend_with(MockResp::Out(ok("")), queued);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "E46 RED-5: provider-agnostic detector — codex-shaped pane (no \
             claude UI string) where composer clears post-Enter must still \
             report EnterSentWithoutPlaceholderCheck via structural \
             input-empty check. Got {:?}",
        report.submit_verification
    );
}

/// **E46 Escape pre-Enter**: when bracketed=true + Text payload + submit=Enter,
/// the inject must send Escape BEFORE the Enter (exits bracketed-paste
/// mode on stuck composer). Non-Enter submits (e.g. Key::Down for codex
/// menu) must NOT receive the Escape pre-step.
#[test]
/// 0.3.27 amendment: Escape is now RETRY-ONLY (attempt > 0). The first
/// attempt sends Enter directly (Python parity — Python never sends Escape).
/// Escape fires only if the first Enter failed to consume the token and a
/// retry is needed. This test now asserts the first attempt has NO Escape.
fn e46_inject_text_first_attempt_no_escape_retry_only() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_esc]";
    // Mock returns empty — token not in tail → consumed=true on first attempt.
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let _report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    let calls = rec.lock().unwrap().clone();
    let enter_count = calls
        .iter()
        .filter(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.contains(&tmux_submit_key_name(Key::Enter).to_string())
        })
        .count();
    let escape_count = calls
        .iter()
        .filter(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.contains(&"Escape".to_string())
        })
        .count();
    assert!(
        enter_count >= 1,
        "0.3.27: first attempt must send Enter; calls={calls:?}"
    );
    assert_eq!(
        escape_count, 0,
        "0.3.27: first attempt (consumed=true) must NOT send Escape \
             (Escape is retry-only); calls={calls:?}"
    );
}

/// **E46 regression guard**: non-Enter submit (codex menu navigation,
/// `Key::Down`) does NOT trigger the Escape pre-step (would interfere
/// with the menu).
#[test]
fn e46_inject_text_non_enter_submit_does_not_emit_escape_prestep() {
    let token_text = "menu interaction\n[team-agent-token:msg_down]";
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let _report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Down,
            true,
        )
        .expect("inject runs");
    let calls = rec.lock().unwrap().clone();
    let escape_count = calls
        .iter()
        .filter(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.contains(&"Escape".to_string())
        })
        .count();
    assert_eq!(
        escape_count, 0,
        "E46 regression guard: Key::Down submits (codex menu navigation) \
             must NOT receive Escape pre-step. Got {escape_count}. \
             calls={calls:?}"
    );
}

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

/// **E50 RED-1 (判据误判, pure function)**: `pasted_prompt_match` must
/// distinguish bottom-region (composer) from scrollback (line 6+ from
/// bottom). Pre-fix `capture_has_pasted_content_prompt` returned `true`
/// for BOTH; the new tuple-returning helper surfaces the offset so PR-2
/// can fix the criterion.
#[test]
fn e50_pasted_prompt_match_reports_where_in_tail_for_scrollback_vs_composer() {
    use super::pasted_prompt_match;

    // Composer (bottom) match.
    let composer_only = "some output\n\
                             [Pasted content 1.2k]\n\
                             ❯ ";
    // The match is "pasted content" in the line at index 1 from bottom
    // among non-empty lines (`[Pasted content...]`, `❯ ` is at idx 0).
    let m = pasted_prompt_match(composer_only).expect("match");
    assert_eq!(m.0, "pasted content");
    assert_eq!(
        m.1, 1,
        "E50: where_in_tail must reflect bottom offset (idx 1 = above the \
             `❯` composer line); got {m:?}"
    );

    // Scrollback (top) match — 8 lines from bottom.
    let scrollback_only = "[Pasted content 5.0k]\n\
                               assistant reply line 1\n\
                               assistant reply line 2\n\
                               assistant reply line 3\n\
                               assistant reply line 4\n\
                               assistant reply line 5\n\
                               assistant reply line 6\n\
                               assistant reply line 7\n\
                               ❯ ";
    let m2 = pasted_prompt_match(scrollback_only).expect("match");
    assert_eq!(m2.0, "pasted content");
    assert!(
        m2.1 >= 6,
        "E50: scrollback `Pasted content` must report where_in_tail >= 6 \
             (clearly above the bottom composer region); got {m2:?}. PR-2 \
             will use this offset to distinguish live composer vs scrollback \
             residue."
    );

    // No match.
    assert!(pasted_prompt_match("nothing here\n❯ ").is_none());
}

/// **E50 PR-2 Fix-B (amends PR-1 RED-2)**: with Fix-B, token-bearing
/// payloads SKIP the paste-prompt weak loop and fall through to the E46
/// token consumption gate. The `submit_diagnostics` still records the
/// appear-gate timing (was `saw_pasted_prompt` matched?), but
/// `attempts_detail` is populated by the E46 consumption gate path, not
/// the deleted weak loop. This test pins the NEW shape: `appear_gate_matched`
/// may be false (mock default returns the pasted-content text, but the
/// appear-gate polls 5 times with 25ms sleep and the mock answers the SAME
/// text every time → appear-gate DOES match). However, the `has_token` guard
/// skips the weak loop body → the token path's diagnostics fill in instead.
#[test]
fn e50_inject_paste_prompt_path_populates_submit_diagnostics_per_attempt() {
    let token_text = "Team Agent message from leader:\n\nhello\n\n[team-agent-token:msg_pr1]";
    // Mock keeps returning the pasted-content placeholder. With Fix-B,
    // the token path takes over: post_submit_input_consumed checks
    // bottom 5 lines for the token marker. The mock returns the pasted
    // placeholder text (without the token) → token NOT in tail → consumed
    // = Some(true) → EnterSentWithoutPlaceholderCheck.
    // We test: (a) diagnostics present (b) submit verification reflects
    // the E46 token path, not the deleted weak loop.
    let pasted = "[Pasted content 1.2k]";
    let (be, _rec) = backend_with(MockResp::Out(ok(pasted)), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    // token 从未可见 + composer 里仍有 pasted content（无 #N）→ 不得报已消费。
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "folded placeholder still in composer (no #N, token never seen) \
             must be unverified, not vacuous EnterSentWithoutPlaceholderCheck; \
             got {:?}",
        report.submit_verification
    );
    let diagnostics = report.submit_diagnostics.expect("diagnostics");
    assert!(
        !diagnostics.attempts_detail.is_empty(),
        "E50: E46 consumption gate must preserve per-capture diagnostics"
    );
    assert_eq!(diagnostics.attempts_detail[0].attempt_index, 1);
    assert!(
        diagnostics.attempts_detail[0]
            .pane_tail_excerpt
            .contains("Pasted content"),
        "E50: recorded pane tail should contain the post-submit capture; got {:?}",
        diagnostics.attempts_detail[0]
    );
}

/// **E50 PR-1 secret scrubbing**: `scrub_pane_excerpt` must strip ANSI
/// + redact common secret shapes so emitted events.jsonl doesn't leak.
#[test]
fn e50_scrub_pane_excerpt_strips_ansi_and_redacts_secret_shapes() {
    use super::scrub_pane_excerpt;
    let raw = "\x1b[31merror\x1b[0m: sk-abcdef123ghi\n\
                   token Bearer abc.def.ghi\n\
                   key AKIAIOSFODNN7EXAMPLE\n\
                   ghp_1234567890abcdef\n\
                   hex deadbeefdeadbeefdeadbeefdeadbeefcafebabe\n\
                   plain text below";
    let (out, lines) = scrub_pane_excerpt(raw, 20);
    assert!(lines >= 5, "E50: at least 5 non-empty lines; got {lines}");
    assert!(
        !out.contains("\x1b"),
        "E50: ANSI escape stripped; got {out:?}"
    );
    assert!(
        !out.contains("sk-abcdef123ghi"),
        "E50: sk- secret must be redacted; got {out:?}"
    );
    assert!(
        !out.contains("ghp_1234567890abcdef"),
        "E50: ghp_ secret must be redacted; got {out:?}"
    );
    assert!(
        !out.contains("AKIAIOSFODNN7EXAMPLE"),
        "E50: AKIA secret must be redacted; got {out:?}"
    );
    assert!(
        !out.contains("abc.def.ghi"),
        "E50: Bearer secret token must be redacted; got {out:?}"
    );
    assert!(
        !out.contains("deadbeefdeadbeefdeadbeefdeadbeefcafebabe"),
        "E50: 32+ hex run must be redacted; got {out:?}"
    );
    assert!(
        out.contains("REDACTED"),
        "E50: redactions visible; got {out:?}"
    );
    assert!(
        out.contains("plain text below"),
        "E50: non-secret content preserved; got {out:?}"
    );
}

/// **E50 PR-1 byte-identical legacy matcher**: `capture_has_pasted_content_prompt`
/// wrapper must still return the same bool the 3 legacy callers depend on
/// (the bool-returning fn shape is the contract for byte-locked behaviour).
#[test]
fn e50_capture_has_pasted_content_prompt_byte_identical_wrapper() {
    use super::capture_has_pasted_content_prompt;
    assert!(capture_has_pasted_content_prompt("[Pasted content 1k]"));
    assert!(capture_has_pasted_content_prompt("[Pasted text foo]"));
    assert!(!capture_has_pasted_content_prompt("just composer text"));
    assert!(!capture_has_pasted_content_prompt(""));
}

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

#[test]
fn inject_text_uses_message_id_scoped_buffer_from_token() {
    // Golden delivery.py:109-114 passes buffer_name = `team-agent-send-{message_id}` into
    // `_tmux_inject_text`; tmux_io.py then uses that exact name for set/load, paste, delete.
    // This prevents interleaved sends from sharing a stale global tmux buffer.
    // RED: Rust currently hard-codes `team-agent-buf`.
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let text =
        "Team Agent message from leader:\n\nhello\n\n[team-agent-token:msg_abc123]".to_string();
    be.inject(
        &Target::Pane(PaneId::new("%7")),
        &InjectPayload::Text(text),
        Key::Enter,
        true,
    )
    .expect("inject");

    let calls = rec.lock().unwrap().clone();
    let buffer_args: Vec<String> = calls
        .iter()
        .filter(|argv| {
            matches!(
                argv.get(1).map(String::as_str),
                Some("set-buffer" | "load-buffer" | "paste-buffer" | "delete-buffer")
            )
        })
        .filter_map(|argv| {
            argv.iter()
                .position(|arg| arg == "-b")
                .and_then(|i| argv.get(i + 1))
                .cloned()
        })
        .collect();
    assert_eq!(
            buffer_args,
            vec![
                "team-agent-send-msg_abc123".to_string(),
                "team-agent-send-msg_abc123".to_string(),
                "team-agent-send-msg_abc123".to_string(),
            ],
            "every tmux buffer operation must use the message-id-scoped golden buffer name; calls={calls:?}"
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
    // PaneId field: argv == the golden builder; present value parsed (trimmed) into Some.
    let (be, rec) = backend_with(MockResp::Out(ok("%7\n")), vec![]);
    let got = be
        .query(&Target::Pane(PaneId::new("%7")), PaneField::PaneId)
        .expect("query ok");
    assert_eq!(
        rec.lock().unwrap()[0],
        tmux_query_argv(&PaneId::new("%7"), PaneField::PaneId),
        "query must build the golden single-field `display-message -p -t <t> -F #{{pane_id}}` argv"
    );
    assert_eq!(
        got,
        Some("%7".to_string()),
        "a present field value is parsed (stripped) into Some"
    );

    // PaneWidth uses -F too; lock argv + the parsed numeric-as-string field.
    let (be, rec) = backend_with(MockResp::Out(ok("180\n")), vec![]);
    let got = be
        .query(&Target::Pane(PaneId::new("%7")), PaneField::PaneWidth)
        .expect("query ok");
    assert_eq!(
        rec.lock().unwrap()[0],
        tmux_query_argv(&PaneId::new("%7"), PaneField::PaneWidth)
    );
    assert_eq!(got, Some("180".to_string()));

    // nonzero exit (pane gone) -> None, NOT an Err (golden _tmux_pane_info: returncode != 0 -> None).
    let (be, _r) = backend_with(MockResp::Out(fail(1, "can't find pane %7")), vec![]);
    assert_eq!(
        be.query(&Target::Pane(PaneId::new("%7")), PaneField::PaneId)
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
    be.set_pane_binding_nonce(&PaneId::new("%7"), "nonce-7")
        .expect("set pane binding nonce");
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&[
            "tmux",
            "-S",
            "/tmp/ta-pane-binding.sock",
            "set-option",
            "-p",
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
                "display-message",
                "-p",
                "-t",
                "%7",
                "#{@team_agent_pane_binding_nonce}",
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
#[test]
fn attach_session_argv_and_attached_outcome() {
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let outcome = be
        .attach_session(&SessionName::new("sess"))
        .expect("attach ok");
    assert_eq!(
        rec.lock().unwrap()[0],
        svec(&["tmux", "attach-session", "-t", "sess"]),
        "attach_session must run `tmux attach-session -t <session>`"
    );
    assert_eq!(
        outcome,
        AttachOutcome::Attached,
        "a successful tmux attach -> AttachOutcome::Attached"
    );
}

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

    fn run_with_stdin(
        &self,
        argv: &[String],
        _stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
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
fn unfixed_vacuous_consumed(text: &str, marker: &str) -> bool {
    !super::token_in_bottom_n(text, marker, 15)
}

#[test]
fn pasted_signal_unfixed_predicate_treats_folded_placeholder_as_consumed() {
    let folded = "❯ [Pasted text #1 +8 lines]\n";
    let marker = "[team-agent-token:msg_fold]";
    assert!(
        unfixed_vacuous_consumed(folded, marker),
        "causal: pre-fix !token_in_bottom_n is vacuously true on a folded composer"
    );
    assert_eq!(
        super::consumption_from_capture(folded, marker, false, Some(super::PasteLatch::HashId(1))),
        Some(false),
        "post-fix NeverSeen + #1 still in composer must NOT be consumed"
    );
}

#[test]
fn pasted_signal_never_seen_without_placeholder_is_not_consumed() {
    let marker = "[team-agent-token:msg_empty]";
    assert_eq!(
        super::token_sighting(false, false),
        super::TokenSighting::NeverSeen
    );
    assert_eq!(
        super::consumption_from_capture("", marker, false, None),
        Some(false)
    );
}

#[test]
fn pasted_signal_never_seen_placeholder_id_gone_is_consumed() {
    let marker = "[team-agent-token:msg_fold]";
    let after = "assistant reply\n❯ \n";
    assert_eq!(
        super::consumption_from_capture(after, marker, false, Some(super::PasteLatch::HashId(7))),
        Some(true),
        "NeverSeen but latched #7 left composer ⇒ consumed (positive signal)"
    );
}

#[test]
fn pasted_signal_never_seen_wrong_id_is_consumed() {
    let marker = "[team-agent-token:msg_fold]";
    let other = "❯ [Pasted text #2 +3 lines]\n";
    assert_eq!(
        super::consumption_from_capture(other, marker, false, Some(super::PasteLatch::HashId(1))),
        Some(true),
        "our #1 gone, a later #2 in composer still means ours submitted"
    );
}

#[test]
fn pasted_signal_never_seen_no_id_cannot_claim_consumed() {
    let marker = "[team-agent-token:msg_fold]";
    let gone = "❯ \n";
    assert_eq!(
        super::consumption_from_capture(gone, marker, false, None),
        Some(false),
        "no #N latched: disappearance is unverified, not consumed"
    );
}

#[test]
fn pasted_signal_seen_token_then_gone_is_consumed() {
    let marker = "[team-agent-token:msg_short]";
    assert_eq!(
        super::token_sighting(false, true),
        super::TokenSighting::Gone
    );
    assert_eq!(
        super::consumption_from_capture("❯ \n", marker, true, None),
        Some(true)
    );
}

#[test]
fn pasted_signal_seen_token_still_present_is_not_consumed() {
    let marker = "[team-agent-token:msg_short]";
    let still = format!("❯ ping {marker}\n");
    assert_eq!(
        super::token_sighting(true, true),
        super::TokenSighting::Visible
    );
    assert_eq!(
        super::consumption_from_capture(&still, marker, true, None),
        Some(false)
    );
}

#[test]
fn pasted_signal_marker_some_observation_records_composer_placeholder() {
    use crate::transport::CapturedText;
    let cap = CapturedText {
        text: "❯ [Pasted text #4 +12 lines]\n".to_string(),
        range: CaptureRange::Tail(40),
    };
    let obs = super::submit_attempt_observation(1, &cap, Some("[team-agent-token:msg_x]"), 10);
    assert!(
        obs.matched,
        "marker Some must still walk pasted_prompt_match / composer placeholder; obs={obs:?}"
    );
    assert_eq!(obs.matched_literal.as_deref(), Some("pasted text"));
}

#[test]
fn pasted_signal_inject_folded_enter_not_effective_is_unverified() {
    let token_text = "Team Agent message from leader:\nline1\nline2\nline3\nline4\nline5\n\n[team-agent-token:msg_fold_stay]";
    let placeholder = "❯ [Pasted text #4 +6 lines]\n";
    let (be, _rec) = backend_folded(placeholder, placeholder);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "folded + Enter not effective (#4 stays) must be unverified; got {:?}",
        report.submit_verification
    );
}

#[test]
fn pasted_signal_inject_folded_enter_effective_is_consumed() {
    let token_text = "Team Agent message from leader:\nline1\nline2\nline3\nline4\nline5\n\n[team-agent-token:msg_fold_go]";
    let placeholder = "❯ [Pasted text #4 +6 lines]\n";
    // 只有 #4 离开 composer；不加 Working，否则 busy 升级会让半三不经正信号就绿。
    let after = "assistant reply\n❯ \n";
    let (be, _rec) = backend_folded(placeholder, after);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "folded + Enter effective (#4 left composer) must be consumed; got {:?}",
        report.submit_verification
    );
}

#[test]
fn pasted_signal_inject_short_token_visible_consumed_matches_pre_fix() {
    let token_text = "ping [team-agent-token:msg_short_ok]";
    let (be, _rec) = backend_folded(token_text, "❯ \n");
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "short visible token then gone must stay consumed; got {:?}",
        report.submit_verification
    );
}

#[test]
fn pasted_signal_inject_short_token_visible_unconsumed_matches_pre_fix() {
    let token_text = "ping [team-agent-token:msg_short_stay]";
    let (be, _rec) = backend_folded(token_text, token_text);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "short visible token still in composer must stay unverified; got {:?}",
        report.submit_verification
    );
}

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

    fn run_with_stdin(
        &self,
        argv: &[String],
        _stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
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

/// PR-18: tree-mode 必须在第一次 Enter 前 send-keys q。修坏：只对 Copy|Unknown cancel。
#[test]
fn inject_cancels_tree_mode_with_q_before_enter() {
    let token = inject_token_for("tree_q");
    let (be, rec) = backend_mode("tree-mode", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token.clone()),
            Key::Enter,
            false,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.iter().any(|a| a == "q") && !argv.iter().any(|a| a == "-X")),
        "tree-mode must send-keys q before first Enter; before={before:?} calls={calls:?}"
    );
    assert!(
        !before.iter().any(|argv| argv.iter().any(|a| a == "-X")),
        "tree-mode must not use copy-mode -X cancel; before={before:?}"
    );
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "consumed after cancel+Enter; calls={calls:?}"
    );
}

/// copy-mode 回归：仍走 -X cancel，不得改成 q。
#[test]
fn inject_cancels_copy_mode_with_x_cancel_before_enter() {
    let token = inject_token_for("copy_x");
    let (be, rec) = backend_mode("copy-mode", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token.clone()),
            Key::Enter,
            false,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.iter().any(|a| a == "-X") && argv.iter().any(|a| a == "cancel")),
        "copy-mode must keep -X cancel before Enter; before={before:?} calls={calls:?}"
    );
}

/// pane_mode=0 是输入就绪，不得 cancel。
#[test]
fn inject_skips_cancel_when_pane_mode_is_zero() {
    let token = inject_token_for("mode0");
    let (be, rec) = backend_mode("0", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token.clone()),
            Key::Enter,
            false,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        !before
            .iter()
            .any(|argv| argv.iter().any(|a| a == "q" || a == "-X")),
        "pane_mode=0 must not send mode-cancel; before={before:?}"
    );
    // 用户裁定 2026-08-23：不发 ESC，也不发 ESC 的替代品（含字面 CSI 201~）。翻面同上。
    assert!(
        !before
            .iter()
            .any(|argv| argv.iter().any(|a| a.contains('\u{1b}'))),
        "mode=0 must not send any ESC/CSI byte before Enter; before={before:?}"
    );
}

#[test]
fn inject_cancels_view_mode_with_q_before_enter() {
    let token = inject_token_for("view_q");
    let (be, rec) = backend_mode("view-mode", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token.clone()),
            Key::Enter,
            false,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.last().map(String::as_str) == Some("q")),
        "view-mode must send-keys q before Enter; before={before:?}"
    );
}

#[test]
fn inject_cancels_client_mode_with_d_before_enter() {
    let token = inject_token_for("client_d");
    let (be, rec) = backend_mode("client-mode", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token.clone()),
            Key::Enter,
            false,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.last().map(String::as_str) == Some("d")),
        "client-mode must send-keys d before Enter; before={before:?}"
    );
}

/// 用户裁定 2026-08-23：不发 ESC，也不发 ESC 的替代品（含字面 CSI 201~）。
/// 发 ESC/CSI 的唯一好处是让消息当场直接上屏；本项目只要求「下一次工具调用时能上屏」
/// ⇒ 不存在需要它才能满足的场景，发它没有收益，只有污染 composer 输入的风险。
/// 本判据由「必须发 201~」翻面为「Enter 前不得发任何 ESC/CSI 字节」。
#[test]
fn inject_sends_no_escape_bytes_before_enter() {
    let token = inject_token_for("paste201");
    let (be, rec) = backend_mode("0", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token.clone()),
            Key::Enter,
            true,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        !before
            .iter()
            .any(|argv| argv.iter().any(|a| a.contains('\u{1b}'))),
        "no ESC/CSI byte may be sent before Enter; before={before:?}"
    );
}

/// A1 Empty / trust 路径也必须经过 prepare。
#[test]
fn empty_inject_cancels_tree_mode_before_cm() {
    let (be, rec) = backend_mode("tree-mode", "");
    be.inject(
        &Target::Pane(PaneId::new("%7")),
        &InjectPayload::Empty,
        Key::Enter,
        false,
    )
    .expect("empty inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.last().map(String::as_str) == Some("q")),
        "A1 Empty must cancel tree-mode before C-m; before={before:?} calls={calls:?}"
    );
}

/// A3 skip-consumption（leader 投递）也必须经过 prepare。
#[test]
fn skip_consumption_cancels_tree_mode_before_cm() {
    let token = inject_token_for("skip_tree");
    let (be, rec) = backend_mode("tree-mode", &token);
    with_paste_to_submit_floor(Duration::ZERO, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::TextSkipConsumptionPoll(token.clone()),
            Key::Enter,
            false,
        )
    })
    .expect("skip inject");
    let calls = rec.lock().unwrap().clone();
    assert_no_e55_keys(&calls);
    let before = inject_send_keys_before_first_enter(&calls);
    assert!(
        before
            .iter()
            .any(|argv| argv.last().map(String::as_str) == Some("q")),
        "A3 skip-poll must cancel tree-mode before C-m; before={before:?}"
    );
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

    fn run_with_stdin(
        &self,
        argv: &[String],
        _stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
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

#[test]
fn grok_fold_literal_locks_external_team_line() {
    let prompt =
        super::pasted_prompt_in_composer(GROK_INCIDENT_LINE, 15).expect("lock grok incident line");
    assert_eq!(prompt.literal, "pasted:");
    assert_eq!(prompt.id, None, "grok fold has no #N");
    assert_eq!(prompt.line_count, Some(42));
    let matched = super::pasted_prompt_match(GROK_INCIDENT_LINE).expect("match");
    assert_eq!(matched.0, "pasted:");
}

#[test]
fn grok_fold_three_tui_literals_lock() {
    let grok = super::pasted_prompt_in_composer(GROK_INCIDENT_LINE, 15).unwrap();
    let claude = super::pasted_prompt_in_composer("❯ [Pasted text #4 +12 lines]\n", 15).unwrap();
    let codex = super::pasted_prompt_in_composer("❯ [Pasted content #2 +3 lines]\n", 15).unwrap();
    assert_eq!(grok.literal, "pasted:");
    assert_eq!(claude.literal, "pasted text");
    assert_eq!(claude.id, Some(4));
    assert_eq!(codex.literal, "pasted content");
    assert_eq!(codex.id, Some(2));
}

#[test]
fn grok_fold_same_line_count_still_unconsumed() {
    let marker = "[team-agent-token:msg_grok]";
    assert_eq!(
        super::consumption_from_capture(
            GROK_INCIDENT_LINE,
            marker,
            false,
            Some(super::PasteLatch::GrokLineCount(42))
        ),
        Some(false)
    );
}

#[test]
fn grok_fold_line_count_gone_is_consumed() {
    let marker = "[team-agent-token:msg_grok]";
    assert_eq!(
        super::consumption_from_capture(
            "Waiting for response\n❯ \n",
            marker,
            false,
            Some(super::PasteLatch::GrokLineCount(42))
        ),
        Some(true)
    );
}

#[test]
fn grok_fold_different_n_is_not_same_paste() {
    let marker = "[team-agent-token:msg_grok]";
    let other = "│ ❯ [Pasted: 10 lines]           │";
    assert_eq!(
        super::consumption_from_capture(
            other,
            marker,
            false,
            Some(super::PasteLatch::GrokLineCount(42))
        ),
        Some(false),
        "different N must not claim the latched paste consumed"
    );
    assert!(
        !super::should_resubmit_enter(other, marker, Some(super::PasteLatch::GrokLineCount(42))),
        "different N is not the same paste; do not hammer Enter"
    );
}

#[test]
fn grok_fold_no_latch_cannot_claim_consumed() {
    let marker = "[team-agent-token:msg_grok]";
    assert_eq!(
        super::consumption_from_capture("❯ \n", marker, false, None),
        Some(false)
    );
}

#[test]
fn grok_fold_inject_stays_retries_while_identity_present() {
    let token_text = "Team Agent message from leader:\nline1\n\n[team-agent-token:msg_grok_stay]";
    let (be, rec) = backend_folded(GROK_INCIDENT_LINE, GROK_INCIDENT_LINE);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        3,
        "same grok N still in composer must retry Enter up to cap=3; Unverified B must not add a 4th; invert wrap-gap guard must turn this red; calls={calls:?}"
    );
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

#[test]
fn cursor_single_enter_busy_transcript_placeholder_does_not_retry() {
    let token_text = "Team Agent message from leader:\nline1\n\n[team-agent-token:msg_cur_busy]";
    let (be, rec) = backend_folded("[Pasted text #1 +46 lines]\n", CURSOR_BUSY_AFTER_SUBMIT);
    let report = with_cursor_single_enter(true, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
    })
    .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "busy after first Enter is consumption for cursor; got {:?}",
        report.submit_verification
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "cursor busy: at most one Enter; extra Enter interrupts. calls={calls:?}"
    );
}

#[test]
fn cursor_single_enter_off_still_retries_on_transcript_placeholder() {
    let token_text = "Team Agent message from leader:\nline1\n\n[team-agent-token:msg_cur_ctrl]";
    let (be, rec) = backend_folded("[Pasted text #1 +46 lines]\n", CURSOR_BUSY_AFTER_SUBMIT);
    let _report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        3,
        "default path (claude/grok/codex) must keep retry cap; calls={calls:?}"
    );
}

#[test]
fn cursor_single_enter_retries_only_if_token_still_in_input() {
    let marker = "[team-agent-token:msg_cur_idle]";
    let token_text = format!("Team Agent message from leader:\nline1\n\n{marker}");
    let still_in_input = format!("{marker}\n→ \n");
    let (be, rec) = backend_folded(still_in_input.as_str(), still_in_input.as_str());
    let _report = with_cursor_single_enter(true, || {
        be.inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
    })
    .expect("inject");
    let calls = rec.lock().unwrap().clone();
    let n = count_submit_enters(&calls);
    assert!(
        n >= 2,
        "token still in cursor input box (no busy) may retry Enter; got {n} calls={calls:?}"
    );
}

#[test]
fn cursor_should_resubmit_false_when_busy_even_if_paste_placeholder_visible() {
    let marker = "[team-agent-token:msg_cur_gate]";
    assert!(
        !super::should_resubmit_enter_cursor(CURSOR_BUSY_AFTER_SUBMIT, marker),
        "busy + transcript placeholder must not resubmit on cursor"
    );
    assert!(
        super::should_resubmit_enter(
            CURSOR_BUSY_AFTER_SUBMIT,
            marker,
            Some(super::PasteLatch::HashId(1))
        ),
        "non-cursor detector still sees latched #1 as resubmit"
    );
}

#[test]
fn grok_fold_inject_leaves_consumed_one_enter() {
    let token_text = "Team Agent message from leader:\nline1\n\n[team-agent-token:msg_grok_go]";
    let (be, rec) = backend_folded(GROK_INCIDENT_LINE, "Waiting for response\n❯ \n");
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "fold left composer after one Enter; no empty-composer hammer; calls={calls:?}"
    );
}

#[test]
fn grok_fold_unverified_without_identity_one_enter() {
    let token_text = "Team Agent message from leader:\nhi\n\n[team-agent-token:msg_grok_empty]";
    let (be, rec) = backend_with(MockResp::Out(ok("")), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "no latch / empty composer: one Enter then stop; calls={calls:?}"
    );
}

#[test]
fn grok_fold_does_not_enter_before_fold_on_raw_tall_paste() {
    let marker = "[team-agent-token:msg_grok_early]";
    let token_text = format!(
        "Team Agent message from leader:\n{}\n\n{marker}",
        "x\n".repeat(20)
    );
    let raw = grok_raw_unfolded_paste(marker);
    assert!(
        !super::paste_ready_for_enter(&raw, marker),
        "raw tall paste with token is not ready"
    );
    assert!(super::paste_ready_for_enter(GROK_INCIDENT_LINE, marker));
    let (be, rec) =
        backend_raw_then_fold(&raw, GROK_INCIDENT_LINE, "Waiting for response\n❯ \n", 2);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck
    );
    let calls = rec.lock().unwrap().clone();
    let submit_index = calls
        .iter()
        .position(|argv| {
            argv.get(1).map(String::as_str) == Some("send-keys")
                && argv.iter().any(|a| a == "C-m" || a == "Enter")
        })
        .expect("must submit");
    let captures_before: Vec<&Vec<String>> = calls[..submit_index]
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("capture-pane"))
        .collect();
    assert!(
        captures_before.len() > 2,
        "must keep polling until fold, not Enter on first token; calls={calls:?}"
    );
    assert_eq!(count_submit_enters(&calls), 1);
}

/// 悬案B 主嫌：token 见过又消失（Gone）时 composer 仍有 grok 折叠占位符。
/// 修前 Gone 无条件 Some(true)。本测试必须先红。
#[test]
fn gone_with_grok_placeholder_still_in_composer_must_not_be_consumed() {
    let cap = "some transcript line\n> [Pasted: 27 lines]\nEnter:send  Esc:cancel\n";
    let marker = "[team-agent-token:deadbeef]";
    assert_eq!(
        super::token_sighting(false, true),
        super::TokenSighting::Gone
    );
    let got = super::consumption_from_capture(
        cap,
        marker,
        true,
        Some(super::PasteLatch::GrokLineCount(27)),
    );
    assert_eq!(
        got,
        Some(false),
        "Gone must re-check PasteLatch: placeholder still in composer must not be consumed, got {got:?}"
    );
}

#[test]
fn gone_with_grok_kb_placeholder_still_in_composer_must_not_be_consumed() {
    let cap = "│ ❯ [Pasted: 13 KB]                                                          │\n Enter:send\n";
    let marker = "[team-agent-token:kbform]";
    let prompt = super::pasted_prompt_in_composer(cap, 15).expect("KB form is grok fold");
    assert_eq!(prompt.literal, "pasted:");
    assert_eq!(prompt.line_count, Some(13));
    let got = super::consumption_from_capture(
        cap,
        marker,
        true,
        Some(super::PasteLatch::GrokLineCount(13)),
    );
    assert_eq!(
        got,
        Some(false),
        "Gone + [Pasted: 13 KB] still in composer must not be consumed, got {got:?}"
    );
}

#[test]
fn gone_empty_composer_without_latch_stays_consumed_for_claude() {
    let marker = "[team-agent-token:msg_short]";
    assert_eq!(
        super::consumption_from_capture("❯ \n", marker, true, None),
        Some(true),
        "claude short token gone + empty composer + no latch must stay consumed"
    );
}

/// 样本#5 归因钉：重试只重按 Enter，不得再 load/set/paste-buffer。
#[test]
fn grok_fold_raw_then_fold_placeholder_stays_retries_enter_not_repaste() {
    let marker = "[team-agent-token:msg_grok_reappear]";
    let token_text = format!(
        "Team Agent message from leader:\n{}\n\n{marker}",
        "x\n".repeat(20)
    );
    let raw = grok_raw_unfolded_paste(&marker);
    let (be, rec) = backend_raw_then_fold(&raw, GROK_INCIDENT_LINE, GROK_INCIDENT_LINE, 2);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "raw visible then folded, placeholder stays: must not false-green delivered; got {:?}",
        report.submit_verification
    );
    let calls = rec.lock().unwrap().clone();
    let enters = count_submit_enters(&calls);
    assert!(
        enters >= 2,
        "placeholder still in composer must retry Enter; got {enters} calls={calls:?}"
    );
    let paste_n = count_paste_stage(&calls);
    let first_enter = calls.iter().position(|argv| {
        argv.get(1).map(String::as_str) == Some("send-keys")
            && argv.iter().any(|a| a == "C-m" || a == "Enter")
    });
    let paste_after_enter = first_enter.map(|i| {
        calls[i + 1..]
            .iter()
            .filter(|argv| {
                matches!(
                    argv.get(1).map(String::as_str),
                    Some("load-buffer") | Some("set-buffer") | Some("paste-buffer")
                )
            })
            .count()
    });
    assert_eq!(
        paste_after_enter,
        Some(0),
        "retry must not re-paste (inject contract); paste_n={paste_n} calls={calls:?}"
    );
}

#[test]
fn grok_fold_worker_text_payload_polls_consumption() {
    assert!(
        !InjectPayload::Text("worker [team-agent-token:x]".to_string()).skip_consumption_poll(),
        "worker Text path poll_consumption=true; submit_attempt_limit is 3 not 1"
    );
    assert!(
        InjectPayload::TextSkipConsumptionPoll("leader [team-agent-token:x]".to_string())
            .skip_consumption_poll(),
        "leader skip path is the only poll_consumption=false arm"
    );
}

const G4_TOKEN: &str = "[team-agent-token:msg_g4]";

#[test]
fn g4_turn_grok_unsubmitted_paste_is_missing() {
    let text = "│ ❯ [Pasted: 42 lines]           │";
    assert_eq!(
        observe_turn_from_capture(text, Some(G4_TOKEN)),
        TurnVerification::LeaderNewTurnBoundaryMissing
    );
}

#[test]
fn g4_turn_claude_unsubmitted_paste_is_missing() {
    let text = "❯ pasted text #4 +12 lines";
    assert_eq!(
        observe_turn_from_capture(text, Some(G4_TOKEN)),
        TurnVerification::LeaderNewTurnBoundaryMissing
    );
}

#[test]
fn g4_turn_cursor_unsubmitted_token_is_missing() {
    let text = format!("❯ {G4_TOKEN}");
    assert_eq!(
        observe_turn_from_capture(&text, Some(G4_TOKEN)),
        TurnVerification::LeaderNewTurnBoundaryMissing
    );
}

#[test]
fn g4_turn_claude_spinner_is_verified() {
    let text = "✶ Thinking…\nesc to interrupt";
    assert_eq!(
        observe_turn_from_capture(text, Some(G4_TOKEN)),
        TurnVerification::LeaderNewTurnBoundaryVerified
    );
}

#[test]
fn g4_turn_cursor_processing_is_verified() {
    let text = "processing\n❯ ";
    assert_eq!(
        observe_turn_from_capture(text, Some(G4_TOKEN)),
        TurnVerification::LeaderNewTurnBoundaryVerified
    );
}

#[test]
fn g4_turn_grok_working_is_verified() {
    let text = "working\n❯ ";
    assert_eq!(
        observe_turn_from_capture(text, Some(G4_TOKEN)),
        TurnVerification::LeaderNewTurnBoundaryVerified
    );
}

#[test]
fn g4_turn_empty_composer_without_busy_is_unknown() {
    assert_eq!(
        observe_turn_from_capture("❯ ", Some(G4_TOKEN)),
        TurnVerification::NotYetObserved,
        "empty composer and no busy is 不知道, never 开跑"
    );
}

#[test]
fn g4_turn_inject_grok_incident_line_does_not_report_started() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_g4_inject]";
    let (be, _rec) = backend_with(MockResp::Out(ok(GROK_INCIDENT_LINE)), vec![]);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_ne!(
        report.turn_verification,
        TurnVerification::LeaderNewTurnBoundaryVerified,
        "unsubmitted grok paste must not report 开跑"
    );
    assert_eq!(
        report.turn_verification,
        TurnVerification::LeaderNewTurnBoundaryMissing
    );
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified
    );
}

#[test]
fn g4_turn_inject_consumed_without_busy_is_unknown() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_g4_gone]";
    let visible = ok(token_text);
    let queued = vec![
        MockResp::Out(ok("")),
        MockResp::Out(ok("")),
        MockResp::Out(ok("")),
        MockResp::Out(visible.clone()),
        MockResp::Out(visible),
        MockResp::Out(ok("")),
        MockResp::Out(ok("")),
    ];
    let (be, _rec) = backend_with(MockResp::Out(ok("❯ ")), queued);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "token gone is still consumed/delivered; that is not 开跑"
    );
    assert_eq!(
        report.turn_verification,
        TurnVerification::NotYetObserved,
        "composer empty without busy is 不知道, not 开跑"
    );
}

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

    fn run_with_stdin(
        &self,
        argv: &[String],
        _stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
    }
}

fn backend_capture_pane_fail() -> (TmuxBackend, RecordedArgv) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let runner = CapturePaneFailRunner {
        recorded: Arc::clone(&recorded),
    };
    (TmuxBackend::with_runner(Box::new(runner)), recorded)
}

/// P0 unknown-not-success: capture() Err 是「没能判断」，不得与 Some(true) 同值。
/// 齿覆盖「读数拿不到」，不是「读数不一样」。
#[test]
fn p0_capture_failure_is_unverified_not_enter_sent() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_cap_fail]";
    let (be, rec) = backend_capture_pane_fail();
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "consumed=None (capture missing) must not share \
         EnterSentWithoutPlaceholderCheck with Some(true); got {:?}",
        report.submit_verification
    );
    assert!(
        !crate::messaging::delivery::inject_submit_verified(&report),
        "no positive consumption signal ⇒ submit_verified must be false; report={report:?}"
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "capture retry must not send extra Enter; calls={calls:?}"
    );
}

/// P0: capture 失败先重读再判；重试计入 attempts；不重粘文本。
#[test]
fn p0_capture_failure_retries_read_and_counts_attempts() {
    let token_text = "Team Agent message from leader:\n\nhi\n\n[team-agent-token:msg_cap_retry]";
    let (be, rec) = backend_capture_pane_fail();
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text.to_string()),
            Key::Enter,
            true,
        )
        .expect("inject runs");
    assert!(
        report.attempts > 1,
        "capture failure must retry the read and count it in attempts; got attempts={}",
        report.attempts
    );
    let calls = rec.lock().unwrap().clone();
    let captures = calls
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("capture-pane"))
        .count();
    assert!(
        captures >= 2,
        "must re-read capture after failure; capture-pane count={captures} calls={calls:?}"
    );
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "retry is capture-only; extra Enter is forbidden; calls={calls:?}"
    );
    let pastes = calls
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("paste-buffer"))
        .count();
    assert_eq!(
        pastes, 1,
        "must not re-paste on capture failure; paste-buffer count={pastes}"
    );
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

    fn run_with_stdin(
        &self,
        argv: &[String],
        _stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run(argv)
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

#[test]
fn should_resend_enter_after_unverified_true_on_wrapped_tui_token() {
    let marker = "[team-agent-token:msg_wrap_gate]";
    let text = wrapped_unverified_composer(marker);
    assert!(
        !super::should_resubmit_enter(&text, marker, None),
        "per-line token_in_bottom_n must stay false on wrap"
    );
    assert!(
        super::should_resend_enter_after_unverified(&text, marker, None),
        "joined token identity must allow the one Unverified resend"
    );
}

#[test]
fn should_resend_wrapped_identity_on_three_tui_skins() {
    let marker = "[team-agent-token:msg_skins]";
    for skin in ["❯ ", "> ", "ready."] {
        let text = wrapped_identity_with_skin(marker, skin);
        assert!(
            super::should_resend_enter_after_unverified(&text, marker, None),
            "skin {skin:?} with this-paste token must resend"
        );
    }
}

#[test]
fn should_resend_false_when_payload_has_prompt_glyphs_but_identity_absent() {
    let marker = "[team-agent-token:msg_glyph_fp]";
    let text = "STARTED\nsleep holds pty\nsee composer> and ❯ in the payload\n>\n";
    assert!(
        !super::this_paste_identity_in_composer(text, marker, None),
        "glyphs in payload are not this-paste identity"
    );
    assert!(
        !super::should_resend_enter_after_unverified(text, marker, None),
        "payload composer>/❯ must not by themselves trigger resend"
    );
}

#[test]
fn should_resend_enter_after_unverified_true_when_unwrapped_token_still_in_tui() {
    let marker = "[team-agent-token:msg_still_tui]";
    let text = format!("> {marker}\n");
    assert!(super::should_resubmit_enter(&text, marker, None));
    assert!(
        super::should_resend_enter_after_unverified(&text, marker, None),
        "bare > skin with token identity still in composer must resend"
    );
}

#[test]
fn wrap_gap_false_when_a_already_sees_grok_latch() {
    let marker = "[team-agent-token:msg_grok_stay]";
    let tracked = Some(super::PasteLatch::GrokLineCount(42));
    assert!(super::should_resubmit_enter(
        GROK_INCIDENT_LINE,
        marker,
        tracked
    ));
    assert!(super::should_resend_enter_after_unverified(
        GROK_INCIDENT_LINE,
        marker,
        tracked
    ));
    assert!(
        !super::should_resend_unverified_wrap_gap(GROK_INCIDENT_LINE, marker, tracked),
        "A latch path already covers grok-fold stay; invert wrap-gap must turn this red"
    );
}

#[test]
fn wrap_gap_true_on_joined_token_a_cannot_see() {
    let marker = "[team-agent-token:msg_wrap_gate]";
    let text = wrapped_unverified_composer(marker);
    assert!(!super::should_resubmit_enter(&text, marker, None));
    assert!(
        super::should_resend_unverified_wrap_gap(&text, marker, None),
        "B must still cover the wrap gap A cannot see"
    );
}

#[test]
fn unverified_wrapped_token_resends_one_enter_then_consumes() {
    let marker = "[team-agent-token:msg_wrap_ok]";
    let token_text = format!("Team Agent message from leader:\nline1\n\n{marker}");
    let pending = wrapped_identity_with_skin(marker, "> ");
    let cleared = "> \n";
    let (be, rec) = backend_clears_after_n_enters(&pending, cleared, 2);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::EnterSentWithoutPlaceholderCheck,
        "Unverified + wrapped identity must resend one C-m then consume; got {:?}",
        report.submit_verification
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        2,
        "exactly one extra C-m after Unverified; calls={calls:?}"
    );
    let pastes = calls
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("paste-buffer"))
        .count();
    assert_eq!(pastes, 1, "must not re-paste; paste-buffer count={pastes}");
}

#[test]
fn unverified_resend_skips_when_this_message_identity_absent() {
    let marker = "[team-agent-token:msg_busy_nodup]";
    let token_text = format!("Team Agent message from leader:\nline1 composer> ❯\n\n{marker}");
    let pending = "STARTED\nsleep holds pty\ncomposer>\n❯\n>\n";
    let (be, rec) = backend_clears_after_n_enters(pending, pending, 99);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "identity absent must stay Unverified, no extra Enter; got {:?}",
        report.submit_verification
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "no this-paste identity ⇒ no Unverified resend; commenting the identity gate must turn this red (2+ C-m); calls={calls:?}"
    );
}

#[test]
fn unverified_identity_never_clears_hits_resend_cap_of_one() {
    let marker = "[team-agent-token:msg_cap_lock]";
    let token_text = format!("Team Agent message from leader:\nline1\n\n{marker}");
    let pending = wrapped_identity_with_skin(marker, "ready.");
    let (be, rec) = backend_clears_after_n_enters(&pending, &pending, u32::MAX);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
        .expect("inject");
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified,
        "never-clear identity must not fake-consume; got {:?}",
        report.submit_verification
    );
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        2,
        "cap must be exactly one extra C-m (1 inject + 1 resend); raising UNVERIFIED_COMPOSER_RESEND_MAX must turn this red; calls={calls:?}"
    );
    let pastes = calls
        .iter()
        .filter(|argv| argv.get(1).map(String::as_str) == Some("paste-buffer"))
        .count();
    assert_eq!(pastes, 1, "must not re-paste; paste-buffer count={pastes}");
}

#[test]
fn unverified_wrapped_identity_resends_on_each_of_three_skins() {
    for skin in ["❯ ", "> ", "ready."] {
        let marker = format!(
            "[team-agent-token:msg_skin_{}]",
            skin.chars().next().unwrap() as u32
        );
        let token_text = format!("Team Agent message from leader:\nline1\n\n{marker}");
        let pending = wrapped_identity_with_skin(&marker, skin);
        let cleared = format!("{skin}\n");
        let (be, rec) = backend_clears_after_n_enters(&pending, &cleared, 2);
        let report = be
            .inject(
                &Target::Pane(PaneId::new("%7")),
                &InjectPayload::Text(token_text),
                Key::Enter,
                true,
            )
            .expect("inject");
        assert_eq!(
            report.submit_verification,
            SubmitVerification::EnterSentWithoutPlaceholderCheck,
            "skin {skin:?} must consume after one extra C-m"
        );
        let n = count_submit_enters(&rec.lock().unwrap().clone());
        assert_eq!(n, 2, "skin {skin:?} extra C-m; got {n}");
    }
}

#[test]
fn unverified_payload_glyphs_do_not_resend_without_identity() {
    let marker = "[team-agent-token:msg_payload_glyph]";
    let token_text = format!("Team Agent message from leader:\ncomposer> ❯\n\n{marker}");
    let pending = "output\ncomposer> leftover from payload\n❯ quoted\n";
    let (be, rec) = backend_clears_after_n_enters(pending, pending, 99);
    let report = be
        .inject(
            &Target::Pane(PaneId::new("%7")),
            &InjectPayload::Text(token_text),
            Key::Enter,
            true,
        )
        .expect("inject");
    let calls = rec.lock().unwrap().clone();
    assert_eq!(
        count_submit_enters(&calls),
        1,
        "glyphs in payload must not cause extra C-m when token identity is gone; calls={calls:?}"
    );
    assert_eq!(
        report.submit_verification,
        SubmitVerification::SubmitConsumptionUnverified
    );
}
