//! Compatibility namespace for explicit offline legacy schema tooling.
pub use crate::importer::migration::*;
use std::path::{Path, PathBuf};
use rusqlite::{Connection, OpenFlags};
use crate::db::schema::{ensure_schema_indexes, table_layout, SCHEMA_VERSION};
use crate::db::DbError;



#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::db::schema::initialize_schema;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_db() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let db = std::env::temp_dir()
            .join(format!("ta_rs_db_{}_{}", std::process::id(), n))
            .join(".team")
            .join("runtime")
            .join("team.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        db
    }

    /// 造 legacy DB:4 表 owner_team_id 在末列(列序漂移),各 2 行,user_version=1。
    fn build_legacy(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "create table messages (message_id text primary key, task_id text, sender text, recipient text, reply_to text, requires_ack integer, status text, content text, artifact_refs text, created_at text, updated_at text, delivered_at text, acknowledged_at text, error text, delivery_attempts integer not null default 0, owner_team_id text);
             create table results (result_id text primary key, task_id text not null, agent_id text not null, envelope text not null, status text not null, created_at text not null, owner_team_id text);
             create table scheduled_events (id integer primary key, due_at text not null, target text not null, kind text not null, payload_json text not null, status text not null, created_at text not null, fired_at text, result_json text, owner_team_id text);
             create table agent_health (agent_id text not null, status text not null, last_output_at text, context_usage_pct integer, current_task_id text, updated_at text not null, owner_team_id text);",
        ).unwrap();
        conn.execute("insert into messages(message_id, status, owner_team_id) values ('m1','sent','t'),('m2','sent','t')", []).unwrap();
        conn.execute("insert into results(result_id, task_id, agent_id, envelope, status, created_at, owner_team_id) values ('r1','t1','a','{}','success','now','t'),('r2','t2','a','{}','success','now','t')", []).unwrap();
        conn.execute("insert into scheduled_events(due_at,target,kind,payload_json,status,created_at) values ('d','x','k','{}','pending','now'),('d','x','k','{}','pending','now')", []).unwrap();
        conn.execute("insert into agent_health(agent_id,status,updated_at,owner_team_id) values ('a','idle','now','t'),('b','idle','now','t')", []).unwrap();
        conn.execute_batch("pragma user_version = 1").unwrap();
    }

    fn layout(conn: &Connection, t: &str) -> Vec<String> {
        table_layout(conn, t).unwrap()
    }

    // golden:Python build_legacy_workspace + fix_schema_layout(team-agent-public@439bef8)。
    #[test]
    fn legacy_db_migrates_to_canonical_layout_preserving_rows() {
        let path = temp_db();
        build_legacy(&path);

        // 迁移前:diagnosis = schema_repair_available,全 7 表 diff(4 漂移 + 3 缺失)。
        let before = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert_eq!(before.status, "schema_repair_available");
        assert!(!before.ok);
        assert_eq!(before.user_version, 1);
        assert_eq!(before.layout_diffs.len(), MANAGED_TABLE_LAYOUTS.len());

        // initialize_schema 走 ensure_table_layout(rebuild)+ 建表 + user_version=3。
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();

        // 迁移后:4 表 canonical 列序 + 行数保全。
        assert_eq!(
            layout(&conn, "messages")[..3],
            ["message_id", "owner_team_id", "task_id"]
        );
        assert_eq!(
            layout(&conn, "agent_health")[..2],
            ["owner_team_id", "agent_id"]
        );
        assert_eq!(table_count(&conn, "messages").unwrap(), 2);
        assert_eq!(table_count(&conn, "results").unwrap(), 2);
        assert_eq!(table_count(&conn, "scheduled_events").unwrap(), 2);
        assert_eq!(table_count(&conn, "agent_health").unwrap(), 2);
        // 行内容保全(owner_team_id 迁移后仍可读)。
        let m1: String = conn
            .query_row(
                "select owner_team_id from messages where message_id='m1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m1, "t");
        drop(conn);

        // 迁移后 diagnosis = ok / current user_version。
        let after = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert!(after.ok);
        assert_eq!(after.status, "ok");
        assert_eq!(after.user_version, SCHEMA_VERSION);

        // 备份文件已写(team.db.pre-migration-*-from-v1.bak)。
        let runtime = path.parent().unwrap();
        let baks: Vec<_> = std::fs::read_dir(runtime)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("team.db.pre-migration-") && n.ends_with("-from-v1.bak")
            })
            .collect();
        assert_eq!(baks.len(), 1, "应有且仅有一个 from-v1 备份");
    }

    #[test]
    fn fresh_db_no_diffs_and_diagnosis_ok() {
        let path = temp_db();
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap(); // fresh:ensure_table_layout no-op
        assert!(layout_diffs(&conn).unwrap().is_empty());
        drop(conn);
        let d = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert!(d.ok);
        assert_eq!(d.status, "ok");
        assert_eq!(d.user_version, SCHEMA_VERSION);
    }

    // A drifted legacy peer table is inert: no diagnosis, rebuild, backup or row loss.
    #[test]
    fn legacy_peer_allowlist_is_inert_and_preserved() {
        let path = temp_db();
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();
        conn.execute_batch(
            "create table peer_allowlist (created_at text not null, b text not null, a text not null, primary key (a, b));
             insert into peer_allowlist(a, b, created_at) values ('worker', 'reviewer', 'v4');",
        )
        .unwrap();
        assert!(layout_diffs(&conn).unwrap().is_empty());
        initialize_schema(&conn, Some(&path)).unwrap();
        assert_eq!(layout(&conn, "peer_allowlist"), ["created_at", "b", "a"]);
        let row: (String, String, String) = conn
            .query_row("select a, b, created_at from peer_allowlist", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(
            row,
            (
                "worker".to_string(),
                "reviewer".to_string(),
                "v4".to_string()
            )
        );
        drop(conn);
        let diagnosis = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert!(
            diagnosis.ok,
            "legacy peer table must remain inert: {diagnosis:?}"
        );
        assert!(
            !std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .any(|entry| entry.file_name().to_string_lossy().contains("pre-migration")),
            "legacy peer table must not trigger a migration backup"
        );
    }

    #[test]
    fn diagnosis_missing_db() {
        let path = temp_db(); // 未创建文件
        let d = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert!(d.ok);
        assert_eq!(d.status, "missing");
        assert_eq!(d.user_version, 0);
    }

    #[test]
    fn rebuild_preserves_row_count_invariant() {
        // 再迁移一次(已 canonical)→ 无 diff → 无 rebuild。
        let path = temp_db();
        build_legacy(&path);
        let conn = crate::db::schema::open_db(&path).unwrap();
        let events1 = ensure_table_layout(&conn, SCHEMA_VERSION, Some(&path)).unwrap();
        assert_eq!(events1.len(), MANAGED_TABLE_LAYOUTS.len()); // 4 漂移 rebuild + 3 缺失建表
        for e in &events1 {
            assert_eq!(e.row_count_before, e.row_count_after);
        }
        let events2 = ensure_table_layout(&conn, SCHEMA_VERSION, Some(&path)).unwrap();
        assert!(events2.is_empty(), "已 canonical,二次 ensure 无 rebuild");
    }

    // 对抗检查 HIGH:崩溃原子性 —— insert 后、rename 前注入故障,验事务回滚到迁移前态、无半成品。
    #[test]
    fn crash_after_insert_rolls_back_to_pre_migration_state() {
        let path = temp_db();
        build_legacy(&path);
        let conn = crate::db::schema::open_db(&path).unwrap();
        FAULT_AFTER_INSERT.with(|c| c.set(true));
        let r = ensure_table_layout(&conn, SCHEMA_VERSION, Some(&path));
        FAULT_AFTER_INSERT.with(|c| c.set(false));
        assert!(r.is_err(), "注入故障后 ensure_table_layout 必须 Err");
        // 重开 DB:必须是迁移前态 —— messages 仍 owner_team_id 末列,无残留 temp/old 表,行数不变。
        drop(conn);
        let conn2 = Connection::open(&path).unwrap();
        assert_eq!(
            table_layout(&conn2, "messages")
                .unwrap()
                .last()
                .map(String::as_str),
            Some("owner_team_id"),
            "回滚后 messages 应仍是 legacy 布局(owner_team_id 末列)"
        );
        let residue: i64 = conn2
            .query_row(
                "select count(*) from sqlite_master where type='table' and (name like '__team_agent_rebuild_%' or name like '__team_agent_old_%')",
                [], |r| r.get(0),
            )
            .unwrap();
        assert_eq!(residue, 0, "不得残留 rebuild/old 临时表");
        assert_eq!(table_count(&conn2, "messages").unwrap(), 2);
    }

    // db_path=None + 文件 conn + 有 diff → 经 pragma database_list 回退解析路径并成功(修复的 divergence)。
    #[test]
    fn ensure_table_layout_none_db_path_falls_back_to_conn_path() {
        let path = temp_db();
        build_legacy(&path);
        let conn = crate::db::schema::open_db(&path).unwrap();
        let events = ensure_table_layout(&conn, SCHEMA_VERSION, None).unwrap(); // None 不再 Err
        assert_eq!(events.len(), MANAGED_TABLE_LAYOUTS.len());
        assert_eq!(
            table_layout(&conn, "messages").unwrap()[..2],
            ["message_id", "owner_team_id"]
        );
    }

    // agent_health 完全缺 owner_team_id 列 → generic rebuild 补 NULL 列、行保全。
    #[test]
    fn agent_health_missing_owner_team_id_column_rebuilds_with_null() {
        let path = temp_db();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "create table agent_health (agent_id text not null, status text not null, last_output_at text, context_usage_pct integer, current_task_id text, updated_at text not null, unique(agent_id));
             insert into agent_health(agent_id,status,updated_at) values ('a','idle','now');
             pragma user_version = 1;",
        ).unwrap();
        drop(c);
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();
        assert_eq!(
            table_layout(&conn, "agent_health").unwrap()[0],
            "owner_team_id"
        );
        assert_eq!(table_count(&conn, "agent_health").unwrap(), 1);
        let owner: Option<String> = conn
            .query_row(
                "select owner_team_id from agent_health where agent_id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner, None, "补的 owner_team_id 列应为 NULL");
    }

    // legacy 多出 expected 之外的废列 → rebuild 丢弃(common = expected ∩ actual)。
    #[test]
    fn extra_legacy_column_is_dropped_on_rebuild() {
        let path = temp_db();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(&path).unwrap();
        // messages canonical 列 + 末尾一个废列 legacy_junk → 触发列序 diff。
        c.execute_batch(
            "create table messages (message_id text primary key, owner_team_id text, task_id text, sender text, recipient text, reply_to text, requires_ack integer, status text, content text, artifact_refs text, created_at text, updated_at text, delivered_at text, acknowledged_at text, error text, delivery_attempts integer not null default 0, legacy_junk text);
             insert into messages(message_id, legacy_junk) values ('m1','junk');
             pragma user_version = 1;",
        ).unwrap();
        drop(c);
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();
        let cols = table_layout(&conn, "messages").unwrap();
        assert!(!cols.iter().any(|c| c == "legacy_junk"), "废列应被丢弃");
        assert_eq!(cols.len(), 17);
        assert_eq!(table_count(&conn, "messages").unwrap(), 1);
    }

    // unicode / NULL 行内容经 rebuild 字节保全。
    #[test]
    fn unicode_and_null_content_preserved_through_rebuild() {
        let path = temp_db();
        build_legacy(&path); // messages 漂移(owner_team_id 末列)
        let c = Connection::open(&path).unwrap();
        c.execute("insert into messages(message_id, content, status, owner_team_id) values ('u1', 'héllo🦀\n世界', NULL, 't')", []).unwrap();
        drop(c);
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();
        let (content, status): (String, Option<String>) = conn
            .query_row(
                "select content, status from messages where message_id='u1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(content, "héllo🦀\n世界");
        assert_eq!(status, None);
    }

    #[test]
    fn fix_schema_layout_missing_blocked_fixed() {
        // missing:无 db。
        let ws_missing = temp_db()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        match fix_schema_layout(&ws_missing, SCHEMA_VERSION).unwrap() {
            FixResult::Missing(d) => assert_eq!(d.status, "missing"),
            other => panic!("expected Missing, got {other:?}"),
        }

        // fixed:legacy db。workspace = .../<dir>(db 在 ws/.team/runtime/team.db)。
        let db = temp_db();
        build_legacy(&db);
        let ws = db
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        match fix_schema_layout(&ws, SCHEMA_VERSION).unwrap() {
            FixResult::Fixed {
                diagnosis,
                rebuilds,
            } => {
                assert!(diagnosis.ok);
                assert_eq!(diagnosis.user_version, SCHEMA_VERSION);
                assert_eq!(rebuilds.len(), MANAGED_TABLE_LAYOUTS.len());
            }
            other => panic!("expected Fixed, got {other:?}"),
        }

        // blocked:另一个连接持 BEGIN IMMEDIATE 写锁时 fix 应拒绝(不写备份)。
        let db2 = temp_db();
        build_legacy(&db2);
        let ws2 = db2
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let holder = Connection::open(&db2).unwrap();
        holder
            .busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap(); // 占写锁
        match fix_schema_layout(&ws2, SCHEMA_VERSION).unwrap() {
            FixResult::Blocked { reason } => assert_eq!(reason, "active_lock"),
            other => panic!("expected Blocked, got {other:?}"),
        }
        // 撞锁时不应写任何备份。
        let baks = std::fs::read_dir(db2.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("pre-migration"))
            .count();
        assert_eq!(baks, 0, "blocked 路径不得写备份");
        holder.execute_batch("ROLLBACK").unwrap();
    }

    // step 3↔4 集成:rebuild 后 events.jsonl 发射每表一条 schema.layout_rebuild(row_count 前后相等)。
    #[test]
    fn rebuild_emits_schema_layout_rebuild_events() {
        let path = temp_db(); // <ws>/.team/runtime/team.db
        build_legacy(&path);
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();
        let ws = path.parent().unwrap().parent().unwrap().parent().unwrap();
        let events = crate::event_log::EventLog::new(ws).tail(50).unwrap();
        let rebuilds: Vec<_> = events
            .iter()
            .filter(|e| e["event"] == serde_json::json!("schema.layout_rebuild"))
            .collect();
        assert_eq!(
            rebuilds.len(),
            MANAGED_TABLE_LAYOUTS.len(),
            "每张 managed 表各一条 schema.layout_rebuild"
        );
        for e in &rebuilds {
            assert_eq!(
                e["row_count_before"], e["row_count_after"],
                "事件须 row_count 前后相等"
            );
            assert!(e["backup_path"].as_str().is_some());
        }
        // 含 messages 那条,to_layout_columns 是 canonical。
        let m = rebuilds
            .iter()
            .find(|e| e["table"] == serde_json::json!("messages"))
            .unwrap();
        assert_eq!(
            m["to_layout_columns"][1],
            serde_json::json!("owner_team_id")
        );
    }
}
