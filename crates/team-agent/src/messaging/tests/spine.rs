use super::*;
use crate::transport::test_support::OfflineTransport;

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════
// Golden re-probed via /tmp/probe_p2b_msg.py vs team-agent-public @ 439bef8.

// P0 — IRON LAW (bug-064/082): trust auto-answer must REFUSE a prompt whose path is a
// SUBDIRECTORY or SIBLING of the workspace. The current substring `.contains()` answers
// both (a subdir/sibling string contains the workspace as a substring). leader_panes.py
// requires EXACT canonical equality.
#[test]
fn p2_trust_refuses_subdirectory_and_sibling_of_workspace() {
    let ws = tmp_ws("trustsubsib");
    let canonical = std::fs::canonicalize(&ws).unwrap();
    let log = EventLog::new(&ws);
    let t = NoopTransport;
    let pane = PaneId::new("%7");
    let canon = canonical.to_string_lossy().to_string();

    let subdir = format!("Allow Codex to write to {canon}/subproject ?");
    let s = attempt_trust_auto_answer(
        &canonical, &t, Some(&pane), &subdir, &PaneWidthQuery::Ok { pane_width: 200 }, &log,
    )
    .unwrap();
    assert!(!s.answered, "a SUBDIRECTORY of the workspace must NOT auto-answer trust");
    assert_eq!(s.reason, "workspace_dir_mismatch");

    let sibling = format!("Allow Codex to write to {canon}-backup ?");
    let sib = attempt_trust_auto_answer(
        &canonical, &t, Some(&pane), &sibling, &PaneWidthQuery::Ok { pane_width: 200 }, &log,
    )
    .unwrap();
    assert!(!sib.answered, "a SIBLING (<ws>-backup) must NOT auto-answer trust");
    assert_eq!(sib.reason, "workspace_dir_mismatch");
}

// P1 — owner-gate worker bypass: sender == custom leader_id (state.leader.id) or the
// 'Leader' literal must NOT bypass (state.py worker_sender_bypasses_owner_gate).
#[test]
fn p2_owner_bypass_rejects_custom_leader_id_and_capital_leader() {
    let ws = tmp_ws("bypassid");
    let log = EventLog::new(&ws);
    let target = MessageTarget::Single("leader".to_string());

    let s1 = serde_json::json!({"leader":{"id":"boss"},"agents":{"boss":{}}});
    assert!(
        !apply_worker_sender_bypass(&s1, Some("boss"), &target, None, &log).unwrap(),
        "sender == custom leader id must not bypass the owner gate"
    );
    let s2 = serde_json::json!({"agents":{"Leader":{}}});
    assert!(
        !apply_worker_sender_bypass(&s2, Some("Leader"), &target, None, &log).unwrap(),
        "'Leader' literal must not bypass"
    );
}

// P1 — owner-gate bypass honors the TEAM_AGENT_ID env identity gate: set and != sender → deny.
#[test]
#[serial_test::serial(env)]
fn p2_owner_bypass_denies_on_env_agent_id_mismatch() {
    let _g = ENV_LOCK_MSG.lock().unwrap_or_else(|p| p.into_inner());
    let _e = EnvGuardMsg::set("TEAM_AGENT_ID", Some("other"));
    let ws = tmp_ws("bypassenv");
    let log = EventLog::new(&ws);
    let target = MessageTarget::Single("leader".to_string());
    let s = serde_json::json!({"agents":{"w1":{}}});
    assert!(
        !apply_worker_sender_bypass(&s, Some("w1"), &target, None, &log).unwrap(),
        "TEAM_AGENT_ID set and != sender must deny the bypass"
    );
}

