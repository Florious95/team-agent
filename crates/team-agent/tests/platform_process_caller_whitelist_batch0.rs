//! Windows portability Batch 0 CR C-4 grep guard.
//!
//! Design boundary: `crate::platform::process` is the OS process
//! primitive layer used by coordinator/lifecycle/CLI shutdown paths.
//! It is NOT the team-worker liveness source of truth — that goes
//! through the `Transport` trait (backend `list_targets`/`liveness`).
//!
//! Grep guard: `crate::platform::process` MUST NOT be imported from
//! `messaging/` (delivery + leader_receiver + results + send etc.).
//! Team-worker liveness inside those paths must come from the
//! transport, not a shell-out process snapshot.
//!
//! (CR C-4, `.team/artifacts/0.5.x-windows-portability-cr-verdict.md`.)

use std::path::PathBuf;

fn src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_rs_recursive(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            read_rs_recursive(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Extract non-test, non-comment content so guard doesn't fire on
/// doc comments that explain what's forbidden.
fn non_comment_non_test(src: &str) -> String {
    let cutoff = src.find("#[cfg(test)]").unwrap_or(src.len());
    let non_test = &src[..cutoff];
    let mut out = String::with_capacity(non_test.len());
    for line in non_test.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn messaging_paths_do_not_import_platform_process() {
    // CR C-4: `messaging/delivery.rs` + `messaging/leader_receiver.rs`
    // + `messaging/results.rs` + `messaging/send.rs` etc. must NOT
    // import `crate::platform::process`. Team-worker liveness inside
    // messaging must go through the Transport trait.
    let messaging = src_root().join("messaging");
    let mut files = Vec::new();
    read_rs_recursive(&messaging, &mut files);
    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        let code = non_comment_non_test(&body);
        for needle in [
            "crate::platform::process",
            "use crate::platform::process",
            "platform::process::",
        ] {
            if code.contains(needle) {
                let rel = path
                    .strip_prefix(src_root().parent().unwrap())
                    .unwrap_or(path);
                offenders.push(format!("{} references {needle:?}", rel.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "CR C-4 grep guard: `crate::platform::process` MUST NOT be \
         imported from `messaging/` — team-worker liveness inside \
         delivery/leader_receiver/results/send must go through the \
         Transport trait, not a platform process snapshot. Offenders: \
         {offenders:?}"
    );
}

#[test]
fn platform_process_module_exports_expected_public_api() {
    // C-4 positive anchor: the `platform::process` module must exist
    // and expose the enum/fn names that Batches 2-4 will migrate
    // callers onto. If someone renames these, this test fires so the
    // migration cannot silently drift.
    let module_path = src_root().join("platform").join("process.rs");
    let body = std::fs::read_to_string(&module_path)
        .expect("platform/process.rs must exist (Batch 0 landing)");
    for expected in [
        "pub struct Pid",
        "pub enum SignalKind",
        "pub enum ProcessLiveness",
        "pub struct ProcessInfo",
        "pub enum TerminationOutcome",
        "pub fn current_parent_pid",
        "pub fn pid_liveness",
        "pub fn terminate_pid",
        "pub fn terminate_group",
        "pub fn reap_child_if_possible",
    ] {
        assert!(
            body.contains(expected),
            "platform/process.rs missing expected public API: {expected:?}"
        );
    }
}
