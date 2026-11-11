#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use team_agent::cli::{cmd_doctor, CmdOutput, DoctorArgs, ExitCode};

// 262f6cdb (doctor v1) removed the implicit HTTP profile smoke from doctor
// (release note: "doctor no longer runs the HTTP profile smoke"). Both names
// now pin that contract: doctor never contacts a configured compatible_api
// endpoint, never reports a profile verdict it did not measure, and never
// prints a profile secret; its ok/exit come from the six links only.
#[test]
fn doctor_compatible_api_profile_smoke_passes_only_after_real_http_probe() {
    let server = MockLlmServer::new(
        200,
        r#"{"id":"ok","content":[{"type":"text","text":"pong"}]}"#,
    );
    let fixture = ProfileFixture::new("smoke-pass", server.base_url(), "local-secret");

    let (exit, report) = doctor_report(&fixture.team);
    assert_no_profile_smoke(&report, exit);
    drop_and_assert_not_called(server, &report);
    assert!(
        !report.to_string().contains("local-secret"),
        "doctor output must not leak auth tokens; report={report}"
    );
}

#[test]
fn doctor_compatible_api_profile_smoke_401_fails_honestly_with_diagnostics() {
    let server = MockLlmServer::new(
        401,
        r#"{"error":{"message":"bad token should be redacted"}}"#,
    );
    let fixture = ProfileFixture::new("smoke-401", server.base_url(), "wrong-secret");

    let (exit, report) = doctor_report(&fixture.team);
    assert_no_profile_smoke(&report, exit);
    drop_and_assert_not_called(server, &report);
    assert!(
        !report.to_string().contains("wrong-secret") && !report.to_string().contains("bad token"),
        "doctor diagnostics must not leak auth token values; report={report}"
    );
}

fn assert_no_profile_smoke(report: &Value, exit: ExitCode) {
    assert_eq!(report["schema"], "team-agent.doctor.v1", "{report}");
    let rendered = report.to_string();
    assert!(report.get("profile_smoke").is_none() && !rendered.contains("smoke_passed") && !rendered.contains("smoke_failed"),
        "doctor must not report an unmeasured profile smoke verdict: {report}");
    let links = report["links"].as_array().expect("doctor v1 links");
    let all_pass = links.len() == 6 && links.iter().all(|link| link["status"] == "PASS");
    assert_eq!(report["ok"], serde_json::json!(all_pass), "{report}");
    assert_eq!(exit, if all_pass { ExitCode::Ok } else { ExitCode::Error }, "{report}");
}

fn drop_and_assert_not_called(server: MockLlmServer, report: &Value) {
    let called = server.called.clone();
    drop(server); // joins the bounded (700ms) accept loop
    assert!(
        !called.load(Ordering::SeqCst),
        "doctor must not POST to the compatible_api profile endpoint; report={report}"
    );
}

fn doctor_report(team: &Path) -> (ExitCode, Value) {
    let result = cmd_doctor(&DoctorArgs {
        spec: Some(team.join("TEAM.md")),
        workspace: team.to_path_buf(),
        gate: None,
        comms: false,
        team: None,
        fix: false,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
        json: true,
    })
    .expect("doctor should return a JSON report");
    match result.output {
        CmdOutput::Json(value) => (result.exit, value),
        other => panic!("expected JSON doctor report, got {other:?}"),
    }
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
            "---\nname: clauder\nrole: Claude Worker\nprovider: claude\nauth_mode: compatible_api\nprofile: local\nmodel: null\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n",
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
