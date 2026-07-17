//! MED A-batch (3/3): slice A-7 — MCP tool contract severing.
//!
//! Triage doc (sole basis): `.team/artifacts/med-triage-fixed-failure-sweep.md` §A-7.
//! Python truth source: 0.2.11.
//!
//! - fork_agent drops `label` — agent_ops.rs:89-91 `let _ = label` vs Python
//!   operations.py:315 `new_agent["role"] = str(label or role or as_agent_id)`:
//!   the label IS the forked agent's new role (it feeds the identity section of the
//!   system prompt — same family as the B2 prompt-soul contract).
//! - send fabricates a nonexistent message_id — tools.rs:194-196 invents
//!   `mcp_<timestamp>` (and a poll_via hint pointing at it) when the delivery outcome
//!   carries no id; Python tools.py:175-181 only returns accepted+poll_via for a REAL
//!   message_id and otherwise falls back to the compacted direct result. An id that is
//!   not in the store makes `team-agent inbox <id>` a dead end.
//! - stuck_cancel widens unknown alert_type to "all" + wrong wire default —
//!   tools.rs:461-466 `_ => None` (= all three types) and wire.rs:447 default "all"
//!   vs Python scheduler.py:268-273 unknown -> `{ok:false,status:"refused",
//!   reason:"invalid_alert_type"}` and tools.py:351 default alert_type="stuck".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use mcp_sim_harness::McpSimHarness;
use serde_json::{json, Value};
use serial_test::serial;

/// A-7: `stuck_cancel` with an unknown alert_type must refuse with the Python literal
/// `{ok:false,status:"refused",reason:"invalid_alert_type"}` (scheduler.py:268-273),
/// not silently widen to suppressing ALL alert types.
#[test]
#[serial(a7_mcp)]
fn a7_stuck_cancel_unknown_alert_type_must_refuse() {
    let harness = McpSimHarness::new();
    let _coordinator_guard = CoordinatorStopGuard {
        ws: harness.workspace_path().to_path_buf(),
    };
    let mut worker = harness.spawn_mcp_client("worker_b", "teamA");

    let call = worker.call_tool(
        "stuck_cancel",
        json!({"agent_id": "worker_a", "alert_type": "bogus_type"}),
    );

    let mut failures = Vec::new();
    if call.body.get("ok") != Some(&json!(false)) {
        failures.push(format!("ok must be false; body={}", call.body));
    }
    if call.body.get("status") != Some(&json!("refused")) {
        failures.push(format!(
            "status must be the Python literal `refused`; body={}",
            call.body
        ));
    }
    if call.body.get("reason") != Some(&json!("invalid_alert_type")) {
        failures.push(format!(
            "reason must be the Python literal `invalid_alert_type`; body={}",
            call.body
        ));
    }
    assert!(
        failures.is_empty(),
        "A-7 stuck_cancel unknown alert_type contract failed:\n{}",
        failures.join("\n")
    );
}

/// A-7: omitting alert_type must default to "stuck" (Python tools.py:351), not "all" —
/// observable through the returned alert_types list (scheduler result field).
#[test]
#[serial(a7_mcp)]
fn a7_stuck_cancel_default_alert_type_is_stuck() {
    let harness = McpSimHarness::new();
    let _coordinator_guard = CoordinatorStopGuard {
        ws: harness.workspace_path().to_path_buf(),
    };
    let mut worker = harness.spawn_mcp_client("worker_b", "teamA");

    let call = worker.call_tool("stuck_cancel", json!({"agent_id": "worker_a"}));

    assert_eq!(
        call.body.get("alert_types"),
        Some(&json!(["stuck"])),
        "A-7: the MCP default alert_type is `stuck` (Python tools.py:351); the wire \
default `all` (wire.rs:447) silently suppresses every alert family; body={}",
        call.body
    );
}

