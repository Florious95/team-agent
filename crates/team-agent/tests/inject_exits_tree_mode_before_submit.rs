//! ---
//! purpose: 真实 tmux（需 attached client）上断言 tree-mode 下 inject 先 q 再提交
//! contract:
//!   provides:
//!     - name: inject-exits-tree-mode
//!       what: GOT:[msg] 次数=1（Enter 没被 tree-mode 吃掉）
//! boundary:
//!   - 不测确认面
//!   - 不复活 inject_journal
//!   - 不装机
//!   - 不送 Escape/C-c
//! maturity: wired
//! ---
//!
//! 构造性场景：主动 choose-tree 后再 inject。不声称复现用户故障样本
//! （用户样本归悬案 B 族）。`-X cancel` 在 tree-mode 上失败（"not in a mode"），
//! 必须 send-keys q。无 attached client 时 q 到不了 tree key table。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::tmux_backend::{CommandOutput, RealCommandRunner, TmuxBackend};
use team_agent::transport::{InjectPayload, Key, PaneId, Target, Transport};

const MSG: &str = "treemode-inject-proof-marker";

fn short_tmux_socket(tag: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!("ta-treemode-{tag}-{pid}-{n}.sock"))
        .to_string_lossy()
        .into_owned()
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

struct TreemodeFixture {
    inner: String,
    outer: String,
    pane: String,
}

