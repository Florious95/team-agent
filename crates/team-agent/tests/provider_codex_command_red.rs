#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use team_agent::provider::{get_adapter, AuthMode, Provider, SessionId};

fn has_adjacent(argv: &[String], needle: &[&str]) -> bool {
    if needle.is_empty() {
        return true;
    }
    argv.windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(a, b)| a == b))
}

fn assert_codex_safety_flags(argv: &[String]) {
    assert!(
        has_adjacent(argv, &["--sandbox", "read-only"]),
        "codex command must always include sandbox mode: {argv:?}"
    );
    assert!(
        has_adjacent(argv, &["--ask-for-approval", "on-request"]),
        "codex command must always include approval mode: {argv:?}"
    );
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"),
        "non-dangerous codex command must not bypass approvals/sandbox: {argv:?}"
    );
}

#[test]
fn codex_sandbox_and_approval_flags_are_not_subscription_gated() {
    let adapter = get_adapter(Provider::Codex);
    let launch = adapter
        .build_command(AuthMode::CompatibleApi, None, Some("work"), Some("gpt-5.5"))
        .expect("codex launch command");
    assert_codex_safety_flags(&launch);

    let sid = SessionId::new("session-123");
    let resume = adapter
        .build_resume_command_with_context(
            Some(&sid),
            AuthMode::OfficialApi,
            None,
            Some("work"),
            Some("gpt-5.5"),
            &[],
        )
        .expect("codex resume command");
    assert_eq!(resume.get(1).map(String::as_str), Some("resume"));
    assert_eq!(resume.last().map(String::as_str), Some("session-123"));
    assert_codex_safety_flags(&resume);
}

#[test]
fn codex_dangerous_auto_approve_replaces_sandbox_and_approval_flags() {
    let adapter = get_adapter(Provider::Codex);
    let argv = adapter
        .build_command_with_tools(
            AuthMode::Subscription,
            None,
            Some("work"),
            Some("gpt-5.5"),
            &["dangerous_auto_approve"],
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
            &["dangerous_auto_approve"],
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
