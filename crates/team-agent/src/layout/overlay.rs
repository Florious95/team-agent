//! 0.3.28 layout step 8 — display overlay (3-pane tiling).
//!
//! Python truth source: 3-pane tiling exists ONLY in the display overlay
//! (`display/tiling.py:7` `DISPLAY_PANES_PER_WINDOW = 3`), which is
//! appended to the LEADER session (never the worker session). Math is
//! `i // 3` (tab index), `i % 3` (slot index), deterministic by job
//! index. Tab naming: `team-agent:<session_tag>:overview[-N]`.
//!
//! Step 8 ships the overlay computation helpers + the
//! `assert_overlay_call_site` warning that fires when `spawn_split` is
//! reached from a non-overlay path (Step 9 promotes to hard error).

use crate::transport::{SessionName, WindowName};

/// 3 panes per overlay window — Python parity.
pub const OVERLAY_PANES_PER_WINDOW: usize = 3;

/// Overlay window name for a 0-based group index. Index 0 → `overview`;
/// N ≥ 1 → `overview-{N+1}`. Names are scoped by `session_tag` so multiple
/// teams can coexist on one leader session.
pub fn overlay_window_name(session_tag: &str, group_index: usize) -> WindowName {
    if group_index == 0 {
        WindowName::new(format!("team-agent:{session_tag}:overview"))
    } else {
        WindowName::new(format!("team-agent:{session_tag}:overview-{}", group_index + 1))
    }
}

/// Plan output: where each agent_id job lands in the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPlacement {
    pub agent_id: String,
    pub group_index: usize,
    pub slot_index: usize,
    pub window: WindowName,
}

/// Compute the overlay placements for a list of agent_ids. Deterministic by
/// input order: agent `i` → window `i / 3`, slot `i % 3`.
pub fn plan_overlay_placements(session_tag: &str, agents: &[&str]) -> Vec<OverlayPlacement> {
    agents
        .iter()
        .enumerate()
        .map(|(i, agent_id)| OverlayPlacement {
            agent_id: agent_id.to_string(),
            group_index: i / OVERLAY_PANES_PER_WINDOW,
            slot_index: i % OVERLAY_PANES_PER_WINDOW,
            window: overlay_window_name(session_tag, i / OVERLAY_PANES_PER_WINDOW),
        })
        .collect()
}

/// True when a window name belongs to a display overlay (matches
/// `team-agent:<...>:overview[-N]`).
pub fn is_overlay_window(name: &WindowName) -> bool {
    let n = name.as_str();
    if !n.starts_with("team-agent:") {
        return false;
    }
    n.contains(":overview")
}

/// 0.3.28 Step 8 invariant: `spawn_split` may ONLY be called from the
/// overlay module. This helper emits a warn-level event when called from
/// anywhere else (Step 9 promotes to a hard error / panic gate). The
/// `target_session` and `target_window` are passed so the operator sees
/// which spawn_split call drifted.
pub fn assert_overlay_call_site(target_session: &SessionName, target_window: &WindowName) {
    if !is_overlay_window(target_window) {
        eprintln!(
            "team_agent::layout overlay_invariant_violation kind=SplitOutsideOverlay \
             session=`{}` window=`{}` action=permitted_with_warning \
             (post-Step-9 will hard-fail; splits must only occur in display overlay)",
            target_session.as_str(),
            target_window.as_str()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_window_name_first_group_is_overview() {
        assert_eq!(
            overlay_window_name("alpha", 0).as_str(),
            "team-agent:alpha:overview"
        );
    }

    #[test]
    fn overlay_window_name_second_group_appends_two() {
        assert_eq!(
            overlay_window_name("alpha", 1).as_str(),
            "team-agent:alpha:overview-2"
        );
    }

    #[test]
    fn plan_overlay_placements_three_per_window_deterministic() {
        let agents = ["a", "b", "c", "d", "e", "f", "g"];
        let plan = plan_overlay_placements("alpha", &agents);
        assert_eq!(plan[0].group_index, 0);
        assert_eq!(plan[0].slot_index, 0);
        assert_eq!(plan[2].group_index, 0);
        assert_eq!(plan[2].slot_index, 2);
        assert_eq!(plan[3].group_index, 1);
        assert_eq!(plan[3].slot_index, 0);
        assert_eq!(plan[6].group_index, 2);
        assert_eq!(plan[6].slot_index, 0);
    }

    #[test]
    fn is_overlay_window_matches_overview_namespace_only() {
        assert!(is_overlay_window(&WindowName::new("team-agent:alpha:overview")));
        assert!(is_overlay_window(&WindowName::new("team-agent:alpha:overview-2")));
        assert!(!is_overlay_window(&WindowName::new("developer")));
        assert!(!is_overlay_window(&WindowName::new("team-w1")));
        assert!(!is_overlay_window(&WindowName::new("team-alpha:leader")));
    }
}
