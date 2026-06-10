use super::*;
use super::launch_spawn::{
    quick_start_team_dir, seed_healthy_coordinator, DELEG_ROLE_ALPHA, DELEG_ROLE_BRAVO,
    QS_VALID_ROLE,
};
use crate::transport::test_support::OfflineTransport;

/// A no-owner workspace (= self-contained team dir) with a compiled 2-agent spec (alpha, bravo) + state
/// listing both at `status`. ensure_owner_allowed passes (no team_owner); load_spec finds alpha/bravo.
pub(super) fn lanea_team_ws(status: &str) -> PathBuf {
    let ws = temp_ws().join("laneateam");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(ws.join("TEAM.md"), "---\nname: laneateam\nobjective: Lane A probe.\nprovider: codex\n---\n\nteam.\n").unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile lane-A team");
    // Re-point routing/tasks at the STAYING agent `bravo` so removing `alpha` validates cleanly (golden
    // remove_agent removes from agents+startup_order then validate_spec raises on dangling refs — a routed
    // agent is not removable in golden; alpha here stands in for an unrouted/dynamic worker). See lanea_ws_agents.
    let yaml = crate::model::yaml::dumps(&spec)
        .replace("default_assignee: \"alpha\"", "default_assignee: \"bravo\"")
        .replace("assign_to: \"alpha\"", "assign_to: \"bravo\"")
        .replace("assignee: \"alpha\"", "assignee: \"bravo\"");
    assert!(!yaml.contains("default_assignee: \"alpha\""), "fixture unroute: default_assignee still alpha");
    assert!(!yaml.contains("assign_to: \"alpha\""), "fixture unroute: a routing rule still assign_to alpha");
    assert!(!yaml.contains("assignee: \"alpha\""), "fixture unroute: task still assignee alpha");
    std::fs::write(ws.join("team.spec.yaml"), yaml).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-laneateam",
            "agents": {
                "alpha": {"status": status, "provider": "codex", "window": "alpha"},
                "bravo": {"status": status, "provider": "codex", "window": "bravo"}
            }
        }),
    )
    .unwrap();
    ws
}

// remove_agent [P0] — from_spec + force on a NON-running agent atomically removes it from state.agents
// (golden agents.py: pop agents[agent_id] + save). Pure fs/state (non-running -> no stop/tmux). Today the
// stub returns OwnerRefused and removes nothing -> RED.
#[test]
fn lanea_remove_agent_from_spec_force_removes_from_state() {
    let ws = lanea_team_ws("stopped"); // non-running -> the running+force stop branch is skipped (no tmux)
    let _ = remove_agent(&ws, &aid("alpha"), true, true, None); // from_spec=true, force=true
    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    let agents = state.get("agents").and_then(serde_json::Value::as_object);
    assert!(
        agents.is_some_and(|a| !a.contains_key("alpha")),
        "remove_agent(from_spec, force) must atomically remove 'alpha' from state.agents (golden agents.py); \
         the stub returns OwnerRefused and removes nothing; state.agents={agents:?}"
    );
}

// remove_agent [P1] — a RUNNING agent removed without --force is RefusedForceRequired (golden
// agents.py:56; _is_running is status-based for status in {running,busy} -> no tmux). Today the stub
// returns OwnerRefused (a different, wrong refusal) -> RED.
#[test]
fn lanea_remove_agent_running_without_force_is_refused_force_required() {
    let ws = lanea_team_ws("running"); // alpha is running (status-based _is_running -> true, no tmux)
    match remove_agent(&ws, &aid("alpha"), true, false, None) {
        Ok(RemoveAgentOutcome::RefusedForceRequired { agent_id }) => assert_eq!(agent_id, aid("alpha")),
        other => panic!(
            "a RUNNING agent removed without --force must be RefusedForceRequired (golden agents.py:56); \
             the stub returns OwnerRefused; got {other:?}"
        ),
    }
}

// stop_agent [P0] — an UNKNOWN agent id (past the owner gate) raises "unknown worker agent id: <id>"
// (golden operations.py:73), NOT OwnerRefused. The owner gate passes on a no-owner ws, then _find_agent
// fails. The check precedes any tmux. Today the stub returns OwnerRefused (never loads the spec) -> RED.
#[test]
fn lanea_stop_agent_unknown_agent_is_unknown_worker_not_owner_refused() {
    let ws = lanea_team_ws("stopped");
    let text = format!("{:?}", stop_agent(&ws, &aid("ghost"), None));
    assert!(
        text.contains("unknown worker"),
        "stop_agent past the owner gate must raise 'unknown worker agent id: ghost' for an unknown agent \
         (golden operations.py:73), not OwnerRefused; got {text}"
    );
}

