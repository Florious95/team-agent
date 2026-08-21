//! ---
//! purpose: 按 pending chatId 捕获 ~/.cursor/chats/ 存档，不读会话正文
//! contract:
//!   provides:
//!     - name: cursor_session_dir
//!       what: 由 HOME + chatId 派生磁盘存档目录（直达或一层 hex 子目录）
//!     - name: cursor_session_archive_present
//!       what: 只凭 marker 文件存在性判定 backing，不打开正文
//!     - name: scan_session_store
//!       what: expected_session_id 命中存档则返回唯一候选，否则空
//!   requires:
//!     - name: expected_session_id
//!       what: 无 pending id 不扫描、不拾取同 HOME 其它 chats
//! boundary:
//!   - 只服务 Provider::CursorAgent
//!   - 不发明 --session-id；不读 store.db/meta.json 正文
//!   - 不改 grok/claude/codex/copilot 扫描
//! maturity: wired
//! ---

use std::path::{Path, PathBuf};

use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath};

use super::{CaptureSessionContext, CapturedSessionCandidate};

fn session_id_legal(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains('/')
        && !session_id.contains('\\')
        && !session_id.contains('\0')
}

/// Marker names from 2026-08-21 U-02 `ls` of `~/.cursor/chats/<hex>/<uuid>/`
/// (names only). Existence is backing; contents are not read.
const MARKERS: &[&str] = &["store.db", "meta.json"];

/// ---
/// purpose: 只凭 marker 文件存在性判断 cursor 会话存档是否可 resume
/// params:
///   dir: cursor_session_dir 给出的目录
/// returns: 目录存在且含 store.db 或 meta.json
/// contract:
///   provides:
///     - name: cursor_session_archive_present
///       what: 用 is_file 判 backing，不打开正文
/// boundary:
///   - 不读 json / sqlite 内容
/// ---
pub(crate) fn cursor_session_archive_present(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    MARKERS.iter().any(|name| dir.join(name).is_file())
}

fn first_present_dir(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|dir| cursor_session_archive_present(dir))
}

