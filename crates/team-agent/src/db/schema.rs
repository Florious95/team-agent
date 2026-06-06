//! `team.db` schema 初始化(真相源 `message_store/schema.py` + `schema_migration.py`)。
//!
//! slice 1:fresh DB schema —— 8 表 DDL、6 索引、ALTER 式列迁移、agent_health 重建、
//! WAL/busy_timeout pragmas、`user_version = SCHEMA_VERSION`。
//! slice 2(待做):列**顺序**漂移的整表 rebuild(`ensure_table_layout`/`_rebuild_tables`)
//! 与 `schema_diagnosis`,验 legacy_team_db_fixture + schema_migration 契约。

use rusqlite::Connection;

use crate::db::DbError;

/// `schema.py:90`。
pub const SCHEMA_VERSION: i64 = 3;

/// 8 张表的 DDL(逐字照搬 `schema.py:initialize_schema` 的内联建表;含 `if not exists`)。
/// 顺序与 Python 一致(leader_notification_log 在 ensure 块后创建)。
const CREATE_MESSAGES: &str = "create table if not exists messages (\n              message_id text primary key,\n              owner_team_id text,\n              task_id text,\n              sender text,\n              recipient text,\n              reply_to text,\n              requires_ack integer,\n              status text,\n              content text,\n              artifact_refs text,\n              created_at text,\n              updated_at text,\n              delivered_at text,\n              acknowledged_at text,\n              error text,\n              delivery_attempts integer not null default 0\n            )";
const CREATE_RESULTS: &str = "create table if not exists results (\n              result_id text primary key,\n              owner_team_id text,\n              task_id text not null,\n              agent_id text not null,\n              envelope text not null,\n              status text not null,\n              created_at text not null\n            )";
const CREATE_SCHEDULED_EVENTS: &str = "create table if not exists scheduled_events (\n              id integer primary key,\n              owner_team_id text,\n              due_at text not null,\n              target text not null,\n              kind text not null,\n              payload_json text not null,\n              status text not null,\n              created_at text not null,\n              fired_at text,\n              result_json text\n            )";
const CREATE_DELIVERY_TOKENS: &str = "create table if not exists delivery_tokens (\n              message_id text primary key,\n              unique_token text not null,\n              injected_at text not null,\n              visible_at text,\n              consumed_at text,\n              failed_at text,\n              failure_reason text\n            )";
const CREATE_AGENT_HEALTH: &str = "create table if not exists agent_health (\n              owner_team_id text,\n              agent_id text not null,\n              status text not null,\n              last_output_at text,\n              context_usage_pct integer,\n              current_task_id text,\n              updated_at text not null,\n              unique(owner_team_id, agent_id)\n            )";
const CREATE_PEER_ALLOWLIST: &str = "create table if not exists peer_allowlist (\n              a text not null,\n              b text not null,\n              created_at text not null,\n              primary key (a, b)\n            )";
const CREATE_RESULT_WATCHERS: &str = "create table if not exists result_watchers (\n              watcher_id text primary key,\n              owner_team_id text,\n              task_id text,\n              agent_id text,\n              message_id text,\n              leader_id text not null,\n              status text not null,\n              created_at text not null,\n              completed_at text,\n              result_id text,\n              notified_message_id text,\n              error text\n            )";
const CREATE_LEADER_NOTIFICATION_LOG: &str = "create table if not exists leader_notification_log (\n              result_id text not null,\n              owner_team_id text not null default '',\n              owner_epoch integer not null default 0,\n              leader_session_uuid text,\n              notified_message_id text not null,\n              notified_at text not null,\n              leader_pane_id_at_notify text,\n              envelope_content_hash text,\n              primary key (result_id, owner_team_id, owner_epoch)\n            )";
const CREATE_AGENT_HEALTH_NEW: &str = "create table agent_health_new (\n              owner_team_id text,\n              agent_id text not null,\n              status text not null,\n              last_output_at text,\n              context_usage_pct integer,\n              current_task_id text,\n              updated_at text not null,\n              unique(owner_team_id, agent_id)\n            )";

