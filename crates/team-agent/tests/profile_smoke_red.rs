#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use team_agent::cli::{cmd_doctor, cmd_preflight, CmdOutput, DoctorArgs, ExitCode, PreflightArgs};

#[test]
fn preflight_compatible_api_profile_smoke_passes_only_after_real_http_probe() {
    let server = MockLlmServer::new(
        200,
        r#"{"id":"ok","content":[{"type":"text","text":"pong"}]}"#,
    );
    let fixture = ProfileFixture::new("smoke-pass", server.base_url(), "local-secret");

    let report = preflight_json(&fixture.team);
    let check = profile_smoke_check(&report);

    assert_eq!(
        check.get("ok").and_then(Value::as_bool),
        Some(true),
        "reachable mock profile should pass; report={report}"
    );
    assert_eq!(
        check.get("status").and_then(Value::as_str),
        Some("smoke_passed"),
        "compatible_api smoke status must reflect a real bounded HTTP call, not a hardcoded pass; check={check} report={report}"
    );
    assert_eq!(
        check.get("http_status").and_then(Value::as_u64),
        Some(200),
        "profile smoke must expose the HTTP status from the compatible_api endpoint; check={check}"
    );
    assert_eq!(
        check.get("secret_values_printed").and_then(Value::as_bool),
        Some(false),
        "profile smoke diagnostics must explicitly audit that secrets were redacted; check={check}"
    );
    assert!(
        server.was_called(),
        "preflight must actually POST to the compatible_api profile endpoint; report={report}"
    );
    assert!(
        !report.to_string().contains("local-secret"),
        "preflight/profile smoke output must not leak auth tokens; report={report}"
    );
}

#[test]
fn preflight_compatible_api_profile_smoke_401_fails_honestly_with_diagnostics() {
    let server = MockLlmServer::new(
        401,
        r#"{"error":{"message":"bad token should be redacted"}}"#,
    );
    let fixture = ProfileFixture::new("smoke-401", server.base_url(), "wrong-secret");

    let result = cmd_preflight(&PreflightArgs {
        team: fixture.team.clone(),
        json: true,
    })
    .expect("preflight should return a JSON report, not panic, when smoke endpoint is 401");
    let report = match &result.output {
        CmdOutput::Json(value) => value.clone(),
        other => panic!("expected JSON preflight report, got {other:?}"),
    };
    let check = profile_smoke_check(&report);

    assert_eq!(
        result.exit,
        ExitCode::Error,
        "compatible_api HTTP 401 must make preflight fail before worker launch; report={report}"
    );
    assert_eq!(
        check.get("ok").and_then(Value::as_bool),
        Some(false),
        "check={check}"
    );
    assert_eq!(
        check.get("status").and_then(Value::as_str),
        Some("smoke_failed"),
        "check={check}"
    );
    assert_eq!(
        check.get("reason").and_then(Value::as_str),
        Some("http_error"),
        "check={check}"
    );
    assert_eq!(
        check.get("http_status").and_then(Value::as_u64),
        Some(401),
        "check={check}"
    );
    assert!(
        report
            .get("blockers")
            .and_then(Value::as_array)
            .is_some_and(|blockers| blockers
                .iter()
                .any(|blocker| blocker.to_string().contains("profile_smoke"))),
        "profile_smoke failure must be surfaced as a blocker; report={report}"
    );
    assert!(
        server.was_called(),
        "preflight must hit the mock endpoint before reporting 401; report={report}"
    );
    assert!(
        !report.to_string().contains("wrong-secret"),
        "failure diagnostics must redact auth token values; report={report}"
    );
}

