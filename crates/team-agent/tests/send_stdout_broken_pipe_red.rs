//! Send RED: stdout failure after persistence must not turn one obligation into a retryable lie.

#![allow(clippy::expect_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;

fn cli_output_sources() -> String {
    [
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/emit.rs"))
            .expect("read CLI emitter"),
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
            .expect("read binary entry"),
    ]
    .join("\n")
}

#[test]
fn cli_output_handles_broken_pipe_without_println_panic() {
    let source = cli_output_sources();
    assert!(
        source.contains("ErrorKind::BrokenPipe") && source.contains("write_all"),
        "send output RED: CLI must use fallible writes and handle ErrorKind::BrokenPipe; println! panic hides the durable send fact"
    );
}

#[test]
fn persisted_send_has_a_recoverable_message_id_when_stdout_fails() {
    let source = cli_output_sources();
    assert!(
        source.contains("persisted_message_id") && source.contains("stderr"),
        "send output RED: after persistence, an output failure must retain/report the canonical message_id so the sender does not retry blindly"
    );
}

#[test]
fn positive_control_send_persists_before_cli_emission() {
    let send = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/send/resolve.rs"
    ))
    .expect("read send funnel");
    let emit = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/emit.rs"))
        .expect("read emitter");
    assert!(
        send.contains("persist_resolved_target(args")
            && emit.contains("cmd_send(&send_args(args, cwd)?).map(emit_result)"),
        "positive control: send must persist its obligation before the top-level emitter runs"
    );
}

#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}
