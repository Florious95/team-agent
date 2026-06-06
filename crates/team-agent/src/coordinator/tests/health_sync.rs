use super::*;

// ═════════════════════════════════════════════════════════════════════════
// SPINE SLICE-2a RED — capture-based health-sync obligations (sync_health / refresh_statuses
// + capture_missing). These are bare record_step() probes today (tick.rs `TODO(spine slice 2):
// wire via capture seam`), so the daemon does not read live scrollback or update agent status.
// Golden: coordinator/lifecycle.py → approvals/status.py sync_agent_health /
// refresh_agent_runtime_statuses (capture pane → classify_agent_activity → activity/status +
// last_output) and sessions/capture.py capture_missing_sessions (no session_id + transcript →
// capture_session_id). §11 iron law (bug-071/077/085): an UNKNOWN scrollback is NEVER idle.
// ═════════════════════════════════════════════════════════════════════════

/// The CAPTURE SEAM (test side): a transport whose `capture()` returns SEEDED scrollback, so a test
/// can stage exactly what a worker's pane shows. The porter wires tick to call transport.capture per
/// agent. `has_session`→true (gate passes); `inject`→Ok (delivery may run); the rest are Ok defaults.
struct CapturingTransport {
    scrollback: String,
}
impl Transport for CapturingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(&self, _s: &SessionName, _w: &WindowName, _a: &[String], _c: &std::path::Path, _e: &std::collections::BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached")
    }
    fn spawn_into(&self, _s: &SessionName, _w: &WindowName, _a: &[String], _c: &std::path::Path, _e: &std::collections::BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached")
    }
    fn inject(&self, _t: &Target, _p: &InjectPayload, _s: Key, _b: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: crate::transport::InjectStage::Submit,
            inject_verification: crate::transport::InjectVerification::CaptureContainsToken,
            submit_verification: crate::transport::SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: crate::transport::TurnVerification::NotYetObserved,
            attempts: 1,
        })
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _t: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: self.scrollback.clone(), range })
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("w1")])
    }
    fn set_session_env(&self, _s: &SessionName, _k: &str, _v: &str) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
        Ok(())
    }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

/// Build a Coordinator over a real seeded workspace (truthy session_name + the given agents map) with
/// the CapturingTransport staging `scrollback` for every pane. Returns the workspace dir so the test
/// can load_runtime_state after the tick.
fn seeded_health_coord(agents: serde_json::Value, scrollback: &str) -> (Coordinator, std::path::PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-health-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::state::persist::save_runtime_state(
        &dir,
        &serde_json::json!({ "session_name": "team-health", "agents": agents }),
    )
    .unwrap();
    let ws = WorkspacePath::new(dir.clone());
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let coord = Coordinator::for_test(
        ws,
        reg,
        Box::new(CapturingTransport { scrollback: scrollback.to_string() }),
        None,
        None,
    );
    (coord, dir)
}

fn agent_activity_status(dir: &std::path::Path, agent: &str) -> Option<String> {
    let state = crate::state::persist::load_runtime_state(dir).ok()?;
    state
        .get("agents")?
        .get(agent)?
        .get("activity")?
        .get("status")?
        .as_str()
        .map(str::to_string)
}
fn agent_field(dir: &std::path::Path, agent: &str, field: &str) -> Option<serde_json::Value> {
    let state = crate::state::persist::load_runtime_state(dir).ok()?;
    state.get("agents")?.get(agent)?.get(field).cloned()
}

fn one_agent(provider: &str) -> serde_json::Value {
    serde_json::json!({ "w1": { "provider": provider, "window": "w1", "pane_id": "%1" } })
}

// P0 §11 — an IDLE-prompt scrollback must classify the agent idle (golden classify_agent_activity →
// state.agents[w1].activity). Today the obligation is a probe → no activity written.
#[test]
fn spine2_sync_health_classifies_idle_scrollback() {
    let (coord, dir) = seeded_health_coord(one_agent("codex"), "previous output\n❯\n");
    coord.tick().expect("tick");
    assert_eq!(
        agent_activity_status(&dir, "w1").as_deref(),
        Some("idle"),
        "an idle-prompt scrollback must classify the agent idle (sync_health writes state.agents[w1].activity)"
    );
}

// P0 §11 IRON LAW (bug-071/077/085) — an UNKNOWN/unrecognized scrollback must classify the agent but
// NEVER as idle. Today: no activity written.
#[test]
fn spine2_sync_health_unknown_scrollback_never_idle() {
    let (coord, dir) = seeded_health_coord(one_agent("codex"), "garbled noise xyz 12345 no recognizable signal");
    coord.tick().expect("tick");
    let status = agent_activity_status(&dir, "w1");
    assert!(status.is_some(), "sync_health must classify the agent (write activity); today the probe writes nothing. got {status:?}");
    assert_ne!(status.as_deref(), Some("idle"), "§11: an UNKNOWN scrollback must NEVER be classified idle");
}

// P0 §11 — a WORKING scrollback classifies the agent but never idle.
#[test]
fn spine2_sync_health_working_scrollback_never_idle() {
    let (coord, dir) = seeded_health_coord(one_agent("codex"), "Working (5s · esc to interrupt)");
    coord.tick().expect("tick");
    let status = agent_activity_status(&dir, "w1");
    assert!(status.is_some(), "sync_health must classify a working agent; today no activity. got {status:?}");
    assert_ne!(status.as_deref(), Some("idle"), "§11: a WORKING scrollback must not be idle");
}