// fork_agent [P0] — precedence: an UNKNOWN source is rejected as "unknown worker agent id" BEFORE the
// source-session-id check (golden operations.py:284-25). Today the stub always returns "source session_id
// is missing" (never loads the spec) -> RED.
#[test]
fn lanea_fork_agent_unknown_source_is_unknown_worker_before_session_check() {
    let ws = lanea_team_ws("stopped");
    let text = format!("{:?}", fork_agent(&ws, &aid("ghost"), &aid("newfork"), None, false, None));
    assert!(
        text.contains("unknown worker"),
        "fork_agent must reject an UNKNOWN source as 'unknown worker agent id: ghost' BEFORE the session-id \
         check (golden precedence); the stub returns 'source session_id is missing'; got {text}"
    );
}

// fork_agent [P0] — precedence: a DUPLICATE fork-target id is rejected as "agent id already exists" BEFORE
// the source-session-id check (golden operations.py:284-19, the FIRST guard). Today the stub returns
// "source session_id is missing" -> RED.
#[test]
fn lanea_fork_agent_duplicate_target_is_already_exists_before_session_check() {
    let ws = lanea_team_ws("stopped");
    // target 'bravo' already exists in the spec -> duplicate; source 'alpha' exists (its session_id is irrelevant).
    let text = format!("{:?}", fork_agent(&ws, &aid("alpha"), &aid("bravo"), None, false, None));
    assert!(
        text.contains("already exists"),
        "fork_agent must reject a DUPLICATE target 'bravo' as 'agent id already exists' BEFORE the session-id \
         check (golden precedence, the first guard); the stub returns 'source session_id is missing'; got {text}"
    );
}

// SEAM-GATED real-machine boundary (note to porter): stop_agent / reset_agent / fork_agent must drive a
// transport (kill_window / re-spawn / native-fork spawn). The porter adds stop_agent_with_transport /
// reset_agent_with_transport / fork_agent_with_transport(.., &dyn Transport) (mirror restart_with_transport)
// so kill_window / spawn are assertable in-process via a recording transport. Until then the PUBLIC fns
// hit real tmux -> #[ignore]. This documents the stop_agent kill_window + mark-stopped observable.
#[test]
#[ignore = "real-machine: stop_agent kills the real tmux window. PORTER SEAM: stop_agent_with_transport(.., \
            &dyn Transport) so kill_window(session:window) + agents[a].status='stopped' is assertable in-process."]
fn lanea_stop_agent_kills_window_and_marks_stopped() {
    let ws = lanea_team_ws("running");
    let _ = stop_agent(&ws, &aid("alpha"), None); // real machine: tmux kill-window team-laneateam:alpha
    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    let status = state.pointer("/agents/alpha/status").and_then(serde_json::Value::as_str);
    assert_eq!(status, Some("stopped"), "stop_agent must mark agents[alpha].status='stopped' after killing the window");
}

// ═════════════════════════════════════════════════════════════════════════
// rt-host-a P1 — `add-agent w2` recompiles + spawns w2 but does NOT JOIN it to the LIVE team: (a) the
// running roster (state.agents at team_workspace) stays ['w1'] — add_agent upserts w2 into the team-DIR
// workspace, not the running workspace; (b) w2 is spawned detached / not spawn_into the existing team
// session. Coordinator can't deliver to w2 -> `send w2` never round-trips. Golden lifecycle/operations.py:
// add_agent -> start_agent(allow_fresh) spawns INTO the team session + the agent is in runtime state.
// ═════════════════════════════════════════════════════════════════════════

