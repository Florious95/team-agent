//! ---
//! purpose: 断言同一 pane 上两个跨进程写入者的实际字节被串行化，不是锁对象存在
//! contract:
//!   provides:
//!     - name: A4-pane-lock-serializes
//!       what: 两个共享 workspace/pane 键的子进程慢写入不得交错；合文只能是 AAA…BBB… 或 BBB…AAA…
//!     - name: A4-pane-lock-timeout-proceeds
//!       what: 持锁超过 200ms 时等待者发出 pane_input_lock.timeout 后继续写入
//!   depends:
//!     - crate::lifecycle::pane_input_lock
//!     - OS cross-process file locking
//! boundary:
//!   - 通过同一 integration-test 可执行文件的子进程模拟 operator 的跨进程域
//!   - holder acquired、holder finished、waiter started 与 holder release 使用文件 barrier，不用固定 sleep 排竞态
//!   - 不加自动重粘文本
//! maturity: wired
//! ---
//!
//! 世界侧判据：CommandRunner 在 set-buffer 时按字符写入共享 pane 缓冲并休眠。
//! 当前无 pane 输入锁时两路 inject 会交错，本测试必须红。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{InjectPayload, Key, PaneId, Target, Transport};

const A: &str = "AAAAAAAA";
const B: &str = "BBBBBBBB";

struct ProcessPaneRunner {
    world: PathBuf,
    handshake: Option<PathBuf>,
    finished: Option<PathBuf>,
    release: Option<PathBuf>,
    delay: Duration,
}

fn ok() -> CommandOutput {
    CommandOutput {
        success: true,
        code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
    }
}

impl ProcessPaneRunner {
    fn append_text(&self, text: &str) {
        for (index, ch) in text.chars().enumerate() {
            let mut world = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.world)
                .expect("world file");
            write!(world, "{ch}").expect("world byte");
            if index == 0 {
                if let Some(handshake_path) = &self.handshake {
                    let mut handshake = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(handshake_path)
                        .expect("holder handshake");
                    writeln!(handshake, "pid={}", std::process::id()).expect("handshake record");
                }
            }
            // Deliberately slow world model: this creates observable interleaving
            // if the production lock is removed; barriers still control ordering.
            thread::sleep(self.delay);
        }
        if let Some(finished) = &self.finished {
            create_barrier(finished, "holder finished");
        }
        if let Some(release) = &self.release {
            wait_for_path(release, Duration::from_secs(5), "holder release");
        }
    }
}

impl CommandRunner for ProcessPaneRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|arg| arg == "set-buffer") {
            if let Some(text) = argv.last() {
                self.append_text(text);
            }
        }
        Ok(ok())
    }

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|arg| arg == "load-buffer") {
            self.append_text(stdin);
            return Ok(ok());
        }
        self.run(argv)
    }
}

fn temp_ws(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pane-input-lock-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp ws");
    dir
}

fn wait_for_path(path: &std::path::Path, timeout: Duration, description: &str) -> Duration {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {description}: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(1));
    }
    started.elapsed()
}

fn wait_for_child(child: &mut Child, timeout: Duration, description: &str) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("child status") {
            return status;
        }
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {description} child pid={}",
            child.id()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn spawn_lock_child(
    mode: &str,
    workspace: &std::path::Path,
    world: &std::path::Path,
    handshake: &std::path::Path,
    finished: &std::path::Path,
    waiter_started: &std::path::Path,
    release: Option<&std::path::Path>,
    payload: &str,
    delay_ms: u64,
) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("integration test executable"));
    command
        .arg("--exact")
        .arg("pane_input_lock_child_process")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("PANE_LOCK_CHILD_MODE", mode)
        .env("PANE_LOCK_WORKSPACE", workspace)
        .env("PANE_LOCK_WORLD", world)
        .env("PANE_LOCK_HANDSHAKE", handshake)
        .env("PANE_LOCK_FINISHED", finished)
        .env("PANE_LOCK_WAITER_STARTED", waiter_started)
        .env("PANE_LOCK_PAYLOAD", payload)
        .env("PANE_LOCK_DELAY_MS", delay_ms.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(release) = release {
        command.env("PANE_LOCK_RELEASE", release);
    }
    command.spawn().expect("spawn lock child")
}

fn create_barrier(path: &std::path::Path, description: &str) {
    let mut barrier = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("create {description} barrier {}: {error}", path.display()));
    writeln!(barrier, "pid={}", std::process::id()).expect("barrier record");
}

