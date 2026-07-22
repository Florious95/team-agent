use super::agent_ops::lanea_team_ws;
use super::lane_ops::{LaneSpawns, LaneTransport};
use super::launch_spawn::{
    restart_ws_two_resumable_workers, seed_healthy_coordinator, DELEG_ROLE_ALPHA, DELEG_ROLE_BRAVO,
};
use super::*;

type RecordedSpawns = LaneSpawns;

// ═════════════════════════════════════════════════════════════════════════
// BUG-1 [highest · regression] — respawn into a DEAD tmux session/`-L` server
// must RE-CREATE the session (new-session / spawn_first), NOT add a window to a
// server that no longer exists (new-window / spawn_into → "no server running" →
// orphaned agent). `reset --discard-session` kills the agent's last window; tmux
// then destroys the whole session AND its CP-1 `-L` server. The respawn that
// follows must therefore new-session.
//
// Golden runtime.py:1017 `_tmux_start_command_for_agent_window`:
//     if _tmux_session_exists(session_name):  # live `tmux has-session -t <name>`
//         return ["tmux","new-window",...], "new-window"
//     return ["tmux","new-session","-d",...], "new-session"
// The new-session-vs-new-window choice is driven by a LIVE `has-session` probe,
// NOT by whether state["session_name"] is a non-empty string.
//
// Today `start_agent_with_transport` computes `into_existing_session =
// session_name_present(&state)` (a pure state-string check; restart.rs:129) and
// NEVER consults `transport.has_session(...)` — so a discarded/dead session
// (state string still set) takes the spawn_into branch. The socket fix
// (0f886a9/4435f9a) covered status/shutdown but missed this lifecycle respawn path.
// ═════════════════════════════════════════════════════════════════════════
/// Recording transport with a CONFIGURABLE `has_session` — models the real
/// `tmux has-session` probe the respawn spawn-decision MUST consult. Records
/// every spawn (kind + argv) like `RecordingLaunchTransport`.
struct SessionProbeRecordingTransport {
    spawns: RecordedSpawns,
    spawn_targets: std::sync::Mutex<Vec<(String, String, String)>>,
    session_exists: bool,
    // 0.4.10+ reset hard-gate: record killed panes so liveness/has_pane
    // reflect the kill (the gate checks "did stop actually remove the
    // old pane"). Default mock behavior reported Live unconditionally —
    // safe pre-fix because no gate ran, but the gate now needs the kill
    // to be observable.
    killed_panes: std::sync::Mutex<std::collections::BTreeSet<String>>,
}
impl crate::transport::Transport for SessionProbeRecordingTransport {
    fn kind(&self) -> crate::transport::BackendKind {
        crate::transport::BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        session: &crate::transport::SessionName,
        window: &crate::transport::WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        _env: &std::collections::BTreeMap<String, String>,
    ) -> Result<crate::transport::SpawnResult, crate::transport::TransportError> {
        self.spawns
            .lock()
            .unwrap()
            .push(("spawn_first".to_string(), argv.to_vec()));
        self.spawn_targets.lock().unwrap().push((
            session.as_str().to_string(),
            window.as_str().to_string(),
            "%0".to_string(),
        ));
        Ok(crate::transport::SpawnResult {
            pane_id: crate::transport::PaneId::new("%0"),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(6856),
        })
    }
    fn spawn_into(
        &self,
        session: &crate::transport::SessionName,
        window: &crate::transport::WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        _env: &std::collections::BTreeMap<String, String>,
    ) -> Result<crate::transport::SpawnResult, crate::transport::TransportError> {
        self.spawns
            .lock()
            .unwrap()
            .push(("spawn_into".to_string(), argv.to_vec()));
        let pane = format!("%{}", window.as_str());
        self.spawn_targets.lock().unwrap().push((
            session.as_str().to_string(),
            window.as_str().to_string(),
            pane.clone(),
        ));
        Ok(crate::transport::SpawnResult {
            pane_id: crate::transport::PaneId::new(pane),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(6856),
        })
    }
    fn inject(
        &self,
        _t: &crate::transport::Target,
        _p: &crate::transport::InjectPayload,
        _s: crate::transport::Key,
        _b: bool,
    ) -> Result<crate::transport::InjectReport, crate::transport::TransportError> {
        unimplemented!("not reached by start_agent respawn")
    }
    fn send_keys(
        &self,
        _t: &crate::transport::Target,
        _k: &[crate::transport::Key],
    ) -> Result<(), crate::transport::TransportError> {
        unimplemented!("not reached")
    }
    fn capture(
        &self,
        _t: &crate::transport::Target,
        _r: crate::transport::CaptureRange,
    ) -> Result<crate::transport::CapturedText, crate::transport::TransportError> {
        Ok(crate::transport::CapturedText {
            text: String::new(),
            range: crate::transport::CaptureRange::Full,
        })
    }
    fn query(
        &self,
        _t: &crate::transport::Target,
        _f: crate::transport::PaneField,
    ) -> Result<Option<String>, crate::transport::TransportError> {
        unimplemented!("not reached")
    }
    fn liveness(
        &self,
        p: &crate::transport::PaneId,
    ) -> Result<crate::model::enums::PaneLiveness, crate::transport::TransportError> {
        let killed = self.killed_panes.lock().unwrap();
        if killed.contains(p.as_str()) {
            return Ok(crate::model::enums::PaneLiveness::Dead);
        }
        Ok(crate::model::enums::PaneLiveness::Live)
    }
    fn has_pane(
        &self,
        p: &crate::transport::PaneId,
    ) -> Result<Option<bool>, crate::transport::TransportError> {
        let killed = self.killed_panes.lock().unwrap();
        Ok(Some(!killed.contains(p.as_str())))
    }
    fn kill_pane(
        &self,
        p: &crate::transport::PaneId,
    ) -> Result<(), crate::transport::TransportError> {
        self.killed_panes
            .lock()
            .unwrap()
            .insert(p.as_str().to_string());
        Ok(())
    }
    fn list_targets(
        &self,
    ) -> Result<Vec<crate::transport::PaneInfo>, crate::transport::TransportError> {
        Ok(self
            .spawn_targets
            .lock()
            .unwrap()
            .iter()
            .map(|(session, window, pane)| crate::transport::PaneInfo {
                pane_id: crate::transport::PaneId::new(pane.as_str()),
                session: crate::transport::SessionName::new(session.as_str()),
                window_index: None,
                window_name: Some(crate::transport::WindowName::new(window.as_str())),
                pane_index: None,
                tty: None,
                current_command: Some("node".to_string()),
                current_path: None,
                active: false,
                pane_pid: Some(6856),
                leader_env: std::collections::BTreeMap::new(),
            })
            .collect())
    }
    fn has_session(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<bool, crate::transport::TransportError> {
        Ok(self.session_exists)
    }
    fn list_windows(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<Vec<crate::transport::WindowName>, crate::transport::TransportError> {
        // a dead server has no windows; a live one has the agent window.
        Ok(if self.session_exists {
            vec![crate::transport::WindowName::new("alpha")]
        } else {
            Vec::new()
        })
    }
    fn set_session_env(
        &self,
        _s: &crate::transport::SessionName,
        _k: &str,
        _v: &str,
    ) -> Result<crate::transport::SetEnvOutcome, crate::transport::TransportError> {
        unimplemented!("not reached")
    }
    fn kill_session(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<(), crate::transport::TransportError> {
        unimplemented!("not reached")
    }
    fn kill_window(
        &self,
        _t: &crate::transport::Target,
    ) -> Result<(), crate::transport::TransportError> {
        Ok(())
    }
    fn attach_session(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<crate::transport::AttachOutcome, crate::transport::TransportError> {
        unimplemented!("not reached")
    }
}
fn respawn_ws_one_resumable_worker() -> PathBuf {
    let ws = temp_ws().join("respawn_dead_session");
    std::fs::create_dir_all(&ws).unwrap();
    let rollout = ws.join("alpha-rollout.jsonl");
    std::fs::write(&rollout, "{}\n").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-sa",
            "agents": {"alpha": {"status": "running", "provider": "codex", "session_id": "sess-a", "rollout_path": rollout.to_string_lossy(), "first_send_at": "2026-05-27T10:00:00+00:00"}}
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    ws
}
// RED — the respawn-time spawn decision must consult the LIVE session probe. With
// `has_session=false` (the `-L` server destroyed by --discard-session killing the
// last window), the respawn MUST new-session (spawn_first). Today `into_existing_session
// = session_name_present(&state)` is true (state string still "team-sa") so the code
// records spawn_into against a dead server -> assertion fails -> RED. NOT a panic:
// the transport implements has_session, so the only failure is the wrong spawn kind.
#[test]
fn start_agent_respawn_into_dead_session_uses_new_session_not_new_window() {
    let ws = respawn_ws_one_resumable_worker();
    let spawns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // session_exists=false models the dead CP-1 `-L` server after --discard-session.
    let transport = SessionProbeRecordingTransport {
        spawns: std::sync::Arc::clone(&spawns),
        spawn_targets: std::sync::Mutex::new(Vec::new()),
        session_exists: false,
        killed_panes: std::sync::Mutex::new(std::collections::BTreeSet::new()),
    };
    let _ = start_agent_with_transport(
        &ws,
        &AgentId::new("alpha"),
        false,
        false,
        false,
        None,
        &transport,
    );
    let recorded = spawns.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        1,
        "exactly one respawn for alpha; got {recorded:?}"
    );
    assert_eq!(
        recorded[0].0, "spawn_first",
        "golden runtime.py:1017: when `tmux has-session` is FALSE (dead -L server after \
         --discard-session destroyed the last window), the respawn MUST new-session \
         (spawn_first), NOT new-window (spawn_into) — spawn_into a dead server crashes with \
         'no server running' and orphans the agent. The decision must come from \
         transport.has_session(), not session_name_present(&state); got {recorded:?}"
    );
}
// ═════════════════════════════════════════════════════════════════════════
// reset/remove CANONICAL-WORKSPACE regression-locks (bug-A class: e17096c/0f886a9/4435f9a).
// rt-host-a @ c262bf7: `reset`/`remove --workspace <teamdir/.team>` resolved to the SUBPATH's own
// .team/runtime instead of the LIVE parent run-workspace -> they operated on a detached context
// (live roster/windows untouched; respawn went nowhere -> post-reset dispatch died). `start` dodged it
// by being invoked with $ws. FIX (porter-a): reset/remove canonicalize the run-workspace
// (lifecycle_paths -> run_workspace = canonical_run_workspace(input)), read spec from the teamdir, and
// reset reuses start's live-session-probe respawn. These LOCK the canonical resolution so it can't
// re-regress.
// ═════════════════════════════════════════════════════════════════════════
// item ① — reset --discard-session must REBUILD the worker's window via the SAME respawn path as start
// (reset_agent_at_paths -> start_agent_with_transport). Into a LIVE session (has_session=true) the
// rebuild is a new-window (spawn_into). The regression had reset operate on a detached runtime so the
// window was never rebuilt. OS-safe: SessionProbeRecordingTransport (no real tmux) + seeded coordinator.
#[test]
fn reset_agent_discard_session_rebuilds_window_via_start_respawn() {
    let ws = restart_ws_two_resumable_workers(); // compiled spec + state(alpha,bravo running) + seeded coordinator
    let spawns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // live session present (other workers keep it alive) but alpha's window was discarded.
    let transport = SessionProbeRecordingTransport {
        spawns: std::sync::Arc::clone(&spawns),
        spawn_targets: std::sync::Mutex::new(Vec::new()),
        session_exists: true,
        killed_panes: std::sync::Mutex::new(std::collections::BTreeSet::new()),
    };
    let _ = reset_agent_with_transport(&ws, &AgentId::new("alpha"), true, false, None, &transport);
    let recorded = spawns.lock().unwrap().clone();
    assert!(
        !recorded.is_empty(),
        "reset --discard-session must RESPAWN alpha's window (reuse start's respawn path); the bug-A \
         regression operated on a detached runtime so nothing was rebuilt -> ZERO spawns. got {recorded:?}"
    );
    assert_eq!(
        recorded.last().unwrap().0, "spawn_into",
        "into a LIVE session (has_session=true) the window rebuild is a new-window (spawn_into); got {recorded:?}"
    );
}

#[test]
#[serial_test::serial(env)]
fn reset_agent_discard_session_syncs_projection_epoch_inputs_for_restart_agent_command() {
    let ws = restart_ws_two_resumable_workers();
    let mut state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    state["team_dir"] = json!(ws.to_string_lossy());
    state["active_team_key"] = json!("restartteam");
    state["agents"]["alpha"]["window"] = json!("alpha");
    state["agents"]["alpha"]["pane_id"] = json!("%old");
    state["agents"]["alpha"]["pane_pid"] = json!(6304);
    state["agents"]["alpha"]["spawned_at"] = json!("2026-06-13T01:00:00+00:00");
    state["agents"]["alpha"]["startup_prompts"] = json!("disabled_for_epoch");
    state["agents"]["alpha"]["startup_prompt_status"] = json!("disabled_for_epoch");
    state["agents"]["alpha"]["startup_prompt_probe_epoch"] = json!("pane_pid:6304");
    state["agents"]["alpha"]["startup_prompt_probe_disabled_at"] =
        json!("2026-06-13T01:01:00+00:00");
    let compact = crate::state::projection::compact_team_state(&state);
    state["teams"] = json!({
        "current": compact.clone(),
        "restartteam": compact,
    });
    crate::state::persist::save_runtime_state(&ws, &state).expect("save stale projections");

    let before = chrono::Utc::now();
    let spawns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let transport = SessionProbeRecordingTransport {
        spawns: std::sync::Arc::clone(&spawns),
        spawn_targets: std::sync::Mutex::new(Vec::new()),
        session_exists: true,
        killed_panes: std::sync::Mutex::new(std::collections::BTreeSet::new()),
    };
    let outcome =
        reset_agent_with_transport(&ws, &AgentId::new("alpha"), true, false, None, &transport)
            .expect("reset-agent alpha --discard-session --no-display");
    assert!(
        matches!(outcome, ResetAgentOutcome::Reset { .. }),
        "restart-agent alpha --discard-session --no-display must complete reset; got {outcome:?}"
    );

    let state = crate::state::persist::load_runtime_state(&ws).expect("reload state");
    for pointer in [
        "/agents/alpha",
        "/teams/current/agents/alpha",
        "/teams/restartteam/agents/alpha",
    ] {
        let agent = state.pointer(pointer).expect(pointer);
        assert_eq!(
            agent.get("pane_id").and_then(serde_json::Value::as_str),
            Some("%alpha"),
            "{pointer} must project the reset-spawned pane, not the stale pre-reset pane"
        );
        assert_eq!(
            agent.get("pane_pid").and_then(serde_json::Value::as_u64),
            Some(6856),
            "{pointer} must project the reset-spawned pane pid, not stale pid 6304"
        );
        let spawned_at = agent
            .get("spawned_at")
            .and_then(serde_json::Value::as_str)
            .expect("spawned_at refreshed");
        assert_ne!(
            spawned_at, "2026-06-13T01:00:00+00:00",
            "{pointer} must not retain stale spawned_at"
        );
        let spawned_at = chrono::DateTime::parse_from_rfc3339(spawned_at)
            .expect("spawned_at rfc3339")
            .with_timezone(&chrono::Utc);
        assert!(
            spawned_at >= before - chrono::Duration::seconds(1),
            "{pointer} must get a fresh startup-probe grace epoch; spawned_at={spawned_at}, before={before}"
        );
        for key in [
            "startup_prompts",
            "startup_prompt_status",
            "startup_prompt_probe_epoch",
            "startup_prompt_probe_disabled_at",
        ] {
            assert!(
                agent.get(key).is_none(),
                "{pointer} must not retain old startup probe epoch field {key}"
            );
        }
    }
}
// item ② — remove invoked with a NON-canonical input (the workspace's own `.team` subpath) must resolve
// the CANONICAL live run-workspace and apply the removal to the LIVE roster — not the subpath's detached
// runtime. canonical_run_workspace($ws/.team) == $ws. The regression made remove "succeed" against a
// detached context, leaving the live roster [alpha,bravo] intact. (lanea_team_ws re-points routing to
// bravo so removing alpha validates cleanly.)
#[test]
fn remove_agent_via_team_subpath_applies_to_canonical_live_roster() {
    let ws = lanea_team_ws("stopped"); // canonical workspace: spec + state roster [alpha,bravo]
    let subpath = ws.join(".team"); // non-canonical input; canonicalizes back to $ws
    let tx = LaneTransport::new("team-laneateam", &[]);
    let outcome = remove_agent_with_transport(&subpath, &aid("alpha"), true, true, None, &tx);
    assert!(
        matches!(outcome, Ok(RemoveAgentOutcome::Removed { .. })),
        "remove via the .team subpath must resolve the canonical live workspace and succeed; got {outcome:?}"
    );
    let state = crate::state::persist::load_runtime_state(&ws).expect("load LIVE $ws state");
    assert!(
        state.get("agents").and_then(serde_json::Value::as_object).is_some_and(|a| !a.contains_key("alpha")),
        "remove must apply to the LIVE $ws roster (canonical resolution), removing alpha; the bug-A \
         regression operated on the subpath's detached runtime, leaving the live roster intact. state={state}"
    );
}
/// bug-B [✦ DIVERGENCE] fixture — a NORMAL compiled 2-agent team with routing/tasks LEFT EXACTLY AS
/// COMPILED (route-alpha + route-bravo, default_assignee=alpha, tasks[0].assignee=alpha). Unlike
/// lanea_team_ws / lanea_ws_agents (which deliberately re-point refs away so a remove validates), this
/// is the REAL "remove a normal worker" case — the one golden cannot handle (every compiled agent is
/// routed, compiler.py:57). Both workers seeded `stopped` (no tmux); LaneTransport reports no windows.
fn bugb_routed_team_ws() -> PathBuf {
    let ws = temp_ws().join("bugb_routed");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(ws.join("TEAM.md"), "---\nname: laneateam\nobjective: bug-B routed-remove probe.\nprovider: codex\n---\n\nteam.\n").unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile bug-B team");
    let yaml = crate::model::yaml::dumps(&spec);
    // sanity: the UNMODIFIED compiled spec really routes bravo — that route-bravo rule is the dangling
    // ref the cascade-prune must drop (and which makes golden's validate_spec raise + rollback).
    assert!(
        yaml.contains("assign_to: \"bravo\""),
        "fixture: compiled spec must carry route-bravo (assign_to bravo); got:\n{yaml}"
    );
    std::fs::write(ws.join("team.spec.yaml"), yaml).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-laneateam",
            "agents": {
                "alpha": {"status": "stopped", "provider": "codex", "window": "alpha"},
                "bravo": {"status": "stopped", "provider": "codex", "window": "bravo"}
            }
        }),
    )
    .unwrap();
    ws
}
// ── bug-B [✦ DIVERGENCE — REVERSES the previously-DECLINED byte-parity decision (ticket:154)] ─────────
// Removing a NORMAL routed worker must SUCCEED and surgically PRUNE that worker's dangling routing rule,
// NOT refuse+rollback. Golden agents.py:88-94 removes the agent only from `agents` + `runtime.startup_order`
// (routing UNTOUCHED), then validate_spec RAISES `/routing/rules/N/assign_to: unknown agent 'bravo'`
// (spec.py:341-346) -> remove catches -> rollback (rollback_ok=true). Because compile_team auto-wires a
// `route-<id>` rule (assign_to:<id>) for EVERY agent (compiler.py:57), NO compiled worker is removable in
// golden. Probed live (/tmp/probe_removeB2.py): validate_spec of the bravo-removed spec raises exactly
// "/routing/rules/1/assign_to: unknown agent 'bravo'". The advisor/leader requests the cascade-prune
// parity-IMPROVEMENT (ticket:155,197): remove must drop routing rules whose assign_to == the removed
// agent (and reset default_assignee if it pointed at it) so the spec stays valid. Current Rust faithfully
// mirrors golden (restart.rs:897-899 validate_spec -> Err(Compile) -> rollback) -> RED.
//
// NOTE for the porter: this REVERSES the documented byte-parity intent at restart.rs:931-937 and the
// lanea_team_ws/lanea_ws_agents fixtures' re-point workaround. Implementing the prune is correct ONLY
// if the leader's ✦ divergence ruling stands — do not "fix" by reverting this test.
//
// Removing `bravo` (the SECOND agent) isolates the routing-rule prune cleanly: only `route-bravo` dangles;
// default_assignee (alpha) and tasks[0].assignee (alpha) stay valid, so this test does NOT entangle the
// default_assignee / task-reassignment policy.
#[test]
fn remove_routed_worker_prunes_dangling_routing_rule_divergence() {
    let ws = bugb_routed_team_ws();
    let tx = LaneTransport::new("team-laneateam", &[]); // bravo not running (no windows)
    let outcome = remove_agent_with_transport(&ws, &aid("bravo"), true, true, None, &tx); // from_spec + force
    assert!(
        matches!(outcome, Ok(RemoveAgentOutcome::Removed { .. })),
        "✦ divergence: removing the routed worker 'bravo' must SUCCEED (Removed), not refuse+rollback. \
         Golden + current Rust raise /routing/rules/.../assign_to: unknown agent 'bravo' -> \
         StatePersist rollback_ok=true; got {outcome:?}"
    );
    let yaml = std::fs::read_to_string(ws.join("team.spec.yaml")).expect("read spec after remove");
    assert!(
        !yaml.contains("assign_to: \"bravo\""),
        "✦ divergence: remove must PRUNE the dangling route-bravo rule (assign_to: \"bravo\"); spec still has it:\n{yaml}"
    );
    assert!(
        yaml.contains("assign_to: \"alpha\""),
        "the prune must be SURGICAL — route-alpha (the STAYING agent) must remain; got:\n{yaml}"
    );
    assert!(
        yaml.contains("default_assignee: \"alpha\""),
        "default_assignee (alpha, still present) must be untouched by the prune; got:\n{yaml}"
    );
    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    assert!(
        state
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|a| !a.contains_key("bravo")),
        "bravo must also be removed from state.agents; got {state:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
