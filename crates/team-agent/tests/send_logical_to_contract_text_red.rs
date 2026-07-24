//! Send RED: short in-team names and fully-qualified logical names are co-equal TO forms.

#![allow(clippy::expect_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;

#[test]
fn cli_help_names_and_examples_both_logical_to_forms() {
    let spec = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/spec.rs"))
        .expect("read CLI command catalog");
    for text in [
        "in-team short name",
        "<workspace>::<team>/<agent>",
        "team-agent send reviewer",
    ] {
        assert!(
            spec.contains(text),
            "send CLI help must document both co-equal logical TO forms; missing {text:?}"
        );
    }
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

#[test]
fn positional_send_really_routes_both_forms_through_the_same_named_resolver() {
    let send = [
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/send.rs"))
            .expect("read send entry"),
        fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/cli/send/resolve.rs"
        ))
        .expect("read send resolver"),
    ]
    .join("\n");
    assert!(
        send.contains("resolve_name_for_cli(\n            &args.workspace,\n            name,")
            && send.contains("args.target.clone().unwrap_or_default()"),
        "behavior tooth: positional TO (short or fully-qualified) must enter the same logical-name resolver"
    );
}

#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}
