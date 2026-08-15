//! inject copy-mode RED — 提交前退 copy-mode,否则 Enter 被吃掉消息不提交。
//!
//! ```yaml
//! purpose: 真实 tmux 上断言 copy-mode 下 inject 会先 -X cancel 再提交
//! contract: GOT:[msg] 次数=1（Enter 没被 copy-mode 吃掉）
//! boundary: 不测确认面 / 不复活 inject_journal / 不装机
//! ```
//!
//! 根因(leader 100% 复现):pane 处于 tmux copy-mode 时,paste-buffer 照常把文字写进
//! pty(输入框看得见),但 `send-keys Enter` 被 copy-mode 吃掉——既不提交也不换行,
//! tmux 每次返回 rc=0,框架误以为发送成功。重试 3 次全被吃。下一条消息注入就
//! 焊在前一条后面(粘连),copy-mode 退出后一次 Enter 把攒的几条当一条提交。
//!
//! 旁证:黑匣子 953 次 send-keys 全部 rc=0(阳性对照:全体 1630 次非零),所以不是
//! send-keys 失败,是键被拦。
//!
//! 本测试用**真实 tmux**(与 leader 的 probe_copymode.sh 同构)构造 copy-mode 场景,
//! 驱动真实 `TmuxBackend::inject`,断言注入后消息**确实被提交**(reader 收到 GOT:[msg])。
//! 修复前必须真红。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::tmux_backend::{CommandOutput, CommandRunner, RealCommandRunner, TmuxBackend};
use team_agent::transport::{InjectPayload, Key, PaneId, Target, Transport};

/// 判据:注入后 reader 是否收到 `GOT:[<msg>]`(=消息真被提交,Enter 没被吃)。
const MSG: &str = "copymode-inject-proof-marker";

fn short_tmux_socket(tag: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("/private/tmp/ta-copymode-{tag}-{pid}-{n}.sock")
}

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("tmux");
    cmd.arg("-S").arg(socket);
    cmd.args(args);
    cmd.output().expect("run tmux")
}

fn tmux_ok(socket: &str, args: &[&str]) -> bool {
    tmux(socket, args).status.success()
}

/// 起一个真实 tmux:sh 循环读取 stdin,收到一行打印 `GOT:[<line>]`(判据)。
struct CopymodeFixture {
    socket: String,
    pane: String,
    submitted: String, // 供后续扩展
}

impl CopymodeFixture {
    fn new(tag: &str) -> Self {
        let socket = short_tmux_socket(tag);
        // 80x24 精确复刻 probe_copymode.sh。
        tmux_ok(
            &socket,
            &[
                "new-session",
                "-d",
                "-s",
                "probe",
                "-x",
                "80",
                "-y",
                "24",
                "sh -c 'while IFS= read -r l; do echo \"GOT:[$l]\"; done'",
            ],
        );
        std::thread::sleep(std::time::Duration::from_millis(800));
        let out = tmux(&socket, &["list-panes", "-F", "#{pane_id}"]);
        let pane = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("%0")
            .to_string();
        Self {
            socket,
            pane,
            submitted: String::new(),
        }
    }

