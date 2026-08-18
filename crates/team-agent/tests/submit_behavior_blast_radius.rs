//! ---
//! purpose: 钉死 claude/codex 提交键次数不因 grok send-now 而增加
//! contract:
//!   provides:
//!     - name: A12-other-providers-submit-unchanged
//!       what: 无 Enter:send now 时 flush 零次 send_keys；有标记时才额外回车
//! boundary:
//!   - 不把符号存在当通过
//! maturity: wired
//! ---
//!
//! 世界侧：Recording runner 上的 send-keys Enter 次数。
//! 基线（flush 空实现）grok 有标记也不会多按回车 → 本文件必须红。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use team_agent::provider::submit_now::{
    flush_explicit_queue_for, CURSOR_SEND_NOW_MARK, GROK_SEND_NOW_MARK,
};
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{Key, PaneId, Target, Transport};

struct ScriptedRunner {
    screen: Arc<Mutex<String>>,
    enters: Arc<Mutex<u32>>,
}

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|a| a == "capture-pane") {
            return Ok(ok(&self.screen.lock().expect("screen")));
        }
        if argv.iter().any(|a| a == "send-keys") {
            *self.enters.lock().expect("enters") += 1;
            // After a send-now Enter, grok leaves the queue footer.
            let mut screen = self.screen.lock().expect("screen");
            *screen = screen
                .replace(GROK_SEND_NOW_MARK, "")
                .replace(CURSOR_SEND_NOW_MARK, "");
        }
        Ok(ok(""))
    }
}

fn backend(screen: &str, enters: &Arc<Mutex<u32>>) -> TmuxBackend {
    TmuxBackend::with_runner(Box::new(ScriptedRunner {
        screen: Arc::new(Mutex::new(screen.to_string())),
        enters: Arc::clone(enters),
    }))
}

fn flush_enters(screen: &str) -> u32 {
    flush_enters_for(screen, &[GROK_SEND_NOW_MARK, CURSOR_SEND_NOW_MARK])
}

fn flush_enters_for(screen: &str, marks: &[&str]) -> u32 {
    let enters = Arc::new(Mutex::new(0));
    let be = backend(screen, &enters);
    let target = Target::Pane(PaneId::new("%1"));
    flush_explicit_queue_for(&be, &target, marks).expect("flush");
    let n = *enters.lock().expect("enters");
    n
}

/// Claude never shows the grok queue footer. Flush must not add an Enter.
#[test]
fn claude_screen_without_queue_mark_sends_zero_extra_enters() {
    let screen = "● Running tool\nesc to interrupt\n❯ ";
    assert_eq!(
        flush_enters(screen),
        0,
        "claude submit must not gain an extra Enter from grok send-now"
    );
}

/// Codex is next-tool-call insertion, not grok's turn-end queue.
#[test]
fn codex_screen_without_queue_mark_sends_zero_extra_enters() {
    let screen = "workdir · thinking\nesc to interrupt\n";
    assert_eq!(
        flush_enters(screen),
        0,
        "codex submit must not gain an extra Enter from grok send-now"
    );
}

/// World change: grok queue footer must be flushed with extra Enter(s).
#[test]
fn grok_queue_mark_gets_send_now_enter() {
    let screen = format!("#1 stop-do-not-delete\n{GROK_SEND_NOW_MARK}\n");
    let n = flush_enters(&screen);
    assert!(
        n >= 1,
        "grok queue mark must be flushed by extra Enter (not re-paste); got {n}"
    );
}

/// cursor 忙时页脚是 `enter send now`（大小写按屏上），同样只重按回车。
#[test]
fn cursor_queue_mark_gets_send_now_enter() {
    let screen = format!("#1 follow-up\n{CURSOR_SEND_NOW_MARK} · ↑ select/edit · esc cancel\n");
    let n = flush_enters(&screen);
    assert!(
        n >= 1,
        "cursor queue mark must be flushed by extra Enter (not re-paste); got {n}"
    );
}

const CLAUDE_NO_QUEUE: &str = "● Running tool\nesc to interrupt\n❯ ";
const CODEX_NO_QUEUE: &str = "workdir · thinking\nesc to interrupt\n";

/// 该路单独启用：grok 标记在无队列 pane 上必须零次 send_keys。
#[test]
fn grok_mark_alone_on_no_queue_pane_sends_zero_enters() {
    assert_eq!(
        flush_enters_for(CLAUDE_NO_QUEUE, &[GROK_SEND_NOW_MARK]),
        0,
        "grok detector alone must not press Enter on a claude pane"
    );
    assert_eq!(
        flush_enters_for(CODEX_NO_QUEUE, &[GROK_SEND_NOW_MARK]),
        0,
        "grok detector alone must not press Enter on a codex pane"
    );
}

/// 该路单独启用：cursor 标记在无队列 pane 上必须零次 send_keys。
#[test]
fn cursor_mark_alone_on_no_queue_pane_sends_zero_enters() {
    assert_eq!(
        flush_enters_for(CLAUDE_NO_QUEUE, &[CURSOR_SEND_NOW_MARK]),
        0,
        "cursor detector alone must not press Enter on a claude pane"
    );
    assert_eq!(
        flush_enters_for(CODEX_NO_QUEUE, &[CURSOR_SEND_NOW_MARK]),
        0,
        "cursor detector alone must not press Enter on a codex pane"
    );
}

/// grok 路单独启用时，cursor 的页脚不算队列。
#[test]
fn grok_mark_alone_ignores_cursor_queue_footer() {
    let screen = format!("#1 follow-up\n{CURSOR_SEND_NOW_MARK} · ↑ select/edit · esc cancel\n");
    assert_eq!(
        flush_enters_for(&screen, &[GROK_SEND_NOW_MARK]),
        0,
        "enabling only the grok mark must not fire on a cursor follow-ups footer"
    );
}

/// cursor 路单独启用时，grok 的页脚不算队列。
#[test]
fn cursor_mark_alone_ignores_grok_queue_footer() {
    let screen = format!("#1 stop-do-not-delete\n{GROK_SEND_NOW_MARK}\n");
    assert_eq!(
        flush_enters_for(&screen, &[CURSOR_SEND_NOW_MARK]),
        0,
        "enabling only the cursor mark must not fire on a grok Enter:send now footer"
    );
}
