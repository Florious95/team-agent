//! 0.5.61 send mailbox one-bit public-surface RED contract.
//!
//! Requirement anchors: B01-B03; C01-C03; M04.
//! S-011 sensitive: this file assumes one compatibility release, but the new
//! recommended surface is already one bit and never exposes the old taxonomy.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::process::Output;

use hermetic_guard::HermeticTestEnv;
use serde_json::Value;
use team_agent::mcp_server::wire::tools_contract;

const RETIRED_PUBLIC_TERMS: &[&str] = &["--presentation-sink", "--message-class", "--case-id"];

#[test]
#[serial_test::serial(env)]
fn red_1_cli_send_help_exposes_only_the_mailbox_bit() {
    let env = HermeticTestEnv::enter("061-send-mailbox-help");
    let output = env.run_cli(env.root(), &["send", "--help"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "setup: {}",
        combined(&output)
    );
    let help = combined(&output).to_ascii_lowercase();

    assert!(
        help.contains("--mailbox"),
        "RED-1 capability_missing: public send help has no one-bit `--mailbox` choice; help={help}"
    );
    for retired in RETIRED_PUBLIC_TERMS {
        assert!(
            !help.contains(retired),
            "RED-1 taxonomy_not_retired: new public send help still exposes `{retired}`; help={help}"
        );
    }
}

#[test]
fn red_2_mcp_send_schema_is_optional_boolean_mailbox_only() {
    let send = send_contract();
    let schema = &send["inputSchema"];
    let properties = schema["properties"]
        .as_object()
        .expect("setup: send_message properties must be an object");

    assert_eq!(
        properties.get("mailbox").and_then(|value| value.get("type")),
        Some(&Value::String("boolean".to_string())),
        "RED-2 capability_missing: send_message schema must expose one optional boolean `mailbox`; schema={schema}"
    );
    assert!(
        !schema["required"]
            .as_array()
            .expect("setup: send_message required must be an array")
            .iter()
            .any(|value| value.as_str() == Some("mailbox")),
        "RED-2 default_broken: omitting mailbox must remain legal and mean live leader delivery"
    );
    assert!(
        properties.get("presentation").is_none(),
        "RED-2 taxonomy_not_retired: the recommended MCP send surface still exposes presentation classification; schema={schema}"
    );
}

#[test]
fn red_3_mcp_send_description_assigns_no_classification_job() {
    let send = send_contract();
    let description = send["description"]
        .as_str()
        .expect("setup: send_message description must be a string")
        .to_ascii_lowercase();

    assert!(
        description.contains("mailbox") && description.contains("default"),
        "RED-3 capability_missing: send_message must explain mailbox=true versus omitted/default live delivery; description={description}"
    );
    for retired in ["class", "sink", "policy", "presentation"] {
        assert!(
            !description.contains(retired),
            "RED-3 classification_burden_remains: description still asks the sender to reason about `{retired}`; description={description}"
        );
    }
}

#[test]
fn canary_retired_term_detector_distinguishes_both_sides() {
    let clean = "send accepts an optional mailbox boolean; omitted means live delivery";
    let dirty = "send accepts --message-class stage_pass";
    assert!(!contains_retired_public_term(clean));
    assert!(contains_retired_public_term(dirty));
}

fn send_contract() -> Value {
    tools_contract()
        .into_iter()
        .find(|tool| tool["name"] == Value::String("send_message".to_string()))
        .expect("setup: tools/list must contain send_message")
}

fn contains_retired_public_term(text: &str) -> bool {
    RETIRED_PUBLIC_TERMS
        .iter()
        .any(|retired| text.contains(retired))
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
