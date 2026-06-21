//! unit-8 (Stage 3) — `lifecycle::launch::spec_state` phase boundary.
//!
//! The dedicated home for spec/runtime-path resolution and state-tree
//! materialization phases of `quick_start`. Lives in the
//! `lifecycle/launch/` submodule so future commits can migrate the
//! existing inline phase fns (launch.rs:2781-2906 and 1680-1756 ranges)
//! here in small, reviewable pieces.
//!
//! Established phases (canonical names — keep stable for future
//! migration):
//!
//! * `resolve_spec_paths`   — `.team/runtime/<team>/team.spec.yaml`
//!                            resolution + workspace canonicalization
//! * `materialize_state`    — T1 layer state.json initialization including
//!                            agent capture fields and `spawn_cwd`
//!
//! This commit lands the boundary + a marker enum so unit-8's adoption
//! sites can reference the phases by name. The phase fns themselves
//! remain in launch.rs until the next batch of relocations.

/// Named launch phases under spec_state. Used in phase labels for logs
/// and (future) for the orchestrator's step dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecStatePhase {
    ResolveSpecPaths,
    MaterializeState,
}

impl SpecStatePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveSpecPaths => "launch.spec_state.resolve_spec_paths",
            Self::MaterializeState => "launch.spec_state.materialize_state",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_dotted_paths_under_launch_spec_state() {
        assert!(SpecStatePhase::ResolveSpecPaths
            .as_str()
            .starts_with("launch.spec_state."));
        assert!(SpecStatePhase::MaterializeState
            .as_str()
            .starts_with("launch.spec_state."));
    }
}