/// ---
/// purpose: 由 chatId 派生 ~/.cursor/chats 下的存档目录
/// params:
///   session_id: CLI chatId；空或含路径分隔则拒绝
/// returns: HOME 下含 marker 的目录；HOME 缺失或 id 非法则 None
/// contract:
///   provides:
///     - name: cursor_session_dir
///       what: 只派生并核 marker 存在性，不创建目录、不读存档
/// boundary:
///   - 无 pending 时不枚举其它 chatId
/// ---
pub(crate) fn cursor_session_dir(session_id: &str) -> Option<PathBuf> {
    if !session_id_legal(session_id) {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let chats = home.join(".cursor").join("chats");
    let direct = chats.join(session_id);
    if let Some(dir) = first_present_dir([direct.clone()]) {
        return Some(dir);
    }
    // U-02 layout: ~/.cursor/chats/<hex32>/<uuid>/store.db
    if direct.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&direct) {
            for entry in entries.flatten() {
                let child = entry.path();
                if cursor_session_archive_present(&child) {
                    return Some(child);
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&chats) {
        for entry in entries.flatten() {
            let nested = entry.path().join(session_id);
            if cursor_session_archive_present(&nested) {
                return Some(nested);
            }
        }
    }
    None
}

/// ---
/// purpose: 按 pending expected_session_id 捕获唯一 cursor 磁盘存档
/// params:
///   context: 含 expected_session_id 的捕获上下文
/// returns: 命中则一条 FsWatch 候选，否则空向量
/// contract:
///   provides:
///     - name: scan_session_store
///       what: expected id 与目录对齐才捕获
/// boundary:
///   - 无 pending id 不扫描；不拾取既有 hex 会话
/// ---
pub(super) fn scan_session_store(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    let Some(expected) = context.expected_session_id.as_ref() else {
        return Vec::new();
    };
    let Some(dir) = cursor_session_dir(expected.as_str()) else {
        return Vec::new();
    };
    vec![CapturedSessionCandidate {
        captured: CapturedSession {
            session_id: Some(expected.clone()),
            rollout_path: Some(RolloutPath::new(dir)),
            captured_via: CaptureVia::FsWatch,
            attribution_confidence: Confidence::High,
            spawn_cwd: context.spawn_cwd.clone(),
        },
        embedded_agent_id: None,
        positive_agent_id_match: false,
        agent_path_match: false,
    }]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::provider::types::SessionId;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(home: &Path) -> Self {
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", home);
            Self { prev }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn tmp_root(tag: &str) -> PathBuf {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-d115-cursor-scan-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn seed_direct(home: &Path, session_id: &str) -> PathBuf {
        let dir = home.join(".cursor").join("chats").join(session_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("store.db"), b"").unwrap();
        dir
    }

    fn seed_nested(home: &Path, outer: &str, session_id: &str) -> PathBuf {
        let dir = home
            .join(".cursor")
            .join("chats")
            .join(outer)
            .join(session_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"THIS-BODY-MUST-NOT-BE-PARSED\n").unwrap();
        dir
    }

    fn ctx(cwd: &Path, expected: Option<&str>) -> CaptureSessionContext {
        CaptureSessionContext {
            agent_id: "w1".to_string(),
            spawn_cwd: cwd.to_path_buf(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: expected.map(SessionId::new),
            provider_projects_root: None,
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_expected_id_captures_store_db_without_reading_body() {
        let base = tmp_root("hit");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let sid = "00a437742b92089861da7821a62f232a";
        let _guard = HomeGuard::set(&home);
        let dir = seed_direct(&home, sid);
        std::fs::write(dir.join("store.db"), b"THIS-BODY-MUST-NOT-BE-PARSED\n").unwrap();
        let out = scan_session_store(&ctx(&cwd, Some(sid)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].captured.session_id.as_ref().unwrap().as_str(), sid);
        assert_eq!(
            out[0]
                .captured
                .rollout_path
                .as_ref()
                .map(|p| p.as_path().to_path_buf()),
            Some(dir)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_nested_hex_uuid_layout_captures_expected_id() {
        let base = tmp_root("nested");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let outer = "00a437742b92089861da7821a62f232a";
        let sid = "502896a1-72ba-4c53-9a86-b2da28780806";
        let _guard = HomeGuard::set(&home);
        let dir = seed_nested(&home, outer, sid);
        let out = scan_session_store(&ctx(&cwd, Some(sid)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].captured.session_id.as_ref().unwrap().as_str(), sid);
        assert_eq!(
            out[0]
                .captured
                .rollout_path
                .as_ref()
                .map(|p| p.as_path().to_path_buf()),
            Some(dir)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_scan_without_expected_id_does_not_steal_existing_hex() {
        let base = tmp_root("no-pending");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        seed_direct(&home, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let out = scan_session_store(&ctx(&cwd, None));
        assert!(
            out.is_empty(),
            "no pending must not pick existing chats; got {out:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_expected_id_does_not_steal_foreign_chat() {
        let base = tmp_root("foreign");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        seed_direct(&home, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let out = scan_session_store(&ctx(&cwd, Some("cccccccccccccccccccccccccccccccc")));
        assert!(out.is_empty(), "foreign chat must not bind; got {out:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_empty_or_path_id_is_rejected() {
        let base = tmp_root("illegal");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _guard = HomeGuard::set(&home);
        assert!(cursor_session_dir("").is_none());
        assert!(cursor_session_dir("../escape").is_none());
        assert!(cursor_session_dir("a\\b").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_empty_dir_without_marker_is_not_backing() {
        let base = tmp_root("empty");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let sid = "dddddddddddddddddddddddddddddddd";
        let _guard = HomeGuard::set(&home);
        let dir = home.join(".cursor").join("chats").join(sid);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!cursor_session_archive_present(&dir));
        assert!(scan_session_store(&ctx(&cwd, Some(sid))).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
