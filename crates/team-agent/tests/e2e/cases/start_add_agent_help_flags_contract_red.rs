use crate::framework::*;

fn assert_help_options(tag: &str, command: &str, expected: &[&str]) {
    let ws = TestWorkspace::new(tag);
    let output = run_ta(&ws, &[command, "--help"]);
    assert!(
        output.is_success(),
        "{command} --help must succeed: exit={} stdout={} stderr={}",
        output.exit_code,
        output.stdout,
        output.stderr
    );

    let help = format!("{}{}", output.stdout, output.stderr);
    let missing = expected
        .iter()
        .copied()
        .filter(|flag| !help.contains(flag))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "{command} --help must document all supported options; missing={missing:?}\nhelp:\n{help}"
    );
}

#[test]
fn start_agent_help_lists_all_supported_overrides() {
    assert_help_options(
        "start-agent-help",
        "start-agent",
        &["--model", "--effort", "--bypass", "--prompt", "--provider", "--profile"],
    );
}

#[test]
fn add_agent_help_lists_all_supported_role_fields() {
    assert_help_options(
        "add-agent-help",
        "add-agent",
        &["--provider", "--bypass", "--model", "--effort", "--prompt", "--profile"],
    );
}
