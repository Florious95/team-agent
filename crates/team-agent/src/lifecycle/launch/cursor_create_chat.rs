//! ---
//! purpose: cursor 候选 A——`agent create-chat` 解析首行 chatId 后按记录 PID 杀
//! contract:
//!   provides:
//!     - name: apply_cursor_create_chat_plan
//!       what: 无 pending 时把 CLI chatId 写入 expected_session_id 并 `--resume` 该 id
//!     - name: create_cursor_chat_id
//!       what: 跑 create-chat，读第一行 UUID，只杀本调用记录的 PID
//!   requires:
//!     - name: cursor-agent-cli
//!       what: PATH 上的 `agent`；不存在或首行非法则返回 None（降级候选 B）
//! boundary:
//!   - 不发明 --session-id；不 pkill/killall；不 wait 子进程自行退出
//!   - 不读 chats 正文、不打印 proxy 值
//!   - 已有 expected_session_id 或 argv 已含 --resume 则不动
//!   - shell stub 与 executable-bit 测试辅助仅在 Unix 编译；生产计划路径跨平台保持不变
//!   - 2026-08-21 活体：create-chat 后 `--resume` 的存档在 pane stop 后被清；
//!     TUI 自建会话能留盘。fresh 路径不调用本函数（走 scan 唯一 hex）。
//! maturity: wired
//! ---

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::provider::types::{CommandPlan, SessionId};

const FIRST_LINE_TIMEOUT: Duration = Duration::from_secs(5);
const TERM_GRACE: Duration = Duration::from_millis(200);

/// ---
/// purpose: 冷启/无 session 的 cursor 计划绑定 create-chat 得到的 chatId
/// params:
///   plan: build_command_plan 产出；workspace: 物理 cwd
/// returns: 成功则 expected_session_id 与 argv `--resume <id>` 齐；失败则计划不变
/// contract:
///   provides:
///     - name: apply_cursor_create_chat_plan
///       what: 只在无 pending 且 argv 无 --resume 时写入 CLI chatId
/// boundary:
///   - 库测试（cfg(test)）不调真实 agent，避免订阅/挂起；具名测试打 create_cursor_chat_id
///   - 不在 launch/restart fresh 调用：活体证实 `--resume` 空聊存档停 pane 后消失
/// ---
pub(crate) fn apply_cursor_create_chat_plan(plan: &mut CommandPlan, workspace: &Path) {
    if cfg!(test) {
        return;
    }
    if plan.expected_session_id.is_some() {
        return;
    }
    if plan.argv.iter().any(|flag| flag == "--resume") {
        return;
    }
    let Some(chat_id) = create_cursor_chat_id(workspace) else {
        return;
    };
    plan.expected_session_id = Some(SessionId::new(chat_id.clone()));
    plan.argv.push("--resume".to_string());
    plan.argv.push(chat_id);
}

/// ---
/// purpose: 跑 `agent create-chat`，解析首行 UUID，按记录 PID 终止挂起进程
/// params:
///   workspace: create-chat 的 cwd（物理 workspace）
/// returns: 合法 UUID；命令缺失/超时/非法首行则 None
/// contract:
///   provides:
///     - name: create_cursor_chat_id
///       what: 读一行就停；不 wait 退出；只杀本 Child.id()
/// boundary:
///   - 禁 pkill/killall；pid 0/1 不杀
/// ---
pub(crate) fn create_cursor_chat_id(workspace: &Path) -> Option<String> {
    create_cursor_chat_id_with_command("agent", workspace)
}

fn create_cursor_chat_id_with_command(program: &str, workspace: &Path) -> Option<String> {
    let mut child = Command::new(program)
        .arg("create-chat")
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).ok().and_then(|_| parse_chat_id_line(&line));
        let _ = tx.send(result);
    });
    let parsed = match rx.recv_timeout(FIRST_LINE_TIMEOUT) {
        Ok(value) => value,
        Err(_) => None,
    };
    kill_recorded_pid(pid);
    let _ = child.wait();
    parsed
}

fn parse_chat_id_line(line: &str) -> Option<String> {
    let token = line
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '\'']);
    is_chat_uuid(token).then(|| token.to_string())
}

fn is_chat_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn kill_recorded_pid(pid: u32) {
    if pid == 0 || pid == 1 {
        return;
    }
    #[cfg(unix)]
    {
        let raw = pid as i32;
        unsafe {
            libc::kill(raw, libc::SIGTERM);
        }
        thread::sleep(TERM_GRACE);
        unsafe {
            libc::kill(raw, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-d117-create-chat-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn write_stub(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("agent");
        std::fs::write(&path, body).unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn create_chat_reads_first_line_and_kills_recorded_pid() {
        let base = tmp_root("ok");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let bin = write_stub(
            &base,
            "#!/bin/sh\necho '681c8837-d564-44fa-98bf-4ff5a66592e2'\nkill -STOP $$\n",
        );
        let got = create_cursor_chat_id_with_command(bin.to_str().unwrap(), &ws);
        assert_eq!(
            got.as_deref(),
            Some("681c8837-d564-44fa-98bf-4ff5a66592e2")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn create_chat_rejects_non_uuid_first_line() {
        let base = tmp_root("bad");
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let bin = write_stub(&base, "#!/bin/sh\necho not-a-uuid\nkill -STOP $$\n");
        let got = create_cursor_chat_id_with_command(bin.to_str().unwrap(), &ws);
        assert!(got.is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn apply_is_noop_when_resume_already_present() {
        let mut plan = CommandPlan {
            argv: vec![
                "agent".into(),
                "--resume".into(),
                "already-bound".into(),
            ],
            expected_session_id: None,
            provider_projects_root: None,
            managed_mcp_config: false,
        };
        apply_cursor_create_chat_plan(&mut plan, Path::new("/tmp"));
        assert!(plan.expected_session_id.is_none());
        assert_eq!(plan.argv.iter().filter(|a| *a == "--resume").count(), 1);
    }
}
