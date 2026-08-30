//! ---
//! purpose: 只按 pending session id 到 copilot 的 session-store.db 里查一行，查得到才算捕获
//! contract:
//!   provides:
//!     - name: scan_session_store
//!       what: 有 expected_session_id 且 sessions 表里存在该 id 时返回唯一候选，否则空
//!   requires:
//!     - name: crate::provider::adapters::copilot_fork::copilot_home
//!       what: session-store.db 的位置由 copilot adapter 决定，本文件不自行推导 HOME
//!     - name: rusqlite
//!       what: 只读打开 sqlite，不建表、不写入
//! boundary:
//!   - 只服务 Provider::Copilot
//!   - 无 pending id 一律返回空——绝不按 cwd/时间挑「最近那个会话」
//!   - 候选的 rollout_path 是共享的 session-store.db 本身，不是每席位一份的文件
//!   - embedded_agent_id 恒 None:copilot 存档里没有 team-agent 身份 marker 可读
//!   - 打不开 HOME / 库不存在 / SQL 失败一律退成空向量，不上抛错误
//! maturity: wired
//! ---
use std::path::Path;

use crate::provider::types::{CaptureVia, CapturedSession, Confidence, RolloutPath, SessionId};

use super::{CaptureSessionContext, CapturedSessionCandidate};

/// ---
/// purpose: 按 pending id 在 copilot session-store.db 的 sessions 表里做一次存在性查询
/// params:
///   context: 只用到 expected_session_id 与 spawn_cwd(后者仅原样写进候选)
/// returns: 命中给一条 FsWatch/High 候选，rollout_path 指向 session-store.db;未命中或无 pending id 给空向量
/// contract:
///   provides:
///     - name: scan_session_store
///       what: 只读 sqlite 一行 id，不读会话正文、不按 cwd 过滤
/// boundary:
///   - 无 expected_session_id 直接返回空，不做任何回落挑选
///   - 不校验该会话的 cwd 是否等于 spawn_cwd——库里 cwd 列存在但本函数不用
///   - 所有失败路径都退成空向量,调用方无法区分「库不存在」与「查无此行」
/// ---
pub(super) fn scan_session_store(context: &CaptureSessionContext) -> Vec<CapturedSessionCandidate> {
    let Ok(home) = crate::provider::adapters::copilot_fork::copilot_home() else {
        return Vec::new();
    };
    let db_path = home.join("session-store.db");
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
        embedded_agent_id: None,
        positive_agent_id_match: false,
        agent_path_match: false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;
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
    fn copilot_scanner_respects_copilot_home() {
        let base = tmp_root("custom-home");
        let copilot_home = base.join("isolated-copilot");
        std::fs::create_dir_all(&copilot_home).unwrap();
        let worker_id = "1142c4c2-0000-4000-8000-000000000001";
        let conn = rusqlite::Connection::open(copilot_home.join("session-store.db")).unwrap();
        conn.execute(
            "create table sessions (id text primary key, cwd text, updated_at integer)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into sessions (id, cwd, updated_at) values (?1, '/tmp/ws', 1)",
            [worker_id],
        )
        .unwrap();
        drop(conn);
        let previous = std::env::var_os("COPILOT_HOME");
        std::env::set_var("COPILOT_HOME", &copilot_home);
        let ctx = CaptureSessionContext {
            agent_id: "worker".to_string(),
            spawn_cwd: base.join("ws"),
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
        match previous {
            Some(value) => std::env::set_var("COPILOT_HOME", value),
            None => std::env::remove_var("COPILOT_HOME"),
        }
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
