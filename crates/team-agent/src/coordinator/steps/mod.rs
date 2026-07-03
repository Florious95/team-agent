//! unit-11 (Stage 4) — coordinator tick steps namespace.
//!
//! The 3083 LOC `coordinator/tick.rs` orchestrator runs an ordered
//! sequence of step groups per tick. This module establishes a named
//! home for each group so future commits migrate them in small,
//! reviewable pieces. The orchestrator stays in `tick.rs`; this is the
//! boundary for the upcoming step relocations.
//!
//! Step groups (canonical order):
//!   1. `session_gate`     — provider session capture + readiness gates
//!   2. `health_sync`      — worker liveness + reconciliation
//!   3. `delivery`         — message delivery FSM ticks
//!   4. `runtime_prompts`  — startup / approval / abnormal-exit prompts
//!   5. `abnormal`         — abnormal-exit detection + classification
//!   6. `persist`          — durable state persistence at tick end

pub mod abnormal;
pub mod delivery;
pub mod health_sync;
pub mod persist;
pub mod runtime_prompts;
pub mod session_gate;

/// Canonical ordering of tick step groups. The orchestrator must invoke
/// steps in this order so persisted state reflects every earlier step's
/// outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickStepGroup {
    SessionGate,
    HealthSync,
    Delivery,
    RuntimePrompts,
    Abnormal,
    Persist,
}

impl TickStepGroup {
    /// Stable label used in event-log and metric fields.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionGate => "tick.session_gate",
            Self::HealthSync => "tick.health_sync",
            Self::Delivery => "tick.delivery",
            Self::RuntimePrompts => "tick.runtime_prompts",
            Self::Abnormal => "tick.abnormal",
            Self::Persist => "tick.persist",
        }
    }

    /// Canonical ordering as a const slice — exhaustive over the enum.
    pub fn ordered() -> &'static [TickStepGroup] {
        &[
            TickStepGroup::SessionGate,
            TickStepGroup::HealthSync,
            TickStepGroup::Delivery,
            TickStepGroup::RuntimePrompts,
            TickStepGroup::Abnormal,
            TickStepGroup::Persist,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_groups_cover_every_variant() {
        let ordered = TickStepGroup::ordered();
        assert_eq!(ordered.len(), 6);
        // Persist is last (state writes follow every other step).
        assert_eq!(*ordered.last().unwrap(), TickStepGroup::Persist);
        // SessionGate is first (downstream steps depend on it).
        assert_eq!(*ordered.first().unwrap(), TickStepGroup::SessionGate);
    }

    #[test]
    fn labels_are_dotted_paths_under_tick() {
        for g in TickStepGroup::ordered() {
            assert!(g.as_str().starts_with("tick."));
        }
    }

    #[test]
    fn ordered_group_labels_are_byte_stable() {
        let labels = TickStepGroup::ordered()
            .iter()
            .map(|group| group.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "tick.session_gate",
                "tick.health_sync",
                "tick.delivery",
                "tick.runtime_prompts",
                "tick.abnormal",
                "tick.persist",
            ]
        );
    }
}