fn run_process_scenario(tag: &str, holder_payload: &str, expect_timeout: bool) {
    let ws = temp_ws(tag);
    let world = ws.join("pane-world.txt");
    let handshake = ws.join("holder-acquired");
    let finished = ws.join("holder-finished");
    let waiter_started = ws.join("waiter-started");
    let release = ws.join("release-holder");
    let delay_ms = if expect_timeout { 8 } else { 1 };
    let mut holder = spawn_lock_child(
        "holder",
        &ws,
        &world,
        &handshake,
        &finished,
        &waiter_started,
        Some(&release),
        holder_payload,
        delay_ms,
    );
    let handshake_ms =
        wait_for_path(&handshake, Duration::from_secs(2), "holder acquired").as_millis();
    let waiter_payload = B;
    let mut waiter = spawn_lock_child(
        "waiter",
        &ws,
        &world,
        &handshake,
        &finished,
        &waiter_started,
        None,
        waiter_payload,
        delay_ms,
    );
    let waiter_started_ms =
        wait_for_path(&waiter_started, Duration::from_secs(2), "waiter started").as_millis();

    let waiter_wait_started = Instant::now();
    let waiter_wait_ms;
    if expect_timeout {
        let finished_ms =
            wait_for_path(&finished, Duration::from_secs(2), "holder finished").as_millis();
        let waiter_status = wait_for_child(&mut waiter, Duration::from_secs(2), "timeout waiter");
        assert!(
            waiter_status.success(),
            "timeout waiter failed: {waiter_status}"
        );
        waiter_wait_ms = waiter_wait_started.elapsed().as_millis();
        assert!(
            finished_ms >= 200,
            "timeout arm must prove holder exceeded 200ms: finished_ms={finished_ms}"
        );
        create_barrier(&release, "holder release");
    } else {
        wait_for_path(&finished, Duration::from_secs(2), "holder finished");
        create_barrier(&release, "holder release");
        let waiter_status = wait_for_child(&mut waiter, Duration::from_secs(2), "short waiter");
        assert!(
            waiter_status.success(),
            "short waiter failed: {waiter_status}"
        );
        waiter_wait_ms = waiter_wait_started.elapsed().as_millis();
    }
    let holder_status = wait_for_child(&mut holder, Duration::from_secs(2), "holder");
    assert!(holder_status.success(), "holder failed: {holder_status}");

    let written = std::fs::read_to_string(&world).expect("world output");
    let events = ws.join(".team").join("logs").join("events.jsonl");
    let event_text = std::fs::read_to_string(&events).unwrap_or_default();
    eprintln!(
        "[pane-lock-evidence] tag={tag} holder_pid={} waiter_pid={} handshake_ms={} waiter_started_ms={} waiter_wait_ms={} timeout_ms=200 world_bytes={} events={:?}",
        holder.id(),
        waiter.id(),
        handshake_ms,
        waiter_started_ms,
        waiter_wait_ms,
        written.len(),
        event_text.lines().collect::<Vec<_>>()
    );

    if expect_timeout {
        assert!(
            event_text.contains("pane_input_lock.timeout"),
            "timeout must be loud in events.jsonl; got {event_text:?}"
        );
        assert!(
            written.contains('B'),
            "waiter must still write after timeout; pane={written:?}"
        );
    } else {
        let ab = format!("{A}{B}");
        let ba = format!("{B}{A}");
        assert!(
            written == ab || written == ba,
            "cross-process pane writes must be serialized (no interleaving); got {written:?}"
        );
        assert!(
            !event_text.contains("pane_input_lock.timeout"),
            "short release must not time out; events={event_text:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&ws);
}

/// Two concurrent injects in separate processes and the same pane domain must land as blocks.
#[test]
fn concurrent_injects_on_same_pane_are_serialized() {
    run_process_scenario("serial", A, false);
}

/// Holder keeps the pane lock longer than PANE_INPUT_LOCK_TIMEOUT (200ms).
/// The waiter must still inject (proceed) and must emit pane_input_lock.timeout.
#[test]
fn lock_timeout_proceeds_and_emits_alarm() {
    // 40 chars * 8ms = 320ms > 200ms timeout.
    let long = "HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH";
    run_process_scenario("timeout", long, true);
}

/// Child entry point used by the parent tests above. It is intentionally a test
/// so the subprocess uses the same integration-test binary and crate linkage.
#[test]
fn pane_input_lock_child_process() {
    let Some(mode) = std::env::var_os("PANE_LOCK_CHILD_MODE") else {
        return;
    };
    let mode = mode.to_string_lossy();
    let workspace = PathBuf::from(std::env::var_os("PANE_LOCK_WORKSPACE").expect("workspace"));
    let world = PathBuf::from(std::env::var_os("PANE_LOCK_WORLD").expect("world"));
    let handshake = PathBuf::from(std::env::var_os("PANE_LOCK_HANDSHAKE").expect("handshake"));
    let finished = PathBuf::from(std::env::var_os("PANE_LOCK_FINISHED").expect("finished"));
    let waiter_started = PathBuf::from(
        std::env::var_os("PANE_LOCK_WAITER_STARTED").expect("waiter started barrier"),
    );
    if mode == "waiter" {
        create_barrier(&waiter_started, "waiter started");
    }
    let release = std::env::var_os("PANE_LOCK_RELEASE").map(PathBuf::from);
    let payload = std::env::var("PANE_LOCK_PAYLOAD").expect("payload");
    let delay_ms = std::env::var("PANE_LOCK_DELAY_MS")
        .expect("delay")
        .parse::<u64>()
        .expect("delay milliseconds");
    let backend = TmuxBackend::with_runner_for_workspace(
        Box::new(ProcessPaneRunner {
            world,
            handshake: (mode == "holder").then_some(handshake),
            finished: (mode == "holder").then_some(finished),
            release,
            delay: Duration::from_millis(delay_ms),
        }),
        &workspace,
    );
    let target = Target::Pane(PaneId::new("%42"));
    backend
        .inject(
            &target,
            &InjectPayload::TextSkipConsumptionPoll(payload),
            Key::Enter,
            false,
        )
        .unwrap_or_else(|error| panic!("{mode} inject: {error:?}"));
    eprintln!(
        "[pane-lock-child] pid={} mode={mode} completed",
        std::process::id()
    );
}
