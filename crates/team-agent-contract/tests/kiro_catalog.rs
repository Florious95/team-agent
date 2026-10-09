//! Controlled subprocess/raw-read fixtures, not a live Kiro/auth test. H1 schema
//! validation of the separately captured native JSON is in kiro_model_catalog.rs.
#![cfg(unix)]
mod support;
use std::time::{Duration, Instant};
use support::Sandbox;
use team_agent_contract::contract::{hooks::*, types::*};
use team_agent_contract::host::{command::*, digest, process::resolve_cwd};
use team_agent_contract::kiro::{native::*, KiroAdapter, McpStdio};

struct CapturedRunner {
    output: Option<CommandReceipt>,
    calls: usize,
}
impl CommandRunner for CapturedRunner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        self.calls += 1;
        assert!(
            request.stdin.is_none(),
            "closed stdin, not inherited terminal"
        );
        assert_eq!(request.reject_stdout, Some(AUTH_PORTAL_MARKER));
        assert_eq!(
            request
                .arguments
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect::<Vec<_>>(),
            CHAT_CATALOG_ARGUMENTS
        );
        assert!(request.budget <= discovery_bounds().deadline);
        self.output.take().expect("no automatic retry")
    }
}
fn receipt(end: CommandEnd, code: Option<i32>, stdout: Vec<u8>) -> CommandReceipt {
    CommandReceipt {
        end,
        exit_code: code,
        child_pid: Some(42),
        child_reaped: true,
        stdout,
        stderr: vec![],
        elapsed: Duration::from_millis(1),
    }
}
fn fixture(
    sandbox: &Sandbox,
    output: CommandReceipt,
) -> (CatalogRequest, CatalogReader<CapturedRunner>) {
    let engine = sandbox.parent.join("kiro-cli-chat");
    std::fs::write(&engine, b"fake engine bytes; never executed").unwrap();
    let request = CatalogRequest {
        provider: ProviderId::new("kiro").unwrap(),
        native: NativeIdentity {
            version: "2.28.0".into(),
            harness: "v3".into(),
            ui: "tui".into(),
            platform: Platform::Linux,
            executable_sha256: digest(b"fake engine bytes; never executed"),
        },
        executable: engine,
        cwd: resolve_cwd(&sandbox.parent).unwrap(),
        source: CHAT_CATALOG_SOURCE,
        bounds: discovery_bounds(),
    };
    let reader = CatalogReader::new(
        request.clone(),
        CapturedRunner {
            output: Some(output),
            calls: 0,
        },
    )
    .unwrap();
    (request, reader)
}
#[test]
fn observed_auth_spinner_is_typed_even_if_outer_tool_reports_timeout_or_exit_zero() {
    for (end, code) in [
        (CommandEnd::OutputRejected, None),
        (CommandEnd::TimedOut, None),
        (CommandEnd::Exited, Some(0)),
    ] {
        let sandbox = Sandbox::new();
        let output=b"\x1b[?25l\rOpening auth portal and logging in...\rOpening auth portal and logging in...".to_vec();
        let (request, mut reader) = fixture(&sandbox, receipt(end, code, output));
        assert!(matches!(
            reader.read_catalog(&request),
            Err(ReadFailure::AuthRequired)
        ));
        assert_eq!(reader.runner_mut().calls, 1);
    }
}
#[test]
fn complete_large_json_requires_real_exit_but_raw_read_alone_is_not_model_schema_admission() {
    let sandbox = Sandbox::new();
    let bytes =
        serde_json::to_vec(&serde_json::json!({"fixture_only":"x".repeat(120_000)})).unwrap();
    let (request, mut reader) = fixture(
        &sandbox,
        receipt(CommandEnd::Exited, Some(0), bytes.clone()),
    );
    let raw = reader.read_catalog(&request).unwrap();
    assert_eq!(raw.stdout, bytes);
    assert_eq!(raw.exit_code, 0);
    assert_eq!(request.source.schema, "kiro-2.28.0-list-models-json-v1");
}
#[test]
fn captured_native_json_flows_through_bounded_reader_and_bound_h1() {
    let sandbox = Sandbox::new();
    let bytes = include_bytes!("fixtures/kiro-2.28.0-models.json");
    let (request, mut reader) = fixture(
        &sandbox,
        receipt(CommandEnd::Exited, Some(0), bytes.to_vec()),
    );
    let adapter = KiroAdapter::new(McpStdio {
        executable: "/fixture/candidate".into(),
        arguments: vec![],
        environment: Default::default(),
    })
    .unwrap();
    let hooks = adapter.hooks();
    let catalog = hooks
        .catalog
        .require("H1 CatalogHook")
        .unwrap()
        .discover(&request, &mut reader)
        .unwrap();
    assert_eq!(catalog.models.len(), 9);
    assert_eq!(catalog.provider, request.provider);
    assert_eq!(catalog.native, request.native);
    assert_eq!(catalog.schema, CHAT_CATALOG_SOURCE.schema);
    assert_eq!(reader.runner_mut().calls, 1);
}
#[test]
fn timeout_truncation_invalid_json_and_native_exit_are_never_model_success() {
    let cases = [
        (CommandEnd::TimedOut, None, b"{}".as_slice()),
        (CommandEnd::OutputLimit, None, b"{}".as_slice()),
        (CommandEnd::Exited, Some(1), b"{}".as_slice()),
        (CommandEnd::Exited, Some(0), b"{\"models\":".as_slice()),
        (CommandEnd::Exited, Some(0), b"{} trailing".as_slice()),
    ];
    for (end, code, bytes) in cases {
        let sandbox = Sandbox::new();
        let (request, mut reader) = fixture(&sandbox, receipt(end, code, bytes.to_vec()));
        assert!(reader.read_catalog(&request).is_err());
        assert_eq!(reader.runner_mut().calls, 1);
    }
}
#[test]
fn catalog_request_and_binary_are_fenced_before_any_native_command() {
    let sandbox = Sandbox::new();
    let (request, mut reader) = fixture(
        &sandbox,
        receipt(CommandEnd::Exited, Some(0), b"{}".to_vec()),
    );
    let mut wrong = request.clone();
    wrong.bounds.max_output_bytes += 1;
    assert!(reader.read_catalog(&wrong).is_err());
    wrong = request.clone();
    wrong.source.arguments = &["login"];
    assert!(reader.read_catalog(&wrong).is_err());
    std::fs::write(&request.executable, b"replaced").unwrap();
    assert!(reader.read_catalog(&request).is_err());
    assert_eq!(reader.runner_mut().calls, 0);
}
#[test]
fn real_owned_command_guard_aborts_spinner_before_timeout_without_another_command() {
    let request = CommandRequest {
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            r"printf '\033[?25lOpening auth portal and logging in...'; exec sleep 10".into(),
        ],
        cwd: None,
        environment: team_agent_contract::contract::plan::EnvironmentDelta {
            remove: Default::default(),
            set: Default::default(),
        },
        stdin: None,
        budget: Duration::from_secs(3),
        limits: OutputLimits {
            stdout: 4096,
            stderr: 4096,
            stdin: 0,
        },
        reject_stdout: Some(AUTH_PORTAL_MARKER),
    };
    assert!(request
        .arguments
        .iter()
        .all(|arg| !arg.as_encoded_bytes().contains(&0)));
    let mut runner = RealCommandRunner::default();
    let began = Instant::now();
    let result = runner.run(&request);
    assert_eq!(result.end, CommandEnd::OutputRejected);
    assert!(!result.success());
    assert!(began.elapsed() < request.budget);
    assert_eq!(command_failure(&result), Some(CommandFailure::AuthRequired));
    let cleanup_deadline = Instant::now() + Duration::from_secs(1);
    while !runner.pending_children().is_empty() && Instant::now() < cleanup_deadline {
        std::thread::sleep(Duration::from_millis(5));
        runner.reap_owned();
    }
    assert!(runner.pending_children().is_empty());
}
#[test]
fn real_catalog_style_command_sees_eof_on_stdin() {
    let mut runner = RealCommandRunner::default();
    let request = CommandRequest {
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "if read answer; then exit 7; else printf '{}'; fi".into(),
        ],
        cwd: None,
        environment: team_agent_contract::contract::plan::EnvironmentDelta {
            remove: Default::default(),
            set: Default::default(),
        },
        stdin: None,
        budget: Duration::from_secs(3),
        limits: OutputLimits {
            stdout: 4096,
            stderr: 4096,
            stdin: 0,
        },
        reject_stdout: Some(AUTH_PORTAL_MARKER),
    };
    let result = runner.run(&request);
    assert!(result.success());
    assert_eq!(result.stdout, b"{}");
}
