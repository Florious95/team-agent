//! 0.5.54 P0 · Codex weak-window session attribution RED (verifier).
//!
//! From gate4-c-attribution-locate.md + defect-weak-window-attribution.md
//! (2/2 real-machine reproduction: original 140 collisions / fresh case1 811
//! collisions, worker session null before AND after restart while message
//! delivery stayed healthy) — NOT from any proposed fix. Baseline `a909876`.
//!
//! Phenomenon (event-chain reconstruction, locate §Event-chain):
//! 1. `parent_peer` and `worker_a` spawn as one same-workspace codex cohort.
//! 2. An early scan exposes exactly ONE candidate — the worker's rollout in a
//!    weak, identity-not-yet-parsed view.
//! 3. The weak `Any` fallback first-binds that candidate to `parent_peer`.
//! 4. The rollout later proves the exact embedded `worker_a` identity, but the
//!    session key is already claimed: `worker_a` repeats
//!    `attribution_ambiguous(reason=claimed_collision)` forever.
//! 5. Restart cannot prove same-session preservation: worker fresh-starts and
//!    loses its context.
//!
//! Required invariant (locate §Single-truth-source): a candidate with positive
//! embedded `agent_id` proof belongs to that agent; weak fallback cannot
//! preempt that identity during the bounded cohort convergence window.
//! Collision handling stays fail-closed — never steal a positively or durably
//! claimed foreign session.
//!
//! CONTRACT LINEAGE (frozen-SHA evolution, leader ruling msg_ab2a1fecc308):
//! - 484d593c…afee7 = short-record form, four assertions (w1-w4), frozen at
//!   9c41975 on baseline a909876.
//! - THIS revision (re-frozen, same file, new SHA) adds w5/w5b/w5c after the
//!   Gate-4 ROUND-2 real-machine ground truth (gate4-round2-evidence.md +
//!   locate Round 2 addendum): the real worker rollout carries its embedded
//!   identity inside a 44357-byte record whose terminating newline lands past
//!   the CAPTURE_HEAD_BYTES=65536 head cap; `read_head_text` truncates to the
//!   last complete newline, so the WHOLE record is discarded and the rollout
//!   scans as weak forever — 00d63cc's shared-candidate deferral then never
//!   converges (both agents terminally ambiguous, nobody binds). The w1
//!   short-record fixture was a false premise for that real shape (sister
//!   discipline: contract fixture data shapes must take real-machine samples
//!   as ground truth). w1-w4 assertion semantics are byte-untouched.
//!
//! DETERMINISTIC TIMING SEAM (no real-provider race, no sleeps, no waiting):
//! scan visibility/order is pinned by fixture-controlled rollout FILE CONTENT
//! between explicit one-shot `capture_missing_provider_sessions_once` calls —
//! tick 1 sees the weak view (no embedded identity yet), tick 2 sees the same
//! rollout with the exact embedded `worker_a` identity. This drives the REAL
//! canonical scanner + allocator (`scan_session_candidates_once` →
//! `allocate_session_candidates`) — the surfaces the locate names — with zero
//! dependence on host load or wall-clock windows. `spawned_at` is a fixed
//! past timestamp so the spawn-time filters pass deterministically.
//! Hermeticity: HOME is redirected to a per-test root (HomeGuard, serial(env));
//! no ambient provider home, no real tmux, no network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::{classify_restart_plan, StartMode};
use team_agent::model::enums::Provider;
use team_agent::provider::get_adapter;
use team_agent::provider::session::capture::{
    capture_missing_provider_sessions_once, CapturePassReport,
};

