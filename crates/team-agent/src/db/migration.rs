//! Compatibility namespace for explicit offline legacy schema tooling.
pub use crate::importer::migration::*;
use std::path::{Path, PathBuf};
use rusqlite::{Connection, OpenFlags};
use crate::db::schema::{ensure_schema_indexes, table_layout, SCHEMA_VERSION};
use crate::db::DbError;
