//! `doctor` 自检入口:把 step 3 schema_diagnosis +(placeholder)step 11/12 gate 结论归一成
//! typed [`DoctorStatus`]。

use super::types::{
    Blocker, BlockerSource, DoctorGate, DoctorOptions, DoctorStatus, PackagingError,
};
use crate::db::migration::schema_diagnosis_workspace;

/// `team-agent doctor`(`commands.py:218`)。packaging 的自检入口:把 step 3 schema_diagnosis +
/// (placeholder)step 11 comms / step 12 orphan gate 结论归一成 typed [`DoctorStatus`]。
/// **§84**:只调 step 3/11/12 的 trait 入口,注入 mock 时 provider 调用计数 = 0;绝不触发 prompt/token。
pub fn doctor(opts: &DoctorOptions) -> Result<DoctorStatus, PackagingError> {
    if opts.fix && opts.gate.is_none() {
        return Err(PackagingError::InvalidOptions("--fix requires --gate".to_string()));
    }
    let gate_blockers = crate::diagnose::doctor_gate_blockers(
        &opts.workspace,
        opts.gate,
        opts.fix,
        opts.confirm,
    )?;
    if !gate_blockers.is_empty() {
        return Ok(DoctorStatus::HasBlockers {
            blockers: gate_blockers,
        });
    }
    let diagnosis = schema_diagnosis_workspace(&opts.workspace)?;
    if diagnosis.layout_diffs.is_empty() {
        Ok(DoctorStatus::Ok)
    } else {
        Ok(DoctorStatus::HasBlockers {
            blockers: vec![Blocker {
                source: BlockerSource::SchemaLayoutDrift,
                detail: "team.db physical layout drift detected".to_string(),
            }],
        })
    }
}
