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

use team_agent::messaging::{ActivityStatus, WorkerRuntimeState};

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
    assert_eq!(
        WorkerRuntimeState::parse_wire(""),
        WorkerRuntimeState::Unknown
    );
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

/// CR R5 grep guard, re-pointed after M3 (R-M3: the coordinator's `sync_agent_health` in
/// `src/coordinator/tick.rs` was retired, so the guard now covers every runtime source): no
/// production path may write `status = "busy"`. Lifecycle status is launch/stop-owned
/// (running/paused/stopped), and the M4 send authority delivers only to `running` seats, so a
/// turn-level busy written into `status` would silently block delivery.
#[test]
fn no_runtime_path_writes_lifecycle_busy_grep_guard() {
    fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" && name != "test_support" {
                    sources(&path, out);
                }
            } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(files.len() > 50, "FIXTURE: runtime source surface missing: {}", files.len());
    let forbidden_patterns = [
        r#""status".to_string(), serde_json::json!("busy")"#,
        r#"insert("status".to_string(), Value::String("busy""#,
        r#"agent_obj.insert("status".to_string(), json!("busy"#,
        r#"insert("status".to_string(), json!("busy")"#,
    ];
    for file in files {
        let contents = std::fs::read_to_string(&file).unwrap();
        // Runtime code only: stop at a trailing `#[cfg(test)]` module.
        let runtime = contents.split("#[cfg(test)]\nmod ").next().unwrap_or(&contents);
        for pattern in forbidden_patterns {
            assert!(
                !runtime.contains(pattern),
                "{} must NOT write lifecycle status=busy (CR R5). Found forbidden pattern: {pattern}",
                file.display()
            );
        }
    }
}