impl TreemodeFixture {
    fn new(tag: &str) -> Self {
        let inner = short_tmux_socket(&format!("{tag}-in"));
        let outer = short_tmux_socket(&format!("{tag}-out"));
        tmux_ok(
            &inner,
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
        let out = tmux(&inner, &["list-panes", "-F", "#{pane_id}"]);
        let pane = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("%0")
            .to_string();
        let attach = format!("tmux -S {inner} attach-session -t probe");
        tmux_ok(
            &outer,
            &[
                "new-session",
                "-d",
                "-s",
                "wrap",
                "-x",
                "80",
                "-y",
                "24",
                &attach,
            ],
        );
        std::thread::sleep(std::time::Duration::from_millis(800));
        Self { inner, outer, pane }
    }

    fn reader_lines(&self) -> String {
        let out = tmux(&self.inner, &["capture-pane", "-p", "-t", &self.pane]);
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn submitted_count(&self, needle: &str) -> usize {
        self.reader_lines()
            .lines()
            .filter(|l| l.contains(&format!("GOT:[{needle}]")))
            .count()
    }
}

impl Drop for TreemodeFixture {
    fn drop(&mut self) {
        let _ = tmux(&self.inner, &["kill-server"]);
        let _ = tmux(&self.outer, &["kill-server"]);
    }
}

fn run_inject(socket: &str, pane: &str, payload: &str) {
    let backend = TmuxBackend::with_runner_for_tmux_endpoint(Box::new(RealCommandRunner), socket);
    let _ = backend.inject(
        &Target::Pane(PaneId::new(pane)),
        &InjectPayload::Text(payload.to_string()),
        Key::Enter,
        false,
    );
}

#[test]
fn inject_submits_message_when_pane_in_tree_mode() {
    let fixture = TreemodeFixture::new("submit");
    let clients = tmux(&fixture.inner, &["list-clients"]);
    assert!(
        clients.status.success() && !clients.stdout.is_empty(),
        "tree-mode q needs an attached client; list-clients empty"
    );

    let normal_msg = format!("{MSG}-NORMAL");
    run_inject(&fixture.inner, &fixture.pane, &normal_msg);
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert_eq!(
        fixture.submitted_count(&normal_msg),
        1,
        "正常态注入必须被提交（阳性对照）; reader={:?}",
        fixture.reader_lines()
    );

    assert!(
        tmux_ok(&fixture.inner, &["choose-tree", "-t", &fixture.pane]),
        "choose-tree must succeed"
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mode = tmux(
        &fixture.inner,
        &["display-message", "-p", "-t", &fixture.pane, "#{pane_mode}"],
    );
    assert_eq!(
        String::from_utf8_lossy(&mode.stdout).trim(),
        "tree-mode",
        "前置条件:tree-mode 必须已进入"
    );

    let tree_msg = format!("{MSG}-TREEMODE");
    run_inject(&fixture.inner, &fixture.pane, &tree_msg);
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let got = fixture.submitted_count(&tree_msg);
    assert_eq!(
        got,
        1,
        "tree-mode 下注入必须先 q 再提交; GOT:[{tree_msg}] 次数={got}; reader={:?}",
        fixture.reader_lines()
    );
}

/// 用户裁定 2026-08-23：不发 ESC，也不发 ESC 的替代品（含字面 CSI 201~）。
/// 发 ESC/CSI 的唯一好处是让消息当场直接上屏；本项目只要求「下一次工具调用时能上屏」
/// ⇒ 不存在需要它才能满足的场景，发它没有收益，只有污染 composer 输入的风险。
/// 本判据由「未闭合 paste 必须被 201~ 闭合后提交」翻面为「不再修复未闭合态」：
/// 先在干净 pane 上做阳性对照（inject 必须上屏一次），再证明未闭合态下不上屏。
#[test]
fn inject_sends_no_escape_when_bracketed_paste_left_open() {
    let inner = short_tmux_socket("paste-in");
    let script = concat!(
        "import sys\n",
        "p=False\n",
        "a=bytearray()\n",
        "while True:\n",
        "    ch=sys.stdin.buffer.read(1)\n",
        "    if not ch: break\n",
        "    a.extend(ch)\n",
        "    if a.endswith(b'\\x1b[200~'):\n",
        "        del a[-6:]; p=True; continue\n",
        "    if a.endswith(b'\\x1b[201~'):\n",
        "        del a[-6:]; p=False; continue\n",
        "    if ch in (b'\\r', b'\\n') and not p:\n",
        "        print('GOT:['+bytes(a[:-1]).decode('utf-8','replace')+']', flush=True)\n",
        "        a.clear()\n",
    );
    let script_path =
        std::env::temp_dir().join(format!("ta-paste-reader-{}.py", std::process::id()));
    std::fs::write(&script_path, script).unwrap();
    let cmd = format!("python3 -u {}", script_path.display());
    tmux_ok(
        &inner,
        &[
            "new-session",
            "-d",
            "-s",
            "probe",
            "-x",
            "80",
            "-y",
            "24",
            &cmd,
        ],
    );
    std::thread::sleep(std::time::Duration::from_millis(800));
    let out = tmux(&inner, &["list-panes", "-F", "#{pane_id}"]);
    let pane = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("%0")
        .to_string();
    // 阳性对照：paste 未打开时，同一装置上 inject 必须上屏恰一次。
    let ctl = "paste-closed-control-marker";
    run_inject(&inner, &pane, ctl);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let _ = tmux(&inner, &["send-keys", "-t", &pane, "-l", "\u{1b}[200~OPEN"]);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let msg = "paste-open-proof-marker";
    run_inject(&inner, &pane, msg);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let cap = {
        let o = tmux(&inner, &["capture-pane", "-p", "-t", &pane]);
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let _ = tmux(&inner, &["kill-server"]);
    let _ = std::fs::remove_file(&script_path);
    let count = |m: &str| {
        cap.lines()
            .filter(|l| l.contains(&format!("GOT:[{m}]")))
            .count()
    };
    assert_eq!(
        count(ctl),
        1,
        "positive control: closed paste must still submit once; cap={cap:?}"
    );
    assert_eq!(
        count(msg),
        0,
        "unclosed bracketed paste is no longer repaired: no ESC/CSI is sent; cap={cap:?}"
    );
}

#[allow(dead_code)]
fn _keep(_c: &CommandOutput) {}
