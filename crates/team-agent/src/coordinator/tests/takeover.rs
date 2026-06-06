use super::*;

// ═════════════════════════════════════════════════════════════════════════
// SPINE SLICE-2b RED — take-over / stuck / deadlock obligations. record_unknown_idle +
// evaluate_takeover are still record_step() probes; detect_stuck / detect_deadlocks are
// slice-1 placeholders (detect_stuck reads agent_health status='stuck'; detect_deadlocks
// returns []). Golden: coordinator/lifecycle.py (_detect_stuck_agents, _record_unknown_idle,
// _evaluate_takeover), messaging/idle_alerts.py (detect_cross_worker_deadlocks),
// idle_takeover_wiring.py (build_idle_nodes via read_turn_state on rollout_path; push_idle_reminder
// → idle_takeover.reminder), leader evaluate_takeover_reminder. IRON LAWS: §121 arm-from-real-
// delivery only (never from thin air); §84 zero-injection (only push_idle_reminder, only should_ping);
// unknown != idle persists into take-over (unknown_persistent path, not an idle ping).
// ═════════════════════════════════════════════════════════════════════════

/// CapturingTransport + an inject COUNTER (for §84: assert zero injects from the take-over path).
struct CountingCaptureTransport {
    scrollback: String,
    injects: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Transport for CountingCaptureTransport {
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
        self.injects.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        Ok(Vec::new())
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

fn slice2b_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("ta-rs-2b-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a coordinator over a workspace seeded with `state` (verbatim) + a counting transport.
fn takeover_coord(state: serde_json::Value) -> (Coordinator, std::path::PathBuf, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let dir = slice2b_dir();
    crate::state::persist::save_runtime_state(&dir, &state).unwrap();
    let injects = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let transport = CountingCaptureTransport { scrollback: String::new(), injects: std::sync::Arc::clone(&injects) };
    let ws = WorkspacePath::new(dir.clone());
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let coord = Coordinator::for_test(ws, reg, Box::new(transport), None, None);
    (coord, dir, injects)
}

/// Seed an agent_health row (real DB) — what sync_health writes and detect_stuck/detect_deadlocks read.
fn seed_agent_health(dir: &std::path::Path, agent_id: &str, status: &str, last_output_at: &str, owner_team_id: Option<&str>) {
    let store = MessageStore::open(dir).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at) \
         values (?1, ?2, ?3, ?4, null, null, ?4)",
        rusqlite::params![owner_team_id, agent_id, status, last_output_at],
    )
    .unwrap();
}

fn has_event(dir: &std::path::Path, name: &str) -> bool {
    read_event_log_dir(dir).iter().any(|e| e.get("event").and_then(|v| v.as_str()) == Some(name))
}

// #236 nag_removal (N35) — detect_stuck no longer synthesized in coordinator tick.
// [OLD] assertion: a RUNNING agent with stale last_output + inbound work → report.stuck non-empty.
// [NEW] assertion: report.stuck stays empty — the framework no longer infers "stuck" from
// time/state. Delivery primitives (report_result / send / funnel / request_human / broadcast)
// still flow; only the proactive nag output is gone.
#[test]
fn slice2b_detect_stuck_no_longer_synthesized_by_framework_n35() {
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(1000)).to_rfc3339();
    let (coord, dir, _inj) = takeover_coord(serde_json::json!({
        "session_name": "team-h",
        "agents": { "w1": { "provider": "codex" } },
    }));
    seed_agent_health(&dir, "w1", "RUNNING", &stale, Some("team-h"));
    let store = MessageStore::open(&dir).unwrap();
    let _ = store.create_message(Some("t"), "leader", "w1", "do work", None, true, None).unwrap();
    drop(store);

    let report = coord.tick().expect("tick");
    assert!(
        report.stuck.is_empty(),
        "#236 N35: tick no longer manufactures stuck nag; got {:?}",
        report.stuck
    );
}