// P1 — sync_health records last_output_at on a pane delta (so detect_stuck / take-over downstream can
// use it). Golden approvals/status.py:sync_agent_health. Today: probe writes nothing.
#[test]
fn spine2_sync_health_records_last_output_at() {
    let (coord, dir) = seeded_health_coord(one_agent("codex"), "some fresh pane output");
    coord.tick().expect("tick");
    assert!(
        agent_field(&dir, "w1", "last_output_at").is_some(),
        "sync_health must record last_output_at on a pane delta; today the probe writes nothing"
    );
}

// P1 — capture_missing: an agent with NO session_id but a discoverable transcript under spawn_cwd gets
// its session_id captured + persisted (real capture_session_id); an agent that already has one is
// untouched. Golden sessions/capture.py:capture_missing_sessions.
#[test]
fn spine2_capture_missing_captures_session_id_for_missing_agent() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let tdir = std::env::temp_dir().join(format!("ta-rs-health-tx-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&tdir).unwrap();
    std::fs::write(
        tdir.join("session.jsonl"),
        r#"{"type":"user","sessionId":"sess-found","cwd":"x","message":{"content":"hi"}}"#,
    )
    .unwrap();
    let agents = serde_json::json!({
        "w1": { "provider": "claude_code", "window": "w1", "spawn_cwd": tdir.to_string_lossy() },
        "w2": { "provider": "claude_code", "window": "w2", "session_id": "existing-sess" },
    });
    let (coord, dir) = seeded_health_coord(agents, "");
    coord.tick().expect("tick");
    assert!(
        agent_field(&dir, "w1", "session_id").and_then(|v| v.as_str().map(str::to_string)).is_some(),
        "capture_missing must capture+persist a session_id for an agent with a discoverable transcript; today it's a probe"
    );
    assert_eq!(
        agent_field(&dir, "w2", "session_id").and_then(|v| v.as_str().map(str::to_string)).as_deref(),
        Some("existing-sess"),
        "an agent that already has a session_id must be untouched"
    );
}

// CONTRACT — sync_health runs BEFORE deliver_pending, but turn-level WORKING state must not make an
// alive worker undeliverable. Busy delivery deferral is lifecycle-only (`state.agents[id].status=="busy"`);
// activity/agent_health WORKING remains diagnostic turn state.
#[test]
fn spine2_sync_health_working_status_delivers_same_tick() {
    let (coord, dir) = seeded_health_coord(one_agent("codex"), "Working (5s · esc to interrupt)");
    let store = MessageStore::open(&dir).unwrap();
    let mid = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into agent_health(owner_team_id, agent_id, status, updated_at)
         values (?1, ?2, ?3, ?4)
         on conflict(owner_team_id, agent_id) do update set
             status = excluded.status,
             updated_at = excluded.updated_at",
        rusqlite::params!["current", "w1", "WORKING", chrono::Utc::now().to_rfc3339()],
    )
    .unwrap();
    drop(store);
    coord.tick().expect("tick");
    let events = read_event_log_dir(&dir);
    assert!(
        !events.iter().any(|e| e.get("event").and_then(|v| v.as_str()) == Some("send.deferred_busy")),
        "turn-level WORKING must not trigger lifecycle busy deferral; got {events:?}"
    );
    assert!(
        events.iter().any(|e| {
            e.get("event").and_then(|v| v.as_str()) == Some("message.delivered")
                && e.get("message_id").and_then(|v| v.as_str()) == Some(mid.as_str())
        }),
        "alive worker with WORKING turn state must still receive the pending message; got {events:?}"
    );
}

// ADVERSARIAL (real-machine-driven; catches porter fix 1f97163 re-coupling): a worker classified WORKING
// by sync_health is STILL ALIVE (lifecycle status running) and MUST remain deliverable. golden never maps
// turn activity to lifecycle status (status is only running/stopped; the status=="busy" gate is vestigial/
// unreachable — golden delivers to alive workers regardless of activity). The porter fix (write_activity,
// tick.rs:858) maps activity=Working -> status="busy", re-coupling turn state into lifecycle status — it
// just MOVED the deferral from agent_health to status, re-introducing the regression (fake workers are
// permanently WORKING -> permanently status=busy -> permanently deferred -> round-trip never closes).
// The synthetic contract REDs seed status=running directly and skip the tick, so they don't catch this;
// this drives the REAL coordinator tick. (Contradicts the stale lock
// spine2_sync_health_busy_status_defers_delivery_same_tick, which encodes the regression behavior and
// must be reconciled to this contract.)
#[test]
fn contract_working_worker_stays_alive_and_deliverable_in_real_tick() {
    let (coord, dir) = seeded_health_coord(one_agent("codex"), "Working (5s · esc to interrupt)");
    let store = MessageStore::open(&dir).unwrap();
    let _mid = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    drop(store);
    coord.tick().expect("tick");
    let status = agent_field(&dir, "w1", "status").and_then(|v| v.as_str().map(str::to_string));
    assert_ne!(
        status.as_deref(),
        Some("busy"),
        "CONTRACT: sync_health must NOT write lifecycle status='busy' from turn activity=Working (golden never \
         maps activity->status); turn state belongs in agent['activity']/agent_health only. got status={status:?}"
    );
    let events = read_event_log_dir(&dir);
    assert!(
        !events.iter().any(|e| e.get("event").and_then(|v| v.as_str()) == Some("send.deferred_busy")),
        "CONTRACT: an alive worker (lifecycle running) classified WORKING must still receive delivery, not \
         deferred_busy (golden delivers; fake workers are permanently WORKING). got {events:?}"
    );
}