// P1 — classify_agent_activity must read current_command, stale last_output, multiline /
// Codex idle prompts, and Thinking/codex-spinner working indicators
// (activity_detector.py:90-146). **E47 amendment (0.3.24 P0)**: the
// working-indicator probe is now bottom-active-region scoped (last 1-3
// non-empty lines) and looks for STRUCTURAL spinner markers (codex
// `• Working (`/`Thinking`/braille; claude `✶`/`✢`), not bare lowercase
// `working` anywhere in the scrollback. Pre-fix rfind across the whole
// Tail(40) buffer is the macmini假阳 root cause (historical Working tokens
// out-positioning a live idle composer).
#[test]
fn p2_classify_activity_reads_command_stale_and_prompts() {
    let st = serde_json::json!({});
    // (1) non-provider current_command → uncertain 0.75 (current drops command → 0.5).
    let a = classify_agent_activity(&st, "", false, Some("vim"), None);
    assert_eq!((a.status, a.confidence), (ActivityStatus::Uncertain, 0.75));
    // (2) stale last_output (≥ stuck_timeout) → stuck 0.85.
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(400)).to_rfc3339();
    let b = classify_agent_activity(&st, "", false, None, Some(&stale));
    assert_eq!((b.status, b.confidence), (ActivityStatus::Stuck, 0.85));
    // (3) embedded multiline idle prompt (Codex ❯ not on its own trimmed line) → idle 0.9.
    let c = classify_agent_activity(&st, "some line\n❯\nmore", false, None, None);
    assert_eq!((c.status, c.confidence), (ActivityStatus::Idle, 0.9));
    // (4) 'Thinking' working indicator in the bottom active region → working 0.9.
    let d = classify_agent_activity(&st, "Thinking about it", false, None, None);
    assert_eq!((d.status, d.confidence), (ActivityStatus::Working, 0.9));
    // (5) E47 amendment: bare lowercase 'working on it' is NOT a structural
    // spinner marker; the live codex spinner is `• Working (Ns · esc to
    // interrupt)`. Treat as no-decisive-signal → Uncertain. The pre-fix
    // expectation here is exactly the macmini假阳 shape — a stale 'working'
    // token (without the parenthesised seconds + esc-to-interrupt tail) is
    // either scrollback residue or unrelated text, not a live working
    // indicator. To assert the Working path use the structural codex shape.
    let e = classify_agent_activity(&st, "working on it", false, None, None);
    assert_eq!((e.status, e.confidence), (ActivityStatus::Uncertain, 0.5));
    // (5b) E47 structural Working: a real codex live spinner in the bottom
    // active region must still classify Working 0.9.
    let e_struct = classify_agent_activity(
        &st,
        "• Working (12s · esc to interrupt)",
        false,
        None,
        None,
    );
    assert_eq!((e_struct.status, e_struct.confidence), (ActivityStatus::Working, 0.9));
}

// P1 — scheduler dispatch must be exhaustive: an unknown kind surfaces an error, not a
// silent {ok:false} (scheduler.py: 'unknown scheduled event kind: <kind>').
#[test]
fn p2_scheduler_unknown_kind_surfaces_error() {
    let ws = tmp_ws("schedunknown");
    let store = store_for(&ws);
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at) \
         values (null, '2000-01-01T00:00:00+00:00', 't', 'bogus_kind', '{}', 'pending', '2000-01-01T00:00:00+00:00')",
        [],
    )
    .unwrap();
    let log = EventLog::new(&ws);
    // U1 #5: unknown kind is isolated to its own row (marked terminal 'failed' +
    // scheduler.event_failed), NOT propagated as a pass-level Err that would halt the
    // batch. Failure stays loud (evented) but does not take the whole scheduler down.
    let fired = fire_due_scheduled_events(&ws, &store, &NoopTransport, &log)
        .expect("an unknown scheduled event kind must be isolated to its row, not halt the pass");
    assert_eq!(fired.len(), 1);
    assert_eq!(scheduled_status_of(&store, fired[0]), "failed");
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("scheduler.event_failed")
                && event.get("event_id").and_then(serde_json::Value::as_i64) == Some(fired[0])
        }),
        "unknown scheduled event kind must leave scheduler.event_failed; events={events:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// SPINE-WIRING (③ review→fix) RED — deliver_pending_messages + scheduler divergences
// vs golden v0.2.11 (messaging/delivery.py + scheduler.py + core.py). Probes in
// /tmp/spine_divergences.md (#1,#2,#6).
// ═════════════════════════════════════════════════════════════════════════

