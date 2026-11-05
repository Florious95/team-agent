use super::*;
use crate::transport::test_support::OfflineTransport;

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════
// Golden re-probed via /tmp/probe_p2b_msg.py vs team-agent-public @ 439bef8.

// P0 — IRON LAW (bug-064/082): trust auto-answer must REFUSE a prompt whose path is a
// SUBDIRECTORY or SIBLING of the workspace. The current substring `.contains()` answers
// both (a subdir/sibling string contains the workspace as a substring). leader_panes.py
// requires EXACT canonical equality.

// P1 — owner-gate worker bypass: sender == custom leader_id (state.leader.id) or the
// 'Leader' literal must NOT bypass (state.py worker_sender_bypasses_owner_gate).

// P1 — owner-gate bypass honors the TEAM_AGENT_ID env identity gate: set and != sender → deny.

// P1 — classify_agent_activity must read current_command, stale last_output, multiline /
// Codex idle prompts, and Thinking/codex-spinner working indicators
// (activity_detector.py:90-146). **E47 amendment (0.3.24 P0)**: the
// working-indicator probe is now bottom-active-region scoped (last 1-3
// non-empty lines) and looks for STRUCTURAL spinner markers (codex
// `• Working (`/`Thinking`/braille; claude `✶`/`✢`), not bare lowercase
// `working` anywhere in the scrollback. Pre-fix rfind across the whole
// Tail(40) buffer is the macmini假阳 root cause (historical Working tokens
// out-positioning a live idle composer).

// P1 — scheduler dispatch must be exhaustive: an unknown kind surfaces an error, not a
// silent {ok:false} (scheduler.py: 'unknown scheduled event kind: <kind>').

// ═════════════════════════════════════════════════════════════════════════
// SPINE-WIRING (③ review→fix) RED — deliver_pending_messages + scheduler divergences
// vs golden v0.2.11 (messaging/delivery.py + scheduler.py + core.py). Probes in
// /tmp/spine_divergences.md (#1,#2,#6).
// ═════════════════════════════════════════════════════════════════════════

/// A delivery transport whose `inject` RECORDS-and-succeeds (the real deliver loop legitimately
/// injects; the §84-guard NoopTransport panics). Only `inject` is reached by deliver_pending_message.
#[derive(Default)]
struct DeliverOkTransport {
    targets: Vec<PaneInfo>,
}
impl Transport for DeliverOkTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn spawn_into(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _t: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.targets.clone())
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
        Ok(())
    }
}

fn read_event_log(ws: &Path) -> Vec<serde_json::Value> {
    let path = crate::model::paths::logs_dir(ws).join("events.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

// #1 — deliver_pending_messages delivers ONLY {pending,accepted}; queued_* rows are scheduler-owned
// and must be left untouched (golden delivery.py:484 `if row["status"] not in {"pending","accepted"}: continue`).

// Input is unconditional: native CLIs queue messages while working. Activity
// metadata must not prevent a pending message from reaching paste + Enter.



// CONTRACT (shared-root, real-machine-driven; golden = correct-behavior baseline): an ALIVE worker
// (lifecycle status="running") must be DELIVERABLE even when its turn-level agent_health=WORKING.
// golden's busy gate is state.agents[recipient].status=="busy" (delivery.py:491) and status is NEVER
// "busy" for an alive worker (only running/stopped), so golden never defers an alive worker. Rust
// recipient_is_busy (delivery.rs:291) reads `agent_health WHERE status='WORKING'` — turn-level
// idle-detection state — and defers. This is the Stage-B / stop-reset deferred_busy regression root:
// turn-level agent_health must NOT gate delivery; lifecycle status (alive) does.

// #6a — fire_due_scheduled_events marks 'done' if result.ok else 'failed' (scheduler.py:117).
// An EXHAUSTED trust_retry (attempt>=max) → handle_trust_retry_needed returns ok:false → 'failed'.

// U1 #5 (RED) — a single poison scheduled event (malformed payload_json that errors at
// `serde_json::from_str?`) must NOT halt the whole pass: it must be marked terminal 'failed'
// + emit `scheduler.event_failed`, and a healthy event seeded AFTER it must still fire.
// Today the bare `?` aborts `fire_due_scheduled_events`, the healthy event never fires → RED.

// #6b — due events fire in (due_at, id) order (core.py due_scheduled_events), not id order.

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
