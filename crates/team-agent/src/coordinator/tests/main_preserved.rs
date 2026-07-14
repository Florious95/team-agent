use super::spine::{message_status, DeliveringTransport};
use super::*;

// ═════════════════════════════════════════════════════════════════════════
// P0 REGRESSION (root cause pinned) — a per-agent capture/health-check FAILURE must not abort the
// whole tick. rt-host-a baseline @ ea4ba97: after `stop-agent w1`, `send w2` stays status='accepted'
// with delivery_attempts=0 while the coordinator stays alive. Root cause: sync_agent_health
// (tick.rs) does `self.transport.capture(&target, ...)?` per agent — when the stopped w1's window is
// gone the capture ERRORS and the `?` propagates -> sync_agent_health returns Err -> the tick
// early-returns Err BEFORE deliver_pending_messages -> the deliver loop never runs for the active w2.
// Contract: the tick must SWALLOW a per-agent capture/health failure (log + continue) and still run
// deliver_pending_messages for the other agents. Unit repro: a transport whose capture() ERRORS
// (window gone) but whose session exists + inject delivers -> the tick must (a) NOT return Err, and
// (b) still attempt delivery to the active w2 (delivery_attempts +1 / status advances past 'accepted').
// ═════════════════════════════════════════════════════════════════════════
fn message_delivery_attempts(dir: &std::path::Path, message_id: &str) -> i64 {
    let store = MessageStore::open(dir).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select delivery_attempts from messages where message_id = ?1",
        [message_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
}
/// A transport that DELIVERS (inject ok, session present) but whose `capture` ALWAYS fails — modelling a
/// stopped agent whose tmux window is gone. Exercises sync_agent_health's per-agent `capture?`: today the
/// error propagates and aborts the whole tick; the fix must swallow it and continue to deliver.
struct CaptureFailsDeliverTransport {
    inner: DeliveringTransport,
}
impl CaptureFailsDeliverTransport {
    fn new() -> Self {
        Self {
            inner: DeliveringTransport::new(),
        }
    }
}
impl Transport for CaptureFailsDeliverTransport {
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
        t: &Target,
        p: &InjectPayload,
        submit: Key,
        bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.inner.inject(t, p, submit, bracketed)
    }
    fn send_keys(&self, t: &Target, k: &[Key]) -> Result<(), TransportError> {
        self.inner.send_keys(t, k)
    }
    fn capture(&self, _t: &Target, _r: CaptureRange) -> Result<CapturedText, TransportError> {
        // the agent's window is gone — capture fails (tmux can't find the target).
        Err(TransportError::TargetNotFound {
            target: "window gone (stopped agent)".to_string(),
        })
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
#[test]
fn tick_swallows_capture_failure_and_still_delivers_to_other_agent() {
    let dir = std::env::temp_dir().join(format!(
        "team-agent-coord-stopreg-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // w1 STOPPED (window gone -> capture fails), w2 ACTIVE — w2 keeps the tmux session alive.
    crate::state::persist::save_runtime_state(
        &dir,
        &serde_json::json!({
            "session_name": "team-spine",
            "agents": {
                "w1": { "provider": "codex", "status": "stopped", "window": "w1" },
                "w2": { "provider": "codex", "status": "running", "window": "w2" }
            }
        }),
    )
    .unwrap();
    let store = MessageStore::open(&dir).unwrap();
    let mid = store
        .create_message(
            Some("task-1"),
            "leader",
            "w2",
            "after stop",
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
    let ws = WorkspacePath::new(dir.clone());
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    // capture() ERRORS (window gone) but inject delivers + session present.
    let coord = Coordinator::for_test(
        ws,
        reg,
        Box::new(CaptureFailsDeliverTransport::new()),
        None,
        None,
    );
    let result = coord.tick();
    // (a) a per-agent capture failure must NOT abort the whole tick.
    assert!(
        result.is_ok(),
        "P0: sync_agent_health's per-agent `capture?` must be SWALLOWED (log + continue), not propagated — \
         the tick must NOT early-return Err when a stopped agent's window-capture fails; got {result:?}"
    );
    let report = result.unwrap();
    // (b) the deliver loop must still run for the active w2.
    let attempts = message_delivery_attempts(&dir, &mid);
    let delivered_reported = report.delivered.iter().any(|d| d.message_id == mid);
    let advanced = message_status(&dir, &mid) != "accepted";
    assert!(
        attempts >= 1 || delivered_reported || advanced,
        "P0: after swallowing w1's capture failure, the tick MUST still deliver to the active w2 — its \
         message must advance past 'accepted' (delivery_attempts +1 / claimed / delivered). The regression \
         halts before deliver_pending_messages and leaves it at delivery_attempts=0. attempts={attempts} \
         status={} delivered={:?}",
        message_status(&dir, &mid),
        report.delivered
    );
}
