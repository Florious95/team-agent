//! ---
//! purpose: 断言同一 pane 上两个并发写入者的实际字节被串行化，不是锁对象存在
//! contract:
//!   provides:
//!     - name: A4-pane-lock-serializes
//!       what: 两段慢写入不得交错；合文只能是 AAA…BBB… 或 BBB…AAA…
//! boundary:
//!   - 不测函数返回 Ok
//!   - 不加自动重粘文本
//! maturity: wired
//! ---
//!
//! 世界侧判据：CommandRunner 在 set-buffer 时按字符写入共享 pane 缓冲并休眠。
//! 当前无 pane 输入锁时两路 inject 会交错，本测试必须红。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{InjectPayload, Key, PaneId, Target, Transport};

const A: &str = "AAAAAAAA";
const B: &str = "BBBBBBBB";

struct SlowPaneRunner {
    pane: Arc<Mutex<String>>,
}

fn ok() -> CommandOutput {
    CommandOutput {
        success: true,
        code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
    }
}

impl CommandRunner for SlowPaneRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|arg| arg == "set-buffer") {
            if let Some(text) = argv.last() {
                for ch in text.chars() {
                    {
                        let mut pane = self.pane.lock().expect("pane mutex");
                        pane.push(ch);
                    }
                    thread::sleep(Duration::from_millis(8));
                }
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
            for ch in stdin.chars() {
                {
                    let mut pane = self.pane.lock().expect("pane mutex");
                    pane.push(ch);
                }
                thread::sleep(Duration::from_millis(8));
            }
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

/// Two concurrent injects into the same pane must land as two intact blocks.
/// Interleaving (ABAB…) means the world still has no input-channel lock.
#[test]
fn concurrent_injects_on_same_pane_are_serialized() {
    let ws = temp_ws("serial");
    let pane = Arc::new(Mutex::new(String::new()));
    let backend = TmuxBackend::with_runner_for_workspace(
        Box::new(SlowPaneRunner {
            pane: Arc::clone(&pane),
        }),
        &ws,
    );
    let target = Target::Pane(PaneId::new("%42"));

    thread::scope(|scope| {
        scope.spawn(|| {
            backend
                .inject(
                    &target,
                    &InjectPayload::TextSkipConsumptionPoll(A.to_string()),
                    Key::Enter,
                    false,
                )
                .expect("inject A");
        });
        scope.spawn(|| {
            backend
                .inject(
                    &target,
                    &InjectPayload::TextSkipConsumptionPoll(B.to_string()),
                    Key::Enter,
                    false,
                )
                .expect("inject B");
        });
    });

    let written = pane.lock().expect("pane mutex").clone();
    let ab = format!("{A}{B}");
    let ba = format!("{B}{A}");
    assert!(
        written == ab || written == ba,
        "concurrent pane writes must be serialized (no interleaving); got {written:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// Holder keeps the pane lock longer than PANE_INPUT_LOCK_TIMEOUT (200ms).
/// The waiter must still inject (proceed) and must emit pane_input_lock.timeout.
#[test]
fn lock_timeout_proceeds_and_emits_alarm() {
    let ws = temp_ws("timeout");
    let pane = Arc::new(Mutex::new(String::new()));
    let backend = TmuxBackend::with_runner_for_workspace(
        Box::new(SlowPaneRunner {
            pane: Arc::clone(&pane),
        }),
        &ws,
    );
    let target = Target::Pane(PaneId::new("%42"));
    // 40 chars * 8ms = 320ms > 200ms timeout.
    let long = "HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH";

    thread::scope(|scope| {
        scope.spawn(|| {
            backend
                .inject(
                    &target,
                    &InjectPayload::TextSkipConsumptionPoll(long.to_string()),
                    Key::Enter,
                    false,
                )
                .expect("holder inject");
        });
        thread::sleep(Duration::from_millis(20));
        scope.spawn(|| {
            backend
                .inject(
                    &target,
                    &InjectPayload::TextSkipConsumptionPoll(B.to_string()),
                    Key::Enter,
                    false,
                )
                .expect("waiter must proceed after lock timeout");
        });
    });

    let events = ws.join(".team").join("logs").join("events.jsonl");
    let text = std::fs::read_to_string(&events).unwrap_or_default();
    assert!(
        text.contains("pane_input_lock.timeout"),
        "timeout must be loud in events.jsonl; got {text:?}"
    );
    let written = pane.lock().expect("pane mutex").clone();
    assert!(
        written.contains('B'),
        "waiter must still write after timeout (may interleave once it proceeds); pane={written:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
