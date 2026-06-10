//! B5/F1 · `sessions_to_kill_sparing_leader` 纯函数单测(kill 决策下沉后锁定)。
//!
//! 真相源 = `team-agent-leader-` 确定性命名前缀(leader/start.rs LEADER_SESSION_PREFIX);
//! 集成面由 tests/b5_leader_terminal_kill_red.rs 的真 tmux 契约覆盖,此处锁纯决策。

use crate::cli::lifecycle_port::sessions_to_kill_sparing_leader;
use crate::transport::SessionName;

fn names(raw: &[&str]) -> Vec<SessionName> {
    raw.iter().map(|name| SessionName::new(*name)).collect()
}

#[test]
fn no_leader_session_means_whole_server_kill() {
    assert_eq!(sessions_to_kill_sparing_leader(&names(&["team-x"])), None);
    assert_eq!(sessions_to_kill_sparing_leader(&[]), None);
}

#[test]
fn leader_session_present_kills_only_non_leader_sessions() {
    let sessions = names(&[
        "team-agent-leader-claude-myws-deadbeef",
        "team-x",
        "team-y",
    ]);
    let to_kill = sessions_to_kill_sparing_leader(&sessions)
        .expect("leader present must switch to per-session kills");
    assert_eq!(to_kill, names(&["team-x", "team-y"]));
}

#[test]
fn only_leader_sessions_left_kills_nothing_but_keeps_server() {
    let sessions = names(&["team-agent-leader-codex-myws-cafe0123"]);
    assert_eq!(
        sessions_to_kill_sparing_leader(&sessions),
        Some(Vec::new()),
        "a socket holding only leader sessions must not be torn down"
    );
}
