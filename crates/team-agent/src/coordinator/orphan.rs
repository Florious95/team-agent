//! ---
//! purpose: 两个纯判定——daemon 自己是不是该自杀的孤儿，以及整个队是不是已经全没了
//! contract:
//!   provides:
//!     - name: should_orphan_self_terminate
//!       what: ppid 变更 + 被 init 收养 + workspace 已消失，三者同时成立才判孤儿
//!     - name: detect_whole_team_gone
//!       what: 由存活快照判整队消失，并区分 clean/restart（静默）与 unexpected（写 marker + 延迟升报）
//!   depends:
//!     - super::types
//! boundary:
//!   - 不杀任何进程，也不发信号；只给判定结果，动作归调用方
//!   - 不读屏、不命名 provider；只消费结构化存活快照
//!   - 不直接升报用户，unexpected 只落 durable marker，等下一条 leader 命令再升
//! maturity: wired
//! ---
//!
//! 孤儿自终止判定(Gap 37b)+ provider-neutral 整队消失检测。

use super::types::{
    MarkerStore, TeamPresenceSnapshot, WholeTeamGoneClass, WholeTeamGoneReport, WorkspacePath,
};

///
/// 孤儿自终止判定(`__main__.py:51-59`,Gap 37b)。仅当
/// `current_ppid != initial_ppid ∧ current_ppid == 1 ∧ !workspace.exists()` 三者**同时**成立 → true。
/// 少一个条件都不能误杀正常 daemon(card §91)。
/// ---
/// purpose: 判断本 daemon 是否已成为应当自我终止的孤儿
/// params:
///   initial_ppid: 进程启动那一刻记下的父 pid
///   current_ppid: 当前父 pid；被 init/launchd 收养后为 1
///   workspace: 本 daemon 服务的 workspace 根，用其存在性作第三个条件
/// returns: 三个条件同时成立才为 true；少一个都返回 false，宁可不自杀
/// ---
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
/// ---
/// purpose: 由整队存活快照判定「整队消失」并给出分类与后续处置标志
/// params:
///   snapshot: coordinator/leader/各 worker 进程/transport 会话的存活位，以及 clean_shutdown、restart_in_progress 两个意图位
///   marker_store: durable marker 写入口；仅 unexpected_exit 分支会被调用
/// returns: 任一成员存活则 Alive 且全部标志为 false；全死时按 clean_shutdown、restart_in_progress、unexpected_exit 依次分类，只有 unexpected 才置 notify/escalate 并写 marker
/// ---
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