// RED — add_agent_with_transport over a seeded RUNNING team must JOIN w2: (1) the RUNNING roster
// (load_runtime_state(team_workspace)) gains "w2", AND (2) the transport records a spawn_INTO (not
// spawn_first) so w2 joins the existing team session. Today the roster stays ['w1'] (w2 written to the
// wrong workspace) and start_agent_with_transport is a stub (zero spawns) -> RED. OS-safe: recording
// transport (no real tmux) + seeded-healthy-coordinator (start_coordinator AlreadyRunning).
#[test]
fn add_agent_joins_w2_into_running_roster_and_existing_session() {
    let team_dir = quick_start_team_dir(QS_VALID_ROLE); // <base>/teamdir (agents/implementer.md)
    let workspace = team_dir.parent().expect("team_workspace(team_dir) = parent"); // the RUNNING team's workspace
    // a RUNNING team already in the session (roster = ['w1'], session_name set).
    crate::state::persist::save_runtime_state(
        workspace,
        &json!({
            "session_name": "team-implteam",
            "agents": {"w1": {"status": "running", "provider": "codex", "window": "w1"}}
        }),
    )
    .unwrap();
    seed_healthy_coordinator(workspace); // start_coordinator -> AlreadyRunning (no real daemon)
    // the new agent's role file, OUTSIDE agents/ so it's not a duplicate of an existing agent.
    let role_file = team_dir.join("w2-role.md");
    std::fs::write(
        &role_file,
        "---\nname: w2\nrole: Worker Two\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker two.\n",
    )
    .unwrap();
    let transport = OfflineTransport::new().with_session_present(true);

    let _ = add_agent_with_transport(team_dir.as_path(), &aid("w2"), &role_file, false, None, &transport);

    // (1) [load-bearing] the RUNNING roster (team_workspace) must now contain w2.
    let state = crate::state::persist::load_runtime_state(workspace).expect("load running state");
    assert!(
        state.pointer("/agents/w2").is_some(),
        "add-agent must JOIN w2 to the RUNNING roster (state.agents at team_workspace); today w2 is upserted \
         into the team-DIR workspace, not the running workspace, so the roster stays ['w1'] and the \
         coordinator can't deliver to w2; running agents={:?}",
        state.get("agents")
    );
    // (2) w2 must spawn_INTO the existing team session (not a detached/new session via spawn_first).
    let recorded = transport.spawn_records();
    assert!(
        recorded.iter().any(|(kind, _)| kind == "spawn_into"),
        "add-agent must spawn w2 INTO the existing team session (spawn_into); today start_agent_with_transport \
         is a stub -> ZERO spawns recorded; got {recorded:?}"
    );
    assert!(
        !recorded.iter().any(|(kind, _)| kind == "spawn_first"),
        "w2 must NOT create a NEW session (spawn_first) — that's the detached-spawn bug; got {recorded:?}"
    );
}

// REAL-MACHINE e2e boundary (rt-host-a verifies): after `add-agent w2`, the coordinator delivers
// `send w2` and w2 reports a result (full round-trip). #[ignore] — needs a live tmux session + worker.
#[test]
#[ignore = "real-machine: add-agent then send w2 round-trips"]
fn add_agent_then_send_w2_round_trips() {
    // The framework asserts: add-agent w2 -> w2 in the live session + roster -> `send w2 <msg>` ->
    // coordinator delivers -> w2 emits a result_envelope (results row). Not unit-testable in-process.
}

