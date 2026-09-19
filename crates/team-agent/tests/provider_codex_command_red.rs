#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use team_agent::provider::{get_adapter, AuthMode, Provider, SessionId};

fn has_adjacent(argv: &[String], needle: &[&str]) -> bool {
    if needle.is_empty() {
        return true;
    }
    argv.windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(a, b)| a == b))
}

fn assert_codex_has_no_tool_derived_restrictions(argv: &[String]) {
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--sandbox" || arg == "--ask-for-approval"),
        "codex command must not derive sandbox/approval restrictions from role metadata: {argv:?}"
    );
}

#[test]
fn codex_sandbox_and_approval_flags_are_not_role_metadata_gated() {
    let adapter = get_adapter(Provider::Codex);
    let launch = adapter
        .build_command(AuthMode::CompatibleApi, None, Some("work"), Some("gpt-5.5"))
        .expect("codex launch command");
    assert_codex_has_no_tool_derived_restrictions(&launch);

    let sid = SessionId::new("session-123");
    let resume = adapter
        .build_resume_command_with_context(
            Some(&sid),
            AuthMode::OfficialApi,
            None,
            Some("work"),
            Some("gpt-5.5"),
            false,
        )
        .expect("codex resume command");
    assert_eq!(resume.get(1).map(String::as_str), Some("resume"));
    assert_eq!(resume.last().map(String::as_str), Some("session-123"));
    assert_codex_has_no_tool_derived_restrictions(&resume);
}

#[test]
fn codex_bypass_is_directly_boolean_and_replaces_restrictions() {
    let adapter = get_adapter(Provider::Codex);
    let argv = adapter
        .build_command_with_permissions(
            AuthMode::Subscription,
            None,
            Some("work"),
            Some("gpt-5.5"),
            true,
        )
        .expect("codex dangerous command");
    assert!(
        argv.iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"),
        "dangerous codex command must use bypass flag: {argv:?}"
    );
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--sandbox" || arg == "--ask-for-approval"),
        "dangerous codex command must replace sandbox/approval flags: {argv:?}"
    );

    let sid = SessionId::new("source-session");
    let fork = adapter
        .fork_with_context(
            Some(&sid),
            AuthMode::Subscription,
            None,
            None,
            None,
            true,
        )
        .expect("codex fork command");
    assert_eq!(fork.get(1).map(String::as_str), Some("fork"));
    assert_eq!(fork.last().map(String::as_str), Some("source-session"));
    assert!(fork
        .iter()
        .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"));
    assert!(!fork
        .iter()
        .any(|arg| arg == "--sandbox" || arg == "--ask-for-approval"));
}