/// `schema_migration.py:INDEX_SQL`(6 条,顺序照搬)。
const INDEX_SQL: &[&str] = &[
    "create index if not exists idx_leader_notification_log_uuid on leader_notification_log(leader_session_uuid, notified_at)",
    "create index if not exists idx_leader_notification_log_team_epoch on leader_notification_log(owner_team_id, owner_epoch, notified_at)",
    "create index if not exists idx_messages_owner_team_id on messages(owner_team_id)",
    "create index if not exists idx_scheduled_events_owner_team_id on scheduled_events(owner_team_id)",
    "create index if not exists idx_agent_health_owner_team_id on agent_health(owner_team_id)",
    "create index if not exists idx_result_watchers_owner_team_id on result_watchers(owner_team_id)",
];

// 各表 required 列(`schema.py` 的 *_COLUMNS 集合;此处用有序数组,成员判定与集合等价)。
const MESSAGE_COLUMNS: &[&str] = &[
    "owner_team_id", "message_id", "task_id", "sender", "recipient", "reply_to", "requires_ack",
    "status", "content", "artifact_refs", "created_at", "updated_at", "delivered_at",
    "acknowledged_at", "error", "delivery_attempts",
];
const RESULT_COLUMNS: &[&str] =
    &["owner_team_id", "result_id", "task_id", "agent_id", "envelope", "status", "created_at"];
const SCHEDULED_EVENT_COLUMNS: &[&str] = &[
    "id", "owner_team_id", "due_at", "target", "kind", "payload_json", "status", "created_at",
    "fired_at", "result_json",
];
const DELIVERY_TOKEN_COLUMNS: &[&str] = &[
    "message_id", "unique_token", "injected_at", "visible_at", "consumed_at", "failed_at",
    "failure_reason",
];
const AGENT_HEALTH_COLUMNS: &[&str] = &[
    "owner_team_id", "agent_id", "status", "last_output_at", "context_usage_pct", "current_task_id",
    "updated_at",
];
const PEER_ALLOWLIST_COLUMNS: &[&str] = &["a", "b", "created_at"];
const RESULT_WATCHER_COLUMNS: &[&str] = &[
    "owner_team_id", "watcher_id", "task_id", "agent_id", "message_id", "leader_id", "status",
    "created_at", "completed_at", "result_id", "notified_message_id", "error",
];
const LEADER_NOTIFICATION_LOG_COLUMNS: &[&str] = &[
    "result_id", "owner_team_id", "owner_epoch", "leader_session_uuid", "notified_message_id",
    "notified_at", "leader_pane_id_at_notify", "envelope_content_hash",
];

/// 打开 `team.db` 并设 pragmas(`core.py:60-61`:busy_timeout=30000 + WAL)。
pub fn open_db(path: &std::path::Path) -> Result<Connection, DbError> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA busy_timeout=30000; PRAGMA journal_mode=WAL;")?;
    Ok(conn)
}

/// `pragma table_info(table)` 的列名序(`schema_migration.py:table_layout`)。
pub fn table_layout(conn: &Connection, table: &str) -> Result<Vec<String>, DbError> {
    // table 来自固定常量名,非用户输入 → format 安全。
    let mut stmt = conn.prepare(&format!("pragma table_info({table})"))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(names)
}

/// `schema_migration.py:ensure_schema_indexes`。
pub fn ensure_schema_indexes(conn: &Connection) -> Result<(), DbError> {
    for sql in INDEX_SQL {
        conn.execute(sql, [])?;
    }
    Ok(())
}