/// A-7: a send whose delivery outcome carries no real message_id must NOT fabricate an
/// `mcp_<timestamp>` id (Python tools.py:175-181: accepted+poll_via only for a real id;
/// otherwise the compacted direct result). A fabricated id makes the advertised
/// `team-agent inbox <id>` poll a dead end.
#[test]
#[serial(a7_mcp)]
fn a7_send_must_not_fabricate_message_id() {
    let harness = McpSimHarness::new();
    let _coordinator_guard = CoordinatorStopGuard {
        ws: harness.workspace_path().to_path_buf(),
    };
    // A REFUSED delivery (session drift on the recipient — the same refusal the runtime
    // writes, send.rs:449-478) returns ok:false with message_id=None. Combined with a
    // workspace-scoped client (owner_team_id empty) this is exactly the branch where
    // tools.rs:194-196 invents `mcp_<timestamp>` and reports the refused send as
    // accepted with a dead poll_via.
    let mut state = harness.state_value();
    for pointer in [
        "/agents/worker_a/status",
        "/teams/teamA/agents/worker_a/status",
    ] {
        if let Some(status) = state.pointer_mut(pointer) {
            *status = json!("session_drift");
        }
    }
    // Legacy single-team workspace shape (no `teams` map at all — the Python 0.2.11-era
    // state layout): the workspace-scope (owner None) client is only legal then
    // (canonical_owner_team_key refuses owner-less clients whenever `teams` exists).
    if let Some(obj) = state.as_object_mut() {
        obj.remove("teams");
        obj.remove("active_team_key");
    }
    team_agent::state::persist::save_runtime_state(harness.workspace_path(), &state).unwrap();
    let mut worker = harness.spawn_mcp_client("worker_b", "");

    let call = worker.call_tool(
        "send_message",
        json!({"to": "worker_a", "content": "A-7 fabrication probe"}),
    );

    let fabricated = extract_strings(&call.body)
        .into_iter()
        .find(|value| value.starts_with("mcp_"));
    assert!(
        fabricated.is_none(),
        "A-7: send must never invent an `mcp_<timestamp>` message id that does not \
exist in the store (Python tools.py:175-181 falls back to the direct result instead); \
fabricated={fabricated:?} body={} raw={}",
        call.body,
        call.raw
    );
}

