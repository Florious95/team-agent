//! `team.db` 列布局迁移(Gap 46;真相源 `message_store/schema_migration.py`)。
//!
//! 检测**物理列序漂移**(`pragma table_info` 顺序 vs canonical),在**一个原子事务**
//! (`BEGIN IMMEDIATE`)内整表 rebuild(temp 复制 → rename → drop),保证「崩溃只留迁移前
//! 或迁移后,绝不半成品」(事务回滚提供该不变量)。rebuild 前先写
//! `team.db.pre-migration-<utc>-from-v<N>.bak` 备份。行数前后必须不变(否则报错)。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::db::schema::{ensure_schema_indexes, table_layout, SCHEMA_VERSION};
use crate::db::DbError;

/// canonical 列序(`schema_migration.py:MANAGED_TABLE_LAYOUTS`)。
pub const MANAGED_TABLE_LAYOUTS: &[(&str, &[&str])] = &[
    ("messages", &[
        "message_id", "owner_team_id", "task_id", "sender", "recipient", "reply_to", "requires_ack",
        "status", "content", "artifact_refs", "created_at", "updated_at", "delivered_at",
        "acknowledged_at", "error", "delivery_attempts",
    ]),
    ("results", &["result_id", "owner_team_id", "task_id", "agent_id", "envelope", "status", "created_at"]),
    ("scheduled_events", &[
        "id", "owner_team_id", "due_at", "target", "kind", "payload_json", "status", "created_at",
        "fired_at", "result_json",
    ]),
    ("delivery_tokens", &[
        "message_id", "unique_token", "injected_at", "visible_at", "consumed_at", "failed_at",
        "failure_reason",
    ]),
    ("agent_health", &[
        "owner_team_id", "agent_id", "status", "last_output_at", "context_usage_pct", "current_task_id",
        "updated_at",
    ]),
    ("peer_allowlist", &["a", "b", "created_at"]),
    ("result_watchers", &[
        "watcher_id", "owner_team_id", "task_id", "agent_id", "message_id", "leader_id", "status",
        "created_at", "completed_at", "result_id", "notified_message_id", "error",
    ]),
    ("leader_notification_log", &[
        "result_id", "owner_team_id", "owner_epoch", "leader_session_uuid", "notified_message_id",
        "notified_at", "leader_pane_id_at_notify", "envelope_content_hash",
    ]),
];

/// rebuild / 建缺表用的 DDL 模板(`schema_migration.py:CREATE_TABLE_SQL`,`__TABLE__` 占位)。
const CREATE_TABLE_TEMPLATES: &[(&str, &str)] = &[
    ("messages", "create table if not exists __TABLE__ (\n          message_id text primary key,\n          owner_team_id text,\n          task_id text,\n          sender text,\n          recipient text,\n          reply_to text,\n          requires_ack integer,\n          status text,\n          content text,\n          artifact_refs text,\n          created_at text,\n          updated_at text,\n          delivered_at text,\n          acknowledged_at text,\n          error text,\n          delivery_attempts integer not null default 0\n        )"),
    ("results", "create table if not exists __TABLE__ (\n          result_id text primary key,\n          owner_team_id text,\n          task_id text not null,\n          agent_id text not null,\n          envelope text not null,\n          status text not null,\n          created_at text not null\n        )"),
    ("scheduled_events", "create table if not exists __TABLE__ (\n          id integer primary key,\n          owner_team_id text,\n          due_at text not null,\n          target text not null,\n          kind text not null,\n          payload_json text not null,\n          status text not null,\n          created_at text not null,\n          fired_at text,\n          result_json text\n        )"),
    ("delivery_tokens", "create table if not exists __TABLE__ (\n          message_id text primary key,\n          unique_token text not null,\n          injected_at text not null,\n          visible_at text,\n          consumed_at text,\n          failed_at text,\n          failure_reason text\n        )"),
    ("agent_health", "create table if not exists __TABLE__ (\n          owner_team_id text,\n          agent_id text not null,\n          status text not null,\n          last_output_at text,\n          context_usage_pct integer,\n          current_task_id text,\n          updated_at text not null,\n          unique(owner_team_id, agent_id)\n        )"),
    ("peer_allowlist", "create table if not exists __TABLE__ (\n          a text not null,\n          b text not null,\n          created_at text not null,\n          primary key (a, b)\n        )"),
    ("result_watchers", "create table if not exists __TABLE__ (\n          watcher_id text primary key,\n          owner_team_id text,\n          task_id text,\n          agent_id text,\n          message_id text,\n          leader_id text not null,\n          status text not null,\n          created_at text not null,\n          completed_at text,\n          result_id text,\n          notified_message_id text,\n          error text\n        )"),
    ("leader_notification_log", "create table if not exists __TABLE__ (\n          result_id text not null,\n          owner_team_id text not null default '',\n          owner_epoch integer not null default 0,\n          leader_session_uuid text,\n          notified_message_id text not null,\n          notified_at text not null,\n          leader_pane_id_at_notify text,\n          envelope_content_hash text,\n          primary key (result_id, owner_team_id, owner_epoch)\n        )"),
];