#[test]
fn doctor_reports_same_compatible_api_profile_smoke_signal_as_preflight() {
    let server = MockLlmServer::new(
        401,
        r#"{"error":{"message":"bad token should be redacted"}}"#,
    );
    let fixture = ProfileFixture::new("doctor-smoke", server.base_url(), "doctor-wrong-secret");

    let result = cmd_doctor(&DoctorArgs {
        spec: Some(fixture.team.join("TEAM.md")),
        workspace: fixture.team.clone(),
        gate: None,
        comms: false,
        team: None,
        fix: false,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
        json: true,
    })
    .expect("doctor should return a JSON report when compatible_api smoke fails");
    let report = match &result.output {
        CmdOutput::Json(value) => value.clone(),
        other => panic!("expected JSON doctor report, got {other:?}"),
    };

    let smoke = report
        .pointer("/profile_smoke")
        .or_else(|| report.pointer("/checks/profile_smoke"))
        .or_else(|| {
            report
                .get("checks")
                .and_then(Value::as_array)
                .and_then(|checks| checks.iter().find(|check| check.get("name").and_then(Value::as_str) == Some("profile_smoke")))
        })
        .unwrap_or_else(|| panic!("doctor must expose compatible_api profile_smoke, not omit the gate; report={report}"));
    assert_eq!(
        result.exit,
        ExitCode::Error,
        "doctor must fail honestly when compatible_api profile smoke fails; report={report}"
    );
    assert_eq!(
        smoke.get("ok").and_then(Value::as_bool),
        Some(false),
        "smoke={smoke} report={report}"
    );
    assert_eq!(
        smoke.get("status").and_then(Value::as_str),
        Some("smoke_failed"),
        "smoke={smoke}"
    );
    assert_eq!(
        smoke.get("http_status").and_then(Value::as_u64),
        Some(401),
        "smoke={smoke}"
    );
    assert!(
        server.was_called(),
        "doctor must perform the same bounded HTTP smoke as preflight; report={report}"
    );
    assert!(
        !report.to_string().contains("doctor-wrong-secret"),
        "doctor smoke diagnostics must redact auth tokens; report={report}"
    );
}

fn preflight_json(team: &Path) -> Value {
    let result = cmd_preflight(&PreflightArgs {
        team: team.to_path_buf(),
        json: true,
    })
    .expect("preflight should return JSON");
    match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON preflight report, got {other:?}"),
    }
}

fn profile_smoke_check(report: &Value) -> &Value {
    report
        .get("checks")
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check.get("name").and_then(Value::as_str) == Some("profile_smoke"))
        })
        .unwrap_or_else(|| {
            panic!("preflight report must include profile_smoke check; report={report}")
        })
}

struct MockLlmServer {
    url: String,
    called: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockLlmServer {
    fn new(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let called_in_thread = called.clone();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(700);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        called_in_thread.store(true, Ordering::SeqCst);
                        let mut buf = [0_u8; 4096];
                        let _ = stream.read(&mut buf);
                        let response = format!(
                            "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        return;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            url: format!("http://{addr}/v1"),
            called,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.url
    }

    fn was_called(&self) -> bool {
        self.called.load(Ordering::SeqCst)
    }
}

impl Drop for MockLlmServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct ProfileFixture {
    root: PathBuf,
    team: PathBuf,
}

impl ProfileFixture {
    fn new(tag: &str, base_url: &str, token: &str) -> Self {
        let root = tmp_dir(tag);
        let team = root.join("team");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::create_dir_all(team.join("profiles")).unwrap();
        std::fs::write(
            team.join("profiles").join("local.env"),
            format!(
                "ANTHROPIC_BASE_URL={base_url}\nANTHROPIC_AUTH_TOKEN={token}\nANTHROPIC_MODEL=mock-claude\n"
            ),
        )
        .unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: profteam\nobjective: Profile smoke contract.\nprovider: claude\nauth_mode: compatible_api\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents").join("clauder.md"),
            "---\nname: clauder\nrole: Claude Worker\nprovider: claude\nauth_mode: compatible_api\nprofile: local\nmodel: null\ntools:\n  - mcp_team\n---\n\nWorker.\n",
        )
        .unwrap();
        Self { root, team }
    }
}

impl Drop for ProfileFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-profile-smoke-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
