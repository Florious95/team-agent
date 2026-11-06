//! Warn when native split calls target a non-overlay window.

use crate::transport::{SessionName, WindowName};





///
/// True when a window name belongs to a display overlay (matches
/// `team-agent:<...>:overview[-N]`).
fn is_overlay_window(name: &WindowName) -> bool {
    let n = name.as_str();
    if !n.starts_with("team-agent:") {
        return false;
    }
    n.contains(":overview")
}

///
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
    fn is_overlay_window_matches_overview_namespace_only() {
        assert!(is_overlay_window(&WindowName::new(
            "team-agent:alpha:overview"
        )));
        assert!(is_overlay_window(&WindowName::new(
            "team-agent:alpha:overview-2"
        )));
        assert!(!is_overlay_window(&WindowName::new("developer")));
        assert!(!is_overlay_window(&WindowName::new("team-w1")));
        assert!(!is_overlay_window(&WindowName::new("team-alpha:leader")));
    }
}