fn create_table_ddl(table: &str, name: &str) -> Option<String> {
    CREATE_TABLE_TEMPLATES
        .iter()
        .find(|(t, _)| *t == table)
        .map(|(_, ddl)| ddl.replace("__TABLE__", name))
}

/// 一张表的布局 diff。
#[derive(Debug, Clone)]
pub struct Diff {
    pub table: &'static str,
    pub expected: Vec<&'static str>,
    pub actual: Vec<String>,
    pub missing: bool,
}

/// 一次表 rebuild 的审计事件(对应 Python `schema.layout_rebuild`)。
#[derive(Debug, Clone)]
pub struct RebuildEvent {
    pub table: &'static str,
    pub from_layout_columns: Vec<String>,
    pub to_layout_columns: Vec<String>,
    pub backup_path: String,
    pub row_count_before: i64,
    pub row_count_after: i64,
    pub missing: bool,
}

/// `schema_diagnosis` 的只读结论。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Diagnosis {
    pub ok: bool,
    pub status: String,
    pub user_version: i64,
    pub layout_diffs: Vec<String>,
    /// Python parity (`schema_migration.py:188/206`): the layered guidance line —
    /// "missing" carries the initialize_schema first-use hint; drift carries the
    /// fix-schema command; healthy carries "none".
    pub recommended_action: String,
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, DbError> {
    let n: i64 = conn.query_row(
        "select count(*) from sqlite_master where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

fn table_count(conn: &Connection, table: &str) -> Result<i64, DbError> {
    // table 来自 MANAGED_TABLE_LAYOUTS 固定常量名,非用户输入 → format 安全。
    Ok(conn.query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))?)
}

pub fn pragma_user_version(conn: &Connection) -> Result<i64, DbError> {
    Ok(conn.query_row("pragma user_version", [], |r| r.get(0))?)
}

/// `schema_migration.py:_db_path_from_conn`:从 `pragma database_list` 的 main 行解析文件路径。
/// in-memory(file 列为空)→ `None`。
fn db_path_from_conn(conn: &Connection) -> Option<PathBuf> {
    conn.query_row("pragma database_list", [], |r| r.get::<_, String>(2))
        .ok()
        .filter(|f| !f.is_empty())
        .map(PathBuf::from)
}

