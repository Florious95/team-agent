//! ---
//! purpose: tick 步骤组的命名空间与规范顺序——给每个步骤组一个稳定标签与固定次序，供编排器与事件日志共用
//! contract:
//!   provides:
//!     - name: ordered
//!       what: 步骤组的规范顺序（穷尽枚举），编排器必须按此序调用
//!     - name: as_str
//!       what: 步骤组的稳定标签（tick.<group>），进事件与指标字段
//!   depends: []
//! boundary:
//!   - 不含任何步骤的实现，只定义分组身份与顺序
//!   - persist 必须排最后、session_gate 必须排最前，这是顺序本身承载的约束
//! maturity: wired
//! ---
//!
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
    /// ---
    /// purpose: 给步骤组一个稳定标签，供事件日志与指标字段使用
    /// returns: 形如 tick.<group> 的稳定串；已发布的取值不可改写，只可新增
    /// ---
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

    ///
    /// Canonical ordering as a const slice — exhaustive over the enum.
    /// ---
    /// purpose: 给出步骤组的规范调用顺序
    /// returns: 穷尽覆盖本 enum 全部变体的静态切片，session_gate 在首、persist 在尾；编排器按此序调用，持久化才能反映之前每一步的结果
    /// ---
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
