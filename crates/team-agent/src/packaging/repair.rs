//! schema migration / repair 转调:`doctor --fix-schema` → step 3 `fix_schema_layout`,包成
//! [`MigrationOutcome`]。

use std::path::Path;

use super::types::{MigrationOutcome, PackagingError};
use crate::db::migration::{fix_schema_layout, FixResult};
use crate::db::schema::SCHEMA_VERSION;

/// `team-agent doctor --fix-schema`(`commands.py:239`)→ 转调 step 3 [`fix_schema_layout`]。
/// migration·repair 实体在 step 3;packaging 仅转调 + 包成 [`MigrationOutcome`]。
/// // REAL-MACHINE-E2E:破坏性 rebuild(撞锁/备份/rollback)的真路径需真机验;转调逻辑可单测。
pub fn repair_schema(workspace: &Path) -> Result<MigrationOutcome, PackagingError> {
    match fix_schema_layout(workspace, SCHEMA_VERSION)? {
        FixResult::Missing(diagnosis) => Ok(MigrationOutcome::UpToDate { diagnosis }),
        FixResult::Blocked { reason } => Ok(MigrationOutcome::Blocked { reason }),
        FixResult::Fixed {
            diagnosis,
            rebuilds,
        } if rebuilds.is_empty() => Ok(MigrationOutcome::UpToDate { diagnosis }),
        FixResult::Fixed {
            diagnosis,
            rebuilds,
        } => Ok(MigrationOutcome::Migrated {
            fix: FixResult::Fixed {
                diagnosis,
                rebuilds,
            },
        }),
    }
}
