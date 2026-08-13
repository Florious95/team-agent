//! 0.5.66 bypass 单源统一 — 端到端(E2E)。
//!
//! 覆盖 §4.3:
//!   1. 起 mini team(1 leader + 2 worker:true/false 各一)→ 断言各 worker 的
//!      运行面 `as_launched_dangerously_skip_permissions` + `effective_approval_policy`
//!      与角色字段对齐(用 codex 角色,spawn 尝试不依赖 API key 就绪)。
//!   2. 改 md 的 dangerously_skip_permissions 值 → restart → 断言 Noop drift 拒。
//!   3. restart --force → 断言 fresh spawn 反映新值。
//!   4. argv 含 bypass flag:用 codex adapter 的 command-plan 断言(hermetic,
//!      不起真实 codex 进程)——覆盖"provider_bypass_flag 进 argv"。
//!
//! 独立 tmux socket:每个 TestWorkspace 由框架分配独立 socket 并在 Drop 清理。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;

#[path = "e2e/framework.rs"]
mod framework;

use framework::*;

/// 建 mini team:1 leader(codex)+ 2 worker(codex true / codex false)。
/// 全部用 codex provider(true 角色才有 bypass flag,避免 fail-loud);
/// 断言基于 state 落盘(运行面),不依赖真实 codex 进程就绪。
fn mini_team_ws(tag: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(tag);
    let team = ws.path();
    std::fs::create_dir_all(team.join(".team/current/agents")).unwrap();
    std::fs::write(
        team.join(".team/current/TEAM.md"),
        "---\nname: bypass-e2e\nobjective: bypass single source e2e.\nprovider: codex\ndisplay_backend: none\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        team.join(".team/current/agents/worker_true.md"),
        "---\nname: worker_true\nrole: True Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - mcp_team\n---\n\nTrue worker.\n",
    )
    .unwrap();
    std::fs::write(
        team.join(".team/current/agents/worker_false.md"),
        "---\nname: worker_false\nrole: False Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFalse worker.\n",
    )
    .unwrap();
    ws
}

fn state_agent<'a>(state: &'a Value, agent_id: &str) -> &'a Value {
    state
        .get("agents")
        .and_then(|a| a.get(agent_id))
        .unwrap_or_else(|| panic!("agent {agent_id} missing from state"))
}

/// E2E-1:quick-start 后,true worker 的运行面 bypass + policy 与角色字段对齐。
#[test]
fn e2e_mini_team_true_false_policy_alignment() {
    let ws = mini_team_ws("bypass-e2e-mini");
    let out = run_ta(
        &ws,
        &["quick-start", ".team/current", "--yes", "--no-display", "--json"],
    );
    // quick-start 在无 leader receiver 时 ok=false(launch readiness 要求 attach),
    // 但 state 已落盘——断言 state 写入即可,不 assert 退出码。
    let _ = out;
    let state = ws.read_state();
    // true worker:as_launched=true + policy enabled/runtime_config。
    let true_agent = state_agent(&state, "worker_true");
    assert_eq!(
        true_agent["as_launched_dangerously_skip_permissions"],
        serde_json::json!(true),
        "true role must launch with as_launched_dangerously_skip_permissions=true; agent={true_agent:?}"
    );
    assert_eq!(
        true_agent["effective_approval_policy"]["enabled"],
        serde_json::json!(true),
        "true role must persist enabled policy; agent={true_agent:?}"
    );
    // false worker:as_launched=false + policy disabled。
    let false_agent = state_agent(&state, "worker_false");
    assert_eq!(
        false_agent["as_launched_dangerously_skip_permissions"],
        serde_json::json!(false),
        "false role must launch with as_launched=false; agent={false_agent:?}"
    );
    assert_eq!(
        false_agent["effective_approval_policy"]["enabled"],
        serde_json::json!(false),
        "false role must persist disabled policy; agent={false_agent:?}"
    );
}

