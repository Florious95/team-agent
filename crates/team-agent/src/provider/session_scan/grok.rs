//! ---
//! purpose: 按 pending uuid 捕获 ~/.grok/sessions/<urlencoded-cwd>/<uuid>/，不读会话正文
//! contract:
//!   provides:
//!     - name: grok_session_dir
//!       what: 由 spawn cwd + session id 派生磁盘存档目录
//!     - name: grok_session_archive_present
//!       what: 只凭 marker 文件存在性判定 backing，不打开正文
//!     - name: scan_session_store
//!       what: expected_session_id 命中存档则返回唯一候选，否则空
//!   requires:
//!     - name: expected_session_id
//!       what: 无 pending id 不扫描、不拾取同 cwd 其它会话
//! boundary:
//!   - 只服务 Provider::Grok
//!   - 不读 chat_history/events 正文，不改 claude/codex/copilot 扫描
//! maturity: wired
//! ---

use std::path::{Path, PathBuf};

use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath};

use super::{CaptureSessionContext, CapturedSessionCandidate};

/// RFC 3986 unreserved set — same as Python `urllib.parse.quote(path, safe='')`.
fn percent_encode_unreserved(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for &byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    out
}

pub(crate) fn grok_session_dir(spawn_cwd: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains('\0')
    {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let canonical = std::fs::canonicalize(spawn_cwd).unwrap_or_else(|_| spawn_cwd.to_path_buf());
    let encoded = percent_encode_unreserved(&canonical.to_string_lossy());
    if encoded.is_empty() {
        return None;
    }
    Some(
        home.join(".grok")
            .join("sessions")
            .join(encoded)
            .join(session_id),
    )
}

pub(crate) fn grok_session_archive_present(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    ["events.jsonl", "chat_history.jsonl", "summary.json"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

pub(super) fn scan_session_store(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    let Some(expected) = context.expected_session_id.as_ref() else {
        return Vec::new();
    };
    let Some(dir) = grok_session_dir(&context.spawn_cwd, expected.as_str()) else {
        return Vec::new();
    };
    if !grok_session_archive_present(&dir) {
        return Vec::new();
    }
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
            "ta-d111-grok-scan-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn seed_archive(home: &Path, cwd: &Path, session_id: &str) -> PathBuf {
        let dir = grok_session_dir(cwd, session_id).expect("dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), b"").unwrap();
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
    fn grok_cwd_encoding_matches_python_quote_empty_safe() {
        assert_eq!(
            percent_encode_unreserved("/private/tmp/ta-t105-repro-83327/ws"),
            "%2Fprivate%2Ftmp%2Fta-t105-repro-83327%2Fws"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn grok_expected_id_captures_archive_without_reading_body() {
        let base = tmp_root("hit");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let sid = "75fce302-0ce9-4635-a788-f4aec11a3f6a";
        let _guard = HomeGuard::set(&home);
        let dir = seed_archive(&home, &cwd, sid);
        std::fs::write(
            dir.join("chat_history.jsonl"),
            b"THIS-BODY-MUST-NOT-BE-PARSED\n",
        )
        .unwrap();
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
    fn grok_expected_id_does_not_steal_foreign_cwd_archive() {
        let base = tmp_root("foreign");
        let home = base.join("home");
        let cwd = base.join("ws");
        let foreign = base.join("other");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        let sid = "4e7b5a6e-e09a-4291-9c75-563107105185";
        let _guard = HomeGuard::set(&home);
        seed_archive(&home, &foreign, sid);
        let out = scan_session_store(&ctx(&cwd, Some(sid)));
        assert!(
            out.is_empty(),
            "foreign cwd archive must not bind; got {out:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn grok_scan_without_expected_id_returns_empty() {
        let base = tmp_root("no-pending");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let _guard = HomeGuard::set(&home);
        seed_archive(&home, &cwd, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        let out = scan_session_store(&ctx(&cwd, None));
        assert!(out.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn grok_scan_empty_dir_without_marker_is_not_backing() {
        let base = tmp_root("empty");
        let home = base.join("home");
        let cwd = base.join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let sid = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let _guard = HomeGuard::set(&home);
        let dir = grok_session_dir(&cwd, sid).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!grok_session_archive_present(&dir));
        assert!(scan_session_store(&ctx(&cwd, Some(sid))).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
