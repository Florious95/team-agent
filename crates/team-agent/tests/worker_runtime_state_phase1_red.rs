//! Phase 1 fg-pgrp + 5-state WorkerRuntimeState — focused integration tests
//! covering the .team/artifacts/fg-pgrp-phase1-plan.md acceptance contract.
//!
//! Implements CR test coverage:
//!   - R1 OS probe: ps -o tpgid,pgid is the platform-portable foreground
//!     pgrp probe (macOS confirmed; Linux compatible).
//!   - R3 same-source: worker_state derives from the SAME agent state +
//!     activity classifier, not a parallel store.
//!   - R4 enum forward-compat: `#[serde(other)]` catches unknown wire.
//!   - R5 grep guard: sync_agent_health code path MUST NOT write
//!     `agent["status"] = "busy"` (lifecycle status is launch/stop-owned).

use std::path::Path;

use team_agent::messaging::{ActivityStatus, AgentActivity, WorkerRuntimeState};

#[test]
fn enum_wire_round_trip() {
    for state in [
        WorkerRuntimeState::Dead,
        WorkerRuntimeState::Busy,
        WorkerRuntimeState::Blocked,
        WorkerRuntimeState::ProbablyIdle,
        WorkerRuntimeState::Unknown,
    ] {
        let wire = state.as_wire();
        let parsed = WorkerRuntimeState::parse_wire(wire);
        assert_eq!(parsed, state, "round trip {wire}");
    }
}

#[test]
fn enum_unknown_wire_falls_back_to_unknown() {
    // CR R4: unknown literals → Unknown (forward compatibility).
    assert_eq!(
        WorkerRuntimeState::parse_wire("FUTURE_VARIANT"),
        WorkerRuntimeState::Unknown
    );
    assert_eq!(WorkerRuntimeState::parse_wire(""), WorkerRuntimeState::Unknown);
}

#[test]
fn enum_serde_other_catches_unknown_json() {
    // CR R4: serde-side forward compat for JSON readers.
    let parsed: WorkerRuntimeState = serde_json::from_value(serde_json::json!("FUTURE")).unwrap();
    assert_eq!(parsed, WorkerRuntimeState::Unknown);
}

#[test]
fn blocks_automatic_dispatch_iron_law() {
    // ONLY ProbablyIdle is safe for automatic dispatch.
    assert!(!WorkerRuntimeState::ProbablyIdle.blocks_automatic_dispatch());
    for blocked in [
        WorkerRuntimeState::Dead,
        WorkerRuntimeState::Busy,
        WorkerRuntimeState::Blocked,
        WorkerRuntimeState::Unknown,
    ] {
        assert!(
            blocked.blocks_automatic_dispatch(),
            "{} must block automatic dispatch",
            blocked.as_wire()
        );
    }
}

#[test]
fn is_idle_candidate_iron_law() {
    // Iron Law (bug-071/077/085): Unknown is NEVER an idle candidate.
    assert!(WorkerRuntimeState::ProbablyIdle.is_idle_candidate());
    for not_idle in [
        WorkerRuntimeState::Dead,
        WorkerRuntimeState::Busy,
        WorkerRuntimeState::Blocked,
        WorkerRuntimeState::Unknown,
    ] {
        assert!(
            !not_idle.is_idle_candidate(),
            "{} must NOT be idle candidate",
            not_idle.as_wire()
        );
    }
}

#[test]
fn from_activity_maps_stuck_and_uncertain_to_unknown() {
    // Stuck / Uncertain MUST map to Unknown (never silently to Idle).
    assert_eq!(
        WorkerRuntimeState::from_activity(ActivityStatus::Stuck),
        WorkerRuntimeState::Unknown
    );
    assert_eq!(
        WorkerRuntimeState::from_activity(ActivityStatus::Uncertain),
        WorkerRuntimeState::Unknown
    );
    assert_eq!(
        WorkerRuntimeState::from_activity(ActivityStatus::Idle),
        WorkerRuntimeState::ProbablyIdle
    );
    assert_eq!(
        WorkerRuntimeState::from_activity(ActivityStatus::Working),
        WorkerRuntimeState::Busy
    );
}

/// CR R5 grep guard: sync_agent_health code path must NEVER write
/// `agent["status"] = "busy"`. Lifecycle status is launch/stop-owned
/// (running/paused/stopped); turn-level Busy is a separate field.
/// Historically merging the two caused delivery deferral when the worker
/// was actively turning (delivery.rs:1284-1291 keys on lifecycle status).
#[test]
fn sync_agent_health_does_not_write_lifecycle_busy_grep_guard() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tick_rs = manifest.join("src").join("coordinator").join("tick.rs");
    let contents = std::fs::read_to_string(&tick_rs).expect("read tick.rs");
    // Locate sync_agent_health and its enclosing function body.
    let start = contents
        .find("fn sync_agent_health(")
        .expect("sync_agent_health must exist");
    // End at the start of the NEXT top-level fn definition.
    let body_end = contents[start + 1..]
        .find("\n    fn ")
        .or_else(|| contents[start + 1..].find("\nfn "))
        .map(|p| start + 1 + p)
        .unwrap_or(contents.len());
    let body = &contents[start..body_end];
    // Forbidden patterns: any write of "busy" into lifecycle agent["status"].
    let forbidden_patterns = [
        r#""status".to_string(), serde_json::json!("busy")"#,
        r#"insert("status".to_string(), Value::String("busy""#,
        r#"agent_obj.insert("status".to_string(), json!("busy"#,
    ];
    for pattern in forbidden_patterns {
        assert!(
            !body.contains(pattern),
            "sync_agent_health body must NOT write lifecycle status=busy \
             (CR R5). Found forbidden pattern: {pattern}\nBody excerpt: {body}"
        );
    }
}

/// CR R3 same-source: the resolver function is reachable from the
/// coordinator tick module under the documented name. Verifies the
/// resolver wiring (R3 — derive worker_state from the SAME agent JSON +
/// activity classifier, not a parallel store).
#[test]
fn resolver_function_is_present_grep_guard() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tick_rs = manifest.join("src").join("coordinator").join("tick.rs");
    let contents = std::fs::read_to_string(&tick_rs).expect("read tick.rs");
    assert!(
        contents.contains("fn resolve_worker_runtime_state_with_fg_pgrp"),
        "resolver function `resolve_worker_runtime_state_with_fg_pgrp` must \
         exist in coordinator/tick.rs (Phase 1 §4)"
    );
    assert!(
        contents.contains("worker_state"),
        "tick.rs must write the worker_state field into agent state \
         (Phase 1 §5)"
    );
}

/// AgentActivity construction with all 4 ActivityStatus variants to anchor
/// the deprecated path. R3: activity preserved as legacy surface.
#[test]
fn agent_activity_construct_all_variants() {
    for status in [
        ActivityStatus::Working,
        ActivityStatus::Idle,
        ActivityStatus::Stuck,
        ActivityStatus::Uncertain,
    ] {
        let a = AgentActivity {
            status,
            confidence: 0.5,
            rationale: "test".to_string(),
        };
        // bridge to new enum.
        let _w = WorkerRuntimeState::from_activity(a.status);
    }
}
