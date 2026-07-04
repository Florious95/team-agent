//! CR C-1 hard grep guard: `pipe_token` NEVER appears in the persisted
//! state layer. The token lives only in shim/backend memory.
//!
//! This test walks every `.rs` file in `crates/team-agent/src/` and
//! asserts:
//!
//! - the string `pipe_token` appears only inside `src/conpty/` (the
//!   backend + protocol module) and inside test files;
//! - it does NOT appear in `src/state/`, `src/messaging/`, `src/lifecycle/`,
//!   or any other path that could serialise to state.json.
//!
//! If someone adds a `state.transport.pipe_token` field in the future
//! this test fires immediately.

use std::path::Path;

fn walk_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn pipe_token_string_appears_only_inside_conpty_module() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all_rs = Vec::new();
    walk_rs(&src_root, &mut all_rs);
    let mut offenders: Vec<String> = Vec::new();
    for path in &all_rs {
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        if !body.contains("pipe_token") {
            continue;
        }
        // Allowed home: `src/conpty/**` (protocol.rs / backend.rs).
        let rel = path
            .strip_prefix(&src_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if rel.starts_with("conpty/") || rel == "conpty.rs" {
            continue;
        }
        offenders.push(rel);
    }
    assert!(
        offenders.is_empty(),
        "CR C-1: `pipe_token` must only live inside `src/conpty/`; \
         found in: {offenders:?}. If you added a state.json field carrying \
         the token, revert and store it in ConPtyBackend / PipeClient memory only."
    );
}

#[test]
fn state_persist_layer_does_not_reference_pipe_token() {
    // Belt-and-braces: the state persistence path (which serialises to
    // JSON via serde) is the highest-risk spot. Scan it explicitly.
    let paths = [
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state/persist.rs"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state/ownership.rs"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state/projection.rs"),
    ];
    for path in &paths {
        if !path.exists() {
            continue;
        }
        let body = std::fs::read_to_string(path).unwrap();
        assert!(
            !body.contains("pipe_token"),
            "CR C-1 grep guard: `pipe_token` must NOT appear in state \
             persistence at {}. Any Windows-native transport token must \
             stay in ConPtyBackend memory only.",
            path.display()
        );
    }
}
