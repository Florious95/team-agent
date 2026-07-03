use std::path::{Path, PathBuf};

use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};

use super::{CaptureSessionContext, CapturedSessionCandidate};

pub(super) fn scan_session_store(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let db_path = home.join(".copilot").join("session-store.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    if let Some(expected) = context.expected_session_id.as_ref() {
        let hit: Option<String> = conn
            .prepare("select id from sessions where id = ?1 limit 1")
            .ok()
            .and_then(|mut stmt| {
                stmt.query_row([expected.as_str()], |row| row.get::<_, String>(0))
                    .ok()
            });
        if let Some(session_id) = hit {
            return vec![copilot_candidate(session_id, &db_path, context)];
        }
        return Vec::new();
    }
    let _ = (&db_path, &conn);
    Vec::new()
}

fn copilot_candidate(
    session_id: String,
    db_path: &Path,
    context: &CaptureSessionContext,
) -> CapturedSessionCandidate {
    CapturedSessionCandidate {
        captured: CapturedSession {
            session_id: Some(SessionId::new(session_id)),
            rollout_path: Some(RolloutPath::new(db_path.to_path_buf())),
            captured_via: CaptureVia::FsWatch,
            attribution_confidence: Confidence::High,
            spawn_cwd: context.spawn_cwd.clone(),
        },
        positive_agent_id_match: false,
        agent_path_match: false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
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
            "ta-e11-copilot-{}-{}-{}",
            tag,
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_copilot_db(home: &Path, rows: &[(&str, &str, i64)]) {
        let dir = home.join(".copilot");
        std::fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("session-store.db")).unwrap();
        conn.execute(
            "create table sessions (id text primary key, cwd text, updated_at integer)",
            [],
        )
        .unwrap();
        for (id, cwd, updated) in rows {
            conn.execute(
                "insert into sessions (id, cwd, updated_at) values (?1, ?2, ?3)",
                rusqlite::params![id, cwd, updated],
            )
            .unwrap();
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_expected_id_wins_over_leader_latest_same_cwd() {
        let base = tmp_root("expected");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let worker_id = "1142c4c2-0000-4000-8000-000000000001";
        let leader_id = "f9c5485d-0000-4000-8000-00000000beef";
        seed_copilot_db(
            &home,
            &[
                (worker_id, &cwd.to_string_lossy(), 100),
                (leader_id, &cwd.to_string_lossy(), 999),
            ],
        );
        let home_guard = HomeGuard::set(&home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new(worker_id)),
            provider_projects_root: None,
        };
        let out = scan_session_store(&ctx);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].captured.session_id.as_ref().unwrap().as_str(),
            worker_id
        );
        drop(home_guard);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_expected_id_absent_in_db_returns_empty_not_leader() {
        let base = tmp_root("absent");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let leader_id = "f9c5485d-0000-4000-8000-00000000beef";
        seed_copilot_db(&home, &[(leader_id, &cwd.to_string_lossy(), 999)]);
        let home_guard = HomeGuard::set(&home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: Some(SessionId::new("1142c4c2-0000-4000-8000-000000000001")),
            provider_projects_root: None,
        };
        let out = scan_session_store(&ctx);
        assert!(out.is_empty());
        drop(home_guard);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[serial_test::serial(env)]
    fn copilot_no_expected_same_cwd_only_leader_row_returns_empty_not_leader() {
        let base = tmp_root("noexp");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = base.join("ws");
        std::fs::create_dir_all(&cwd).unwrap();
        let leader_id = "f9c5485d-0000-4000-8000-00000000beef";
        seed_copilot_db(&home, &[(leader_id, &cwd.to_string_lossy(), 999)]);
        let home_guard = HomeGuard::set(&home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: cwd.clone(),
            pane_id: None,
            pane_pid: None,
            spawned_at: None,
            expected_session_id: None,
            provider_projects_root: None,
        };
        let out = scan_session_store(&ctx);
        assert!(out.is_empty());
        drop(home_guard);
        let _ = std::fs::remove_dir_all(&base);
    }
}
