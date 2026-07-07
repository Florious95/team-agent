//! step 3 · db — `team.db`(SQLite)的 schema 与迁移层(真相源 `message_store/schema*.py`)。
//!
//! §6 step 3:schema/migrations/indexes/WAL+busy-timeout/只读兼容。**消息操作本身(send/
//! receive/result/watcher 等)是 step 7**,不在此。
//!
//! 字节策略(§7):DDL 逐字照搬 → `sqlite_master` 等价;`schema_diagnosis` 比的是
//! `table_layout`(列名序),与 Python 一致。rusqlite `bundled` 静态链接 SQLite(§8)。
//!
//! §10:db 层无 unwrap/expect/panic;rusqlite 错误经 [`DbError`] 传播。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod agent_health_capture;
pub mod message_store;
pub mod migration;
pub mod schema;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("schema: {0}")]
    Schema(String),
}
