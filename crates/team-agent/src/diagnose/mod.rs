//! Shared doctor gate seams.

use std::path::Path;

use crate::packaging::types::{
    Blocker, BlockerSource, DoctorGate, PackagingError,
};

pub mod comms;
pub mod orphans;

pub fn doctor_gate_blockers(
    workspace: &Path,
    gate: Option<DoctorGate>,
    fix: bool,
    confirm: bool,
) -> Result<Vec<Blocker>, PackagingError> {
    let _ = (fix, confirm);
    match gate {
        Some(DoctorGate::Orphans) => {
            if orphans::has_orphan_residue() {
                Ok(vec![Blocker {
                    source: BlockerSource::OrphanCoordinator,
                    detail: orphans::orphan_blocker_detail(),
                }])
            } else {
                Ok(Vec::new())
            }
        }
        Some(DoctorGate::Comms) => {
            let report = comms::comms_selftest_report(workspace, None)
                .map_err(|e| PackagingError::State(e.to_string()))?;
            if report.ok {
                Ok(Vec::new())
            } else {
                let failed = comms::failing_check_names(&report).join(", ");
                Ok(vec![Blocker {
                    source: BlockerSource::CommsGate,
                    detail: format!("comms selftest failed: {failed}"),
                }])
            }
        }
        _ => Ok(Vec::new()),
    }
}