// 原子性探针(对抗检查 high gap):测试态下在 rebuild 的 insert 之后注入 Err,
// 验证 ensure_table_layout 的 BEGIN IMMEDIATE 事务**回滚到迁移前态、不留半成品**
// (Gap-46 唯一存在理由)。对应 Python `_maybe_fault_after_insert`(env→os._exit)。
#[cfg(test)]
thread_local! {
    static FAULT_AFTER_INSERT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn maybe_test_fault_after_insert() -> Result<(), DbError> {
    #[cfg(test)]
    {
        if FAULT_AFTER_INSERT.with(std::cell::Cell::get) {
            return Err(DbError::Schema("injected test fault after insert (atomicity probe)".to_string()));
        }
    }
    Ok(())
}

/// `schema_migration.py:_run_version_migrations`:current→schema_version 链式(v1/2/3 均 no-op)。
fn run_version_migrations(conn: &Connection, schema_version: i64) -> Result<(), DbError> {
    let current = pragma_user_version(conn)?;
    for _version in current..=schema_version {
        // SCHEMA_MIGRATIONS:目前 1/2/3 全 no-op(`schema_migration.py:161-165`)。
    }
    Ok(())
}

/// `schema_migration.py:_layout_diffs`:按 `table_info` 物理列序检测漂移。
/// 无任何 managed 表存在(fresh DB)→ 空(initialize 随后建表)。
pub fn layout_diffs(conn: &Connection) -> Result<Vec<Diff>, DbError> {
    let mut any = false;
    for (t, _) in MANAGED_TABLE_LAYOUTS {
        if table_exists(conn, t)? {
            any = true;
            break;
        }
    }
    if !any {
        return Ok(Vec::new());
    }
    let mut diffs = Vec::new();
    for (table, expected) in MANAGED_TABLE_LAYOUTS {
        if !table_exists(conn, table)? {
            diffs.push(Diff { table, expected: expected.to_vec(), actual: vec![], missing: true });
            continue;
        }
        let actual = table_layout(conn, table)?;
        if !actual.iter().map(String::as_str).eq(expected.iter().copied()) {
            diffs.push(Diff { table, expected: expected.to_vec(), actual, missing: false });
        }
    }
    Ok(diffs)
}

/// `schema_migration.py:_rebuild_tables`:missing → 建表;漂移 → temp 复制 common 列 → rename。
/// 行数前后不变(否则报错)。须在调用方的事务内执行。
fn rebuild_tables(
    conn: &Connection,
    diffs: &[Diff],
    backup_path: &Path,
) -> Result<Vec<RebuildEvent>, DbError> {
    let backup = backup_path.display().to_string();
    let mut events = Vec::new();
    for diff in diffs {
        let table = diff.table;
        let expected = MANAGED_TABLE_LAYOUTS
            .iter()
            .find(|(t, _)| *t == table)
            .map(|(_, e)| *e)
            .ok_or_else(|| DbError::Schema(format!("unknown managed table {table}")))?;
        let ddl = |name: &str| {
            create_table_ddl(table, name)
                .ok_or_else(|| DbError::Schema(format!("no DDL template for {table}")))
        };
        if diff.missing {
            let before = 0;
            conn.execute(&ddl(table)?, [])?;
            let after = table_count(conn, table)?;
            if after != before {
                return Err(DbError::Schema(format!(
                    "schema rebuild row count changed for {table}: {before} != {after}"
                )));
            }
            events.push(RebuildEvent {
                table,
                from_layout_columns: vec![],
                to_layout_columns: expected.iter().map(|s| s.to_string()).collect(),
                backup_path: backup.clone(),
                row_count_before: before,
                row_count_after: after,
                missing: true,
            });
            continue;
        }
        let temp = format!("__team_agent_rebuild_{table}");
        let old = format!("__team_agent_old_{table}");
        let before = table_count(conn, table)?;
        conn.execute(&format!("drop table if exists {temp}"), [])?;
        conn.execute(&format!("drop table if exists {old}"), [])?;
        conn.execute(&ddl(&temp)?, [])?;
        // common = expected ∩ actual,按 expected 顺序。
        let common: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|c| diff.actual.iter().any(|a| a == c))
            .collect();
        let column_sql = common.join(", ");
        conn.execute(&format!("insert into {temp}({column_sql}) select {column_sql} from {table}"), [])?;
        maybe_test_fault_after_insert()?; // insert 后、rename 前注入点(原子性探针)
        conn.execute(&format!("alter table {table} rename to {old}"), [])?;
        conn.execute(&format!("alter table {temp} rename to {table}"), [])?;
        conn.execute(&format!("drop table {old}"), [])?;
        let after = table_count(conn, table)?;
        if before != after {
            return Err(DbError::Schema(format!(
                "schema rebuild row count changed for {table}: {before} != {after}"
            )));
        }
        events.push(RebuildEvent {
            table,
            from_layout_columns: diff.actual.clone(),
            to_layout_columns: expected.iter().map(|s| s.to_string()).collect(),
            backup_path: backup.clone(),
            row_count_before: before,
            row_count_after: after,
            missing: false,
        });
    }
    ensure_schema_indexes(conn)?;
    Ok(events)
}

fn backup_path(db_path: &Path, user_version: i64) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    db_path.with_file_name(format!("team.db.pre-migration-{stamp}-from-v{user_version}.bak"))
}