const PARENT: &str = "parent_peer";
const WORKER: &str = "worker_a";
/// Fixed past spawn boundary — both cohort members spawned before the rollout
/// was created, so `apply_spawned_at_filter` passes deterministically. NOTE:
/// the boundary must precede the session ids' uuid-v7 embedded timestamp
/// (`created_at_from_rollout_uuid` takes precedence over file mtime).
const SPAWNED_AT: &str = "2026-01-01T00:00:00Z";
const WORKER_SESSION: &str = "019f0054-70aa-7bbb-8ccc-000000000001";
const PARENT_SESSION: &str = "019f0054-70aa-7bbb-8ccc-000000000002";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    spawn_cwd: PathBuf,
    _home: HomeGuard,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let raw_root = std::env::temp_dir().join(format!(
            "ta-weakwin-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&raw_root);
        std::fs::create_dir_all(&raw_root).unwrap();
        // Canonicalize BEFORE deriving HOME/spawn_cwd so every path the
        // scanner records (rooted at $HOME) is byte-comparable with the
        // fixture's expectations (macOS /var vs /private/var symlink).
        let root = std::fs::canonicalize(&raw_root).unwrap();
        let spawn_cwd = root.join("ws");
        std::fs::create_dir_all(&spawn_cwd).unwrap();
        let guard = HomeGuard::with_home(root.join("home"));
        Self {
            root,
            spawn_cwd,
            _home: guard,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn sessions_dir(&self) -> PathBuf {
        let dir = self.home().join(".codex/sessions/2026/07/22");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rollout_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("rollout-2026-07-22T10-00-00-{session_id}.jsonl"))
    }

    /// WEAK VIEW — the rollout as an early scan sees it: session meta + cwd +
    /// conversation, identity NOT yet present. This is the locate's
    /// "one-candidate weak-window view".
    fn write_weak_rollout(&self, session_id: &str) -> PathBuf {
        let path = self.rollout_path(session_id);
        std::fs::write(&path, self.rollout_text(session_id, None)).unwrap();
        path
    }

    /// STRONG VIEW — the same rollout once the exact embedded Team Agent
    /// worker identity has been written by the provider.
    fn write_identity_rollout(&self, session_id: &str, agent_id: &str) -> PathBuf {
        let path = self.rollout_path(session_id);
        std::fs::write(&path, self.rollout_text(session_id, Some(agent_id))).unwrap();
        path
    }

    fn rollout_text(&self, session_id: &str, embedded_agent_id: Option<&str>) -> String {
        let mut text = format!(
            "{}\n{}\n",
            json!({
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": self.spawn_cwd.to_string_lossy().to_string()
                }
            }),
            json!({
                "type": "response_item",
                "payload": {
                    "content": [{"type": "input_text", "text": "cohort conversation record"}]
                }
            }),
        );
        if let Some(agent_id) = embedded_agent_id {
            text.push_str(&format!(
                "{}\n",
                json!({
                    "type": "response_item",
                    "payload": {
                        "content": [{
                            "type": "input_text",
                            "text": format!(
                                "You are Team Agent worker `{agent_id}` with role `fixture`."
                            )
                        }]
                    }
                })
            ));
        }
        text
    }

    fn agent_row(&self) -> Value {
        json!({
            "status": "running",
            "provider": "codex",
            "spawn_cwd": self.spawn_cwd.to_string_lossy().to_string(),
            "spawned_at": SPAWNED_AT,
        })
    }

    /// Two-member spawn cohort: parent_peer + worker_a, same workspace,
    /// neither has a session yet (locate §Event-chain step 1).
    fn cohort_state(&self) -> Value {
        json!({
            "agents": {
                PARENT: self.agent_row(),
                WORKER: self.agent_row(),
            }
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct HomeGuard {
    previous_home: Option<String>,
}

impl HomeGuard {
    fn with_home(home: PathBuf) -> Self {
        std::fs::create_dir_all(&home).unwrap();
        let previous_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self { previous_home }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous_home.take() {
                std::env::set_var("HOME", value);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }
}

fn capture_tick(state: &mut Value) -> CapturePassReport {
    capture_missing_provider_sessions_once(state, &mut |p: Provider| get_adapter(p), false, 0)
        .expect("capture pass must not error")
}

fn agent_field<'a>(state: &'a Value, agent: &str, field: &str) -> Option<&'a str> {
    state
        .get("agents")
        .and_then(|a| a.get(agent))
        .and_then(|a| a.get(field))
        .and_then(Value::as_str)
}

/// W1 (RED core) — positive embedded identity must reclaim / must never be
/// preempted by the weak-window first binding.
///
/// Tick 1: only the worker's rollout is visible, in the weak
/// identity-not-yet-parsed view (single candidate). Tick 2..3: the same
/// rollout now carries the exact embedded `worker_a` identity — the bounded
/// cohort convergence window.
///
/// Baseline red (locate steps 3-4): at tick 1 the weak `Any` fallback binds
/// the sole candidate to `parent_peer`; the worker's later positive proof
/// collides with the claimed key and `worker_a` never converges.
#[test]
#[serial(env)]
fn w1_weak_window_first_bind_must_not_preempt_positive_worker_identity() {
    let fx = Fixture::new("w1");
    let rollout = fx.write_weak_rollout(WORKER_SESSION);
    let mut state = fx.cohort_state();

    // Tick 1 — weak single-candidate window.
    let _ = capture_tick(&mut state);
    // Tick 2..3 — the rollout now proves the exact worker identity; the
    // cohort convergence window is bounded (two ticks is plenty for a
    // deterministic allocator — no wait widening allowed).
    fx.write_identity_rollout(WORKER_SESSION, WORKER);
    let _ = capture_tick(&mut state);
    let final_report = capture_tick(&mut state);

    let parent_session = agent_field(&state, PARENT, "session_id");
    let worker_session = agent_field(&state, WORKER, "session_id");
    let worker_rollout = agent_field(&state, WORKER, "rollout_path");

    assert_ne!(
        parent_session,
        Some(WORKER_SESSION),
        "the parent must NEVER commit the worker's session: the rollout's embedded identity \
         proves it belongs to `{WORKER}`; a weak identity-unproven first binding must not be \
         irreversible (locate required invariant). state={state}"
    );
    assert_eq!(
        worker_session,
        Some(WORKER_SESSION),
        "`{WORKER}` must converge to its OWN session tuple once its positive embedded identity \
         is scannable within the bounded cohort window; baseline leaves it null forever \
         (2/2 real-machine: 140/811 claimed_collision rows). state={state}"
    );
    assert_eq!(
        worker_rollout,
        Some(rollout.to_string_lossy().as_ref()),
        "`{WORKER}` must converge to the exact rollout path (tuple atomicity). state={state}"
    );
    assert!(
        !final_report
            .ambiguous
            .iter()
            .any(|entry| entry.agent_id == WORKER),
        "after convergence there must be ZERO residual ambiguity for `{WORKER}` (the coordinator \
         relabels this exact shape as `claimed_collision` and repeats it every tick). \
         ambiguous={:?}",
        final_report.ambiguous
    );
}

/// W2 (positive control, naturally green) — the NORMAL window: the very first
/// scan already exposes the embedded `worker_a` identity. The worker binds its
/// own tuple positively, the parent binds nothing (its rollout does not exist),
/// and there is zero ambiguity. Guards against a fix that "solves" W1 by
/// rejecting or deferring correct attribution wholesale (constant-behavior
/// false-green).
#[test]
#[serial(env)]
fn w2_normal_window_same_fixture_attributes_worker_correctly() {
    let fx = Fixture::new("w2");
    let rollout = fx.write_identity_rollout(WORKER_SESSION, WORKER);
    let mut state = fx.cohort_state();

    let report = capture_tick(&mut state);

    assert_eq!(
        agent_field(&state, WORKER, "session_id"),
        Some(WORKER_SESSION),
        "positive control: with the identity visible from the first scan the worker must bind \
         its own session immediately. state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "rollout_path"),
        Some(rollout.to_string_lossy().as_ref()),
        "positive control: exact tuple. state={state}"
    );
    assert_eq!(
        agent_field(&state, PARENT, "session_id"),
        None,
        "positive control: the parent has no backing rollout and must stay unbound — binding it \
         would be the defect itself. state={state}"
    );
    assert!(
        report.ambiguous.is_empty(),
        "positive control: zero ambiguity/collision in the normal window. ambiguous={:?}",
        report.ambiguous
    );
}

/// W3 (negative control, fail-closed face — must stay green) — a durably
/// claimed FOREIGN session is never stolen. The parent already owns its own
/// completed tuple (PARENT_SESSION); the worker's positive identity binds
/// WORKER_SESSION only. Locate red line: "Collision handling remains
/// fail-closed — never steal a positively or durably claimed foreign session."
#[test]
#[serial(env)]
fn w3_durably_claimed_foreign_session_is_never_stolen() {
    let fx = Fixture::new("w3");
    let parent_rollout = fx.write_identity_rollout(PARENT_SESSION, PARENT);
    let worker_rollout = fx.write_identity_rollout(WORKER_SESSION, WORKER);
    let mut state = fx.cohort_state();
    {
        let parent = state
            .get_mut("agents")
            .and_then(|a| a.get_mut(PARENT))
            .and_then(Value::as_object_mut)
            .unwrap();
        parent.insert("session_id".into(), json!(PARENT_SESSION));
        parent.insert(
            "rollout_path".into(),
            json!(parent_rollout.to_string_lossy().to_string()),
        );
    }

    let _ = capture_tick(&mut state);

    assert_eq!(
        agent_field(&state, PARENT, "session_id"),
        Some(PARENT_SESSION),
        "negative control: the parent's durably owned session must remain untouched. state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "session_id"),
        Some(WORKER_SESSION),
        "negative control: the worker binds its own positively identified session only. \
         state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "rollout_path"),
        Some(worker_rollout.to_string_lossy().as_ref()),
        "negative control: exact tuple, never the foreign rollout. state={state}"
    );
}

/// W4 (RED consequence face) — the externally observed failure is real
/// session-preservation loss at restart (locate §Event-chain step 5; defect
/// dossier: worker session null before AND after restart). After the weak
/// window and the bounded convergence window (same seam as W1), the restart
/// plan must resume `worker_a` on its OWN exact session; the baseline
/// mis-binding instead leaves the worker sessionless (fresh start = context
/// loss) — and no OTHER agent may resume the worker's session.
#[test]
#[serial(env)]
fn w4_restart_must_preserve_worker_session_after_weak_window() {
    let fx = Fixture::new("w4");
    let _rollout = fx.write_weak_rollout(WORKER_SESSION);
    let mut state = fx.cohort_state();

    let _ = capture_tick(&mut state);
    fx.write_identity_rollout(WORKER_SESSION, WORKER);
    let _ = capture_tick(&mut state);
    let _ = capture_tick(&mut state);

    let plan = classify_restart_plan(&state, true).expect("restart plan must classify");

    let worker_decision = plan
        .decisions
        .iter()
        .find(|d| d.agent_id.as_str() == WORKER)
        .expect("worker must appear in the restart plan");
    assert_eq!(
        worker_decision.restart_mode,
        StartMode::Resumed,
        "restart must PRESERVE the worker's session (Resumed), not fresh-start it into context \
         loss; baseline: weak-window first-bind left the worker sessionless. decision={:?} \
         unresumable={:?}",
        worker_decision,
        plan.unresumable
    );
    assert_eq!(
        worker_decision.session_id.as_ref().map(|s| s.as_str()),
        Some(WORKER_SESSION),
        "restart must resume the EXACT worker session id. decision={worker_decision:?}"
    );
    for decision in &plan.decisions {
        if decision.agent_id.as_str() == WORKER {
            continue;
        }
        assert!(
            !(decision.restart_mode == StartMode::Resumed
                && decision.session_id.as_ref().map(|s| s.as_str()) == Some(WORKER_SESSION)),
            "no other agent may resume the WORKER's session (parent resuming a foreign session \
             is the other face of the same defect). decision={decision:?}"
        );
    }
}

// ===========================================================================
// w5 family — LONG-RECORD ground truth (Gate-4 round 2). Fixture byte shape is
// modelled 1:1 on the real rollout sample (gate4-round2-evidence.md): line1 =
// 22068B session_meta, line2 = 233B, line3 = 44357B carrying the embedded
// identity, line3's newline past the 65536B head cap. The fixture SELF-ASSERTS
// its byte invariants so the shape can never silently drift.
// ===========================================================================

/// Head cap observed at the locate surface (`common.rs CAPTURE_HEAD_BYTES`).
/// The contract pins BEHAVIOR relative to this boundary, not the constant
/// itself: identity bytes inside the first HEAD_CAP raw bytes must be seen;
/// identity bytes entirely beyond it must not be (bounded I/O red line —
/// no whole-file reads, no unbounded cap growth).
const HEAD_CAP: u64 = 65_536;

fn marker_text(agent_id: &str) -> String {
    format!("You are Team Agent worker `{agent_id}` with role `fixture`.")
}

fn padded_record(pad_before: usize, marker: Option<&str>, pad_after: usize) -> String {
    let text = format!(
        "{}{}{}",
        "x".repeat(pad_before),
        marker.unwrap_or(""),
        "x".repeat(pad_after)
    );
    serde_json::to_string(&json!({
        "type": "response_item",
        "payload": {"content": [{"type": "input_text", "text": text}]}
    }))
    .unwrap()
}

impl Fixture {
    /// Write the round-2 real-machine rollout shape. Returns (path,
    /// marker_start_offset, line3_end_offset) with the invariants asserted:
    /// line1+line2 complete under the cap; line3's terminating newline beyond
    /// the cap; marker (when inside_cap) strictly under the cap.
    fn write_long_record_rollout(
        &self,
        session_id: &str,
        marker: Option<&str>,
        marker_inside_cap: bool,
    ) -> PathBuf {
        // line1: session_meta padded to ~22068 bytes (real sample size).
        let meta_core = json!({
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "cwd": self.spawn_cwd.to_string_lossy().to_string(),
                "pad": ""
            }
        });
        let overhead = serde_json::to_string(&meta_core).unwrap().len();
        let line1 = serde_json::to_string(&json!({
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "cwd": self.spawn_cwd.to_string_lossy().to_string(),
                "pad": "x".repeat(22_068usize.saturating_sub(overhead))
            }
        }))
        .unwrap();
        // line2: small complete conversation record (~233B real sample).
        let line2 = padded_record(160, None, 0);
        // line3: ~44357B record. marker_inside_cap=true → marker ~500B into
        // the record (real sample: col 548 / absolute 22848); false → marker
        // at the record tail, starting beyond the cap.
        let line3 = match (marker, marker_inside_cap) {
            (Some(m), true) => padded_record(500, Some(m), 43_800 - m.len()),
            (Some(m), false) => padded_record(44_300 - m.len(), Some(m), 40),
            (None, _) => padded_record(500, None, 43_800),
        };
        let body = format!("{line1}\n{line2}\n{line3}\n");

        // Self-asserted byte invariants (ground-truth shape can never drift):
        let line3_start = line1.len() + 1 + line2.len() + 1;
        let line3_end = line3_start + line3.len() + 1;
        assert!(
            (line3_start as u64) < HEAD_CAP,
            "fixture invariant: lines 1-2 must be complete under the head cap; line3_start={line3_start}"
        );
        assert!(
            (line3_end as u64) > HEAD_CAP,
            "fixture invariant: line3's newline must land beyond the head cap; line3_end={line3_end}"
        );
        if let Some(m) = marker {
            let marker_start = line3_start + body[line3_start..].find(m).expect("marker present");
            if marker_inside_cap {
                assert!(
                    (marker_start as u64) < HEAD_CAP,
                    "fixture invariant: marker must START under the cap (real sample offset 22848); \
                     marker_start={marker_start}"
                );
            } else {
                assert!(
                    (marker_start as u64) > HEAD_CAP,
                    "fixture invariant: marker must start BEYOND the cap; marker_start={marker_start}"
                );
            }
        }

        let path = self.rollout_path(session_id);
        std::fs::write(&path, body).unwrap();
        path
    }
}

/// w5 (RED core, round 2) — an embedded identity whose BYTES lie inside the
/// bounded head window must remain visible even when its RECORD's terminating
/// newline lands beyond the cap. Real-machine ground truth: the only rollout
/// carries `worker_a` at absolute offset 22848 inside a 44357-byte record
/// ending at byte 66658; the head parser discards the incomplete record, the
/// rollout scans weak forever, and 00d63cc's shared-candidate deferral makes
/// BOTH cohort agents terminally ambiguous — nobody binds, restart loses the
/// session (run release-054-preship-fixed-20260723T001516Z).
///
/// Post-fix: the worker converges to its exact tuple within the bounded
/// window; the parent never binds it; zero residual ambiguity. The structural
/// JSONL parser may still consume complete lines only (session_meta from
/// line1 remains authoritative for the session id) — this assertion passes
/// exactly when the identity is honored AND the tuple is intact.
#[test]
#[serial(env)]
fn w5_identity_inside_cap_within_cross_cap_record_must_converge() {
    let fx = Fixture::new("w5");
    let rollout = fx.write_long_record_rollout(WORKER_SESSION, Some(&marker_text(WORKER)), true);
    let mut state = fx.cohort_state();

    let _ = capture_tick(&mut state);
    let _ = capture_tick(&mut state);
    let final_report = capture_tick(&mut state);

    assert_ne!(
        agent_field(&state, PARENT, "session_id"),
        Some(WORKER_SESSION),
        "the parent must never commit the worker's session (unchanged w1 invariant). state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "session_id"),
        Some(WORKER_SESSION),
        "`{WORKER}` must converge although its embedded identity sits inside a record whose \
         newline lands beyond the {HEAD_CAP}-byte head cap — the identity BYTES are inside the \
         bounded window (real sample: offset 22848) and must not be discarded with the \
         incomplete record. Baseline 00d63cc: rollout scans weak forever, deferral never \
         converges, nobody binds. state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "rollout_path"),
        Some(rollout.to_string_lossy().as_ref()),
        "exact tuple (structural parser stays line-complete: session_meta on line1 is \
         authoritative). state={state}"
    );
    assert!(
        !final_report
            .ambiguous
            .iter()
            .any(|entry| entry.agent_id == WORKER),
        "zero residual ambiguity for `{WORKER}` after convergence. ambiguous={:?}",
        final_report.ambiguous
    );
}

/// w5b (bounded-read face — must stay green) — an identity whose bytes start
/// ENTIRELY BEYOND the head cap must NOT be extracted: the scan stays weak,
/// the shared-candidate deferral holds, and nobody binds. This pins the
/// OTHER face of "bounded read without identity loss": a fix that reads the
/// whole file (or grows the cap unboundedly) to find this marker turns this
/// form red. (Bounded per-file I/O is the locate's stated design intent;
/// heuristic attribution — cwd/nearest-spawn/newest — is refused elsewhere.)
#[test]
#[serial(env)]
fn w5b_identity_entirely_beyond_cap_stays_weak_and_deferred() {
    let fx = Fixture::new("w5b");
    let _rollout = fx.write_long_record_rollout(WORKER_SESSION, Some(&marker_text(WORKER)), false);
    let mut state = fx.cohort_state();

    let _ = capture_tick(&mut state);
    let report = capture_tick(&mut state);

    assert_eq!(
        agent_field(&state, PARENT, "session_id"),
        None,
        "bounded-read face: beyond-cap identity is invisible; the shared weak candidate must \
         not be bound to the parent. state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "session_id"),
        None,
        "bounded-read face: the worker must not be POSITIVELY bound off identity bytes that \
         live beyond the bounded head window (whole-file reads are forbidden). state={state}"
    );
    assert!(
        report
            .ambiguous
            .iter()
            .any(|entry| entry.agent_id == WORKER),
        "the shared weak candidate stays deferred (ambiguous), not silently dropped. \
         ambiguous={:?}",
        report.ambiguous
    );
}

/// w5c (fail-closed negative control — must stay green) — a cross-cap long
/// rollout with NO embedded identity anywhere must keep the shared-candidate
/// deferral: nobody binds, the worker stays deferred. Guards against a fix
/// that treats "record unreadable/truncated" as a positive identity signal.
#[test]
#[serial(env)]
fn w5c_no_identity_cross_cap_rollout_stays_deferred_for_cohort() {
    let fx = Fixture::new("w5c");
    let _rollout = fx.write_long_record_rollout(WORKER_SESSION, None, true);
    let mut state = fx.cohort_state();

    let _ = capture_tick(&mut state);
    let report = capture_tick(&mut state);

    assert_eq!(
        agent_field(&state, PARENT, "session_id"),
        None,
        "fail-closed: no identity anywhere → the shared weak candidate binds nobody. state={state}"
    );
    assert_eq!(
        agent_field(&state, WORKER, "session_id"),
        None,
        "fail-closed: truncation/absence of identity must never be treated as positive proof. \
         state={state}"
    );
    assert!(
        report
            .ambiguous
            .iter()
            .any(|entry| entry.agent_id == WORKER),
        "the cohort deferral holds (worker stays ambiguous for bounded convergence). \
         ambiguous={:?}",
        report.ambiguous
    );
}
