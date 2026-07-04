//! Phase 1d Batch 0 grep guards for `transport_factory.rs`.
//!
//! - **C-1**: the resolver must NOT contain any "explicit conpty falls
//!   back to tmux" or "pty maps to conpty" branch. Grep the source
//!   file for known-bad patterns.
//! - **C-5**: the resolver + module must NOT have side effects during
//!   assembly. Specifically it must not call `kill_server`, emit
//!   stale events, open pipes, start shims, or rotate tokens.
//!
//! Both are C-6 gate prerequisites — failing here MUST prevent Batch 1
//! caller migration.
//!
//! (CR C-1/C-5, `.team/artifacts/0.5.x-backend-factory-cr-verdict.md`.)

use std::path::PathBuf;

fn factory_src() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("transport_factory.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read transport_factory.rs at {}: {e}",
            path.display()
        )
    })
}

/// Extract non-test, non-comment code. Drops the `mod tests` block +
/// every line whose first non-whitespace characters are `//` or that
/// starts a `/* ... */` block, so doc-comments explaining what's
/// forbidden do not trigger the guard on themselves.
///
/// This is a coarse line-level filter — sufficient because the file
/// uses only `//` and `//!` comments for prose. If someone adds
/// `/* ... */` blocks with forbidden strings inside, extend this.
fn non_test_body(src: &str) -> String {
    let non_test = if let Some(idx) = src.find("#[cfg(test)]") {
        &src[..idx]
    } else {
        src
    };
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
fn c1_grep_guard_no_conpty_or_pty_to_tmux_fallback_branch() {
    let src = factory_src();
    let body = non_test_body(&src);
    // Any of these forbidden literal substrings would signal that a
    // programmer added a "if requested was conpty|pty then use tmux"
    // silent-downgrade escape hatch. Written as multiple checks so
    // failures point at the exact offender.
    for forbidden in [
        // "conpty ..." followed by tmux backend construction on the same branch.
        // We deliberately look for very literal shapes — a broader
        // regex would false-positive on the guard's own comment.
        "Downgrading conpty",
        "downgrade to tmux",
        "fall back to tmux",
        "fallback to tmux",
        "map pty to conpty",
        "map \"pty\" to conpty",
        "pty => Ok",
        "\"pty\" => Ok",
    ] {
        assert!(
            !body.contains(forbidden),
            "CR C-1 grep guard: forbidden fallback substring {forbidden:?} appears in \
             transport_factory.rs non-test body — this would silently downgrade the \
             requested backend, violating MUST-NOT-13 + C-1 fail-closed."
        );
    }
    // Positive assertion: the resolver body MUST mention the two
    // typed refusal variants (proves the file has the fail-closed
    // shape at all).
    assert!(
        body.contains("ConPtyPreconditionUnmet"),
        "CR C-1: ConPtyPreconditionUnmet variant must be present in resolver body"
    );
    assert!(
        body.contains("UnsupportedSpecBackendLiteral"),
        "CR C-1: UnsupportedSpecBackendLiteral variant must be present in resolver body"
    );
}

#[test]
fn c5_grep_guard_no_side_effects_in_factory() {
    // Factory MUST be pure assembly (design §CR C-3:353-365 + CR C-5).
    // These substrings signal boundary violations.
    let src = factory_src();
    let body = non_test_body(&src);
    for forbidden in [
        // No kill_server call inside the factory (kill decisions live
        // in KillDecision on the caller side — CR C-2 unchanged).
        ".kill_server(",
        "kill_server()",
        // No stale event emission (belongs on backend/pipe-client
        // side — CR C-3).
        "shim_pid_stale",
        "pipe_name_stale",
        "pipe_token_mismatch",
        // No event_log writes at all in the factory — all events are
        // emitted by the caller from the `notices` list.
        "EventLog::",
        "event_log::EventLog",
        ".write(\"transport",
        // No pipe/socket opening (that's Batch 2/3 scope + the shim
        // binary; factory is pure).
        "CreateNamedPipe",
        "NamedPipeClient",
        "connect_named_pipe",
        // No shim spawning.
        "windows-shim",
        "Command::new(\"windows-shim\"",
        "std::process::Command::new(",
        // No token rotation.
        "rotate_pipe_token",
        ".pipe_token = ",
    ] {
        assert!(
            !body.contains(forbidden),
            "CR C-5 grep guard: forbidden side-effect substring {forbidden:?} appears in \
             transport_factory.rs non-test body — the factory must be pure assembly."
        );
    }
    // Positive assertion: the C-5 anchor must be preserved in code
    // (not just comments) — the `resolve_transport` symbol proves the
    // pure-assembly entrypoint exists.
    assert!(
        body.contains("pub fn resolve_transport"),
        "CR C-5: `resolve_transport` entrypoint must exist as the single \
         pure-assembly boundary"
    );
}

#[test]
fn c6_gate_ordering_batch_0_tests_present_and_grep_guards_present() {
    // C-6 procedural: this file itself must exist AND the required
    // Batch 0 unit tests must be nameable. If a future PR renames or
    // removes any of the five design-mandated tests, this test fires
    // so Batch 1 cannot proceed on a half-checked abstraction.
    let src = factory_src();
    let required_batch_0_tests = [
        "no_input_resolves_to_default_tmux_workspace_backend",
        "legacy_tmux_endpoint_resolves_to_tmux_endpoint_backend",
        "explicit_conpty_with_team_key_resolves_to_conpty",
        "explicit_conpty_without_team_key_refuses_no_silent_fallback",
        "pty_literal_is_not_silently_mapped_to_conpty",
    ];
    for name in required_batch_0_tests {
        assert!(
            src.contains(&format!("fn {name}(")),
            "CR C-6: Batch 0 mandatory test `{name}` not found in transport_factory.rs — \
             Batch 1 must NOT proceed while any of the five design-mandated tests are missing."
        );
    }
}
