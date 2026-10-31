//! #236 notification redesign negative/retention contracts.
//!
//! User-facing invariant: deterministic notifications replace idle/stuck/deadlock nag. Product
//! delivery notifications for report_result / send-to-leader / broadcast-to-leader remain on the
//! N31/N32 funnel.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::Path;

#[test]
fn idle_stuck_deadlock_nag_paths_have_no_runtime_caller() {
    // R-M3: the coordinator tick that used to call these generators was retired, so the guard is
    // re-pointed from `src/coordinator/tick.rs` to every runtime source: the old generators and
    // their events may survive only as definitions, re-exports, docs or wire-name tables.
    let mut failures = Vec::new();
    for (file, text) in runtime_sources() {
        for generator in [
            "detect_stuck_agents",
            "record_unknown_idle_nodes",
            "evaluate_takeover",
            "detect_cross_worker_deadlocks",
        ] {
            for line in text.lines() {
                let trimmed = line.trim_start();
                let is_call = line.contains(&format!("{generator}("))
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with("pub use")
                    && !line.contains(&format!("fn {generator}("));
                if is_call {
                    failures.push(format!("{file} must no longer call old nag path `{generator}`: {}", line.trim()));
                }
            }
        }
        if file.ends_with("src/leader/types.rs") {
            continue; // the event wire-name table may keep historical names
        }
        for stale_event in [
            "IdleTakeoverReminder",
            "IdleTakeoverPing",
            "idle_takeover.reminder",
            "idle_takeover.ping",
            "idle_takeover.unknown_persistent",
            "\"cross_worker_deadlock\"",
        ] {
            if text.lines().any(|line| line.contains(stale_event) && !line.trim_start().starts_with("//")) {
                failures.push(format!("{file} must not emit old nag event `{stale_event}`"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "old idle/stuck/deadlock nag generation must be removed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn product_delivery_notifications_use_physical_delivery_and_append_only_inbox() {
    // R-M5: the MCP tools and the CLI share one send core (`src/station/send.rs`), so the funnel
    // markers moved there from `src/mcp_server/tools.rs`; the intent is unchanged.
    let tools = source("src/mcp_server/tools.rs");
    assert!(tools.contains("crate::station::send::execute"), "MCP send_message must use the shared send core");
    let core = source("src/station/send.rs");
    assert!(core.contains("crate::inbox_store::store("), "the shared core must persist to the append-only inbox");
    assert!(core.contains("crate::direct_delivery::directory("), "the shared core must address through the directory");
    assert!(core.contains("Team Agent {kind} from {sender}:"), "message and result framing must stay explicit");
    assert!(core.contains("delivery_error"), "failed physical delivery must remain visible");
    let direct = source("src/direct_delivery.rs");
    assert!(direct.contains("paste-buffer") && direct.contains("send-keys"));
}

fn runtime_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("read source dir").flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" && name != "test_support" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
                let text = std::fs::read_to_string(&path).expect("read source");
                let runtime = text.split("#[cfg(test)]\nmod ").next().unwrap_or(&text).to_string();
                out.push((path.display().to_string(), runtime));
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    assert!(out.len() > 50, "FIXTURE: runtime source surface missing");
    out
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}
