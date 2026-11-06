//! Send RED: short in-team names and fully-qualified logical names are co-equal TO forms.

#![allow(clippy::expect_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;

#[test]
fn cli_help_names_and_examples_short_recipient_and_explicit_scope() {
    let help = team_agent::cli::emit::__test_command_help(Some("send"));
    for text in [
        "team-agent send <agent>",
        "agent name",
        "--workspace",
        "--team",
        "--mailbox",
    ] {
        assert!(
            help.contains(text),
            "public send guidance missing {text:?}: {help}"
        );
    }
    assert!(!help.contains("<workspace>::<team>/<agent>"));
    assert!(
        help.lines().any(|line| {
            line.starts_with("team-agent send ")
                && line.contains("--workspace")
                && line.contains("--team")
        }),
        "public send must include a scoped short-name example: {help}"
    );
}

#[test]
fn installed_skill_names_and_examples_both_logical_to_forms() {
    let skill = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/team-agent/SKILL.md"
    ))
    .expect("read installed Team Agent skill source");
    for text in [
        "in-team short name",
        "<workspace>::<team>/<agent>",
        "team-agent send reviewer",
    ] {
        assert!(
            skill.contains(text),
            "Team Agent SKILL send contract must document both co-equal logical TO forms; missing {text:?}"
        );
    }
}



#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}
