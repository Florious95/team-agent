//! unit-8 (Stage 3) — `lifecycle::launch::readiness` phase boundary.
//!
//! Dedicated home for coordinator-start + readiness-verdict computation.
//! Future commits migrate the inline phase fns at launch.rs:2928-2944
//! here.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessPhase {
    StartCoordinator,
    ComputeVerdict,
}

impl ReadinessPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartCoordinator => "launch.readiness.start_coordinator",
            Self::ComputeVerdict => "launch.readiness.compute_verdict",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_dotted_paths_under_launch_readiness() {
        assert!(ReadinessPhase::StartCoordinator
            .as_str()
            .starts_with("launch.readiness."));
    }
}