#[derive(Clone)]
struct SocketRecordingRunner {
    recorded: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

impl crate::tmux_backend::CommandRunner for SocketRecordingRunner {
    fn run(&self, argv: &[String]) -> Result<crate::tmux_backend::CommandOutput, std::io::Error> {
        self.recorded.lock().unwrap().push(argv.to_vec());
        Ok(crate::tmux_backend::CommandOutput {
            success: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

fn socket_for_workspace(workspace: &std::path::Path) -> String {
    use crate::transport::Transport as _;
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let runner = SocketRecordingRunner { recorded: std::sync::Arc::clone(&recorded) };
    let backend = crate::tmux_backend::TmuxBackend::with_runner_for_workspace(
        Box::new(runner),
        workspace,
    );
    backend.has_session(&sess("team-implteam")).unwrap();
    let socket = recorded.lock().unwrap()[0][2].clone();
    socket
}

// BUG A / CP-1: public lifecycle handlers such as stop_agent receive either the run workspace or
// the team dir, but the daemon is bound to the run workspace socket. A team-dir raw for_workspace
// derives a different -L socket and makes stop_agent silently see no windows.
#[test]
fn bug_a_team_dir_lifecycle_socket_matches_daemon_run_workspace_socket() {
    let run_ws = temp_ws();
    std::fs::create_dir_all(crate::model::paths::runtime_dir(&run_ws)).unwrap();
    let team_dir = run_ws.join("agents");
    std::fs::create_dir_all(&team_dir).unwrap();

    let daemon_socket = socket_for_workspace(&run_ws);
    let lifecycle_ws = crate::lifecycle::restart::lifecycle_run_workspace(&team_dir).unwrap();
    let stop_socket = socket_for_workspace(&lifecycle_ws);
    assert_eq!(
        stop_socket, daemon_socket,
        "team-dir lifecycle ops must derive the same -L socket as the daemon run workspace"
    );
    assert_ne!(
        socket_for_workspace(&team_dir),
        daemon_socket,
        "regression guard: raw team_dir for_workspace is the buggy socket"
    );
}

#[test]
#[ignore = "real-machine: stop_agent with a team-dir input kills an existing worker window on the daemon -L socket"]
fn bug_a_stop_agent_team_dir_input_kills_existing_window_real_machine() {
    let team_dir = quick_start_team_dir(QS_VALID_ROLE);
    let report = stop_agent(&team_dir, &aid("w1"), None).expect("real stop-agent");
    assert!(report.stopped, "existing worker window must be killed, not silently reported absent");
}

// ═════════════════════════════════════════════════════════════════════════
// WAVE-2 · LANE A v2 — DEEPENED byte-parity REDs (stop / reset / remove / fork).
//
// The shallow 5 lanea_ tests pass, but the port is NOT byte-parity. /tmp/lanea_blockers.json caught
// 15 CONFIRMED blockers + 2 warns. These lock the GOLDEN observable for each.
// Golden (truth source): lifecycle/operations.py (stop:62 / reset:102 / fork:284),
// lifecycle/agents.py (remove:22 + _RemoveRollback + _is_running + _find_worker),
// runtime.py:1023 _tmux_window_exists, display/close.py (close_ghostty_workspace_slot:51).
//
// Transport-driven via LaneTransport (records kill_window + spawns; list_windows/list_targets answer
// from a configurable window set = golden's _tmux_window_exists primitive). OS-safe (no real tmux;
// seeded-healthy coordinator where start_coordinator is reached). Rollback-internal bits that need a
// production failure-injection seam (agent_health re-upsert; fork post-spawn arms) are #[ignore]

// ═══════════════════════════════════════════════════════════════════════════
// Wave3 EVENT PAYLOAD BYTE-LOCK — lifecycle verbs must write golden's events.jsonl payloads.
// events.jsonl = json.dumps({ts, event, **fields}, sort_keys=True) (event_log.rs:4 / golden events.py:35)
// → byte form has ALPHABETICALLY-sorted keys (NOT insertion order; that is state.json's rule). So the
// byte-lock = event name + field KEY SET + field VALUES (ts is a live timestamp → tolerated; order is
// sort_keys-deterministic given keys+values). Rust restart/remove.rs + restart/agent.rs write ZERO
// lifecycle events (crate-wide grep: the names exist in types.rs but are never emitted) -> RED.
// ═══════════════════════════════════════════════════════════════════════════

// Read every events.jsonl the verb could write to (run-workspace resolves to either the team dir or its
// parent; read both so the lock is robust to that resolution).
fn lifecycle_events(ws: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out = crate::event_log::EventLog::new(ws).tail(0).unwrap_or_default();
    if let Ok(parent) = crate::model::paths::team_workspace(ws) {
        if parent != ws {
            out.extend(crate::event_log::EventLog::new(&parent).tail(0).unwrap_or_default());
        }
    }
    out
}

fn payload_keys(event: &serde_json::Value) -> std::collections::BTreeSet<String> {
    event
        .as_object()
        .map(|o| o.keys().filter(|k| *k != "ts" && *k != "event").cloned().collect())
        .unwrap_or_default()
}

fn find_event<'a>(events: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    events.iter().find(|e| e.get("event").and_then(|v| v.as_str()) == Some(name))
}

fn names(events: &[serde_json::Value]) -> Vec<String> {
    events.iter().filter_map(|e| e.get("event").and_then(|v| v.as_str()).map(ToString::to_string)).collect()
}

// remove-agent — golden agents.py:66-147 writes lifecycle.remove_step_completed (per step) +
// remove_agent.complete. (stopped agent + from_spec + force: no stop step, pure fs/state, no spawn.)
#[test]
fn remove_agent_emits_golden_lifecycle_event_payloads() {
    let ws = lanea_team_ws("stopped");
    let _ = remove_agent(&ws, &aid("alpha"), true, true, None); // from_spec=true, force=true
    let events = lifecycle_events(&ws);

    // remove_agent.complete (golden agents.py:140-147) — EXACT field key set + load-bearing values.
    let complete = find_event(&events, "remove_agent.complete").unwrap_or_else(|| panic!(
        "remove_agent must write `remove_agent.complete` (golden agents.py:140); Rust restart/remove.rs emits NO \
         events. events seen: {:?}", names(&events)
    ));
    let expected: std::collections::BTreeSet<String> =
        ["agent_id", "from_spec", "force", "stopped", "role_file_removed", "cleared_locations"]
            .iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(complete), expected,
        "remove_agent.complete payload key set must match golden agents.py:140-147; got {:?}", payload_keys(complete));
    assert_eq!(complete.get("agent_id").and_then(|v| v.as_str()), Some("alpha"));
    assert_eq!(complete.get("from_spec").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(complete.get("force").and_then(|v| v.as_bool()), Some(true));

    // lifecycle.remove_step_completed (golden agents.py:66-72) — fired per step; key set {agent_id, step,
    // resource}; the workspace_state + agent_health steps fire for a stopped from_spec remove.
    let step_events: Vec<&serde_json::Value> = events.iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("lifecycle.remove_step_completed")).collect();
    let steps: Vec<&str> = step_events.iter().filter_map(|e| e.get("step").and_then(|v| v.as_str())).collect();
    assert!(steps.contains(&"workspace_state") && steps.contains(&"agent_health"),
        "remove_agent must write lifecycle.remove_step_completed for each step (golden agents.py:86,109); got steps {steps:?}");
    let ws_step = step_events.iter().find(|e| e.get("step").and_then(|v| v.as_str()) == Some("workspace_state"))
        .expect("the workspace_state step event");
    let expected_step: std::collections::BTreeSet<String> =
        ["agent_id", "step", "resource"].iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(ws_step), expected_step,
        "lifecycle.remove_step_completed (non-stop step) payload must be EXACTLY {{agent_id, step, resource}} (golden agents.py:68-70)");
    assert_eq!(ws_step.get("resource").and_then(|v| v.as_str()), Some("state.json:agents"),
        "workspace_state step resource == golden 'state.json:agents' (agents.py:82)");
}

// reset-agent — golden operations.py:123/132 writes discard.session_tombstone {agent_id,
// discarded_session_id} + reset_agent.complete {agent_id, stopped, started}. OfflineTransport (no spawn).
#[test]
fn reset_agent_emits_golden_lifecycle_event_payloads() {
    let ws = lanea_team_ws("running");
    // give alpha a stored session so discard.session_tombstone.discarded_session_id is meaningful (golden operations.py:118).
    let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
    state["agents"]["alpha"]["session_id"] = json!("S-alpha");
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();

    let transport = OfflineTransport::new().with_session_present(true);
    let _ = crate::lifecycle::reset_agent_with_transport(&ws, &aid("alpha"), true, false, None, &transport);
    let events = lifecycle_events(&ws);

    // discard.session_tombstone (golden operations.py:123) — EXACT key set + the discarded session id.
    let tombstone = find_event(&events, "discard.session_tombstone").unwrap_or_else(|| panic!(
        "reset_agent must write `discard.session_tombstone` (golden operations.py:123); Rust restart/agent.rs emits \
         NO events. events seen: {:?}", names(&events)
    ));
    let expected_tomb: std::collections::BTreeSet<String> =
        ["agent_id", "discarded_session_id"].iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(tombstone), expected_tomb,
        "discard.session_tombstone payload key set must be EXACTLY {{agent_id, discarded_session_id}} (golden operations.py:123)");
    assert_eq!(tombstone.get("agent_id").and_then(|v| v.as_str()), Some("alpha"));
    assert_eq!(tombstone.get("discarded_session_id").and_then(|v| v.as_str()), Some("S-alpha"),
        "discarded_session_id == the agent's stored session_id (golden operations.py:118)");

