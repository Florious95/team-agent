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
