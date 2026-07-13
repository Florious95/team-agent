//! 0.5.39 RED contracts for the tmux-server-death first car.
//!
//! References:
//! - `.team/artifacts/tmux-server-death-locate.md` Slice 1 / Slice 2
//!   and Addendum 11.1 B.
//! - Latest scope cut: no private-server sizing workaround, no broad upstream
//!   tmux 3.6a compatibility layer, and no Slice 3 restart retry in this car.
//!
//! User-visible contract:
//! - Display cleanup and raw tmux guards must not bypass the workspace-scoped
//!   transport through helper indirection.
//! - Worker provider exits are contained like manual tmux: the pane keeps a
//!   marker/shell fallback instead of disappearing via a final provider exec.
//! - tmux server death is diagnosed as `tmux_server_crashed`, not as a cryptic
//!   agent-local provider failure or only `tmux_session_missing`.
//! - The real-machine fuzz gate includes the missing production shape:
//!   managed leader pane bang injection, bare `add-agent`, private socket.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn display_cleanup_uses_scoped_transport_not_raw_tmux_helper() {
    let display = read_repo_file("crates/team-agent/src/lifecycle/display.rs");
    let mut offenders = Vec::new();

    for needle in [
        ("std::process::Command", "raw process tmux import"),
        ("fn run_tmux(", "ambient tmux helper"),
        (
            "run_tmux(&[\"list-windows\"",
            "ambient list-windows helper call",
        ),
        (
            "run_tmux(&[\"kill-window\"",
            "ambient kill-window helper call",
        ),
        ("run_tmux(&[\"kill-pane\"", "ambient kill-pane helper call"),
        (
            "run_tmux(&[\"kill-session\"",
            "ambient kill-session helper call",
        ),
    ] {
        if display.contains(needle.0) {
            offenders.push(needle.1.to_string());
        }
    }

    for (path, line, op) in raw_tmux_helper_offenders() {
        offenders.push(format!("{path}:{line} helper/raw tmux op `{op}`"));
    }

    assert!(
        offenders.is_empty(),
        "RED1: display cleanup and the source guard must route tmux session/window/pane operations through the selected scoped transport; helper indirection must not inherit ambient $TMUX. offenders:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn worker_spawns_use_provider_wrapper_with_exit_marker_and_shell_fallback() {
    let backend = read_repo_file("crates/team-agent/src/tmux_backend.rs");
    let transport = read_repo_file("crates/team-agent/src/transport.rs");
    let spawn_common = read_repo_file("crates/team-agent/src/lifecycle/restart/common.rs");

    let missing = missing_requirements(&[
        (
            "worker shell wrapper builder adjacent to leader wrapper",
            backend.contains("worker_shell_wrapper_command"),
        ),
        (
            "worker provider exit marker single source",
            backend.contains("WORKER_PROVIDER_EXIT_MARKER")
                || backend.contains("worker_provider_exit_marker"),
        ),
        (
            "worker wrapper falls back to an interactive shell after provider exit",
            backend.contains("worker_shell_wrapper_command")
                && backend.contains("${SHELL:-/bin/zsh}"),
        ),
        (
            "transport trait exposes first-worker wrapper spawn",
            transport.contains("spawn_first_with_worker_shell_wrapper"),
        ),
        (
            "transport trait exposes into-existing-session worker wrapper spawn",
            transport.contains("spawn_into_with_worker_shell_wrapper"),
        ),
        (
            "worker restart/add-agent spawn path calls the worker wrapper",
            spawn_common.contains("spawn_first_with_worker_shell_wrapper")
                || spawn_common.contains("spawn_into_with_worker_shell_wrapper"),
        ),
        (
            "leader wrapper remains as the mechanism to reuse",
            backend.contains("leader_shell_wrapper_command")
                && transport.contains("spawn_first_with_leader_shell_wrapper"),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "RED2: worker panes must contain provider exit the same way managed leader panes do: provider child process, explicit marker, then shell fallback. Missing: {missing:?}"
    );
}

#[test]
fn tmux_server_death_is_classified_as_tmux_server_crashed() {
    let diagnose = read_repo_file("crates/team-agent/src/cli/diagnose.rs");
    let restart = read_repo_file("crates/team-agent/src/lifecycle/restart/rebuild.rs");
    let transport = read_repo_file("crates/team-agent/src/transport.rs");
    let backend = read_repo_file("crates/team-agent/src/tmux_backend.rs");
    let combined = format!("{diagnose}\n{restart}\n{transport}\n{backend}");

    let missing = missing_requirements(&[
        (
            "diagnose emits issue id tmux_server_crashed",
            diagnose.contains("tmux_server_crashed"),
        ),
        (
            "shared classifier recognizes real tmux stderr: server exited unexpectedly",
            combined.contains("server exited unexpectedly")
                && combined.contains("tmux_server_crashed"),
        ),
        (
            "restart maps spawn/readiness tmux server death to tmux_server_crashed",
            restart.contains("tmux_server_crashed"),
        ),
        (
            "restart failure output keeps an executable next action after server death",
            restart.contains("tmux_server_crashed")
                && (restart.contains("next_actions") || restart.contains("team-agent diagnose")),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "RED3: a selected tmux endpoint with live state plus stderr `server exited unexpectedly` must surface `tmux_server_crashed`, not only tmux_session_missing/server_exited/provider failure. Missing: {missing:?}"
    );
}

#[test]
fn minimum_bang_private_socket_bare_add_agent_gate_is_declared() {
    let candidates = candidate_gate_files();
    let required = [
        "TMUX_SERVER_DEATH_0539_BANG_PRIVATE_SOCKET",
        "send-keys",
        "add-agent",
        "--role-file",
        "--workspace",
        "--team",
        "private",
        "mcp.server_exit",
        "coordinator.session_missing",
    ];

    let matches = candidates
        .iter()
        .filter(|(_, text)| required.iter().all(|needle| text.contains(needle)))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();

    assert!(
        !matches.is_empty(),
        "RED4: first-car fuzz/gate coverage must declare the minimum production shape: managed leader pane bang injection (`send-keys`), bare add-agent with role-file/workspace/team args, private socket, and assertions that existing MCP workers do not get stdin_eof and coordinator does not report session_missing. Missing marker `TMUX_SERVER_DEATH_0539_BANG_PRIVATE_SOCKET`; scanned {} files.",
        candidates.len()
    );
}

fn raw_tmux_helper_offenders() -> Vec<(String, usize, &'static str)> {
    let mut files = Vec::new();
    collect_rs_files(&repo_root().join("crates/team-agent/src"), &mut files);

    let mut offenders = Vec::new();
    for path in files {
        let rel = relative_path(&path);
        if rel == "crates/team-agent/src/tmux_backend.rs" || rel.contains("/tests/") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read production source");
        for (idx, line) in text.lines().enumerate() {
            for op in [
                "has-session",
                "display-message",
                "new-session",
                "new-window",
                "kill-session",
                "kill-window",
                "kill-pane",
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
            ] {
                if line.contains("run_tmux(&[") && line.contains(&format!("\"{op}\"")) {
                    offenders.push((rel.clone(), idx + 1, op));
                }
            }
        }
    }
    offenders
}

fn candidate_gate_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for relative_dir in [
        "crates/team-agent/tests",
        "crates/team-agent/tests/e2e/cases",
        "tools",
        ".team/artifacts/gate-harness",
    ] {
        let dir = root.join(relative_dir);
        if dir.exists() {
            collect_text_files(&root, &dir, &mut files);
        }
    }
    files
        .into_iter()
        .filter(|(path, _)| !path.ends_with("tmux_server_death_0539_contract.rs"))
        .collect()
}

fn missing_requirements(requirements: &[(&str, bool)]) -> Vec<String> {
    requirements
        .iter()
        .filter_map(|(name, ok)| (!*ok).then(|| (*name).to_string()))
        .collect()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn collect_text_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("read candidate dir") {
        let path = entry.expect("read candidate entry").path();
        if path.is_dir() {
            collect_text_files(root, &path, out);
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(
            ext,
            "rs" | "py" | "sh" | "zsh" | "md" | "yaml" | "yml" | "json"
        ) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        out.push((rel, text));
    }
}

fn read_repo_file(path: &str) -> String {
    fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("read repo file {path}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn relative_path(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
