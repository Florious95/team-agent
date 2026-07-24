//! F4 RED: a durable send that cannot be delivered immediately tells the sender why.

#![allow(clippy::expect_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;

fn mailbox_source() -> String {
    fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/send/mailbox.rs"
    ))
    .expect("read mailbox module")
}

#[test]
fn never_attached_mailbox_receipt_is_typed_and_honest() {
    let source = mailbox_source();
    for required in [
        "\"status\": \"deferred\"",
        "\"deferred_reason\": \"never_attached\"",
        "\"delivered\": false",
        "\"message_status\": \"queued_until_leader_attach\"",
    ] {
        assert!(
            source.contains(required),
            "F4 never-attached receipt missing required typed fact {required:?}"
        );
    }
}

#[test]
fn deferred_reason_does_not_create_a_second_durable_state_catalog() {
    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/db/message_store.rs"
    ))
    .expect("read canonical message state catalog");
    assert!(
        !source.contains("NeverAttached") && !source.contains("never_attached"),
        "F4 deferred_reason is presentation metadata; it must not become a second row state"
    );
}

#[test]
fn existing_mailbox_positive_control_still_preserves_obligation_and_message_id() {
    let source = mailbox_source();
    for canary in [
        "enqueue_leader_mailbox_until_attach",
        "\"message_id\": message_id",
        "\"channel\": \"leader_mailbox\"",
    ] {
        assert!(
            source.contains(canary),
            "positive control: existing durable mailbox funnel lost {canary:?}"
        );
    }
}

#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}