// #236 nag_removal (N35) — cross_worker_deadlock detection removed from tick.
// [OLD] assertion: idle recipient + undelivered message → report.deadlock_alerts non-empty.
// [NEW] assertion: report.deadlock_alerts stays empty — the framework no longer manufactures
// "deadlock" alerts from idle+pending inference. The actual delivery row still exists and is
// retried/handled by the delivery primitives; only the nag wrapper is gone.
#[test]
fn slice2b_detect_deadlocks_no_longer_synthesized_by_framework_n35() {
    let now = chrono::Utc::now().to_rfc3339();
    let (coord, dir, _inj) = takeover_coord(serde_json::json!({
        "session_name": "team-h",
        "agents": { "w1": { "provider": "codex" }, "w2": { "provider": "codex" } },
    }));
    seed_agent_health(&dir, "w1", "IDLE", &now, Some("team-h"));
    let store = MessageStore::open(&dir).unwrap();
    let _ = store.create_message(Some("t"), "w2", "w1", "are you done?", None, true, None).unwrap();
    drop(store);

    let report = coord.tick().expect("tick");
    assert!(
        report.deadlock_alerts.is_empty(),
        "#236 N35: tick no longer manufactures cross_worker_deadlock alerts; got {:?}",
        report.deadlock_alerts
    );
}

// #236 nag_removal (N35) — the take-over reminder injection is gone from
// leader::inject::push_idle_reminder (now a no-op shim). tick.rs still runs the
// evaluate_takeover_reminder / push_idle_reminder pipeline (developer-b will land
// the tick.rs cleanup separately), but the helper that emitted the reminder event
// no longer does so — handover requires explicit `claim-leader` / `takeover` now.
// [OLD] assertion: ARMED + idle worker → exactly one idle_takeover.reminder event.
// [NEW] assertion: no reminder event is ever emitted (the helper that wrote it is
// no-op'd at the leader::inject layer).
#[test]
fn slice2b_takeover_armed_idle_worker_no_longer_emits_reminder_n35() {
    let rollout = slice2b_dir().join("w1.jsonl");
    std::fs::write(&rollout, r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"end_turn","content":[]}}"#).unwrap();
    let (coord, dir, _inj) = takeover_coord(serde_json::json!({
        "session_name": "team-h",
        "agents": { "w1": { "provider": "claude_code", "rollout_path": rollout.to_string_lossy() } },
        "coordinator": {
            "idle_takeover_monitor": { "opened_worker_turn_since_ack": true, "all_idle_since": -1.0e9, "suppressed": false }
        }
    }));
    coord.tick().expect("tick");
    assert!(
        !has_event(&dir, "idle_takeover.reminder"),
        "#236 N35: push_idle_reminder is a no-op shim; reminder nag must not be emitted; got {:?}",
        read_event_log_dir(&dir)
    );
}

// P0 §121/§84 — NEVER arm from thin air: an idle worker whose monitor is NOT armed
// (no opened_worker_turn_since_ack) is NEVER pinged → no idle_takeover.reminder AND zero injects from
// the take-over path. Guard (correct today and after wiring); a future arm-from-thin-air regression fails it.
#[test]
fn slice2b_takeover_unarmed_idle_worker_is_never_pinged() {
    let rollout = slice2b_dir().join("w1.jsonl");
    std::fs::write(&rollout, r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"end_turn","content":[]}}"#).unwrap();
    let (coord, dir, injects) = takeover_coord(serde_json::json!({
        "session_name": "team-h",
        "agents": { "w1": { "provider": "claude_code", "rollout_path": rollout.to_string_lossy() } },
        // NO coordinator.idle_takeover_monitor → not armed → reason not_armed_no_worker_turn.
    }));
    coord.tick().expect("tick");
    assert!(!has_event(&dir, "idle_takeover.reminder"), "a NOT-armed worker must never be pinged (§121)");
    assert_eq!(
        injects.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "§84 zero-injection: with no should_ping (and no pending messages), the tick injects NOTHING"
    );
}

// #236 nag_removal (N35) — record_unknown_idle removed; unknown_persistent nag deleted.
// [OLD] assertion: unknown_ticks reaching the 60th tick fires `idle_takeover.unknown_persistent`
// (every-12-tick cadence after the 60-tick threshold).
// [NEW] assertion: tick no longer emits the threshold event. Operators don't get time-windowed
// prods; explicit `claim-leader` / `takeover` is the only ownership-change path.
#[test]
fn slice2b_unknown_persistent_no_longer_emitted_at_threshold_tick_n35() {
    let (coord, dir, _inj) = takeover_coord(serde_json::json!({
        "session_name": "team-h",
        "agents": { "w1": { "provider": "codex" } },
        "coordinator": { "unknown_ticks": { "w1": 59 } }
    }));
    coord.tick().expect("tick");
    assert!(
        !has_event(&dir, "idle_takeover.unknown_persistent"),
        "#236 N35: tick no longer emits unknown-persistent nag; got {:?}",
        read_event_log_dir(&dir)
    );
}