/// `schema_migration.py:ensure_table_layout`:版本迁移 + (有漂移则)备份 + 整表 rebuild,
/// 全程一个 `BEGIN IMMEDIATE` 事务(崩溃 → 回滚到迁移前;成功 → 迁移后;绝不半成品)。
/// fresh DB(无 managed 表)→ 无 diff → 无需 db_path/备份,返回空。
pub fn ensure_table_layout(
    conn: &Connection,
    schema_version: i64,
    db_path: Option<&Path>,
) -> Result<Vec<RebuildEvent>, DbError> {
    run_version_migrations(conn, schema_version)?;
    let started_tx = conn.is_autocommit();
    if started_tx {
        conn.execute_batch("BEGIN IMMEDIATE")?;
    }
    let work = || -> Result<Vec<RebuildEvent>, DbError> {
        let diffs = layout_diffs(conn)?;
        if diffs.is_empty() {
            return Ok(Vec::new());
        }
        // db_path=None 时经 `pragma database_list` 回退解析真实文件路径(`_db_path_from_conn`);
        // in-memory(file 空)→ 仍无路径 → Err。修复对抗检查发现的 divergence。
        let dbp = match db_path {
            Some(p) => p.to_path_buf(),
            None => db_path_from_conn(conn).ok_or_else(|| {
                DbError::Schema("cannot rebuild team.db layout without a database path".to_string())
            })?,
        };
        let uv = pragma_user_version(conn)?;
        let backup = backup_path(&dbp, uv);
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DbError::Schema(format!("backup mkdir: {e}")))?;
        }
        std::fs::copy(dbp, &backup).map_err(|e| DbError::Schema(format!("backup copy: {e}")))?;
        rebuild_tables(conn, &diffs, &backup)
    };
    match work() {
        Ok(events) => {
            if started_tx {
                conn.execute_batch("COMMIT")?;
            }
            // Gap 46 契约:每次 rebuild 在 COMMIT 后发射 schema.layout_rebuild 事件(step 3↔4 集成点)。
            // 回滚的 rebuild 不发(post-commit 语义)。best-effort,不让审计失败拖垮迁移。
            if !events.is_empty() {
                let resolved = db_path.map(Path::to_path_buf).or_else(|| db_path_from_conn(conn));
                if let Some(p) = resolved {
                    emit_rebuild_events(&p, &events);
                }
            }
            Ok(events)
        }
        Err(e) => {
            if started_tx && !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            Err(e)
        }
    }
}

/// `schema_migration.py:schema_diagnosis`:只读判定(不变更 DB)。
pub fn schema_diagnosis(db_path: &Path, schema_version: i64) -> Result<Diagnosis, DbError> {
    if !db_path.exists() {
        // T3-3 cr verdict (A parity lock, 2026-06-10): a missing db is the LEGAL
        // first-use state — ok:true is layered with the explicit status axis and the
        // recommended_action guidance (Python schema_migration.py:180-190 verbatim),
        // never a silent fake-green.
        return Ok(Diagnosis {
            ok: true,
            status: "missing".to_string(),
            user_version: 0,
            layout_diffs: vec![],
            recommended_action:
                "No team.db exists yet; initialize_schema will create it on first use."
                    .to_string(),
        });
    }
    let conn = Connection::open(db_path)?;
    let uv = pragma_user_version(&conn)?;
    let diffs = layout_diffs(&conn)?;
    let diff_tables: Vec<String> = diffs.iter().map(|d| d.table.to_string()).collect();
    let ok = diffs.is_empty() && uv == schema_version;
    Ok(Diagnosis {
        ok,
        status: if ok { "ok".to_string() } else { "schema_repair_available".to_string() },
        user_version: uv,
        recommended_action: if diff_tables.is_empty() {
            "none".to_string()
        } else {
            "run team-agent doctor --fix-schema --json".to_string()
        },
        layout_diffs: diff_tables,
    })
}

/// 便捷:对一个 workspace 的 `.team/runtime/team.db` 跑 diagnosis(对应 Python 签名)。
pub fn schema_diagnosis_workspace(workspace: &Path) -> Result<Diagnosis, DbError> {
    schema_diagnosis(&workspace.join(".team").join("runtime").join("team.db"), SCHEMA_VERSION)
}