    // reset_agent.complete (golden operations.py:132) — EXACT key set {agent_id, stopped, started}.
    let complete = find_event(&events, "reset_agent.complete").unwrap_or_else(|| panic!(
        "reset_agent must write `reset_agent.complete` (golden operations.py:132); events seen: {:?}", names(&events)
    ));
    let expected_complete: std::collections::BTreeSet<String> =
        ["agent_id", "stopped", "started"].iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(complete), expected_complete,
        "reset_agent.complete payload key set must be EXACTLY {{agent_id, stopped, started}} (golden operations.py:132)");
    assert_eq!(complete.get("agent_id").and_then(|v| v.as_str()), Some("alpha"));
}

// stop-agent — golden operations.py:98 writes stop_agent.complete {agent_id, target, stopped}
// (+ stop_agent.window_stop_failed {agent_id, target, stderr} on kill failure). Rust emits none.
#[test]
fn stop_agent_emits_golden_complete_event_payload() {
    let ws = lanea_team_ws("running"); // alpha running (window "alpha")
    let transport = OfflineTransport::new().with_session_present(true);
    let _ = crate::lifecycle::stop_agent_with_transport(&ws, &aid("alpha"), None, &transport);
    let events = lifecycle_events(&ws);

    let complete = find_event(&events, "stop_agent.complete").unwrap_or_else(|| panic!(
        "stop_agent must write `stop_agent.complete` (golden operations.py:98); Rust restart/agent.rs emits NO \
         events for stop. events seen: {:?}", names(&events)
    ));
    let expected: std::collections::BTreeSet<String> =
        ["agent_id", "target", "stopped"].iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(complete), expected,
        "stop_agent.complete payload key set must be EXACTLY {{agent_id, target, stopped}} (golden operations.py:98)");
    assert_eq!(complete.get("agent_id").and_then(|v| v.as_str()), Some("alpha"));
}

