#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::Connection;
use team_agent::db::migration::MANAGED_TABLE_LAYOUTS;
use team_agent::db::schema::{table_layout, SCHEMA_VERSION};
use team_agent::message_store::MessageStore;

const V4_UPGRADE_RISK: &str = "0.5.64 is already published with real v4 databases; if this upgrade path loses data, users lose their ledger without being able to report it";

#[test]
fn exact_v4_runtime_database_migrates_to_v5_without_losing_any_row() {
    let workspace = temp_workspace();
    let db_path = workspace.join(".team/runtime/team.db");
    build_v4_runtime_database(&db_path);

    let before_conn = Connection::open(&db_path).expect("open exact v4 runtime database");
    let before_version: i64 = before_conn
        .query_row("pragma user_version", [], |row| row.get(0))
        .expect("read exact v4 user_version");
    assert_eq!(before_version, 4, "{V4_UPGRADE_RISK}: fixture must be v4");
    let v4_layouts = MANAGED_TABLE_LAYOUTS
        .iter()
        .map(|(table, _)| {
            (
                (*table).to_string(),
                table_layout(&before_conn, table).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let before_rows = snapshot_rows(&before_conn, &v4_layouts);
    assert!(
        before_rows.iter().all(|(_, rows)| !rows.is_empty()),
        "{V4_UPGRADE_RISK}: every managed v4 table must contain a preservation canary"
    );
    drop(before_conn);

    MessageStore::open(&workspace).expect("automatic v4 to v5 migration must succeed");

    let after_conn = Connection::open(&db_path).expect("reopen migrated runtime database");
    let after_version: i64 = after_conn
        .query_row("pragma user_version", [], |row| row.get(0))
        .expect("read migrated user_version");
    assert_eq!(
        after_version, 5,
        "{V4_UPGRADE_RISK}: automatic migration must finish at v5"
    );
    assert_eq!(
        SCHEMA_VERSION, 5,
        "{V4_UPGRADE_RISK}: this exact migration contract is pinned to v5"
    );
    assert!(
        table_layout(&after_conn, "result_watchers")
            .unwrap()
            .iter()
            .any(|column| column == "recipient"),
        "{V4_UPGRADE_RISK}: v5 result_watchers must contain recipient"
    );
    let recipient: Option<String> = after_conn
        .query_row(
            "select recipient from result_watchers where watcher_id = 'watch-v4'",
            [],
            |row| row.get(0),
        )
        .expect("read migrated watcher recipient");
    assert_eq!(
        recipient, None,
        "{V4_UPGRADE_RISK}: the new recipient column must be NULL for old watcher rows"
    );
    let after_rows = snapshot_rows(&after_conn, &v4_layouts);
    assert_eq!(
        after_rows, before_rows,
        "{V4_UPGRADE_RISK}: every original column of every managed table must preserve every row"
    );
    drop(after_conn);

    let backups = migration_backups(&db_path);
    assert_eq!(
        backups.len(),
        1,
        "{V4_UPGRADE_RISK}: v4 migration must create exactly one from-v4 backup; backups={backups:?}"
    );
}

fn temp_workspace() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let workspace = std::env::temp_dir().join(format!(
        "team-agent-ds01-v4-v5-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(workspace.join(".team/runtime")).unwrap();
    workspace
}

fn build_v4_runtime_database(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(include_str!("fixtures/case_results_pre_feature_v4.sql"))
        .unwrap();
    conn.execute_batch(
        "insert into messages(message_id, owner_team_id, task_id, sender, recipient, status, content, created_at, updated_at)
             values ('msg-v4', 'teamA', 'task-v4', 'leader', 'worker', 'submitted', 'message-v4', '2026-08-10T00:00:00Z', '2026-08-10T00:00:00Z');
         insert into scheduled_events(id, owner_team_id, due_at, target, kind, payload_json, status, created_at)
             values (41, 'teamA', '2026-08-10T00:01:00Z', 'worker', 'wake', '{\"v\":4}', 'pending', '2026-08-10T00:00:00Z');
         insert into delivery_tokens(message_id, unique_token, injected_at)
             values ('msg-v4', 'token-v4', '2026-08-10T00:00:00Z');
         insert into agent_health(owner_team_id, agent_id, status, current_task_id, updated_at)
             values ('teamA', 'worker', 'idle', 'task-v4', '2026-08-10T00:00:00Z');
         insert into peer_allowlist(a, b, created_at)
             values ('worker', 'reviewer', '2026-08-10T00:00:00Z');
         insert into result_watchers(watcher_id, owner_team_id, task_id, agent_id, message_id, leader_id, status, created_at, result_id)
             values ('watch-v4', 'teamA', 'task-v4', 'worker', 'msg-v4', 'leader', 'pending', '2026-08-10T00:00:00Z', 'res-pre-feature-v4');
         insert into leader_notification_log(result_id, owner_team_id, owner_epoch, leader_session_uuid, notified_message_id, notified_at, leader_pane_id_at_notify, envelope_content_hash)
             values ('res-pre-feature-v4', 'teamA', 4, 'leader-v4', 'notify-v4', '2026-08-10T00:00:00Z', '%4', 'hash-v4');
         pragma user_version = 4;",
    )
    .unwrap();
}

fn snapshot_rows(
    conn: &Connection,
    layouts: &[(String, Vec<String>)],
) -> Vec<(String, Vec<Vec<Value>>)> {
    layouts
        .iter()
        .map(|(table, columns)| {
            let sql = format!(
                "select {} from {table} order by {}",
                columns.join(", "),
                columns[0]
            );
            let mut statement = conn.prepare(&sql).unwrap();
            let rows = statement
                .query_map([], |row| {
                    (0..columns.len())
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table.clone(), rows)
        })
        .collect()
}

fn migration_backups(db_path: &Path) -> Vec<PathBuf> {
    let mut backups = fs::read_dir(db_path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("team.db.pre-migration-") && name.ends_with("-from-v4.bak")
                })
        })
        .collect::<Vec<_>>();
    backups.sort();
    backups
}