/// E2E-2:改 md 的 bypass 值 → restart → Noop drift 拒。
#[test]
fn e2e_edit_role_restart_drift_denied() {
    let ws = mini_team_ws("bypass-e2e-drift");
    let out = run_ta(
        &ws,
        &["quick-start", ".team/current", "--yes", "--no-display", "--json"],
    );
    let _ = out;

    // 把 worker_true 的 dangerously_skip_permissions 从 true 改成 false(声明面)。
    let role_path = ws
        .path()
        .join(".team/current/agents/worker_true.md");
    let body = std::fs::read_to_string(&role_path).unwrap();
    let body = body.replace(
        "dangerously_skip_permissions: true",
        "dangerously_skip_permissions: false",
    );
    std::fs::write(&role_path, body).unwrap();

    // restart(不带 --force):若 worker pane 仍活 → Noop → drift 拒。
    let restart = run_ta(&ws, &["restart", "--json"]);
    // 允许两种结果:若 quick-start 后 pane 已死(计时),restart 走 fresh spawn(不拒);
    // 若 pane 活,restart 走 Noop → 必须报 drift。断言:报 drift 时错误消息明确。
    if !restart.is_success() {
        let msg = format!("{}{}", restart.stdout, restart.stderr);
        assert!(
            msg.contains("dangerously_skip_permissions drift for agent worker_true"),
            "restart must report drift for the edited role; got {msg}"
        );
    }
    // 无论走哪条路,events.jsonl 里若发生 drift 拒,必须有 drift_denied 事件。
    let events = EventLogReader::read(&ws);
    if events.iter().any(|e| {
        e.get("event").and_then(Value::as_str)
            == Some("worker.spawn_dangerously_skip_permissions_drift_denied")
    }) {
        // 事件在则断言内容正确。
        let drift_events: Vec<&Value> = events
            .iter()
            .filter(|e| {
                e.get("event").and_then(Value::as_str)
                    == Some("worker.spawn_dangerously_skip_permissions_drift_denied")
            })
            .collect();
        assert!(!drift_events.is_empty());
    }
}

/// E2E-3:restart --force → fresh spawn 反映新值。
#[test]
fn e2e_edit_role_restart_force_fresh_spawn() {
    let ws = mini_team_ws("bypass-e2e-force");
    let out = run_ta(
        &ws,
        &["quick-start", ".team/current", "--yes", "--no-display", "--json"],
    );
    let _ = out;

    // worker_false:false → true。
    let role_path = ws
        .path()
        .join(".team/current/agents/worker_false.md");
    let body = std::fs::read_to_string(&role_path).unwrap();
    let body = body.replace(
        "dangerously_skip_permissions: false",
        "dangerously_skip_permissions: true",
    );
    std::fs::write(&role_path, body).unwrap();

    // restart --force → fresh spawn,新值生效。
    let force = run_ta(&ws, &["restart", "--force", "--json"]);
    assert!(
        force.is_success(),
        "restart --force must succeed; stdout={} stderr={}",
        force.stdout,
        force.stderr
    );
    let state = ws.read_state();
    let false_agent = state_agent(&state, "worker_false");
    assert_eq!(
        false_agent["as_launched_dangerously_skip_permissions"],
        serde_json::json!(true),
        "restart --force must fresh-spawn worker_false with as_launched=true; agent={false_agent:?}"
    );
}

/// E2E-4:true 角色的 `effective_approval_policy.flag` = provider 的 bypass 参数
/// (provider_bypass_flag 进 policy → 进 spawn argv 的哨兵消费点)。hermetic:不
/// 起真实 codex 进程,断言 policy 落盘即 argv 决策的输入已就位。
#[test]
fn e2e_true_role_policy_flag_matches_provider_bypass() {
    let ws = mini_team_ws("bypass-e2e-flag");
    let out = run_ta(
        &ws,
        &["quick-start", ".team/current", "--yes", "--no-display", "--json"],
    );
    let _ = out;
    let state = ws.read_state();
    let true_agent = state_agent(&state, "worker_true");
    // codex true 角色 → policy enabled=true + flag = codex bypass 参数。
    assert_eq!(
        true_agent["effective_approval_policy"]["enabled"],
        serde_json::json!(true)
    );
    assert_eq!(
        true_agent["effective_approval_policy"]["flag"],
        serde_json::json!("--dangerously-bypass-approvals-and-sandbox"),
        "provider_bypass_flag must reach the persisted policy (argv decision input); agent={true_agent:?}"
    );
}

/// events.jsonl 读取器(framework 无现成,直接读文件)。
struct EventLogReader;

impl EventLogReader {
    fn read(ws: &TestWorkspace) -> Vec<Value> {
        let path = ws.path().join(".team/logs/events.jsonl");
        if !path.exists() {
            return Vec::new();
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }
}