// start-agent — golden start.py:225 writes start_agent.agent_start on the spawn path (9 keys). Rust emits
// ONLY start_agent.noop (common.rs:315, for the already-running path); the fresh-spawn path emits nothing.
#[test]
fn start_agent_emits_golden_agent_start_event_payload() {
    let ws = lanea_team_ws("stopped"); // not running -> fresh spawn path (not noop)
    let transport = OfflineTransport::new();
    let _ = crate::lifecycle::start_agent_with_transport(&ws, &aid("alpha"), true, false, true, None, &transport);
    let events = lifecycle_events(&ws);

    let agent_start = find_event(&events, "start_agent.agent_start").unwrap_or_else(|| panic!(
        "start_agent (fresh-spawn) must write `start_agent.agent_start` (golden start.py:225); Rust emits only \
         start_agent.noop. events seen: {:?}", names(&events)
    ));
    let expected: std::collections::BTreeSet<String> = [
        "agent_id", "provider", "start_mode", "session_id", "session", "window",
        "tmux_start_mode", "command", "mcp_config",
    ].iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(agent_start), expected,
        "start_agent.agent_start payload key set must match golden start.py:225-235 (9 keys)");
    assert_eq!(agent_start.get("agent_id").and_then(|v| v.as_str()), Some("alpha"));
    assert_eq!(agent_start.get("window").and_then(|v| v.as_str()), Some("alpha"),
        "agent_start.window == agent_id (golden start.py:232)");
}

// restart — golden orchestration.py:507 writes ONE restart.resume_decision per non-paused worker
// (7 keys, NOTE `worker_id` not `agent_id`). Rust emits no restart events.
#[test]
fn restart_emits_golden_resume_decision_event_payload() {
    let ws = lanea_team_ws("running"); // alpha + bravo present -> a resume_decision each
    let transport = OfflineTransport::new().with_session_present(true);
    let _ = crate::lifecycle::restart_with_transport(&ws, true, None, &transport);
    let events = lifecycle_events(&ws);

    let decisions: Vec<&serde_json::Value> = events.iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("restart.resume_decision")).collect();
    assert!(!decisions.is_empty(), "restart must write one `restart.resume_decision` per non-paused worker \
        (golden orchestration.py:128/507); Rust emits NO restart events. events seen: {:?}", names(&events));
    let expected: std::collections::BTreeSet<String> = [
        "worker_id", "has_first_send_at", "has_session_id", "allow_fresh", "decision", "first_send_at", "session_id",
    ].iter().map(ToString::to_string).collect();
    assert_eq!(payload_keys(decisions[0]), expected,
        "restart.resume_decision payload key set must match golden orchestration.py:507-514 (7 keys, `worker_id`)");
    let worker_ids: Vec<&str> = decisions.iter().filter_map(|e| e.get("worker_id").and_then(|v| v.as_str())).collect();
    assert!(worker_ids.contains(&"alpha") || worker_ids.contains(&"bravo"),
        "restart.resume_decision.worker_id must name a real worker (golden); got {worker_ids:?}");
}
