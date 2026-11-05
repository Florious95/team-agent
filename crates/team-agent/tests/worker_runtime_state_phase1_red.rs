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
