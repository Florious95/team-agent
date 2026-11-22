#![allow(dead_code)]

//! ---
//! purpose: deterministic synthetic runtime state for resident-memory gates
//! contract:
//!   provides:
//!     - seed_runtime_state(ws, target_bytes): one live team + stopped archive teams
//!   boundary:
//!     - writes only <ws>/.team/runtime/state.json; tmux socket points at a
//!       nonexistent path so no real tmux server is ever contacted
//! maturity: wired
//! ---
//!
//! The shape mirrors a long-lived production workspace: many small objects and
//! strings per agent (about 2.5 KB each), a 60-agent live team and many stopped
//! teams kept in `teams`. Memory amplification depends on node count, so the
//! bulk is made of realistic small nodes, never one large string.

use std::path::Path;

use serde_json::{json, Map, Value};

pub const ACTIVE_TEAM: &str = "memteam";
pub const SENDER: &str = "worker-00";
pub const ACTIVE_AGENTS: usize = 60;
const ARCHIVE_AGENTS: usize = 20;
const TS: &str = "2026-11-01T08:00:00.123456+00:00";
const FIXTURE_SOCKET: &str = "/nonexistent/ta-memory-fixture.sock";

fn agent(ws: &Path, team: &str, idx: usize) -> (String, Value) {
    let id = format!("worker-{idx:02}");
    let ws = ws.display().to_string();
    let lifecycle: Vec<Value> = (0..8)
        .map(|step| {
            json!({
                "ts": TS,
                "event": format!("lifecycle.step_{step}"),
                "epoch": step,
                "source": "coordinator",
            })
        })
        .collect();
    let value = json!({
        "agent_id": id,
        "provider": "codex",
        "auth_mode": "subscription",
        "role": format!("Fixture worker {idx} for resident memory budget gates"),
        "status": "running",
        "window": id,
        "last_window": id,
        "model": "gpt-6.2-sol",
        "model_source": "role_file",
        "dynamic_role_file": format!("{ws}/.team/dynamic-role-files/{team}/{id}.md"),
        "role_source_ownership": "team",
        "explicit_stop": false,
        "as_launched_dangerously_skip_permissions": true,
        "effective_approval_policy": {
            "mode": "bypass",
            "source": "role",
            "sandbox": "danger-full-access",
            "approval": "never",
            "network": true,
            "writable_roots": [ws],
            "captured_at": TS,
        },
        "captured_at": TS,
        "pane_pid": 40_000 + idx,
        "spawned_at": TS,
        "spawn_cwd": ws,
        "mcp_config": format!("{ws}/.team/runtime/mcp/{team}/{id}.json"),
        "spawn_epoch": 1_790_000_000_u64 + idx as u64,
        "owner_team_id": team,
        "capture_state": "captured",
        "runtime_approval_probe": {"status": "clear", "checked_at": TS},
        "last_output_hash": format!("{:064x}", (idx as u128 + 1) * 0x9e37_79b9_7f4a_7c15),
        "last_output_at": TS,
        "coordinator_idle_capture_next_at": TS,
        "activity": {"state": "idle", "since": TS, "source": "capture"},
        "worker_state": "idle",
        "startup_prompt_status": "delivered",
        "startup_prompts": "You are a fixture worker. Follow the leader's instructions and report through MCP tools.",
        "startup_prompt_probe_epoch": format!("epoch-{idx}"),
        "attribution_confidence": "high",
        "captured_via": "session_scan",
        "session_id": format!("019a0000-0000-7000-8000-{idx:012x}"),
        "rollout_path": format!("/nonexistent/sessions/2026/11/01/rollout-{team}-{id}.jsonl"),
        "lifecycle": lifecycle,
    });
    (id, value)
}

fn team(ws: &Path, key: &str, agents: usize, status: &str) -> Value {
    let mut roster = Map::new();
    let mut lifecycle = Map::new();
    for idx in 0..agents {
        let (id, value) = agent(ws, key, idx);
        lifecycle.insert(
            id.clone(),
            json!({"state": "running", "since": TS, "epoch": idx}),
        );
        roster.insert(id, value);
    }
    json!({
        "team_key": key,
        "active_team_key": key,
        "session_name": format!("team-{key}"),
        "status": status,
        "workspace": ws.display().to_string(),
        "leader": {"id": "leader", "provider": "codex", "role": "leader"},
        "agents": Value::Object(roster),
        "agent_lifecycle": Value::Object(lifecycle),
        "tasks": [],
        "transport": {"kind": "tmux", "source": "fixture"},
        "tmux_socket": FIXTURE_SOCKET,
        "tmux_endpoint": FIXTURE_SOCKET,
        "generation": "g1",
        "owner_epoch": 1,
        "team_depth": 0,
    })
}

/// Write `<ws>/.team/runtime/state.json` with one live 60-agent team plus as many
/// stopped 20-agent archive teams as needed to reach `target_bytes`. Returns the
/// on-disk byte size.
pub fn seed_runtime_state(ws: &Path, target_bytes: usize) -> usize {
    let active = team(ws, ACTIVE_TEAM, ACTIVE_AGENTS, "alive");
    let archive_size =
        serde_json::to_vec_pretty(&team(ws, "archive-000", ARCHIVE_AGENTS, "stopped"))
            .expect("serialize archive team")
            .len();
    let active_size = serde_json::to_vec_pretty(&active)
        .expect("serialize active team")
        .len();
    let archives = target_bytes.saturating_sub(2 * active_size) / archive_size;

    let mut teams = Map::new();
    teams.insert(ACTIVE_TEAM.to_string(), active.clone());
    for idx in 0..archives {
        let key = format!("archive-{idx:03}");
        teams.insert(key.clone(), team(ws, &key, ARCHIVE_AGENTS, "stopped"));
    }
    let state = json!({
        "workspace": ws.display().to_string(),
        "team_key": ACTIVE_TEAM,
        "active_team_key": ACTIVE_TEAM,
        "session_name": format!("team-{ACTIVE_TEAM}"),
        "status": "alive",
        "leader": {"id": "leader", "provider": "codex", "role": "leader"},
        "agents": active["agents"].clone(),
        "tasks": [],
        "transport": {"kind": "tmux", "source": "fixture"},
        "tmux_socket": FIXTURE_SOCKET,
        "tmux_endpoint": FIXTURE_SOCKET,
        "generation": "g1",
        "teams": Value::Object(teams),
    });
    let bytes = serde_json::to_vec_pretty(&state).expect("serialize fixture state");
    let runtime = ws.join(".team").join("runtime");
    std::fs::create_dir_all(&runtime).expect("create fixture runtime dir");
    std::fs::write(runtime.join("state.json"), &bytes).expect("write fixture state.json");
    bytes.len()
}