/// `schema_migration.py:_workspace_from_db_path`:`<ws>/.team/runtime/team.db` → `<ws>`。
fn workspace_from_db_path(db_path: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = db_path.iter().collect();
    let n = parts.len();
    if n >= 3 && parts[n - 3] == ".team" && parts[n - 2] == "runtime" && parts[n - 1] == "team.db" {
        db_path.parent()?.parent()?.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// `schema_migration.py:_emit_rebuild_events`:每个 rebuild 事件写 `schema.layout_rebuild`(step 4 EventLog)。
/// best-effort:审计写失败不影响已提交的迁移。
fn emit_rebuild_events(db_path: &Path, events: &[RebuildEvent]) {
    let Some(workspace) = workspace_from_db_path(db_path) else {
        return;
    };
    let log = crate::event_log::EventLog::new(&workspace);
    for e in events {
        let fields = serde_json::json!({
            "table": e.table,
            "from_layout_columns": e.from_layout_columns,
            "to_layout_columns": e.to_layout_columns,
            "backup_path": e.backup_path,
            "row_count_before": e.row_count_before,
            "row_count_after": e.row_count_after,
            "missing": e.missing,
        });
        let _ = log.write("schema.layout_rebuild", fields);
    }
}

/// `fix_schema_layout` 的结构化结果(对应 Python dict)。
/// `schema.layout_rebuild` / `schema.layout_rebuild_blocked` 事件发射 → step 4 EventLog 接线。
#[derive(Debug, Clone)]
pub enum FixResult {
    /// db 不存在 → 等价 diagnosis(missing)。
    Missing(Diagnosis),
    /// 撞活跃锁 → 拒绝且**不写备份**(`status:'blocked', reason:'active_lock'`)。
    Blocked { reason: String },
    /// 已修复 → diagnosis(ok)+ rebuild 事件。
    Fixed { diagnosis: Diagnosis, rebuilds: Vec<RebuildEvent> },
}

/// `schema_migration.py:_db_lock_status`:0-timeout `BEGIN IMMEDIATE` 探锁;locked/busy → `active_lock`。
fn db_lock_status(db_path: &Path) -> Result<Option<String>, DbError> {
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(0))?;
    match conn.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => {
            let _ = conn.execute_batch("ROLLBACK");
            Ok(None)
        }
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if msg.contains("locked") || msg.contains("busy") {
                Ok(Some("active_lock".to_string()))
            } else {
                Err(DbError::Sqlite(e))
            }
        }
    }
}

