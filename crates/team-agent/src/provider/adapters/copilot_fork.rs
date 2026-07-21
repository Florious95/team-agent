use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::provider::{next_session_token, ProviderError, SessionId};

const SESSION_TABLES: [&str; 6] = [
    "turns",
    "checkpoints",
    "session_files",
    "session_refs",
    "forge_trajectory_events",
    "search_index",
];

#[derive(Debug, Clone)]
pub(crate) struct CopilotForkMaterialization {
    session_id: SessionId,
    home: PathBuf,
    keep: bool,
}

impl CopilotForkMaterialization {
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for CopilotForkMaterialization {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

pub(crate) fn copilot_home() -> Result<PathBuf, ProviderError> {
    if let Some(home) = std::env::var_os("COPILOT_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".copilot"))
        .ok_or_else(|| ProviderError::Io("HOME is unset; cannot resolve COPILOT_HOME".to_string()))
}

pub(crate) fn materialize_copilot_fork(
    workspace: &Path,
    agent_id: &str,
    source_session_id: &SessionId,
) -> Result<CopilotForkMaterialization, ProviderError> {
    let source_home = copilot_home()?;
    let source_dir = source_home
        .join("session-state")
        .join(source_session_id.as_str());
    let source_db = source_home.join("session-store.db");
    if !source_dir.is_dir() || !source_db.is_file() {
        return Err(ProviderError::Io(format!(
            "copilot source backing is incomplete under {}",
            source_home.display()
        )));
    }

    let session_id = SessionId::new(next_session_token());
    let parent = workspace
        .join(".team/runtime/provider-session-forks/copilot")
        .join(agent_id);
    let home = parent.join(session_id.as_str());
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        session_id.as_str(),
        std::process::id()
    ));
    if home.exists() || temp.exists() {
        return Err(ProviderError::Io(format!(
            "copilot fork destination already exists: {}",
            home.display()
        )));
    }
    std::fs::create_dir_all(temp.join("session-state"))
        .map_err(|error| ProviderError::Io(error.to_string()))?;

    let result = (|| {
        let target_session_dir = temp.join("session-state").join(session_id.as_str());
        copy_tree(&source_dir, &target_session_dir)?;
        rewrite_text_tree(
            &target_session_dir,
            source_session_id.as_str(),
            session_id.as_str(),
        )?;
        rewrite_per_session_db(
            &target_session_dir.join("session.db"),
            source_session_id.as_str(),
            session_id.as_str(),
        )?;
        clone_and_rekey_store(
            &source_db,
            &temp.join("session-store.db"),
            source_session_id.as_str(),
            session_id.as_str(),
        )?;
        std::fs::rename(&temp, &home).map_err(|error| ProviderError::Io(error.to_string()))
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(error);
    }
    Ok(CopilotForkMaterialization {
        session_id,
        home,
        keep: false,
    })
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), ProviderError> {
    std::fs::create_dir_all(target).map_err(|error| ProviderError::Io(error.to_string()))?;
    for entry in std::fs::read_dir(source).map_err(|error| ProviderError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| ProviderError::Io(error.to_string()))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_tree(&source_path, &target_path)?;
        } else {
            std::fs::copy(&source_path, &target_path)
                .map_err(|error| ProviderError::Io(error.to_string()))?;
        }
    }
    Ok(())
}

fn rewrite_text_tree(root: &Path, source: &str, target: &str) -> Result<(), ProviderError> {
    for entry in std::fs::read_dir(root).map_err(|error| ProviderError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| ProviderError::Io(error.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            rewrite_text_tree(&path, source, target)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("db") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains(source) {
            std::fs::write(&path, text.replace(source, target))
                .map_err(|error| ProviderError::Io(error.to_string()))?;
        }
    }
    Ok(())
}

fn clone_and_rekey_store(
    source_db: &Path,
    target_db: &Path,
    source: &str,
    target: &str,
) -> Result<(), ProviderError> {
    let source_conn = Connection::open(source_db).map_err(sql_error)?;
    source_conn
        .execute("VACUUM INTO ?1", [target_db.to_string_lossy().as_ref()])
        .map_err(sql_error)?;
    drop(source_conn);

    let mut conn = Connection::open(target_db).map_err(sql_error)?;
    conn.pragma_update(None, "foreign_keys", "OFF")
        .map_err(sql_error)?;
    let tx = conn.transaction().map_err(sql_error)?;
    let session_rows = tx
        .execute(
            "update sessions set id = ?1 where id = ?2",
            params![target, source],
        )
        .map_err(sql_error)?;
    if session_rows != 1 {
        return Err(ProviderError::Io(format!(
            "copilot session-store source row count must be 1, got {session_rows}"
        )));
    }
    for table in SESSION_TABLES {
        require_session_column(&tx, table)?;
        tx.execute(
            &format!("update {table} set session_id = ?1 where session_id = ?2"),
            params![target, source],
        )
        .map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)?;
    verify_rekeyed_store(&conn, source, target)
}

fn require_session_column(conn: &Connection, table: &str) -> Result<(), ProviderError> {
    let mut stmt = conn
        .prepare(&format!("pragma table_info({table})"))
        .map_err(sql_error)?;
    let mut rows = stmt.query([]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let name: String = row.get(1).map_err(sql_error)?;
        if name == "session_id" {
            return Ok(());
        }
    }
    Err(ProviderError::Io(format!(
        "copilot session-store table {table} is missing session_id"
    )))
}

fn verify_rekeyed_store(
    conn: &Connection,
    source: &str,
    target: &str,
) -> Result<(), ProviderError> {
    let target_sessions: i64 = conn
        .query_row(
            "select count(*) from sessions where id = ?1",
            [target],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let source_sessions: i64 = conn
        .query_row(
            "select count(*) from sessions where id = ?1",
            [source],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if target_sessions != 1 || source_sessions != 0 {
        return Err(ProviderError::Io(
            "copilot session-store session id rekey verification failed".to_string(),
        ));
    }
    for table in SESSION_TABLES {
        let remaining: i64 = conn
            .query_row(
                &format!("select count(*) from {table} where session_id = ?1"),
                [source],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if remaining != 0 {
            return Err(ProviderError::Io(format!(
                "copilot session-store table {table} still references source session"
            )));
        }
    }
    Ok(())
}

fn rewrite_per_session_db(path: &Path, source: &str, target: &str) -> Result<(), ProviderError> {
    if !path.is_file() {
        return Ok(());
    }
    let mut conn = Connection::open(path).map_err(sql_error)?;
    let columns: Vec<(String, String)> = {
        let mut tables = conn
            .prepare("select name from sqlite_master where type = 'table'")
            .map_err(sql_error)?;
        let names = tables
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        let mut columns = Vec::new();
        for table in names {
            let mut stmt = conn
                .prepare(&format!(
                    "pragma table_info('{}')",
                    table.replace('\'', "''")
                ))
                .map_err(sql_error)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(sql_error)?;
            for column in rows {
                let column = column.map_err(sql_error)?;
                if column == "session_id" || column == "recipient_session_id" {
                    columns.push((table.clone(), column));
                }
            }
        }
        columns
    };
    let tx = conn.transaction().map_err(sql_error)?;
    for (table, column) in columns {
        let table = table.replace('"', "\"\"");
        let column = column.replace('"', "\"\"");
        tx.execute(
            &format!("update \"{table}\" set \"{column}\" = ?1 where \"{column}\" = ?2"),
            params![target, source],
        )
        .map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)
}

fn sql_error(error: rusqlite::Error) -> ProviderError {
    ProviderError::Io(format!("copilot sqlite: {error}"))
}