    fn reader_lines(&self) -> String {
        let out = tmux(&self.socket, &["capture-pane", "-p", "-t", &self.pane]);
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn submitted_count(&self, needle: &str) -> usize {
        // 只数 `GOT:[<msg>]` 输出行(reader 真收到),不数输入回显。
        self.reader_lines()
            .lines()
            .filter(|l| l.contains(&format!("GOT:[{needle}]")))
            .count()
    }
}

impl Drop for CopymodeFixture {
    fn drop(&mut self) {
        let _ = tmux(&self.socket, &["kill-server"]);
    }
}

/// 用真实 tmux 驱动 `TmuxBackend::inject`(真实 CommandRunner)。
/// `bracketed`:copy-mode 场景用 false(不送 Escape,还原"Enter 被吃"的缺陷路径;
/// bracketed=true 时 inject 会送 Escape,恰好退出 copy-mode 掩盖缺陷)。
fn run_inject(socket: &str, pane: &str, payload: &str, bracketed: bool) {
    let runner = RealCommandRunner;
    // TmuxBackend 直接持真实 socket——与产品 transport 同构。
    let backend = TmuxBackend::with_runner_for_tmux_endpoint(Box::new(runner), socket);
    let _ = backend.inject(
        &Target::Pane(PaneId::new(pane)),
        &InjectPayload::Text(payload.to_string()),
        Key::Enter,
        bracketed,
    );
}

/// 🔴 RED:copy-mode 下注入,消息必须**确实被提交**(reader 收到 GOT:[msg])。
/// 修复前(不退 copy-mode):Enter 被吃,reader 收不到 → 红。
#[test]
fn inject_submits_message_when_pane_in_copy_mode() {
    let fixture = CopymodeFixture::new("submit");
    // 先正常注入一次(对照:证明 reader 工作)——单行无尾换行,只有 Enter 能提交。
    let normal_msg = format!("{MSG}-NORMAL");
    run_inject(&fixture.socket, &fixture.pane, &normal_msg, false);
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(
        fixture.submitted_count(&normal_msg),
        1,
        "正常态注入必须被提交(reader 收到 GOT:[{normal_msg}])——这是 fixture 自身的阳性对照; \
         reader={:?}",
        fixture.reader_lines()
    );

    // 进入 copy-mode。
    tmux_ok(&fixture.socket, &["copy-mode", "-e", "-t", &fixture.pane]);
    // scroll-up 让 copy-mode 确定进入(probe 同款)。
    let _ = tmux(
        &fixture.socket,
        &["send-keys", "-X", "-N", "1", "-t", &fixture.pane, "scroll-up"],
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    // 确认 copy-mode 已进入(前置条件)。
    let in_mode = tmux(
        &fixture.socket,
        &["display-message", "-p", "-t", &fixture.pane, "#{pane_in_mode}"],
    );
    assert_eq!(
        String::from_utf8_lossy(&in_mode.stdout).trim(),
        "1",
        "前置条件:copy-mode 必须已进入(pane_in_mode=1)"
    );

    // copy-mode 下注入(单行无尾换行):修复后应先退 copy-mode 再 Enter,消息被提交。
    let cm_msg = format!("{MSG}-COPYMODE");
    run_inject(&fixture.socket, &fixture.pane, &cm_msg, false);
    std::thread::sleep(std::time::Duration::from_millis(1000));

    // 判据:reader 收到 GOT:[cm_msg] 说明 Enter 真提交了(没被 copy-mode 吃)。
    // 修复前(不退 copy-mode):Enter 被吃,reader 收不到 → 红。
    let got = fixture.submitted_count(&cm_msg);
    assert_eq!(
        got, 1,
        "copy-mode 下注入必须退 copy-mode 后提交消息(Enter 没被吃); reader 收到 GOT:[{cm_msg}] 次数={got}; \
         reader={:?}",
        fixture.reader_lines()
    );
}

/// 阳性对照(证明判据方向):正常态注入必提交(单行无尾换行,只有 Enter 能提交)。
#[test]
fn inject_submits_message_in_normal_mode_positive_control() {
    let fixture = CopymodeFixture::new("normal");
    let msg = format!("{MSG}-POSCTRL");
    run_inject(&fixture.socket, &fixture.pane, &msg, false);
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(
        fixture.submitted_count(&msg),
        1,
        "正常态注入必须被提交(reader 收到 GOT:[{msg}])——判据方向阳性对照; reader={:?}",
        fixture.reader_lines()
    );
}

// 保留 CommandOutput 类型引用,避免 unused 警告。
#[allow(dead_code)]
fn _keep(_c: &CommandOutput, _p: PathBuf) {}