/// `schema_migration.py:fix_schema_layout`:`doctor --fix-schema` 的迁移实体。
/// 先探锁(撞锁则 blocked 且不写备份),否则备份先于破坏性写 + 原子 rebuild + 重诊断。
pub fn fix_schema_layout(workspace: &Path, schema_version: i64) -> Result<FixResult, DbError> {
    let db_path = workspace.join(".team").join("runtime").join("team.db");
    if !db_path.exists() {
        return Ok(FixResult::Missing(schema_diagnosis(&db_path, schema_version)?));
    }
    if let Some(reason) = db_lock_status(&db_path)? {
        let _ = crate::event_log::EventLog::new(workspace).write(
            "schema.layout_rebuild_blocked",
            serde_json::json!({"reason": reason, "db_path": db_path.display().to_string()}),
        );
        return Ok(FixResult::Blocked { reason });
    }
    let conn = Connection::open(&db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let rebuilds = ensure_table_layout(&conn, schema_version, Some(&db_path))?;
    conn.execute_batch(&format!("pragma user_version = {schema_version}"))?;
    drop(conn);
    let diagnosis = schema_diagnosis(&db_path, schema_version)?;
    Ok(FixResult::Fixed { diagnosis, rebuilds })
}

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

        // 迁移前:diagnosis = schema_repair_available,全 8 表 diff(4 漂移 + 4 缺失)。
        let before = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert_eq!(before.status, "schema_repair_available");
        assert!(!before.ok);
        assert_eq!(before.user_version, 1);
        assert_eq!(before.layout_diffs.len(), 8);

        // initialize_schema 走 ensure_table_layout(rebuild)+ 建表 + user_version=3。
        let conn = crate::db::schema::open_db(&path).unwrap();
        initialize_schema(&conn, Some(&path)).unwrap();

        // 迁移后:4 表 canonical 列序 + 行数保全。
        assert_eq!(layout(&conn, "messages")[..3], ["message_id", "owner_team_id", "task_id"]);
        assert_eq!(layout(&conn, "agent_health")[..2], ["owner_team_id", "agent_id"]);
        assert_eq!(table_count(&conn, "messages").unwrap(), 2);
        assert_eq!(table_count(&conn, "results").unwrap(), 2);
        assert_eq!(table_count(&conn, "scheduled_events").unwrap(), 2);
        assert_eq!(table_count(&conn, "agent_health").unwrap(), 2);
        // 行内容保全(owner_team_id 迁移后仍可读)。
        let m1: String = conn.query_row("select owner_team_id from messages where message_id='m1'", [], |r| r.get(0)).unwrap();
        assert_eq!(m1, "t");
        drop(conn);

        // 迁移后 diagnosis = ok / user_version=3。
        let after = schema_diagnosis(&path, SCHEMA_VERSION).unwrap();
        assert!(after.ok);
        assert_eq!(after.status, "ok");
        assert_eq!(after.user_version, 3);

        // 备份文件已写(team.db.pre-migration-*-from-v1.bak)。
        let runtime = path.parent().unwrap();
        let baks: Vec<_> = std::fs::read_dir(runtime).unwrap()
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
        assert_eq!(d.user_version, 3);
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
        assert_eq!(events1.len(), 8); // 4 漂移 rebuild + 4 缺失建表
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
            table_layout(&conn2, "messages").unwrap().last().map(String::as_str),
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
        assert_eq!(events.len(), 8);
        assert_eq!(table_layout(&conn, "messages").unwrap()[..2], ["message_id", "owner_team_id"]);
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
        assert_eq!(table_layout(&conn, "agent_health").unwrap()[0], "owner_team_id");
        assert_eq!(table_count(&conn, "agent_health").unwrap(), 1);
        let owner: Option<String> = conn
            .query_row("select owner_team_id from agent_health where agent_id='a'", [], |r| r.get(0))
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
        assert_eq!(cols.len(), 16);
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
            .query_row("select content, status from messages where message_id='u1'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(content, "héllo🦀\n世界");
        assert_eq!(status, None);
    }

    #[test]
    fn fix_schema_layout_missing_blocked_fixed() {
        // missing:无 db。
        let ws_missing = temp_db().parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
        match fix_schema_layout(&ws_missing, SCHEMA_VERSION).unwrap() {
            FixResult::Missing(d) => assert_eq!(d.status, "missing"),
            other => panic!("expected Missing, got {other:?}"),
        }

        // fixed:legacy db。workspace = .../<dir>(db 在 ws/.team/runtime/team.db)。
        let db = temp_db();
        build_legacy(&db);
        let ws = db.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
        match fix_schema_layout(&ws, SCHEMA_VERSION).unwrap() {
            FixResult::Fixed { diagnosis, rebuilds } => {
                assert!(diagnosis.ok);
                assert_eq!(diagnosis.user_version, 3);
                assert_eq!(rebuilds.len(), 8);
            }
            other => panic!("expected Fixed, got {other:?}"),
        }

        // blocked:另一个连接持 BEGIN IMMEDIATE 写锁时 fix 应拒绝(不写备份)。
        let db2 = temp_db();
        build_legacy(&db2);
        let ws2 = db2.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
        let holder = Connection::open(&db2).unwrap();
        holder.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap(); // 占写锁
        match fix_schema_layout(&ws2, SCHEMA_VERSION).unwrap() {
            FixResult::Blocked { reason } => assert_eq!(reason, "active_lock"),
            other => panic!("expected Blocked, got {other:?}"),
        }
        // 撞锁时不应写任何备份。
        let baks = std::fs::read_dir(db2.parent().unwrap()).unwrap()
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
        let rebuilds: Vec<_> = events.iter().filter(|e| e["event"] == serde_json::json!("schema.layout_rebuild")).collect();
        assert_eq!(rebuilds.len(), 8, "8 张 managed 表各一条 schema.layout_rebuild");
        for e in &rebuilds {
            assert_eq!(e["row_count_before"], e["row_count_after"], "事件须 row_count 前后相等");
            assert!(e["backup_path"].as_str().is_some());
        }
        // 含 messages 那条,to_layout_columns 是 canonical。
        let m = rebuilds.iter().find(|e| e["table"] == serde_json::json!("messages")).unwrap();
        assert_eq!(m["to_layout_columns"][1], serde_json::json!("owner_team_id"));
    }
}
