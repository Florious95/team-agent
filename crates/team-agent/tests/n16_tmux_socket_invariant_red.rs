//! N16/CP-1 contract: production tmux operations must not hit the default tmux server.
//!
//! Raw `Command::new("tmux")` may only be used for non-server probes such as
//! `tmux -V`, or inside the `TmuxBackend` OS edge. Session/window/pane/server
//! operations must go through the workspace-socketed backend or pass `-L`
//! explicitly.

use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Violation {
    file: String,
    line: usize,
    op: &'static str,
}

#[test]
fn n16_all_tmux_session_window_pane_ops_are_socketed() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);

    let mut violations = Vec::new();
    for path in files {
        scan_file(&manifest, &path, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "N16/CP-1 violation: tmux server/session/window/pane operations must use \
         TmuxBackend::for_workspace (socketed `tmux -L <socket>`) or pass `-L` \
         explicitly. Violations:\n{}",
        violations
            .iter()
            .map(|v| format!("{}:{} raw tmux {}", v.file, v.line, v.op))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read source directory");
    for entry in entries {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn scan_file(manifest: &Path, path: &Path, violations: &mut Vec<Violation>) {
    let rel = relative_source_path(manifest, path);
    if rel == "src/tmux_backend.rs" || rel.contains("/tests/") {
        return;
    }
    let text = std::fs::read_to_string(path).expect("read source file");
    let lines: Vec<&str> = text.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if !line.contains("Command::new(\"tmux\")") {
            continue;
        }
        let window = lines[idx..lines.len().min(idx + 14)].join("\n");
        if is_allowed_probe(&window) || is_socketed_raw_call(&window) {
            continue;
        }
        if let Some(op) = tmux_server_op(&window) {
            violations.push(Violation {
                file: rel.clone(),
                line: idx + 1,
                op,
            });
        }
    }
}

fn relative_source_path(manifest: &Path, path: &Path) -> String {
    path.strip_prefix(manifest)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_allowed_probe(window: &str) -> bool {
    window.contains(".arg(\"-V\")") || window.contains(".args([\"-V\"")
}

fn is_socketed_raw_call(window: &str) -> bool {
    window.contains(".arg(\"-L\")") || window.contains(".args([\"-L\"")
}

fn tmux_server_op(window: &str) -> Option<&'static str> {
    const OPS: &[&str] = &[
        "has-session",
        "display-message",
        "new-session",
        "new-window",
        "kill-session",
        "kill-window",
        "list-panes",
        "list-windows",
        "capture-pane",
        "send-keys",
        "paste-buffer",
        "set-buffer",
        "load-buffer",
        "delete-buffer",
        "set-environment",
        "attach-session",
        "kill-server",
    ];
    OPS.iter().copied().find(|op| window.contains(&format!("\"{op}\"")))
}
