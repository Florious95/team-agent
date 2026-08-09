//!
//! 孤儿自终止判定(Gap 37b)+ provider-neutral 整队消失检测。

use super::types::{
    MarkerStore, TeamPresenceSnapshot, WholeTeamGoneClass, WholeTeamGoneReport, WorkspacePath,
};

///
/// 孤儿自终止判定(`__main__.py:51-59`,Gap 37b)。仅当
/// `current_ppid != initial_ppid ∧ current_ppid == 1 ∧ !workspace.exists()` 三者**同时**成立 → true。
/// 少一个条件都不能误杀正常 daemon(card §91)。
pub fn should_orphan_self_terminate(
    initial_ppid: u32,
    current_ppid: u32,
    workspace: &WorkspacePath,
) -> bool {
    current_ppid != initial_ppid && current_ppid == 1 && !workspace.as_path().exists()
}

// ===========================================================================
// abnormal_track 公共面(abnormal_track.py)—— Gap 32 §4 provider-neutral
// ===========================================================================

///
/// `detect_whole_team_gone`(`abnormal_track.py:91`,C10/C13)。coordinator-independent 整队消失检测:
/// 全死(coordinator + leader + 所有 worker + 所有 session)且非 clean_shutdown/restart →
/// 写 durable marker + 延迟到下条 leader 命令再 escalate。clean/restart 静默。
pub fn detect_whole_team_gone(
    snapshot: &TeamPresenceSnapshot,
    marker_store: &mut dyn MarkerStore,
) -> WholeTeamGoneReport {
    let any_provider_alive = snapshot.provider_processes_alive.iter().any(|alive| *alive);
    if snapshot.coordinator_alive
        || snapshot.leader_alive
        || any_provider_alive
        || snapshot.tmux_sessions_present
    {
        return whole_team_report(false, WholeTeamGoneClass::Alive, false, false, false);
    }
    if snapshot.clean_shutdown {
        return whole_team_report(true, WholeTeamGoneClass::CleanShutdown, false, false, false);
    }
    if snapshot.restart_in_progress {
        return whole_team_report(
            true,
            WholeTeamGoneClass::RestartInProgress,
            false,
            false,
            false,
        );
    }
    let marker_written = marker_store.set_marker(
        "whole_team_gone",
        serde_json::json!({"classification": "unexpected_exit"}),
    );
    whole_team_report(
        true,
        WholeTeamGoneClass::UnexpectedExit,
        true,
        true,
        marker_written,
    )
}

fn whole_team_report(
    whole_team_gone: bool,
    classification: WholeTeamGoneClass,
    notify: bool,
    escalate_user_on_next_leader_command: bool,
    marker_written: bool,
) -> WholeTeamGoneReport {
    WholeTeamGoneReport {
        whole_team_gone,
        classification,
        notify,
        escalate_user_on_next_leader_command,
        marker_written,
    }
}
