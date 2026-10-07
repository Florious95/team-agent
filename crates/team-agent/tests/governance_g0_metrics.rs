//! G0 hard discovery gate, aligned with Issue #286 Human30/Machine12 separation.
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::process::Command;

#[path = "support/human_catalog.rs"]
mod human_catalog;

fn exact_visible_contract(_help: &str) -> BTreeSet<String> {
    human_catalog::expected_human_commands()
}

#[test]
fn visible_command_count_stays_within_g0_limit() {
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .arg("--help")
        .output()
        .expect("run team-agent --help");
    assert!(
        output.status.success(),
        "team-agent --help must execute before G0 can measure visible commands; status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout);
    let commands = human_catalog::public_commands(&help);
    let expected = exact_visible_contract(&help);
    println!(
        "G0_METRIC visible_command_count current={} exact_set={} status=ok",
        commands.len(),
        expected.len()
    );
    println!(
        "G0_VISIBLE_COMMANDS {}",
        commands.iter().cloned().collect::<Vec<_>>().join(",")
    );
    assert_eq!(
        commands, expected,
        "G0 must equal the exact Human30 published set, including launchers and excluding Machine12"
    );
}
