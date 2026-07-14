use super::*;

// ═════════════════════════════════════════════════════════════════════════
// SPINE — Coordinator::tick() must drive REAL cross-subsystem side-effects
// (orchestration port, sub-phase 1, P0). Golden: coordinator/lifecycle.py:250-385
// (deliver_pending → fire_scheduled → … → save_runtime_state LAST). Today the 14
// obligation steps are bare `record_step` probes (tick.rs:171) and base_tick_report
// (tick.rs:345) fabricates empty delivered/scheduled/stuck/results vecs, so these
// integration tests — wiring REAL state.json + team.db, only the OS edge mocked —
// FAIL (RED) and pass once the porter wires tick to the real messaging fns.
// ═════════════════════════════════════════════════════════════════════════

/// Non-panicking transport: mirrors `MockTransport` but `inject` RECORDS and returns Ok
/// (a real `deliver_pending` obligation legitimately injects; the §84-guard `MockTransport`
/// panics on inject, which is correct for the no-obligation tick but not for a delivery test).
pub(super) struct DeliveringTransport {
    inner: MockTransport,
}
impl DeliveringTransport {
    pub(super) fn new() -> Self {
        Self {
            inner: MockTransport::new(true),
        }
    }
}
impl Transport for DeliveringTransport {
    fn kind(&self) -> BackendKind {
        self.inner.kind()
    }
    fn spawn_first(
        &self,
        s: &SessionName,
        w: &WindowName,
        a: &[String],
        c: &std::path::Path,
        e: &std::collections::BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.inner.spawn_first(s, w, a, c, e)
    }
    fn spawn_into(
        &self,
        s: &SessionName,
        w: &WindowName,
        a: &[String],
        c: &std::path::Path,
        e: &std::collections::BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.inner.spawn_into(s, w, a, c, e)
    }
    fn inject(
        &self,
        _t: &Target,
        _p: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.inner.record("inject");
        Ok(InjectReport {
            stage_reached: crate::transport::InjectStage::Submit,
            inject_verification: crate::transport::InjectVerification::CaptureContainsToken,
            submit_verification:
                crate::transport::SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: crate::transport::TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, t: &Target, k: &[Key]) -> Result<(), TransportError> {
        self.inner.send_keys(t, k)
    }
    fn capture(&self, t: &Target, r: CaptureRange) -> Result<CapturedText, TransportError> {
        self.inner.capture(t, r)
    }
    fn query(&self, t: &Target, f: PaneField) -> Result<Option<String>, TransportError> {
        self.inner.query(t, f)
    }
    fn liveness(&self, p: &PaneId) -> Result<PaneLiveness, TransportError> {
        self.inner.liveness(p)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.inner.list_targets()
    }
    fn has_session(&self, s: &SessionName) -> Result<bool, TransportError> {
        self.inner.has_session(s)
    }
    fn list_windows(&self, s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        self.inner.list_windows(s)
    }
    fn set_session_env(
        &self,
        s: &SessionName,
        k: &str,
        v: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        self.inner.set_session_env(s, k, v)
    }
    fn kill_session(&self, s: &SessionName) -> Result<(), TransportError> {
        self.inner.kill_session(s)
    }
    fn kill_window(&self, t: &Target) -> Result<(), TransportError> {
        self.inner.kill_window(t)
    }
    fn attach_session(&self, s: &SessionName) -> Result<AttachOutcome, TransportError> {
        self.inner.attach_session(s)
    }
}

/// Build a `Coordinator` over a freshly-seeded REAL workspace: state.json carries a truthy
/// `session_name` (so the gate runs) + a worker agents map. Returns the workspace dir so the
/// test can seed/inspect the REAL team.db. `transport` is the injected OS edge; `save_hook`
/// injects the bug-084 forced save failure.
fn seeded_spine_coord(
    transport: Box<dyn Transport>,
    save_hook: Option<SaveHook>,
) -> (Coordinator, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "team-agent-coord-spine-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::state::persist::save_runtime_state(
        &dir,
        &serde_json::json!({
            "session_name": "team-spine",
            "agents": { "w1": { "provider": "codex" } },
        }),
    )
    .unwrap();
    let ws = WorkspacePath::new(dir.clone());
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let coord = Coordinator::for_test(ws, reg, transport, save_hook, None);
    (coord, dir)
}

/// Insert ONE due `health_ping` scheduled_event (status pending, far-past due) into the REAL
/// team.db. `fire_due_scheduled_events` fully handles `health_ping` (logs, no transport inject)
/// and marks the row `done`. Returns the autoincrement id.
fn seed_due_health_ping(dir: &std::path::Path) -> i64 {
    let store = MessageStore::open(dir).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at) \
         values (null, '2000-01-01T00:00:00+00:00', 'w1', 'health_ping', '{}', 'pending', '2000-01-01T00:00:00+00:00')",
        [],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn scheduled_status(dir: &std::path::Path, id: i64) -> String {
    let store = MessageStore::open(dir).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status from scheduled_events where id = ?1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

pub(super) fn message_status(dir: &std::path::Path, message_id: &str) -> String {
    let store = MessageStore::open(dir).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status from messages where message_id = ?1",
        [message_id],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

// P0 — tick must FIRE a due scheduled event against the REAL team.db (fire_due_scheduled_events,
// lifecycle.py:286). Observable: report.scheduled carries the id AND the db row is marked 'done'.
#[test]
fn spine_tick_fires_due_scheduled_event_and_marks_db_done() {
    let (coord, dir) = seeded_spine_coord(Box::new(MockTransport::new(true)), None);
    let id = seed_due_health_ping(&dir);
    assert_eq!(
        scheduled_status(&dir, id),
        "pending",
        "precondition: the seeded row starts pending"
    );

    let report = coord.tick().expect("tick returns a typed report");

    assert!(
        report.scheduled.iter().any(|e| e.id == id),
        "tick must report the fired scheduled event id {id} (today base_tick_report fabricates []); got {:?}",
        report.scheduled
    );
    assert_eq!(
        scheduled_status(&dir, id),
        "done",
        "fire_due_scheduled_events must mark the scheduled_events row 'done' in the REAL team.db"
    );
}

// P1 — bug-084 save-LAST ordering with a REAL side-effect: a failing save_hook must NOT undo the
// scheduled fire. The db mutation commits BEFORE atomic_save (lifecycle.py:286 then :346), and the
// degraded report still carries `scheduled` (lifecycle.py:356). Proves save is genuinely the LAST
// mutation — AFTER the real obligation side-effects, not after no-op probes.
#[test]
fn spine_tick_save_failure_still_persists_real_scheduled_mutation() {
    let (coord, dir) = seeded_spine_coord(
        Box::new(MockTransport::new(true)),
        Some(failing_save_hook()),
    );
    let id = seed_due_health_ping(&dir);

    let report = coord
        .tick()
        .expect("a degraded tick is Ok(TickReport), not Err");

    // degraded report shape (bug-084: save failure → ok=false / persisted=false, NOT Err).
    assert!(!report.ok, "save failure → ok=false");
    assert_eq!(
        report.persisted,
        Some(false),
        "save failure → persisted=Some(false)"
    );
    assert_eq!(report.reason, Some(TickStopReason::PersistenceDegraded));
    // the REAL db side-effect happened BEFORE the (failed) save.
    assert_eq!(
        scheduled_status(&dir, id),
        "done",
        "the scheduled fire must commit to the db BEFORE save — save is the LAST mutation (bug-084)"
    );
    // and the degraded report still carries the fired event.
    assert!(
        report.scheduled.iter().any(|e| e.id == id),
        "the degraded report must still carry the fired scheduled event (lifecycle.py:356)"
    );
}

// P0 — tick must DELIVER a pending message against the REAL team.db (deliver_pending_messages,
// lifecycle.py:285). Observable: report.delivered carries the id OR the message row advances past
// its created 'accepted' state (claimed/delivered). NOTE: this also needs deliver_pending_messages
// (messaging/delivery.rs:126, currently a stub returning []) to be implemented — it is RED both
// because tick does not call it AND because the fn is a stub; it greens when both land.
#[test]
fn spine_tick_delivers_pending_message_drives_real_db_or_report() {
    let (coord, dir) = seeded_spine_coord(Box::new(DeliveringTransport::new()), None);
    let store = MessageStore::open(&dir).unwrap();
    let mid = store
        .create_message(
            Some("task-1"),
            "leader",
            "w1",
            "do the thing",
            None,
            true,
            None,
        )
        .unwrap();
    drop(store);
    assert_eq!(
        message_status(&dir, &mid),
        "accepted",
        "precondition: a fresh message is 'accepted'"
    );

    let report = coord.tick().expect("tick returns a typed report");

    let delivered_reported = report.delivered.iter().any(|d| d.message_id == mid);
    let db_advanced = message_status(&dir, &mid) != "accepted";
    assert!(
        delivered_reported || db_advanced,
        "tick must deliver the pending message: report.delivered should carry {mid} OR its db status \
         must advance past 'accepted' (claimed/delivered). report.delivered={:?} db_status={}",
        report.delivered,
        message_status(&dir, &mid)
    );
}

// ═════════════════════════════════════════════════════════════════════════
// SPINE-WIRING (③ review→fix) RED — tick tmux-session-missing gate observability.
// Golden lifecycle.py:277-279: emit a `coordinator.session_missing` event (session=name)
// BEFORE the stop report. /tmp/spine_divergences.md #5.
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn spine_tick_session_missing_emits_event() {
    // seeded_spine_coord seeds a truthy session_name ("team-spine"); MockTransport(false) makes the
    // tmux gate fire → the tick stops. The gate must ALSO emit coordinator.session_missing.
    let (coord, dir) = seeded_spine_coord(Box::new(MockTransport::new(false)), None);
    let report = coord.tick().expect("tick returns a typed report");
    assert!(
        report.stop,
        "precondition: a missing session stops the tick"
    );
    let events = read_event_log_dir(&dir);
    assert!(
        events
            .iter()
            .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("coordinator.session_missing")),
        "the tmux-missing gate must emit a coordinator.session_missing event before the stop report; got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("coordinator.session_missing_alert")),
        "the tmux-missing gate must emit an explicit leader-visible alert before stopping; got {events:?}"
    );
}
