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
fn layer2_backing_missing_refusal_carries_checked_paths_and_recovery_hint() {
    // Leader follow-up 2026-06-22: when classify_restart_plan_with_resume_validation
    // runs with a real workspace, a codex worker whose session_id is set
    // but whose rollout_path does not exist on disk should produce a
    // refusal whose structured ResumeRefusalReason::SessionBackingStoreMissing
    // carries the actual probed paths (the persisted rollout_path) AND a
    // RecoveryHint with the agent_id as the picker name.
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let ws =
        std::env::temp_dir().join(format!("ta_rs_l2_backingmiss_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&ws).unwrap();

    let missing_rollout = ws.join(".missing-rollout.jsonl");
    // Do NOT create the file. Probe must report it as not-existing AND
    // include it in checked_paths.
    let mut agent = serde_json::Map::new();
    agent.insert("provider".to_string(), json!("codex"));
    agent.insert("status".to_string(), json!("running"));
    agent.insert("session_id".to_string(), json!("sess-layer2-missing"));
    agent.insert(
        "rollout_path".to_string(),
        json!(missing_rollout.to_string_lossy()),
    );
    agent.insert("first_send_at".to_string(), json!("2026-01-01T00:00:00Z"));
    agent.insert("spawn_cwd".to_string(), json!(ws.to_string_lossy()));
    let state = json!({ "agents": { "a": serde_json::Value::Object(agent) } });

    let plan = crate::lifecycle::restart::classify_restart_plan_with_resume_validation(
        Some(&ws),
        &state,
        false,
    )
    .unwrap();
    assert_eq!(plan.unresumable.len(), 1);
    let entry = &plan.unresumable[0];
    assert_eq!(entry.agent_id.as_str(), "a");
    assert_eq!(entry.reason, "session_unresumable");
    match entry.refusal_reason.as_ref() {
        Some(crate::provider::session::ResumeRefusalReason::SessionBackingStoreMissing {
            checked_paths,
            recovery_hint,
        }) => {
            assert!(
                !checked_paths.is_empty(),
                "checked_paths must be populated; got empty"
            );
            assert!(
                checked_paths.iter().any(|p| p == &missing_rollout),
                "checked_paths must include the persisted rollout_path; got {checked_paths:?}"
            );
            let hint = recovery_hint
                .as_ref()
                .expect("recovery_hint should be populated");
            assert_eq!(
                hint.provider_session_name_hint.as_deref(),
                Some("a"),
                "name hint must equal agent_id"
            );
            assert_eq!(
                hint.spawn_cwd.as_deref(),
                Some(ws.as_path()),
                "spawn_cwd should round-trip through the hint"
            );
            assert_eq!(hint.provider, "codex");
        }
        other => panic!(
            "expected SessionBackingStoreMissing with structured fields; got {:?}",
            other
        ),
    }
}

fn write_codex_identity_rollout(
    workspace: &std::path::Path,
    file_name: &str,
    session_id: &str,
    embedded_agent_id: &str,
) -> std::path::PathBuf {
    let path = workspace.join(file_name);
    let text = format!(
        "{{\"session_meta\":{{\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"{}\"}}}}}}\n\
         {{\"type\":\"turn_context\",\"payload\":{{}}}}\n\
         {{\"type\":\"response_item\",\"payload\":{{\"content\":[{{\"type\":\"input_text\",\"text\":\"You are Team Agent worker `{embedded_agent_id}` with role `fixture`.\"}}]}}}}\n",
        workspace.to_string_lossy()
    );
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn restart_refuses_codex_session_identity_mismatch_without_allow_fresh() {
    let ws = temp_ws();
    let rollout = write_codex_identity_rollout(
        &ws,
        "rollout-frontend-poison.jsonl",
        "019f3327-c35a-7023-b3cd-1bea93a7a157",
        "ios-dev",
    );
    let state = json!({
        "agents": {
            "frontend": {
                "provider": "codex",
                "status": "running",
                "session_id": "019f3327-c35a-7023-b3cd-1bea93a7a157",
                "rollout_path": rollout.to_string_lossy(),
                "captured_at": "2026-07-05T17:04:04Z",
                "captured_via": "fs_watch",
                "spawn_cwd": ws.to_string_lossy(),
                "first_send_at": "2026-07-05T17:10:00Z"
            }
        }
    });

    let plan = crate::lifecycle::restart::classify_restart_plan_with_resume_validation(
        Some(&ws),
        &state,
        false,
    )
    .unwrap();

    assert_eq!(plan.decisions.len(), 1);
    assert_eq!(plan.decisions[0].decision, ResumeDecision::Refuse);
    assert_eq!(plan.unresumable.len(), 1);
    let entry = &plan.unresumable[0];
    assert_eq!(entry.agent_id.as_str(), "frontend");
    assert_eq!(entry.reason, "session_identity_mismatch");
    assert_eq!(
        entry
            .refusal_reason
            .as_ref()
            .map(crate::provider::session::ResumeRefusalReason::wire),
        Some("session_identity_mismatch")
    );
}

#[test]
fn restart_identity_probe_reports_real_frontend_ios_dev_mismatch_shape() {
    let ws = temp_ws();
    let rollout = write_codex_identity_rollout(
        &ws,
        "rollout-frontend-poison.jsonl",
        "019f3327-c35a-7023-b3cd-1bea93a7a157",
        "ios-dev",
    );
    let rollout_path = crate::provider::RolloutPath::new(rollout.clone());
    let probe = crate::lifecycle::restart::session_identity_probe_for_agent(
        &aid("frontend"),
        crate::provider::Provider::Codex,
        Some(&rollout_path),
    );
    assert_eq!(probe.identity_ok, Some(false));
    assert_eq!(probe.embedded_agent_id.as_deref(), Some("ios-dev"));
    assert_eq!(probe.rollout_path.as_deref(), Some(rollout.as_path()));
}

#[test]
fn restart_allow_fresh_only_fresh_starts_mismatched_codex_worker() {
    let ws = temp_ws();
    let frontend_rollout = write_codex_identity_rollout(
        &ws,
        "rollout-frontend-poison.jsonl",
        "019f3327-c35a-7023-b3cd-1bea93a7a157",
        "ios-dev",
    );
    let backend_rollout = write_codex_identity_rollout(
        &ws,
        "rollout-backend-valid.jsonl",
        "019f3327-backend-valid",
        "backend",
    );
    let state = json!({
        "agents": {
            "frontend": {
                "provider": "codex",
                "status": "running",
                "session_id": "019f3327-c35a-7023-b3cd-1bea93a7a157",
                "rollout_path": frontend_rollout.to_string_lossy(),
                "captured_at": "2026-07-05T17:04:04Z",
                "captured_via": "fs_watch",
                "spawn_cwd": ws.to_string_lossy()
            },
            "backend": {
                "provider": "codex",
                "status": "running",
                "session_id": "019f3327-backend-valid",
                "rollout_path": backend_rollout.to_string_lossy(),
                "captured_at": "2026-07-05T17:04:05Z",
                "captured_via": "fs_watch",
                "spawn_cwd": ws.to_string_lossy()
            }
        }
    });

    let plan = crate::lifecycle::restart::classify_restart_plan_with_resume_validation(
        Some(&ws),
        &state,
        true,
    )
    .unwrap();

    assert!(
        plan.unresumable.is_empty(),
        "allow_fresh should convert the poisoned worker to fresh without refusing; got {:?}",
        plan.unresumable
    );
    let decisions = plan
        .decisions
        .iter()
        .map(|decision| (decision.agent_id.as_str(), decision.decision))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(decisions.get("frontend"), Some(&ResumeDecision::FreshStart));
    assert_eq!(decisions.get("backend"), Some(&ResumeDecision::Resume));
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
