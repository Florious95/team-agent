use std::path::Path;

use rusqlite::Connection;

#[test]
#[serial_test::serial(env)]
fn copilot_fork_copies_isolated_home_and_rekeys_every_session_table() {
    use crate::provider::adapters::copilot_fork::materialize_copilot_fork;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "ta-copilot-fork-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let source_home = root.join("source-home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let source = SessionId::new("11111111-2222-4333-8444-555555555555");
    seed_copilot_fork_fixture(&source_home, source.as_str(), None);
    let source_events = std::fs::read(
        source_home
            .join("session-state")
            .join(source.as_str())
            .join("events.jsonl"),
    )
    .unwrap();
    let previous = std::env::var_os("COPILOT_HOME");
    std::env::set_var("COPILOT_HOME", &source_home);

    let materialized = materialize_copilot_fork(&workspace, "fork-a", &source).unwrap();
    assert_ne!(materialized.session_id(), &source);
    assert!(materialized
        .home()
        .starts_with(workspace.join(".team/runtime/provider-session-forks/copilot/fork-a")));
    let new_id = materialized.session_id().as_str();
    let target_state = materialized.home().join("session-state").join(new_id);
    assert!(target_state.is_dir());
    assert_eq!(
        std::fs::read_to_string(target_state.join("events.jsonl")).unwrap(),
        format!("{{\"session_id\":\"{new_id}\"}}\n")
    );
    let per_session = Connection::open(target_state.join("session.db")).unwrap();
    let recipient: String = per_session
        .query_row(
            "select recipient_session_id from inbox_entries",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recipient, new_id);

    let central = Connection::open(materialized.home().join("session-store.db")).unwrap();
    assert_eq!(session_count(&central, "sessions", "id", new_id), 1);
    assert_eq!(
        session_count(&central, "sessions", "id", source.as_str()),
        0
    );
    for table in [
        "turns",
        "checkpoints",
        "session_files",
        "session_refs",
        "forge_trajectory_events",
        "search_index",
    ] {
        assert_eq!(session_count(&central, table, "session_id", new_id), 1);
        assert_eq!(
            session_count(&central, table, "session_id", source.as_str()),
            0
        );
    }
    assert_eq!(
        std::fs::read(
            source_home
                .join("session-state")
                .join(source.as_str())
                .join("events.jsonl")
        )
        .unwrap(),
        source_events,
        "fork must not mutate source backing"
    );

    restore_env("COPILOT_HOME", previous);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
#[serial_test::serial(env)]
fn copilot_fork_fails_closed_when_a_required_session_table_is_missing() {
    let root = std::env::temp_dir().join(format!("ta-copilot-fork-missing-{}", std::process::id()));
    let source_home = root.join("source-home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let source = SessionId::new("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    seed_copilot_fork_fixture(&source_home, source.as_str(), Some("session_refs"));
    let previous = std::env::var_os("COPILOT_HOME");
    std::env::set_var("COPILOT_HOME", &source_home);

    let error = crate::provider::adapters::copilot_fork::materialize_copilot_fork(
        &workspace, "fork-b", &source,
    )
    .unwrap_err();
    assert!(error.to_string().contains("session_refs"), "{error}");
    let managed_parent = workspace.join(".team/runtime/provider-session-forks/copilot/fork-b");
    assert!(
        !managed_parent.exists() || std::fs::read_dir(managed_parent).unwrap().next().is_none(),
        "failed materialization must leave no clone home"
    );

    restore_env("COPILOT_HOME", previous);
    let _ = std::fs::remove_dir_all(root);
}

fn seed_copilot_fork_fixture(home: &Path, source: &str, omitted: Option<&str>) {
    let state = home.join("session-state").join(source);
    std::fs::create_dir_all(state.join("checkpoints")).unwrap();
    std::fs::write(
        state.join("workspace.yaml"),
        format!("session_id: {source}\n"),
    )
    .unwrap();
    std::fs::write(
        state.join("events.jsonl"),
        format!("{{\"session_id\":\"{source}\"}}\n"),
    )
    .unwrap();
    let session_db = Connection::open(state.join("session.db")).unwrap();
    session_db
        .execute_batch(
            "create table inbox_entries (id text primary key, recipient_session_id text not null);",
        )
        .unwrap();
    session_db
        .execute("insert into inbox_entries values ('one', ?1)", [source])
        .unwrap();

    let db = Connection::open(home.join("session-store.db")).unwrap();
    db.execute_batch(
        "create table sessions (id text primary key);
         create table turns (id integer primary key, session_id text not null);
         create table checkpoints (id integer primary key, session_id text not null);
         create table session_files (id integer primary key, session_id text not null);
         create table session_refs (id integer primary key, session_id text not null);
         create table forge_trajectory_events (id integer primary key, session_id text not null);
         create virtual table search_index using fts5(content, session_id unindexed);",
    )
    .unwrap();
    db.execute("insert into sessions values (?1)", [source])
        .unwrap();
    for table in [
        "turns",
        "checkpoints",
        "session_files",
        "session_refs",
        "forge_trajectory_events",
    ] {
        if omitted != Some(table) {
            db.execute(
                &format!("insert into {table} (session_id) values (?1)"),
                [source],
            )
            .unwrap();
        } else {
            db.execute(&format!("drop table {table}"), []).unwrap();
        }
    }
    db.execute(
        "insert into search_index (content, session_id) values ('x', ?1)",
        [source],
    )
    .unwrap();
}

fn session_count(conn: &Connection, table: &str, column: &str, id: &str) -> i64 {
    conn.query_row(
        &format!("select count(*) from {table} where {column} = ?1"),
        [id],
        |row| row.get(0),
    )
    .unwrap()
}

fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}
