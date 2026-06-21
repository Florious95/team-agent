//! unit-0 (Stage 0) characterization tests for restart resume preflight.
//!
//! These pin the current `classify_restart_plan` behavior so that the
//! Stage 2 refactor (unit-5: replace the opaque `session_unresumable`
//! string with a `ResumeRefusalReason` enum + structured backing-store
//! check) is detectable as a behavior change rather than an accidental
//! regression.
//!
//! Pinned invariants:
//! - A worker with `session_id` set but backing store absent is currently
//!   tagged with the OPAQUE string `session_unresumable`. unit-5 must
//!   evolve this to a structured reason (e.g. `session_backing_store_missing`).
//! - A worker with NO `session_id` is currently tagged
//!   `no_persisted_session_id` — distinct from the missing-backing case.
//!   unit-5 must keep this distinction.
//! - `allow_fresh=true` converts both refusal classes into a `FreshStart`
//!   decision (no entries in `unresumable`).

use super::*;
use crate::lifecycle::restart::classify_restart_plan;
use crate::lifecycle::types::{ResumeDecision, StartMode};

fn agent_codex(session_id: Option<&str>, first_send_at: serde_json::Value) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("provider".to_string(), json!("codex"));
    obj.insert("status".to_string(), json!("running"));
    obj.insert("first_send_at".to_string(), first_send_at);
    if let Some(sid) = session_id {
        obj.insert("session_id".to_string(), json!(sid));
    }
    serde_json::Value::Object(obj)
}

#[test]
fn unit0_restart_missing_session_id_is_no_persisted_session_id() {
    let state = json!({
        "agents": {
            "a": agent_codex(None, json!("2026-01-01T00:00:00Z")),
        }
    });
    let plan = classify_restart_plan(&state, false).unwrap();
    assert_eq!(plan.unresumable.len(), 1);
    assert_eq!(plan.unresumable[0].agent_id.as_str(), "a");
    assert_eq!(plan.unresumable[0].reason, "no_persisted_session_id");
    assert!(plan.unresumable[0].session_id.is_none());
}

#[test]
fn unit0_restart_session_id_present_no_backing_is_session_unresumable() {
    // session_id is set, but resume_backing_exists_for_agent will return
    // false because no workspace was passed AND rollout_path is missing.
    // The current behavior is to flatten this into `session_unresumable`
    // — the opaque reason that unit-5 will replace with structured enum.
    let state = json!({
        "agents": {
            "a": agent_codex(Some("sess-unit0-missing"), json!("2026-01-01T00:00:00Z")),
        }
    });
    let plan = classify_restart_plan(&state, false).unwrap();
    assert_eq!(plan.unresumable.len(), 1);
    assert_eq!(plan.unresumable[0].agent_id.as_str(), "a");
    assert_eq!(plan.unresumable[0].reason, "session_unresumable");
    assert!(plan.unresumable[0].session_id.is_some());
}

#[test]
fn unit0_restart_allow_fresh_converts_refusals_into_fresh_decisions() {
    let state = json!({
        "agents": {
            "a": agent_codex(None, json!("2026-01-01T00:00:00Z")),
            "b": agent_codex(Some("sess-unit0-allow-fresh"), json!("2026-01-01T00:00:00Z")),
        }
    });
    let plan = classify_restart_plan(&state, true).unwrap();
    // Both unresumable-class workers should be turned into FreshStart.
    assert!(
        plan.unresumable.is_empty(),
        "allow_fresh should drain the unresumable bucket; got {:?}",
        plan.unresumable
    );
    assert_eq!(plan.decisions.len(), 2);
    for d in &plan.decisions {
        assert!(
            matches!(d.restart_mode, StartMode::Fresh),
            "agent {} should be Fresh under allow_fresh; got {:?}",
            d.agent_id.as_str(),
            d.restart_mode
        );
    }
    let _ = ResumeDecision::FreshStart; // touch the enum so unused-import lint stays quiet.
}

#[test]
fn unit0_restart_corrupt_first_send_at_blocks_before_resume_classification() {
    // The corrupt-first_send_at branch is the hard-refuse gate that fires
    // BEFORE resume classification. This pins that corrupt entries land in
    // `corrupt_entries` (with python type name) and the unresumable bucket
    // stays empty for the corrupt agent.
    let state = json!({
        "agents": {
            "a": agent_codex(Some("sess-unit0-corrupt"), json!(false)),
        }
    });
    let plan = classify_restart_plan(&state, false).unwrap();
    assert_eq!(plan.corrupt_entries.len(), 1);
    assert_eq!(plan.corrupt_entries[0].worker_id.as_str(), "a");
    assert_eq!(plan.corrupt_entries[0].raw_first_send_at_type, "bool");
    assert!(plan.unresumable.is_empty());
}
