//! Legacy writer for explicit offline schema import/repair only.
use crate::db::{schema::retry_sqlite, DbError};
use rusqlite::Connection;
use std::time::Duration;

pub fn open_db(path: &std::path::Path) -> Result<Connection, DbError> {
    let existed = path.exists();
    let conn = retry_sqlite(|| Connection::open(path))?;
    conn.busy_timeout(Duration::from_millis(30_000))?;
    if !existed {
        retry_sqlite(|| conn.execute_batch("PRAGMA journal_mode=WAL;"))?;
    }
    Ok(conn)
}

