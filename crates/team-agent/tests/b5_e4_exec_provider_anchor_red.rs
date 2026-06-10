//! E4 真机 grounded RED: `team-agent claude` in_tmux 走 LeaderStartMode::ExecProvider,
//! leader 留在用户原 tmux session(普通 session 名,不带 `team-agent-leader-` 前缀),
//! state.json 把 team_owner.pane_id / leader_receiver.pane_id 记成该 pane。
//!
//! 0.3.5 B5 三犯保护集 augmenter(extend_protection_with_leader_panes)单纯走 session
//! 前缀过滤 → ExecProvider 模式取不到 → user's claude pane 不在保护集 → workspace
//! residual sweep cmdline/cwd 命中即杀(用户真机复发)。
//!
//! E4b team-in-team:子 team state 的 team_owner.pane_id 指父 team worker pane(window
//! 名 = agent id,亦非 leader 前缀)— 同一机制覆盖,任何 team 的 shutdown 都不杀任何
//! team 的 leader 锚 pane(N39/B5 自然推广)。
//!
//! 本契约以 `collect_state_leader_anchor_pane_ids` 单元 + 反向 fixture(state 无锚 →
//! 行为不变,MUST-17 不撒宽)直接验保护集来源逻辑。不需要真 tmux/真 kill。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use team_agent::cli::lifecycle_port::collect_state_leader_anchor_pane_ids;

#[test]
fn e4_exec_provider_state_top_level_team_owner_pane_id_protected() {
    // ExecProvider 真实形态:state.team_owner.pane_id 指普通 session pane(`main:0`
    // 的 %0),session 名 NOT `team-agent-leader-*`。新 augmenter 第二来源必须命中该
    // pane_id;旧 augmenter(只走 session 前缀)会漏。
    let state = json!({
        "session_name": "team-demo",
        "team_owner": {
            "pane_id": "%0",
            "provider": "claude_code"
        },
        "leader_receiver": {
            "pane_id": "%0",
            "provider": "claude_code"
        },
        "agents": {
            "coder": {"pane_id": "%1", "provider": "codex", "status": "running"}
        }
    });
    let anchors = collect_state_leader_anchor_pane_ids(&state);
    assert!(
        anchors.contains("%0"),
        "ExecProvider 模式 leader 锚 pane(state.team_owner.pane_id=%0)必须被 augmenter \
         第二来源识别 — 否则 B5 在 NON-prefix session 下漏覆盖。anchors={anchors:?}"
    );
    assert!(
        !anchors.contains("%1"),
        "worker pane(%1)不是 leader 锚,augmenter 不得撒宽到 worker pane(MUST-17 不过度设计)"
    );
}

#[test]
fn e4b_team_in_team_child_state_anchor_protects_parent_worker_pane() {
    // E4b: 用户在父 team 的 worker pane 起子 team → 子 team 的 leader 承载终端 = 该
    // worker pane(window 名 = "coder" 父 agent id,亦非 leader 前缀)。子 team
    // state.json 把 team_owner.pane_id 记成 worker pane;任何 team 的 shutdown 都不
    // 杀任何 team 的 leader 锚 pane(N39/B5 自然推广)。
    let child_state = json!({
        "session_name": "team-child",
        "team_owner": {
            "pane_id": "%7",            // 父 team worker pane
            "provider": "claude_code"
        },
        "leader_receiver": {
            "pane_id": "%7",
            "provider": "claude_code"
        }
    });
    let anchors = collect_state_leader_anchor_pane_ids(&child_state);
    assert!(
        anchors.contains("%7"),
        "E4b: 子 team state 的 team_owner.pane_id(指父 team worker pane %7)必须被 \
         augmenter 识别为 leader 锚 — 子 team shutdown 不杀该 pane。anchors={anchors:?}"
    );
}

#[test]
fn e4_nested_teams_map_anchor_protected_under_teams_key_per_bug2_scope() {
    // Bug 2 owner team-scope 后,state 顶层之外还有 teams[<key>] 嵌套形态。augmenter
    // 必须扫 teams.<key>.team_owner.pane_id / teams.<key>.leader_receiver.pane_id,
    // 任何 team 的 leader 锚都不漏。
    let state = json!({
        "active_team_key": "alpha",
        "teams": {
            "alpha": {
                "team_owner": {"pane_id": "%3"},
                "leader_receiver": {"pane_id": "%3"}
            },
            "beta": {
                "team_owner": {"pane_id": "%9"},
                "leader_receiver": {"pane_id": "%9"}
            }
        }
    });
    let anchors = collect_state_leader_anchor_pane_ids(&state);
    assert!(
        anchors.contains("%3") && anchors.contains("%9"),
        "嵌套 teams[<key>] 形态:augmenter 必须扫每个 team entry 的 team_owner/\
         leader_receiver.pane_id,任何 team 的 leader 锚都纳入保护。anchors={anchors:?}"
    );
}

#[test]
fn e4_negative_state_without_leader_anchor_yields_empty_set_must17_not_overprotect() {
    // 反向:state.json 无 leader 锚(只有 worker / 无任何 team_owner / 全空对象)→
    // augmenter 第二来源必须返空集 — 行为不变,不撒宽,MUST-17 不过度设计。
    // 这条挡住"任何 worker pane 都被错保"的退化。
    let empty = json!({});
    assert!(
        collect_state_leader_anchor_pane_ids(&empty).is_empty(),
        "空 state augmenter 必返空(否则保护撒太宽)"
    );

    let worker_only = json!({
        "session_name": "team-demo",
        "agents": {
            "coder": {"pane_id": "%5", "provider": "codex", "status": "running"}
        }
    });
    let anchors = collect_state_leader_anchor_pane_ids(&worker_only);
    assert!(
        anchors.is_empty(),
        "state 只有 agents.<id>.pane_id(worker)不应被识别为 leader 锚(MUST-17 不\
         撒宽)。anchors={anchors:?}"
    );

    // team_owner 字段存在但 pane_id 空 → 不算锚
    let empty_pane = json!({
        "team_owner": {"pane_id": "", "provider": "codex"},
        "leader_receiver": {"pane_id": "", "provider": "codex"}
    });
    assert!(
        collect_state_leader_anchor_pane_ids(&empty_pane).is_empty(),
        "team_owner.pane_id 空串不能算锚(state 还没绑定时的占位形态)"
    );
}

#[test]
fn e4_dedup_top_level_and_team_entry_pointing_at_same_pane() {
    // top-level + teams[<key>] 都指同 pane → BTreeSet 自动 dedup,不出现重复 pid。
    // (后续 augmenter 还做 sort + dedup,本测试守 collector 已有 dedup 语义)。
    let state = json!({
        "active_team_key": "alpha",
        "team_owner": {"pane_id": "%0"},
        "leader_receiver": {"pane_id": "%0"},
        "teams": {
            "alpha": {
                "team_owner": {"pane_id": "%0"},
                "leader_receiver": {"pane_id": "%0"}
            }
        }
    });
    let anchors = collect_state_leader_anchor_pane_ids(&state);
    assert_eq!(
        anchors.len(),
        1,
        "同一 pane_id 跨 top-level + teams[<key>] 必须 dedup(BTreeSet 语义)。\
         anchors={anchors:?}"
    );
    assert!(anchors.contains("%0"));
}