/// A delivery transport whose `inject` RECORDS-and-succeeds (the real deliver loop legitimately
/// injects; the §84-guard NoopTransport panics). Only `inject` is reached by deliver_pending_message.
struct DeliverOkTransport;
impl Transport for DeliverOkTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(&self, _s: &SessionName, _w: &WindowName, _a: &[String], _c: &Path, _e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn spawn_into(&self, _s: &SessionName, _w: &WindowName, _a: &[String], _c: &Path, _e: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn inject(&self, _t: &Target, _p: &InjectPayload, _s: Key, _b: bool) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: crate::transport::InjectStage::Submit,
            inject_verification: crate::transport::InjectVerification::CaptureContainsToken,
            submit_verification: crate::transport::SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: crate::transport::TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _t: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: String::new(), range })
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
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

fn set_message_status(store: &MessageStore, message_id: &str, status: &str) {
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute("update messages set status = ?2 where message_id = ?1", sql_params![message_id, status]).unwrap();
}
fn message_status_of(store: &MessageStore, message_id: &str) -> String {
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row("select status from messages where message_id = ?1", sql_params![message_id], |r| r.get::<_, String>(0)).unwrap()
}
fn scheduled_status_of(store: &MessageStore, id: i64) -> String {
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row("select status from scheduled_events where id = ?1", [id], |r| r.get::<_, String>(0)).unwrap()
}
fn seed_agent_health(store: &MessageStore, agent_id: &str, status: &str) {
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at) \
         values (null, ?1, ?2, null, null, null, ?3)",
        sql_params![agent_id, status, chrono::Utc::now().to_rfc3339()],
    )
    .unwrap();
}
fn seed_event_due(store: &MessageStore, kind: &str, due_at: &str, payload: &str) -> i64 {
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at) \
         values (null, ?1, 't', ?2, ?3, 'pending', ?1)",
        sql_params![due_at, kind, payload],
    )
    .unwrap();
    conn.last_insert_rowid()
}
fn read_event_log(ws: &Path) -> Vec<serde_json::Value> {
    let path = crate::model::paths::logs_dir(ws).join("events.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
        Err(_) => Vec::new(),
    }
}

// #1 — deliver_pending_messages delivers ONLY {pending,accepted}; queued_* rows are scheduler-owned
// and must be left untouched (golden delivery.py:484 `if row["status"] not in {"pending","accepted"}: continue`).
#[test]
fn spine_delivery_skips_queued_statuses() {
    let ws = tmp_ws("deliver-queued");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let _acc = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    let qidle = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    let qstart = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    set_message_status(&store, &qidle, "queued_until_idle");
    set_message_status(&store, &qstart, "queued_until_start");
    let state = serde_json::json!({"agents": {"w1": {}}});

    let _ = deliver_pending_messages(&ws, &state, &DeliverOkTransport, &log).unwrap();

    assert_eq!(message_status_of(&store, &qidle), "queued_until_idle", "queued_until_idle must NOT be claimed/injected/marked by deliver_pending");
    assert_eq!(message_status_of(&store, &qstart), "queued_until_start", "queued_until_start must NOT be touched by deliver_pending");
}

// #2 (CONTRACT, corrected) — busy recipient = LIFECYCLE status=="busy" → emit ONE send.deferred_busy
// {recipient,reason:"recipient_busy"} and do NOT deliver (row stays 'accepted'). golden delivery.py:491
// gates on `state.agents[recipient].status == "busy"`, NOT on turn-level agent_health=WORKING.
// (The prior version of this test seeded agent_health=WORKING + status="running" and asserted deferral,
//  misreading golden — that stale lock is the root of the deferred_busy regression; corrected here.)
#[test]
fn spine_delivery_busy_recipient_defers_with_event() {
    let ws = tmp_ws("deliver-busy");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let mid = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    let state = serde_json::json!({"agents": {"w1": {"status": "busy"}}});

    let delivered = deliver_pending_messages(&ws, &state, &DeliverOkTransport, &log).unwrap();

    assert!(!delivered.contains(&mid), "a busy recipient's message must NOT be delivered");
    assert_eq!(message_status_of(&store, &mid), "accepted", "the row must stay 'accepted' (no-drop, left for a later tick)");
    let events = read_event_log(&ws);
    let deferred: Vec<_> = events
        .iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("send.deferred_busy"))
        .collect();
    assert_eq!(deferred.len(), 1, "exactly one send.deferred_busy event expected; got {events:?}");
    assert_eq!(deferred[0].get("recipient").and_then(|v| v.as_str()), Some("w1"));
    assert_eq!(deferred[0].get("reason").and_then(|v| v.as_str()), Some("recipient_busy"));
}

