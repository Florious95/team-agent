//! E12 (P0) · `sessions_to_kill` 纯决策单测(kill 决策下沉)。
//!
//! spare = state 锚 session(anchor_sessions) ∪ `team-agent-leader-` 命名前缀(并集,锚优先)。
//! 独享 socket(无 spare)才允许整 server 拆;共享/leader 在 → 逐 session kill。
//! 集成面由 tests/b5_leader_terminal_kill_red.rs 的真 tmux 契约覆盖,此处锁纯决策 + 4 反向 case。

use crate::cli::lifecycle_port::{sessions_to_kill, KillDecision};
use crate::transport::SessionName;
use std::collections::BTreeSet;

fn names(raw: &[&str]) -> Vec<SessionName> {
    raw.iter().map(|name| SessionName::new(*name)).collect()
}

fn anchors(raw: &[&str]) -> BTreeSet<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

// RC-4(独享 socket):仅目标 session(无 spare)→ 整 server 拆。
#[test]
fn rc4_exclusive_socket_kills_server() {
    assert_eq!(
        sessions_to_kill(&names(&["team-x", "team-y"]), &BTreeSet::new()),
        KillDecision::KillServerExclusive
    );
    // 空 session 集 → 逐 kill(no-op),不整 server 拆(没东西可拆)。
    assert_eq!(
        sessions_to_kill(&[], &BTreeSet::new()),
        KillDecision::KillIndividually { to_kill: vec![], spared: vec![] }
    );
}

// RC-1(本 P0 复现):in_tmux leader 在用户 session(**无前缀**),靠 state 锚 spare → 用户 session 存活。
#[test]
fn rc1_in_tmux_no_prefix_anchor_spares_user_session() {
    let sessions = names(&["team-coder-team", "team-x"]); // 用户 session 无 leader 前缀
    let anchor = anchors(&["team-coder-team"]); // state 锚 pane 所在 session
    let decision = sessions_to_kill(&sessions, &anchor);
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert_eq!(to_kill, names(&["team-x"]), "only non-anchor session killed");
            assert_eq!(spared, names(&["team-coder-team"]), "user/leader session spared by anchor");
        }
        other => panic!("anchor session must force per-session kill, not {other:?}"),
    }
}

// 前缀判据仍生效(并集):leader 前缀 session spare,即使无 state 锚。
#[test]
fn prefix_session_spared_without_anchor() {
    let sessions = names(&["team-agent-leader-claude-ws-deadbeef", "team-x"]);
    let decision = sessions_to_kill(&sessions, &BTreeSet::new());
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert_eq!(to_kill, names(&["team-x"]));
            assert_eq!(spared, names(&["team-agent-leader-claude-ws-deadbeef"]));
        }
        other => panic!("prefix session must spare, not {other:?}"),
    }
}

// RC-2(state 损坏无锚):anchor_sessions 空 → 退命名前缀判据(此处仅前缀 spare;无前缀则全 kill)。
// (spare_fallback_to_naming event 在 anchor_anchor_sessions 发,本纯函数只验退化后的决策。)
#[test]
fn rc2_no_anchor_falls_back_to_naming() {
    // 无锚 + 有前缀 leader → 前缀 spare。
    let with_leader = sessions_to_kill(
        &names(&["team-agent-leader-codex-ws-cafe", "team-x"]),
        &BTreeSet::new(),
    );
    assert!(matches!(with_leader, KillDecision::KillIndividually { .. }));
    // 无锚 + 无前缀(真损坏且 in_tmux 无前缀)→ 无 spare → 独享拆(退化兜底,与历史一致)。
    assert_eq!(
        sessions_to_kill(&names(&["team-x"]), &BTreeSet::new()),
        KillDecision::KillServerExclusive
    );
}

// RC-3(共享 socket):目标 2 session + 用户 1 session(锚)→ 只 kill 目标 2,用户存活,不整 server 拆。
#[test]
fn rc3_shared_socket_kills_only_target_sessions() {
    let sessions = names(&["team-a", "team-b", "user-shell"]);
    let anchor = anchors(&["user-shell"]);
    let decision = sessions_to_kill(&sessions, &anchor);
    match decision {
        KillDecision::KillIndividually { to_kill, spared } => {
            assert_eq!(to_kill, names(&["team-a", "team-b"]));
            assert_eq!(spared, names(&["user-shell"]));
        }
        other => panic!("shared socket must not whole-server kill, got {other:?}"),
    }
}

// 并集语义:同一 session 既前缀又锚 → spare 一次(不重复)。
#[test]
fn union_prefix_and_anchor_no_double_count() {
    let sessions = names(&["team-agent-leader-claude-ws-beef"]);
    let anchor = anchors(&["team-agent-leader-claude-ws-beef"]);
    let decision = sessions_to_kill(&sessions, &anchor);
    assert_eq!(
        decision,
        KillDecision::KillIndividually { to_kill: vec![], spared: names(&["team-agent-leader-claude-ws-beef"]) }
    );
}