/// `schema.py:_ensure_table_columns`:缺列且无迁移 → `Err`;缺列有迁移 → 按排序执行 ALTER。
fn ensure_table_columns(
    conn: &Connection,
    table: &str,
    required: &[&str],
    migrations: &[(&str, &str)],
) -> Result<(), DbError> {
    let cols = table_layout(conn, table)?;
    let mut missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|r| !cols.iter().any(|c| c == r))
        .collect();
    let unsupported: Vec<&str> = missing
        .iter()
        .copied()
        .filter(|m| !migrations.iter().any(|(k, _)| k == m))
        .collect();
    if !unsupported.is_empty() {
        let mut names = unsupported;
        names.sort_unstable();
        return Err(DbError::Schema(format!(
            "team.db table {table} is missing required column(s): {}",
            names.join(", ")
        )));
    }
    missing.sort_unstable();
    for name in missing {
        if let Some((_, sql)) = migrations.iter().find(|(k, _)| *k == name) {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

/// `schema.py:_migrate_agent_health_owner_team_id`:缺 owner_team_id → 整表重建补 null 列。
fn migrate_agent_health_owner_team_id(conn: &Connection) -> Result<(), DbError> {
    let cols = table_layout(conn, "agent_health")?;
    if !cols.iter().any(|c| c == "owner_team_id") {
        conn.execute(CREATE_AGENT_HEALTH_NEW, [])?;
        conn.execute(
            "insert into agent_health_new(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)\n             select null, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at from agent_health",
            [],
        )?;
        conn.execute("drop table agent_health", [])?;
        conn.execute("alter table agent_health_new rename to agent_health", [])?;
    }
    ensure_table_columns(conn, "agent_health", AGENT_HEALTH_COLUMNS, &[])
}

/// `schema.py:initialize_schema`:建表 + 列迁移 + agent_health 重建 + 索引 + user_version。
/// 一个事务内完成(对应 Python `with conn:`)。
pub fn initialize_schema(conn: &Connection, db_path: Option<&std::path::Path>) -> Result<(), DbError> {
    // Gap 46 入口门(`schema.py:117`):先检测/修复物理列序漂移(legacy DB)。
    // fresh DB 下 no-op(无 managed 表 → 无 diff);随后建表 + 列迁移在自己的事务里。
    crate::db::migration::ensure_table_layout(conn, SCHEMA_VERSION, db_path)?;
    let tx = conn.unchecked_transaction()?;
    for ddl in [
        CREATE_MESSAGES,
        CREATE_RESULTS,
        CREATE_SCHEDULED_EVENTS,
        CREATE_DELIVERY_TOKENS,
        CREATE_AGENT_HEALTH,
        CREATE_PEER_ALLOWLIST,
        CREATE_RESULT_WATCHERS,
    ] {
        tx.execute(ddl, [])?;
    }
    ensure_table_columns(
        &tx,
        "messages",
        MESSAGE_COLUMNS,
        &[
            ("delivery_attempts", "alter table messages add column delivery_attempts integer not null default 0"),
            ("owner_team_id", "alter table messages add column owner_team_id text"),
        ],
    )?;
    ensure_table_columns(&tx, "results", RESULT_COLUMNS, &[("owner_team_id", "alter table results add column owner_team_id text")])?;
    ensure_table_columns(&tx, "scheduled_events", SCHEDULED_EVENT_COLUMNS, &[("owner_team_id", "alter table scheduled_events add column owner_team_id text")])?;
    ensure_table_columns(&tx, "delivery_tokens", DELIVERY_TOKEN_COLUMNS, &[])?;
    migrate_agent_health_owner_team_id(&tx)?;
    ensure_table_columns(&tx, "peer_allowlist", PEER_ALLOWLIST_COLUMNS, &[])?;
    ensure_table_columns(&tx, "result_watchers", RESULT_WATCHER_COLUMNS, &[("owner_team_id", "alter table result_watchers add column owner_team_id text")])?;
    tx.execute(CREATE_LEADER_NOTIFICATION_LOG, [])?;
    ensure_table_columns(&tx, "leader_notification_log", LEADER_NOTIFICATION_LOG_COLUMNS, &[])?;
    ensure_schema_indexes(&tx)?;
    tx.execute_batch(&format!("pragma user_version = {SCHEMA_VERSION}"))?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialize_schema(&conn, None).unwrap();
        conn
    }

    // golden 由 Python initialize_schema 新建 DB 取(team-agent-public@439bef8)。
    #[test]
    fn fresh_schema_matches_python_sqlite_master() {
        let conn = fresh();
        let mut stmt = conn
            .prepare("select name, type from sqlite_master where sql is not null order by name")
            .unwrap();
        let got: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let want: Vec<(&str, &str)> = vec![
            ("agent_health", "table"),
            ("delivery_tokens", "table"),
            ("idx_agent_health_owner_team_id", "index"),
            ("idx_leader_notification_log_team_epoch", "index"),
            ("idx_leader_notification_log_uuid", "index"),
            ("idx_messages_owner_team_id", "index"),
            ("idx_result_watchers_owner_team_id", "index"),
            ("idx_scheduled_events_owner_team_id", "index"),
            ("leader_notification_log", "table"),
            ("messages", "table"),
            ("peer_allowlist", "table"),
            ("result_watchers", "table"),
            ("results", "table"),
            ("scheduled_events", "table"),
        ];
        let got_ref: Vec<(&str, &str)> = got.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
        assert_eq!(got_ref, want);
    }

    #[test]
    fn user_version_is_three() {
        let conn = fresh();
        let v: i64 = conn.query_row("pragma user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(v, 3);
    }

    #[test]
    fn messages_table_layout_matches_python() {
        let conn = fresh();
        assert_eq!(
            table_layout(&conn, "messages").unwrap(),
            vec![
                "message_id", "owner_team_id", "task_id", "sender", "recipient", "reply_to",
                "requires_ack", "status", "content", "artifact_refs", "created_at", "updated_at",
                "delivered_at", "acknowledged_at", "error", "delivery_attempts",
            ]
        );
    }

    #[test]
    fn idempotent_reinit() {
        // initialize_schema 二次调用(if not exists / 无缺列)→ 不报错、schema 不变 + 索引仍在。
        let conn = fresh();
        initialize_schema(&conn, None).unwrap();
        assert_eq!(table_layout(&conn, "messages").unwrap().len(), 16);
        let idx: i64 = conn
            .query_row("select count(*) from sqlite_master where type='index' and sql is not null", [], |r| r.get(0))
            .unwrap();
        assert_eq!(idx, 6, "二次 init 后 6 个 named index 仍在");
    }

    // 全 8 表逐列 (name, type, notnull, dflt, pk) golden(Python initialize_schema table_info)。
    #[test]
    fn per_column_table_info_all_eight_tables() {
        type Col = (&'static str, &'static str, i64, Option<&'static str>, i64);
        let golden: &[(&str, &[Col])] = &[
            ("messages", &[
                ("message_id","TEXT",0,None,1),("owner_team_id","TEXT",0,None,0),("task_id","TEXT",0,None,0),
                ("sender","TEXT",0,None,0),("recipient","TEXT",0,None,0),("reply_to","TEXT",0,None,0),
                ("requires_ack","INTEGER",0,None,0),("status","TEXT",0,None,0),("content","TEXT",0,None,0),
                ("artifact_refs","TEXT",0,None,0),("created_at","TEXT",0,None,0),("updated_at","TEXT",0,None,0),
                ("delivered_at","TEXT",0,None,0),("acknowledged_at","TEXT",0,None,0),("error","TEXT",0,None,0),
                ("delivery_attempts","INTEGER",1,Some("0"),0),
            ]),
            ("results", &[
                ("result_id","TEXT",0,None,1),("owner_team_id","TEXT",0,None,0),("task_id","TEXT",1,None,0),
                ("agent_id","TEXT",1,None,0),("envelope","TEXT",1,None,0),("status","TEXT",1,None,0),("created_at","TEXT",1,None,0),
            ]),
            ("scheduled_events", &[
                ("id","INTEGER",0,None,1),("owner_team_id","TEXT",0,None,0),("due_at","TEXT",1,None,0),
                ("target","TEXT",1,None,0),("kind","TEXT",1,None,0),("payload_json","TEXT",1,None,0),
                ("status","TEXT",1,None,0),("created_at","TEXT",1,None,0),("fired_at","TEXT",0,None,0),("result_json","TEXT",0,None,0),
            ]),
            ("delivery_tokens", &[
                ("message_id","TEXT",0,None,1),("unique_token","TEXT",1,None,0),("injected_at","TEXT",1,None,0),
                ("visible_at","TEXT",0,None,0),("consumed_at","TEXT",0,None,0),("failed_at","TEXT",0,None,0),("failure_reason","TEXT",0,None,0),
            ]),
            ("agent_health", &[
                ("owner_team_id","TEXT",0,None,0),("agent_id","TEXT",1,None,0),("status","TEXT",1,None,0),
                ("last_output_at","TEXT",0,None,0),("context_usage_pct","INTEGER",0,None,0),("current_task_id","TEXT",0,None,0),("updated_at","TEXT",1,None,0),
            ]),
            ("peer_allowlist", &[
                ("a","TEXT",1,None,1),("b","TEXT",1,None,2),("created_at","TEXT",1,None,0),
            ]),
            ("result_watchers", &[
                ("watcher_id","TEXT",0,None,1),("owner_team_id","TEXT",0,None,0),("task_id","TEXT",0,None,0),
                ("agent_id","TEXT",0,None,0),("message_id","TEXT",0,None,0),("leader_id","TEXT",1,None,0),
                ("status","TEXT",1,None,0),("created_at","TEXT",1,None,0),("completed_at","TEXT",0,None,0),
                ("result_id","TEXT",0,None,0),("notified_message_id","TEXT",0,None,0),("error","TEXT",0,None,0),
            ]),
            ("leader_notification_log", &[
                ("result_id","TEXT",1,None,1),("owner_team_id","TEXT",1,Some("''"),2),("owner_epoch","INTEGER",1,Some("0"),3),
                ("leader_session_uuid","TEXT",0,None,0),("notified_message_id","TEXT",1,None,0),("notified_at","TEXT",1,None,0),
                ("leader_pane_id_at_notify","TEXT",0,None,0),("envelope_content_hash","TEXT",0,None,0),
            ]),
        ];
        let conn = fresh();
        for (table, cols) in golden {
            let mut stmt = conn.prepare(&format!("pragma table_info({table})")).unwrap();
            let got: Vec<(String, String, i64, Option<String>, i64)> = stmt
                .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let got_ref: Vec<(&str, &str, i64, Option<&str>, i64)> = got
                .iter()
                .map(|(n, t, nn, d, pk)| (n.as_str(), t.as_str(), *nn, d.as_deref(), *pk))
                .collect();
            assert_eq!(&got_ref[..], *cols, "table_info mismatch for {table}");
        }
    }

    #[test]
    fn named_indexes_and_autoindexes_match_python() {
        let conn = fresh();
        let mut stmt = conn.prepare("select name, sql from sqlite_master where type='index' and sql is not null order by name").unwrap();
        let idx: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap().collect::<Result<_, _>>().unwrap();
        let want: Vec<(&str, &str)> = vec![
            ("idx_agent_health_owner_team_id", "CREATE INDEX idx_agent_health_owner_team_id on agent_health(owner_team_id)"),
            ("idx_leader_notification_log_team_epoch", "CREATE INDEX idx_leader_notification_log_team_epoch on leader_notification_log(owner_team_id, owner_epoch, notified_at)"),
            ("idx_leader_notification_log_uuid", "CREATE INDEX idx_leader_notification_log_uuid on leader_notification_log(leader_session_uuid, notified_at)"),
            ("idx_messages_owner_team_id", "CREATE INDEX idx_messages_owner_team_id on messages(owner_team_id)"),
            ("idx_result_watchers_owner_team_id", "CREATE INDEX idx_result_watchers_owner_team_id on result_watchers(owner_team_id)"),
            ("idx_scheduled_events_owner_team_id", "CREATE INDEX idx_scheduled_events_owner_team_id on scheduled_events(owner_team_id)"),
        ];
        let got: Vec<(&str, &str)> = idx.iter().map(|(n, s)| (n.as_str(), s.as_str())).collect();
        assert_eq!(got, want);
        // 7 个 sqlite_autoindex(每个有 pk/unique 约束的表各一)。
        let auto: Vec<String> = conn
            .prepare("select name from sqlite_master where type='index' and sql is null order by name").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(auto, vec![
            "sqlite_autoindex_agent_health_1", "sqlite_autoindex_delivery_tokens_1",
            "sqlite_autoindex_leader_notification_log_1", "sqlite_autoindex_messages_1",
            "sqlite_autoindex_peer_allowlist_1", "sqlite_autoindex_result_watchers_1", "sqlite_autoindex_results_1",
        ]);
    }

    #[test]
    fn open_db_sets_wal_and_busy_timeout() {
        // 必须用文件路径(in-memory 的 journal_mode 是 'memory')。
        let dir = std::env::temp_dir().join(format!("ta_rs_pragma_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.db");
        let _ = std::fs::remove_file(&path);
        let conn = open_db(&path).unwrap();
        let jm: String = conn.query_row("pragma journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(jm.to_lowercase(), "wal");
        let bt: i64 = conn.query_row("pragma busy_timeout", [], |r| r.get(0)).unwrap();
        assert_eq!(bt, 30000);
    }

    #[test]
    fn ensure_table_columns_unsupported_missing_errors_with_sorted_names() {
        // results 只建了 result_id → 缺 owner_team_id(有 migration)+ 其余(无 migration,unsupported)。
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("create table results (result_id text primary key)", []).unwrap();
        let err = ensure_table_columns(
            &conn,
            "results",
            &["owner_team_id", "result_id", "task_id", "agent_id", "envelope", "status", "created_at"],
            &[("owner_team_id", "alter table results add column owner_team_id text")],
        ).unwrap_err();
        assert_eq!(
            err.to_string(),
            "schema: team.db table results is missing required column(s): agent_id, created_at, envelope, status, task_id"
        );
    }
}