// CONTRACT (shared-root, real-machine-driven; golden = correct-behavior baseline): an ALIVE worker
// (lifecycle status="running") must be DELIVERABLE even when its turn-level agent_health=WORKING.
// golden's busy gate is state.agents[recipient].status=="busy" (delivery.py:491) and status is NEVER
// "busy" for an alive worker (only running/stopped), so golden never defers an alive worker. Rust
// recipient_is_busy (delivery.rs:291) reads `agent_health WHERE status='WORKING'` — turn-level
// idle-detection state — and defers. This is the Stage-B / stop-reset deferred_busy regression root:
// turn-level agent_health must NOT gate delivery; lifecycle status (alive) does.
#[test]
fn contract_alive_worker_with_working_health_is_deliverable_not_deferred() {
    let ws = tmp_ws("deliver-alive-working");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let mid = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    seed_agent_health(&store, "w1", "WORKING"); // turn-level state — must NOT gate delivery
    let state = serde_json::json!({"agents": {"w1": {"status": "running"}}}); // lifecycle: alive

    let delivered = deliver_pending_messages(&ws, &state, &DeliverOkTransport, &log).unwrap();

    let events = read_event_log(&ws);
    let deferred: Vec<_> = events
        .iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("send.deferred_busy"))
        .collect();
    assert!(
        deferred.is_empty(),
        "CONTRACT: an alive worker (lifecycle status=running) must NOT be deferred_busy on agent_health=WORKING; \
         golden busy gate is lifecycle status=='busy' (never set for alive workers). got {deferred:?}"
    );
    assert!(
        delivered.contains(&mid),
        "the message to an alive worker must be delivered (round-trip), not deferred. delivered={delivered:?}"
    );
}

// #6a — fire_due_scheduled_events marks 'done' if result.ok else 'failed' (scheduler.py:117).
// An EXHAUSTED trust_retry (attempt>=max) → handle_trust_retry_needed returns ok:false → 'failed'.
#[test]
fn spine_scheduler_marks_failed_when_result_not_ok() {
    let ws = tmp_ws("sched-failed");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let payload = serde_json::json!({"message_id": "msg_x", "attempt": 5, "max_attempts": 5, "first_target": "%1"}).to_string();
    let id = seed_event_due(&store, "trust_retry", "2000-01-01T00:00:00+00:00", &payload);

    let _ = fire_due_scheduled_events(&ws, &store, &NoopTransport, &log).unwrap();

    assert_eq!(
        scheduled_status_of(&store, id),
        "failed",
        "a not-ok result (exhausted trust_retry) must mark the scheduled event 'failed', not unconditional 'done'"
    );
}

// U1 #5 (RED) — a single poison scheduled event (malformed payload_json that errors at
// `serde_json::from_str?`) must NOT halt the whole pass: it must be marked terminal 'failed'
// + emit `scheduler.event_failed`, and a healthy event seeded AFTER it must still fire.
// Today the bare `?` aborts `fire_due_scheduled_events`, the healthy event never fires → RED.
#[test]
fn spine_scheduler_poison_event_does_not_halt_batch() {
    let ws = tmp_ws("sched-poison");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    // earlier due_at → selected first; malformed JSON payload errors at from_str.
    let poison = seed_event_due(&store, "send", "2000-01-01T00:00:00+00:00", "{not valid json");
    // later due_at → selected after poison; healthy.
    let healthy = seed_event_due(&store, "health_ping", "2000-01-02T00:00:00+00:00", "{}");

    let fired = fire_due_scheduled_events(&ws, &store, &NoopTransport, &log)
        .expect("a poison event must not propagate as a pass-level Err (must isolate + continue)");

    // poison marked terminal failed (not re-fired every tick).
    assert_eq!(
        scheduled_status_of(&store, poison),
        "failed",
        "U1 #5: a poison scheduled event must be marked terminal 'failed', not halt the pass"
    );
    // healthy event AFTER the poison still fired.
    assert!(
        fired.contains(&healthy),
        "U1 #5: a healthy event after a poison one must still fire (batch not halted); fired={fired:?}"
    );
    // failure is loud: scheduler.event_failed event emitted for the poison.
    let events = read_event_log(&ws);
    assert!(
        events.iter().any(|e| e.get("event").and_then(|v| v.as_str()) == Some("scheduler.event_failed")),
        "U1 #5: a poison event must emit scheduler.event_failed (failure must be loud, not silent)"
    );
}