/// A-7: fork_agent's `label` must become the forked agent's role
/// (Python operations.py:315 `new_agent["role"] = str(label or role or as_agent_id)`).
#[test]
#[serial(a7_mcp)]
fn a7_fork_agent_label_becomes_new_role() {
    let harness = McpSimHarness::new();
    let _coordinator_guard = CoordinatorStopGuard {
        ws: harness.workspace_path().to_path_buf(),
    };
    seed_forkable_source(&harness);
    let sibling_before = harness
        .state_value()
        .pointer("/teams/teamB")
        .cloned()
        .expect("fixture seeds sibling teamB");
    // Fork is an owner-side lifecycle op: give the client the owner's identity
    // (caller_identity_from_env reads TEAM_AGENT_LEADER_SESSION_UUID /
    // TEAM_AGENT_LEADER_PANE_ID; the spawned mcp-server child inherits the test
    // process env) so the team owner gate recognizes it as the owner pane.
    let owner_pane = harness
        .state_value()
        .pointer("/team_owner/pane_id")
        .and_then(Value::as_str)
        .expect("harness seeds team_owner.pane_id")
        .to_string();
    // PATH-shim `claude`: the fork spawns the provider argv into a real tmux window;
    // the shim renders the Claude ready marker and idles, so the fork completes
    // deterministically with zero real provider processes.
    let shim_dir = harness.workspace_path().join("bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(
        shim_dir.join("claude"),
        "#!/bin/sh\nprintf 'Claude Code\\n> \\n'\nexec sleep 300\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            shim_dir.join("claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", shim_dir.to_string_lossy(), old_path),
    );
    std::env::set_var("TEAM_AGENT_LEADER_SESSION_UUID", "leader-session-team-a");
    std::env::set_var("TEAM_AGENT_LEADER_PANE_ID", &owner_pane);
    let mut worker = harness.spawn_mcp_client("leader", "teamA");
    std::env::set_var("PATH", &old_path);
    std::env::remove_var("TEAM_AGENT_LEADER_SESSION_UUID");
    std::env::remove_var("TEAM_AGENT_LEADER_PANE_ID");

    let call = worker.call_tool(
        "fork_agent",
        json!({
            "source_agent_id": "worker_a",
            "as_agent_id": "worker_fork",
            "label": "Custom Fork Role",
        }),
    );
    assert!(
        !call.is_error && call.body.get("ok") == Some(&json!(true)),
        "fixture: fork_agent should succeed for the shimmed claude source with a session; \
body={} raw={}",
        call.body,
        call.raw
    );

    let state = harness.state_value();
    let forked = state
        .pointer("/teams/teamA/agents/worker_fork")
        .expect("A-7: fork must be registered in the canonical selected team row");
    let role = forked
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_string);
    assert_eq!(
        role.as_deref(),
        Some("Custom Fork Role"),
        "A-7: label must become the forked agent's role (Python operations.py:315; the \
role feeds the compiled identity prompt — B2 family); state role={role:?} state={state}"
    );
    assert_eq!(
        forked.get("window").and_then(Value::as_str),
        Some("worker_fork"),
        "A-7: canonical team registration must retain the spawned window tuple; forked={forked}"
    );
    assert!(
        forked
            .get("pane_id")
            .and_then(Value::as_str)
            .is_some_and(|pane| pane.starts_with('%')),
        "A-7: canonical team registration must retain the physical tmux pane id; forked={forked}"
    );
    assert_eq!(
        state.pointer("/teams/teamB"),
        Some(&sibling_before),
        "A-7: teamA fork must preserve sibling teamB byte-for-byte"
    );

    let send = worker.call_tool(
        "send_message",
        json!({"to": "worker_fork", "content": "A-7 scoped fork reachability probe"}),
    );
    assert_ne!(
        send.body.get("reason"),
        Some(&json!("target_not_in_team")),
        "A-7: a successfully forked team member must pass the team membership gate immediately; body={} raw={}",
        send.body,
        send.raw
    );
    let rows = harness.message_rows_containing("A-7 scoped fork reachability probe");
    assert!(
        rows.iter()
            .any(|row| row.owner_team_id.as_deref() == Some("teamA")
                && row.recipient == "worker_fork"),
        "A-7: the short-name send must resolve and persist under canonical teamA; rows={rows:?}"
    );
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Make worker_a forkable: shimmed claude provider + a recorded session id + a spec file the fork
/// path can extend (shapes mirror the compiled spec / runtime state of a real team).
fn seed_forkable_source(harness: &McpSimHarness) {
    let ws = harness.workspace_path().to_path_buf();
    let mut state = harness.state_value();
    // 0.4.6 tuple-atomic contract: fork requires the complete source
    // tuple. Seed a real rollout file + captured_at + captured_via so
    // the backing guard passes and the test exercises the fork mechanics
    // it actually asserts.
    let rollout = ws.join("worker_a-rollout.jsonl");
    if !rollout.exists() {
        std::fs::write(&rollout, b"{}\n").unwrap();
    }
    for agent_id in ["worker_a", "worker_b", "worker_c"] {
        let top_pointer = format!("/agents/{agent_id}");
        let team_pointer = format!("/teams/teamA/agents/{agent_id}");
        if let Some(topology) = state.pointer(&top_pointer).cloned() {
            if let Some(agent) = state
                .pointer_mut(&team_pointer)
                .and_then(Value::as_object_mut)
            {
                copy_live_topology(agent, &topology);
            }
        }
    }
    let source_topology = state.pointer("/agents/worker_a").cloned();
    for pointer in ["/agents/worker_a", "/teams/teamA/agents/worker_a"] {
        if let Some(agent) = state.pointer_mut(pointer).and_then(Value::as_object_mut) {
            agent.insert(
                "session_id".to_string(),
                json!("11111111-2222-4333-8444-555555555555"),
            );
            agent.insert("rollout_path".to_string(), json!(rollout.to_string_lossy()));
            agent.insert(
                "captured_at".to_string(),
                json!("2026-06-25T10:00:00+00:00"),
            );
            agent.insert("captured_via".to_string(), json!("session.captured"));
            agent.insert("role".to_string(), json!("Source Worker"));
            agent.insert("provider".to_string(), json!("claude"));
            agent.insert("auth_mode".to_string(), json!("subscription"));
            // Phase C no longer cross-backfills live topology between projections. Keep
            // the forkable source fixture internally consistent so the test exercises
            // fork label semantics rather than a stale source topology conflict.
            if let Some(topology) = source_topology.as_ref() {
                copy_live_topology(agent, topology);
            }
        }
    }
    // Real-machine state carries spec_path/workspace/team_dir on both projections; the
    // MCP lifecycle workspace resolver follows them to the real compiled spec.
    for pointer in ["", "/teams/teamA"] {
        if let Some(entry) = state.pointer_mut(pointer).and_then(Value::as_object_mut) {
            entry.insert(
                "spec_path".to_string(),
                json!(ws.join("team.spec.yaml").to_string_lossy()),
            );
            entry.insert("workspace".to_string(), json!(ws.to_string_lossy()));
            entry.insert(
                "team_dir".to_string(),
                json!(ws.join("teamdir").to_string_lossy()),
            );
        }
    }
    let session_name = state
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or("team-agent")
        .to_string();
    team_agent::state::persist::save_runtime_state(&ws, &state).unwrap();
    // Produce a VALID spec through the real compiler (TEAM.md + role doc), pinning the
    // session_name to the harness tmux session so the fork spawns into it.
    let team_dir = ws.join("teamdir");
    std::fs::create_dir_all(team_dir.join("agents")).unwrap();
    std::fs::write(
        team_dir.join("TEAM.md"),
        format!(
            "---\nname: mcp-sim\nobjective: A-7 fork fixture.\nprovider: claude\nsession_name: {session_name}\n---\n\nTeam fixture.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team_dir.join("agents/worker_a.md"),
        "---\nname: worker_a\nrole: Source Worker\nprovider: claude\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nSource worker.\n",
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&team_dir).expect("compile fixture team");
    std::fs::write(
        ws.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    let loaded = team_agent::model::yaml::loads(
        &std::fs::read_to_string(ws.join("team.spec.yaml")).unwrap(),
    )
    .unwrap();
    team_agent::model::spec::validate_spec(&loaded, &ws).expect("fixture spec must validate");
}

fn copy_live_topology(agent: &mut serde_json::Map<String, Value>, topology: &Value) {
    for field in ["window", "pane_id", "pane_pid", "spawned_at", "spawn_epoch"] {
        if let Some(value) = topology.get(field) {
            agent.insert(field.to_string(), value.clone());
        }
    }
}

/// MCP lifecycle tools may auto-start a coordinator daemon in the temp workspace
/// (e.g. fork reports coordinator_started). Stop it on drop so tests never leak a
/// busy-spinning daemon after the workspace is deleted.
struct CoordinatorStopGuard {
    ws: std::path::PathBuf,
}

impl Drop for CoordinatorStopGuard {
    fn drop(&mut self) {
        let _ = team_agent::coordinator::stop_coordinator(
            &team_agent::coordinator::WorkspacePath::new(self.ws.clone()),
        );
    }
}

fn extract_strings(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                out.extend(extract_strings(item));
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                out.extend(extract_strings(item));
            }
        }
        _ => {}
    }
    out
}
