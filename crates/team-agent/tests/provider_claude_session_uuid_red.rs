//! Claude session-id argv contracts.
//!
//! Claude CLI rejects `--session-id session-<hex>` with "Invalid session ID. Must be a valid UUID".
//! Fresh launch and fork must therefore generate RFC4122-shaped UUID values, not Team Agent's
//! old synthetic `session-` token.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use team_agent::model::enums::{AuthMode, Provider};
use team_agent::provider::{get_adapter, SessionId};

#[test]
fn claude_fresh_launch_session_id_is_rfc4122_uuid_not_session_prefix() {
    for provider in [Provider::Claude, Provider::ClaudeCode] {
        let adapter = get_adapter(provider);
        let argv = adapter
            .build_command(AuthMode::Subscription, None, None, Some("claude-sonnet-4-6"))
            .expect("Claude fresh launch command should build");
        let session_id = session_id_after_flag(&argv);

        assert!(
            is_rfc4122_uuid(session_id),
            "Claude fresh launch --session-id must be a valid RFC4122 UUID accepted by Claude CLI; provider={provider:?} session_id={session_id:?} argv={argv:?}"
        );
        assert!(
            !session_id.starts_with("session-"),
            "Claude fresh launch --session-id must not use Team Agent's invalid session-<hex> prefix; provider={provider:?} session_id={session_id:?} argv={argv:?}"
        );
    }
}

#[test]
fn claude_fork_new_session_id_is_rfc4122_uuid_not_session_prefix() {
    let source_session = SessionId::new("11111111-2222-4333-8444-555555555555");
    for provider in [Provider::Claude, Provider::ClaudeCode] {
        let adapter = get_adapter(provider);
        let argv = adapter
            .fork(Some(&source_session), AuthMode::Subscription, None)
            .expect("Claude fork command should build");
        let new_session_id = session_id_after_flag(&argv);

        assert!(
            is_rfc4122_uuid(new_session_id),
            "Claude fork --session-id must generate a valid RFC4122 UUID accepted by Claude CLI; provider={provider:?} new_session_id={new_session_id:?} argv={argv:?}"
        );
        assert!(
            !new_session_id.starts_with("session-"),
            "Claude fork --session-id must not use Team Agent's invalid session-<hex> prefix; provider={provider:?} new_session_id={new_session_id:?} argv={argv:?}"
        );
        assert!(
            argv_contains_adjacent(&argv, &["--resume", source_session.as_str(), "--fork-session"]),
            "Claude fork must still resume/fork from the source session while generating a separate UUID for --session-id; argv={argv:?}"
        );
    }
}

fn session_id_after_flag(argv: &[String]) -> &str {
    let index = argv
        .iter()
        .position(|arg| arg == "--session-id")
        .unwrap_or_else(|| panic!("Claude argv must include --session-id; argv={argv:?}"));
    argv.get(index + 1)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("Claude argv must provide a non-empty value after --session-id; argv={argv:?}"))
}

fn is_rfc4122_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for index in [8, 13, 18, 23] {
        if bytes[index] != b'-' {
            return false;
        }
    }
    for (index, byte) in bytes.iter().enumerate() {
        if [8, 13, 18, 23].contains(&index) {
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    matches!(bytes[14], b'1' | b'2' | b'3' | b'4' | b'5' | b'6' | b'7' | b'8')
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
}

fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}
