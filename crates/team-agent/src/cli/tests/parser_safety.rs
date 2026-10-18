//! Public Phase-0 parser regressions. No model, real team or external pane is used.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-cli-parser334-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn reject(&self, args: &[&str]) {
        assert_eq!(run(&strings(args), &self.0), ExitCode::Usage, "{args:?}");
        assert!(std::fs::read_dir(&self.0).unwrap().next().is_none(),
            "parser errors must not create state, logs, roles or a database: {args:?}");
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn p1_read_and_lifecycle_unknown_flags_are_usage_without_writes() {
    for command in ["status", "shutdown", "restart", "approvals", "doctor", "models"] {
        let sandbox = Sandbox::new();
        sandbox.reject(&[command, "--worksapce", "never-selected", "--json"]);
    }
}

#[test]
fn p1_missing_duplicate_extra_and_invalid_numeric_arguments_refuse() {
    for args in [
        vec!["status", "--workspace"],
        vec!["status", "--workspace", "--team", "x"],
        vec!["status", "--team=x", "--team", "y"],
        vec!["status", "worker", "extra"],
        vec!["shutdown", "extra"],
        vec!["inbox", "leader", "--limit", "NaN"],
        vec!["inbox", "leader", "-n", "3", "--limit=4"],
        vec!["restart", "--session-converge-deadline=inf"],
        vec!["status", "--detail", "--summary"],
    ] {
        Sandbox::new().reject(&args);
    }
}

#[test]
fn p2_equal_and_split_values_have_identical_fields_without_losing_equals() {
    let sandbox = Sandbox::new();
    let path = sandbox.0.join("space = directory");
    std::fs::create_dir(&path).unwrap();
    let path = path.to_string_lossy();
    let split = parse_args(&strings(&["--workspace", &path, "--name", "x=y", "--timeout", "1.25"]));
    let inline = parse_args(&strings(&[&format!("--workspace={path}"), "--name=x=y", "--timeout=1.25"]));
    assert_eq!(inline.workspace, split.workspace);
    assert_eq!(inline.name, split.name);
    assert_eq!(inline.timeout, split.timeout);
    for path in [r"C:\owned dir\a=b", r"\\server\share\a=b"] {
        assert_eq!(parse_args(&strings(&[&format!("--workspace={path}")])).workspace,
            Some(PathBuf::from(path)));
    }
}

#[test]
fn p2_missing_explicit_workspace_never_falls_back_to_cwd_or_writes_logs() {
    for inline in [false, true] {
        let sandbox = Sandbox::new();
        let missing = sandbox.0.join("missing=a b").to_string_lossy().to_string();
        let mut args = vec!["status".to_string()];
        if inline { args.push(format!("--workspace={missing}")); }
        else { args.extend(["--workspace".to_string(), missing]); }
        assert_eq!(run(&args, &sandbox.0), ExitCode::Usage);
        assert!(std::fs::read_dir(&sandbox.0).unwrap().next().is_none());
    }
}

#[test]
fn p3_bare_restart_is_never_interpreted_as_workspace_even_if_directory_exists() {
    for exists in [false, true] {
        let sandbox = Sandbox::new();
        if exists { std::fs::create_dir(sandbox.0.join("worker")).unwrap(); }
        assert!(matches!(restart_args(&strings(&["worker"]), &sandbox.0), Err(CliError::Usage(_))));
        assert!(matches!(restart_args(&strings(&["reviewer.v2"]), &sandbox.0), Err(CliError::Usage(_))));
    }
}

#[test]
fn p4_attach_requires_explicit_nonempty_provider_but_keeps_declared_identity() {
    let sandbox = Sandbox::new();
    for args in [vec![], vec!["--provider="], vec!["--provider", ""]] {
        assert!(matches!(attach_leader_args(&strings(&args), &sandbox.0), Err(CliError::Usage(_))));
    }
    for provider in ["pi", "claude", "codex"] {
        let expected = parse_cli_provider(Some(provider)).unwrap();
        assert_eq!(attach_leader_args(&strings(&["--provider", provider]), &sandbox.0).unwrap().provider, expected);
    }
}

#[test]
fn p5_start_force_refuses_and_guides_to_existing_stop_start_commands() {
    let sandbox = Sandbox::new();
    match start_agent_args(&strings(&["worker", "--force"]), &sandbox.0) {
        Err(CliError::Usage(message)) => {
            assert!(message.contains("stop-agent") && message.contains("start-agent"), "{message}");
        }
        _ => panic!("--force must not be accepted and then silently discarded"),
    }
}

#[test]
fn payload_delimiter_preserves_help_no_display_and_equal_bytes() {
    let parsed = parse_args(&strings(&["worker", "--", "--help", "--no-display", "--workspace=a=b"]));
    assert_eq!(parsed.positionals, strings(&["worker", "--help", "--no-display", "--workspace=a=b"]));
    assert!(parsed.workspace.is_none());
    let literal = strings(&["--", "--help", "--no-display", "--workspace=a=b"]);
    assert_eq!(leader_launcher_args(&literal).unwrap().provider_args, literal[1..]);
}

#[test]
fn consumed_prompt_help_does_not_trigger_global_help_or_scope_fallback() {
    let sandbox = Sandbox::new();
    sandbox.reject(&["add-agent", "worker", "--prompt", "--help", "--provider", "pi",
        "--bypass", "false", "--workspace", "missing"]);
}

#[test]
fn real_control_help_remains_an_effect_free_short_circuit() {
    let sandbox = Sandbox::new();
    assert_eq!(run(&strings(&["status", "--unknown", "--help"]), &sandbox.0), ExitCode::Ok);
    assert!(std::fs::read_dir(&sandbox.0).unwrap().next().is_none());
}
