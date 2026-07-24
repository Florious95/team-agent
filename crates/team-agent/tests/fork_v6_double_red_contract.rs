//! 0.5.59 fork V6 deterministic RED.
//!
//! Requirement lineage: F2 "克隆与分叉" requires a fork to carry the source
//! context into a distinct, verifiable session and forbids reporting an
//! ordinary clone/latest sibling as a successful fork.  The V6 preserved
//! real-machine case then proved two exact materialized Codex targets existed
//! with correct cwd/worker identity but stayed pending.  Architecture review
//! §11.4 freezes the five observable behaviors below; no implementation helper
//! name or proposed precision type is part of this contract.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::model::enums::Provider;
use team_agent::provider::get_adapter;
use team_agent::provider::session::capture::{
    capture_missing_provider_sessions_once, CapturePassReport,
};

const AGENT: &str = "fork_target";
const SOURCE: &str = "019f963c-5675-7c9f-a318-347d9f6b58e3";
const EXPECTED: &str = "019f963c-5676-7c9f-a318-347d9f6b58e3";
const SIBLING: &str = "019f963c-5677-7c9f-a318-347d9f6b58e3";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Case {
    root: PathBuf,
    cwd: PathBuf,
    previous_home: Option<String>,
}

impl Case {
    fn new(tag: &str) -> Self {
        let raw = std::env::temp_dir().join(format!(
            "ta-fork-v6-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&raw);
        std::fs::create_dir_all(&raw).unwrap();
        let root = std::fs::canonicalize(raw).unwrap();
        let cwd = root.join("workspace");
        let home = root.join("home");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let previous_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", &home) };
        Self {
            root,
            cwd,
            previous_home,
        }
    }

    fn rollout(&self, id: &str, cwd: &Path, marker: &str) -> PathBuf {
        let dir = self.root.join("home/.codex/sessions/2026/07/25");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-07-25T00-00-00-{id}.jsonl"));
        let text = format!(
            "{}\n{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":id,"cwd":cwd}}),
            json!({"type":"response_item","payload":{"content":[{"type":"input_text","text":"context inherited from source"}]}}),
            json!({"type":"response_item","payload":{"content":[{"type":"input_text","text":format!("You are Team Agent worker `{marker}` with role `fixture`.")}]}})
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn pending(&self, expected: Option<&str>) -> Value {
        let mut row = json!({
            "status": "running",
            "provider": "codex",
            "spawn_cwd": self.cwd,
            "spawned_at": "2026-07-24T22:26:04.534311+00:00",
            "capture_state": "pending_context_fork",
            "fork_source_session_id": SOURCE,
            "pending_target_agent": AGENT
        });
        if let Some(id) = expected {
            row["_pending_session_id"] = json!(id);
        }
        json!({"agents": {AGENT: row}})
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        unsafe {
            if let Some(home) = self.previous_home.take() {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tick(state: &mut Value) -> CapturePassReport {
    capture_missing_provider_sessions_once(state, &mut |p: Provider| get_adapter(p), false, 0)
        .expect("deterministic capture tick")
}

fn captured_id(state: &Value) -> Option<&str> {
    state["agents"][AGENT]["session_id"].as_str()
}

#[test]
#[serial(env)]
fn v6_1_same_millisecond_exact_target_is_not_stale_but_prior_millisecond_is() {
    let case = Case::new("precision");
    case.rollout(EXPECTED, &case.cwd, AGENT);
    let mut same_ms = case.pending(Some(EXPECTED));
    tick(&mut same_ms);
    assert_eq!(
        captured_id(&same_ms),
        Some(EXPECTED),
        "V6-1 same-millisecond exact target was rejected as stale; UUID millisecond precision must not be compared as if it carried spawned_at microseconds"
    );

    let mut prior_ms = case.pending(Some(SOURCE));
    tick(&mut prior_ms);
    assert_eq!(
        captured_id(&prior_ms),
        None,
        "V6-1 guard: a target at least one millisecond before the spawn boundary must remain rejected"
    );
}

#[test]
#[serial(env)]
fn v6_2_pending_codex_uses_persisted_expected_id_and_exact_miss_is_empty() {
    let case = Case::new("expected");
    case.rollout(SIBLING, &case.cwd, AGENT);
    let mut state = case.pending(Some(EXPECTED));
    tick(&mut state);
    assert_eq!(
        captured_id(&state),
        None,
        "V6-2 expected-id miss fell back to a same-cwd/latest sibling instead of returning empty"
    );
}

#[test]
#[serial(env)]
fn v6_3_exact_hit_still_rejects_foreign_marker_and_cwd_mismatch() {
    let case = Case::new("negative");
    case.rollout(EXPECTED, &case.cwd, "foreign_worker");
    let mut foreign = case.pending(Some(EXPECTED));
    tick(&mut foreign);
    assert_eq!(
        captured_id(&foreign),
        None,
        "V6-3 exact id bypassed the positive target identity marker"
    );

    case.rollout(EXPECTED, &case.root.join("other-workspace"), AGENT);
    let mut wrong_cwd = case.pending(Some(EXPECTED));
    tick(&mut wrong_cwd);
    assert_eq!(
        captured_id(&wrong_cwd),
        None,
        "V6-3 exact id bypassed cwd isolation"
    );

    case.rollout(SOURCE, &case.cwd, AGENT);
    let mut source_collision = case.pending(Some(SOURCE));
    tick(&mut source_collision);
    assert_eq!(
        captured_id(&source_collision),
        None,
        "V6-3 exact id bypassed source-session collision exclusion"
    );
}

#[test]
#[serial(env)]
fn v6_4_legacy_pending_without_expected_id_does_not_guess_latest() {
    let case = Case::new("legacy");
    case.rollout(SIBLING, &case.cwd, AGENT);
    let mut state = case.pending(None);
    tick(&mut state);
    assert_eq!(
        captured_id(&state),
        None,
        "V6-4 legacy Codex pending row without _pending_session_id guessed a sibling/latest session"
    );
}

#[test]
#[serial(env)]
fn v6_5_one_tick_finalizes_complete_tuple_and_yields_one_audit_projection() {
    let case = Case::new("finalize");
    let path = case.rollout(EXPECTED, &case.cwd, AGENT);
    let mut state = case.pending(Some(EXPECTED));
    let report = tick(&mut state);
    let row = &state["agents"][AGENT];
    assert_eq!(
        row["capture_state"], "captured",
        "V6-5 complete exact proof did not converge pending to captured in one tick"
    );
    assert_eq!(row["session_id"], EXPECTED, "V6-5 session tuple split");
    assert_eq!(
        row["rollout_path"].as_str(),
        Some(path.to_string_lossy().as_ref()),
        "V6-5 rollout tuple split"
    );
    assert_eq!(
        report.assigned.len(),
        1,
        "V6-5 one finalize must yield exactly one assignment for the coordinator's unique post-save audit projection"
    );
}
