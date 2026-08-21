//! ---
//! purpose: 捕获 ~/.cursor/chats/ 存档：pending chatId 或 workspace hex 下唯一新目录
//! contract:
//!   provides:
//!     - name: cursor_session_dir
//!       what: 由 HOME + chatId 派生磁盘存档目录（直达或一层 hex 子目录）
//!     - name: cursor_session_archive_present
//!       what: 只凭 marker 文件存在性判定 backing，不打开正文
//!     - name: scan_session_store
//!       what: pending 命中则返回该 id；否则仅当 spawn_cwd 的 md5 hex 下唯一新 marker 目录才捕获
//!   requires:
//!     - name: workspace-hex-or-pending
//!       what: 无 pending 时只看 ~/.cursor/chats/<md5(canonical cwd)>/，不扫全库
//! boundary:
//!   - 只服务 Provider::CursorAgent
//!   - 不发明 --session-id；不读 store.db/meta.json 正文
//!   - 0 或 ≥2 个新目录 → 空向量，不拾取邻席
//!   - 不改 grok/claude/codex/copilot 扫描
//! maturity: wired
//! ---

use std::path::{Path, PathBuf};

use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};

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
/// purpose: 由 spawn_cwd 的 md5 hex + chatId 直达存档，避免全库枚举失败
/// params:
///   session_id: CLI chatId；spawn_cwd: 席位物理 workspace
/// returns: hex/uuid 含 marker 则该目录，否则回落 cursor_session_dir
/// contract:
///   provides:
///     - name: cursor_session_dir_for_cwd
///       what: 优先 ~/.cursor/chats/<md5(cwd)>/<chatId>/，再全库回落
/// boundary:
///   - 不读正文；id 非法则 None
/// ---
pub(crate) fn cursor_session_dir_for_cwd(session_id: &str, spawn_cwd: &Path) -> Option<PathBuf> {
    if !session_id_legal(session_id) {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let hex = workspace_chats_hex(spawn_cwd);
    let nested = home
        .join(".cursor")
        .join("chats")
        .join(hex)
        .join(session_id);
    if cursor_session_archive_present(&nested) {
        return Some(nested);
    }
    cursor_session_dir(session_id)
}

fn candidate_for(
    session_id: SessionId,
    dir: PathBuf,
    spawn_cwd: PathBuf,
) -> CapturedSessionCandidate {
    CapturedSessionCandidate {
        captured: CapturedSession {
            session_id: Some(session_id),
            rollout_path: Some(RolloutPath::new(dir)),
            captured_via: CaptureVia::FsWatch,
            attribution_confidence: Confidence::High,
            spawn_cwd,
        },
        embedded_agent_id: None,
        positive_agent_id_match: false,
        agent_path_match: false,
    }
}

fn workspace_chats_hex(spawn_cwd: &Path) -> String {
    let canonical = std::fs::canonicalize(spawn_cwd).unwrap_or_else(|_| spawn_cwd.to_path_buf());
    format!("{:x}", md5::compute(canonical.to_string_lossy().as_bytes()))
}

fn dir_mtime_passes(dir: &Path, spawned_at: Option<&str>) -> bool {
    let Some(raw) = spawned_at else {
        return true;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) else {
        return true;
    };
    let threshold = parsed.with_timezone(&chrono::Utc) - chrono::Duration::seconds(2);
    let Ok(meta) = dir.metadata() else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    chrono::DateTime::<chrono::Utc>::from(mtime) >= threshold
}

/// ---
/// purpose: 无 pending 时按 spawn_cwd 的 md5 hex 捕获唯一新 cursor 存档
/// params:
///   context: spawn_cwd + 可选 spawned_at
/// returns: hex 下恰好一个通过时间窗的 marker 目录则一条候选，否则空
/// contract:
///   provides:
///     - name: scan_unique_workspace_chat
///       what: 只枚举 ~/.cursor/chats/<md5(cwd)>/，0 或 ≥2 不拾取
/// boundary:
///   - 不扫其它 hex；不读 marker 正文
/// ---
fn scan_unique_workspace_chat(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let hex = workspace_chats_hex(&context.spawn_cwd);
    if hex.len() != 32 {
        return Vec::new();
    }
    let root = home.join(".cursor").join("chats").join(&hex);
    if !root.is_dir() {
        return Vec::new();
    }
    let mut hits: Vec<(String, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if !child.is_dir() {
            continue;
        }
        let Some(name) = child.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !session_id_legal(name) {
            continue;
        }
        if !cursor_session_archive_present(&child) {
            continue;
        }
        if !dir_mtime_passes(&child, context.spawned_at.as_deref()) {
            continue;
        }
        hits.push((name.to_string(), child));
    }
    if hits.len() != 1 {
        return Vec::new();
    }
    let (sid, dir) = hits.remove(0);
    vec![candidate_for(
        SessionId::new(sid),
        dir,
        context.spawn_cwd.clone(),
    )]
}

/// ---
/// purpose: 按 pending chatId 或 workspace hex 唯一新目录捕获 cursor 存档
/// params:
///   context: 含 spawn_cwd；可选 expected_session_id / spawned_at
/// returns: 命中则一条 FsWatch 候选，否则空向量
/// contract:
///   provides:
///     - name: scan_session_store
///       what: pending 优先；否则唯一新 hex 目录
/// boundary:
///   - 非唯一不拾取既有 hex 会话
/// ---
pub(super) fn scan_session_store(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    if let Some(expected) = context.expected_session_id.as_ref() {
        let Some(dir) = cursor_session_dir(expected.as_str()) else {
            return Vec::new();
        };
        return vec![candidate_for(
            expected.clone(),
            dir,
            context.spawn_cwd.clone(),
        )];
    }
    scan_unique_workspace_chat(context)
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
    fn cursor_unique_workspace_hex_dir_captures_without_pending() {
        let base = tmp_root("unique-hex");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = std::fs::canonicalize(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        let sid = "09d2aeaa-33ef-4148-a2e6-83a7c3775caa";
        let hex = workspace_chats_hex(&cwd);
        assert_eq!(hex.len(), 32);
        let dir = seed_nested(&home, &hex, sid);
        let out = scan_session_store(&ctx(&cwd, None));
        assert_eq!(out.len(), 1, "unique hex uuid must capture; got {out:?}");
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
    fn cursor_two_workspace_hex_dirs_do_not_steal() {
        let base = tmp_root("two-hex");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = std::fs::canonicalize(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        let hex = workspace_chats_hex(&cwd);
        seed_nested(&home, &hex, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        seed_nested(&home, &hex, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
        let out = scan_session_store(&ctx(&cwd, None));
        assert!(
            out.is_empty(),
            "two marker dirs under cwd hex must not bind; got {out:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_session_dir_for_cwd_hits_hex_layout() {
        let base = tmp_root("for-cwd");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = std::fs::canonicalize(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        let sid = "09d2aeaa-33ef-4148-a2e6-83a7c3775caa";
        let hex = workspace_chats_hex(&cwd);
        let dir = seed_nested(&home, &hex, sid);
        assert_eq!(
            cursor_session_dir_for_cwd(sid, &cwd).as_deref(),
            Some(dir.as_path())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn cursor_foreign_hex_is_ignored_when_cwd_hex_empty() {
        let base = tmp_root("foreign-hex");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = std::fs::canonicalize(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        seed_nested(
            &home,
            "ffffffffffffffffffffffffffffffff",
            "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        );
        let out = scan_session_store(&ctx(&cwd, None));
        assert!(
            out.is_empty(),
            "foreign hex must not bind empty cwd hex; got {out:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
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