// #6b — due events fire in (due_at, id) order (core.py due_scheduled_events), not id order.
#[test]
fn spine_scheduler_orders_due_events_by_due_at_then_id() {
    let ws = tmp_ws("sched-order");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    // smaller id is due LATER → due_at order must reverse the id order.
    let a_late = seed_event_due(&store, "health_ping", "2000-01-02T00:00:00+00:00", "{}");
    let b_early = seed_event_due(&store, "health_ping", "2000-01-01T00:00:00+00:00", "{}");

    let fired = fire_due_scheduled_events(&ws, &store, &NoopTransport, &log).unwrap();

    assert_eq!(
        fired,
        vec![b_early, a_late],
        "fire order must be by (due_at, id) — earlier-due fires first; current orders by id only. got {fired:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// rt-host-a LOOP #3 — coordinator deliver tick injects to the AGENT ID treated as a bare PANE-ID.
// delivery.rs:127 builds Target::Pane(PaneId::new(message.recipient)) where recipient is the agent id
// ("w1"). A coordinator process with NO attached tmux client cannot resolve a bare name -> every tick
// "can't find pane: w1" -> message stuck -> no delivery. The fix (porter/leader): resolve the recipient
// to a SESSION-QUALIFIED target (SessionWindow{session: state.session_name, window: agent.window}) OR
// the persisted state.agents[recipient].pane_id — mirroring coordinator/tick.rs::capture_target —
// NEVER Target::Pane(agent_id). 793/0/21 missed it: the deliver tests use a non-asserting transport.
// ═════════════════════════════════════════════════════════════════════════

// RED — deliver_pending_messages must inject to a RESOLVABLE target (session-qualified or a real
// pane-id), NOT the bare agent-id treated as a pane. Today delivery.rs:127 records
// Target::Pane(PaneId("w1")) -> a non-attached coordinator can't resolve it -> RED at assert_ne!.
#[test]
fn spine_delivery_injects_resolvable_target_not_bare_agent_pane() {
    let ws = tmp_ws("deliver-target");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let _mid = store.create_message(Some("t"), "leader", "w1", "hi", None, true, None).unwrap();
    // in-team, non-busy agent -> deliver proceeds; the session-qualified resolution uses session_name + window.
    let state = serde_json::json!({
        "session_name": "team-x",
        "agents": {"w1": {"status": "idle", "window": "w1"}}
    });
    let transport = OfflineTransport::new();

    let _ = deliver_pending_messages(&ws, &state, &transport, &log).unwrap();

    let recorded = transport.inject_targets();
    assert_eq!(recorded.len(), 1, "deliver must inject the one pending message exactly once; got {recorded:?}");
    let target = recorded[0].clone();
    // THE BUG (rt-host-a #3): deliver builds Target::Pane(PaneId(message.recipient)) — the agent id as a
    // BARE pane, which a non-attached coordinator can't resolve ("can't find pane: w1").
    assert_ne!(
        target,
        Target::Pane(PaneId::new("w1")),
        "deliver must NOT inject to the bare agent-id as a pane-id (a non-attached coordinator can't \
         resolve it -> 'can't find pane: w1'); expected a SESSION-QUALIFIED target \
         (SessionWindow{{session: team-x, window: w1}}) or the persisted pane-id, got {target:?}"
    );
    // ...and it must be a genuinely resolvable target: session-qualified (session=team-x) or a real
    // pane-id (anything but the bare agent name).
    let resolvable = match &target {
        Target::SessionWindow { session, .. } => session.as_str() == "team-x",
        Target::Pane(pane) => pane.as_str() != "w1",
    };
    assert!(
        resolvable,
        "deliver must resolve recipient 'w1' to session=team-x (session-qualified) or a real pane-id; got {target:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// rt-host-a LOOP #4 — coordinator injects the RAW message content, but the worker only builds a
// result_envelope when it sees the RENDERED protocol block ending [team-agent-token:<message_id>]. So
// workers go WORKING but never report -> results=0, no round-trip. delivery.rs:131 injects
// InjectPayload::Text(message.content) (bare). The fix (leader): port render_message before inject —
// GOLDEN (rust_core.py:60-73, captured live):
//   "Team Agent message from {sender}[ for {task_id}]:\n\n{content}\n\n[team-agent-token:{message_id}]"
// ═════════════════════════════════════════════════════════════════════════

// RED — deliver must inject the RENDERED protocol block (header + content + [team-agent-token:<id>]),
// NOT the raw content. Today the payload is the bare "do the thing" (no header, no token) -> RED.
#[test]
fn spine_delivery_injects_rendered_protocol_block_not_raw_content() {
    let ws = tmp_ws("deliver-render");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let mid = store.create_message(Some("t1"), "leader", "w1", "do the thing", None, true, None).unwrap();
    let state = serde_json::json!({
        "session_name": "team-x",
        "agents": {"w1": {"status": "idle", "window": "w1"}}
    });
    let transport = OfflineTransport::new();

    let _ = deliver_pending_messages(&ws, &state, &transport, &log).unwrap();

    let recorded = transport.inject_payloads();
    assert_eq!(recorded.len(), 1, "deliver must inject the one pending message exactly once; got {recorded:?}");
    let payload = recorded[0].clone();
    // THE BUG (rt-host-a #4): deliver injects the RAW content -> the worker never reports (it only builds
    // a result_envelope on the rendered block with the token).
    assert_ne!(
        payload, "do the thing",
        "deliver must NOT inject the bare content; the worker only reports on the rendered protocol block \
         (with [team-agent-token:<id>]) — bare text -> WORKING but never reports -> results=0; got {payload:?}"
    );
    assert!(
        payload.contains("Team Agent message from leader for t1:"),
        "the injected payload must be the rendered protocol header (sender + task): 'Team Agent message \
         from leader for t1:'; got {payload:?}"
    );
    assert!(
        payload.contains("do the thing"),
        "the rendered block must carry the content; got {payload:?}"
    );
    let token_line = format!("[team-agent-token:{mid}]");
    assert!(
        payload.contains(&token_line),
        "the rendered block must end with the token line {token_line:?} (token == message_id) so the \
         worker builds a result_envelope; got {payload:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// E47 (0.3.24 P0, idle/busy 假阳) — TUI keyword grep is structurally bounded.
//
// Real-machine repro (macmini): a Tail(40) capture of an idle codex worker
// still carries historical `• Working (514s · esc to interrupt)` plus a
// `─ Worked for 8m 34s ─` summary plus a fresh `❯ Run /review` prompt. The
// pre-fix `working_seconds` full-buffer find matched the 514s token and
// returned ≥300 → Stuck. The pre-fix `latest_prompt_signal` rfind let the
// historical spinner out-position the bottom `❯` → Working. E47 narrows
// both probes to the bottom active region (last 1-3 non-empty lines).
//
// The authoritative provider JSONL classify is wired in coordinator/tick.rs
// (jsonl_activity_for_agent); these unit tests pin the TUI fallback layer.
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn e47_codex_idle_with_historical_working_in_scrollback_is_idle_not_stuck_or_working() {
    // Exact macmini repro shape from the architect locate (E47).
    let scrollback = "• Working (514s · esc to interrupt)\n\
                      tool call 1\n\
                      tool call 2\n\
                      ─ Worked for 8m 34s ─\n\
                      › Run /review\n\
                      ❯ ";
    let st = serde_json::json!({});
    let a = classify_agent_activity(&st, scrollback, false, None, None);
    assert_eq!(
        a.status,
        ActivityStatus::Idle,
        "E47 (RED-1): codex idle worker whose scrollback still carries \
         historical `• Working (514s · esc to interrupt)` must classify IDLE \
         (bottom active region is `❯`+`› Run /review` = idle composer). \
         pre-fix: Stuck/Working via rfind-recency. Got {a:?}"
    );
}

#[test]
fn e47_past_tense_worked_for_is_not_stale_working_indicator() {
    // RED-2: past-tense `Worked for` summary line must NOT trigger
    // working_seconds Stuck. The pre-fix scanned the whole buffer and would
    // match the embedded `8m 34s ─` against `working (` if formatted slightly
    // differently — but more importantly this test pins the "past-tense
    // summary is idle context" semantics.
    let scrollback = "─ Worked for 8m 34s ─\n❯ ";
    let st = serde_json::json!({});
    let a = classify_agent_activity(&st, scrollback, false, None, None);
    assert_ne!(
        a.status,
        ActivityStatus::Stuck,
        "E47 (RED-2): past-tense `Worked for 8m 34s` must NOT trigger \
         stale_working_indicator Stuck. Got {a:?}"
    );
    assert_eq!(
        a.status,
        ActivityStatus::Idle,
        "E47 (RED-2): bottom `❯` composer = idle. Got {a:?}"
    );
}

#[test]
fn e47_claude_idle_with_historical_spinner_classifies_idle() {
    // RED-3: claude idle worker; historical spinner shape (`✶` per
    // adapter.rs:875-876) earlier in the buffer (>= 3 non-empty lines deep),
    // bottom composer is idle (`›` glyph variant or `> ` claude prompt).
    // The bottom active region (last 3 non-empty lines) has NO spinner.
    let scrollback = "✶ Working on plan\n\
                      tool 1\n\
                      tool 2\n\
                      assistant reply text\n\
                      summary line A\n\
                      summary line B\n\
                      › Continue?";
    let st = serde_json::json!({});
    let a = classify_agent_activity(&st, scrollback, false, None, None);
    assert_eq!(
        a.status,
        ActivityStatus::Idle,
        "E47 (RED-3): claude idle worker — historical ✶ above, idle `›` \
         below — must classify IDLE. Got {a:?}"
    );
}

#[test]
fn e47_codex_live_spinner_in_bottom_active_region_classifies_working() {
    // RED-4 (defence vs over-tightening 假阴): a TRULY busy codex worker
    // whose bottom active region carries the live spinner must still be
    // Working. No idle composer present.
    let scrollback = "tool call x\nassistant response so far\n• Working (12s · esc to interrupt)";
    let st = serde_json::json!({});
    let a = classify_agent_activity(&st, scrollback, false, None, None);
    assert_eq!(
        a.status,
        ActivityStatus::Working,
        "E47 (RED-4): live codex spinner in bottom active region must \
         classify Working. Got {a:?}"
    );
}

#[test]
fn e47_claude_live_working_in_bottom_active_region_classifies_working() {
    // RED-5: a TRULY busy claude worker showing `✶ Working` at the bottom
    // active region must be Working — even without the `(Ns)` codex shape.
    let scrollback = "earlier output\nmore output\n✶ Working through the request";
    let st = serde_json::json!({});
    let a = classify_agent_activity(&st, scrollback, false, None, None);
    assert_eq!(
        a.status,
        ActivityStatus::Working,
        "E47 (RED-5): claude ✶ Working in bottom active region → Working. \
         Got {a:?}"
    );
}

#[test]
fn e47_iron_law_no_signal_in_bottom_region_stays_uncertain_not_idle() {
    // Defence: IRON LAW (activity.rs:3 / bug-071/077/085). When the bottom
    // active region has no spinner AND no `❯`/`›` AND no other decisive
    // signal, classify_agent_activity falls through to no_decisive_signal →
    // Uncertain — NEVER silently to Idle.
    let scrollback = "some neutral text\nmore neutral text\nstill nothing decisive";
    let st = serde_json::json!({});
    let a = classify_agent_activity(&st, scrollback, false, None, None);
    assert_eq!(
        a.status,
        ActivityStatus::Uncertain,
        "E47 IRON LAW guard: bottom region carries no spinner and no idle \
         composer → Uncertain, never silently Idle. Got {a:?}"
    );
}
