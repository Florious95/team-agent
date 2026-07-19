//! P0 RED contract: external diagnostic projections never expose credential values.
//!
//! Source: `.team/artifacts/status-credential-redact-locate.md` §8 RED1-RED7.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::{cmd_preflight, emit, CmdOutput, PreflightArgs};
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::save_runtime_state;
#[cfg(unix)]
use team_agent::tmux_backend::TmuxBackend;
#[cfg(unix)]
use team_agent::transport::{InjectPayload, Key, PaneId, Target, Transport};

const REDACTED: &str = "[REDACTED]";

#[test]
#[serial(env)]
fn red1_external_redaction_has_one_shared_module_and_two_crate_entry_points() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let redaction = fs::read_to_string(root.join("src/redaction.rs")).unwrap_or_default();
    let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let crate_entries = redaction
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub(crate) fn "))
        .filter_map(|line| line.split('(').next())
        .collect::<Vec<_>>();
    let marker = synthetic_marker();
    let env = hermetic_guard::HermeticTestEnv::enter("redact-red1");
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let log = EventLog::new(&env.workspace("red1"));
    let written = log
        .write("redaction.contract", mixed_sensitive_value(&marker))
        .unwrap();
    let tail = log.tail(0).unwrap();
    let text = Value::Array(tail.clone()).to_string();
    let idempotent = tail.first() == Some(&written);

    assert!(
        lib.lines().any(|line| line.trim() == "mod redaction;")
            && crate_entries == ["redact_external_value", "redact_external_text"]
            && !text.contains(&marker)
            && text.contains(REDACTED)
            && text.contains("https://[REDACTED]@proxy.invalid:8443/path")
            && idempotent,
        "RED1: one internal module with exactly two crate entry points must recursively mask mixed values and remain idempotent; no third public crate scrubber is allowed"
    );
}

#[test]
#[serial(env)]
fn red2_false_positive_guard_preserves_team_provider_and_model_truth() {
    let marker = synthetic_marker();
    let (env, workspace) = redaction_case("red2", &marker);
    let status = team_agent::cli::status_port::status(&workspace, false, true)
        .expect("detail status must remain available");
    let text = status.to_string();
    let diagnostics = status
        .pointer("/teams/current/diagnostics")
        .expect("raw selected-team diagnostics remain present");
    let structural_fields_preserved = [
        ("/team_key", json!("current")),
        ("/auth_mode", json!("subscription")),
        ("/provider", json!("codex")),
        ("/model", json!("gpt-contract")),
        ("/provider_source", json!("role")),
        ("/model_source", json!("role")),
        ("/model_stale", json!(true)),
        ("/session_id", json!("session-contract")),
        ("/pane_id", json!("%contract")),
        ("/monkey", json!("banana")),
        ("/author", json!("operator")),
        ("/normal_url", json!("https://proxy.invalid/health")),
        ("/already_redacted", json!(REDACTED)),
    ]
    .iter()
    .all(|(pointer, expected)| diagnostics.pointer(pointer) == Some(expected));
    let marker_absent = !text.contains(&marker);
    let replacement_present = text.contains(REDACTED);
    let url_structure_preserved = text.contains("https://[REDACTED]@proxy.invalid:8443/path");
    assert!(
        marker_absent
            && replacement_present
            && structural_fields_preserved
            && url_structure_preserved,
        "RED2: recursive masking must remove the synthetic marker while preserving structural provider/model/identity fields and URL host/path; marker_absent={marker_absent} replacement_present={replacement_present} structural_fields_preserved={structural_fields_preserved} url_structure_preserved={url_structure_preserved}"
    );
    drop(env);
}

#[test]
#[serial(env)]
fn red3_full_and_compact_status_never_expose_restart_credentials() {
    let marker = synthetic_marker();
    let (env, workspace) = redaction_case("red3", &marker);
    let full = team_agent::cli::status_port::status(&workspace, false, true)
        .expect("full status must remain available");
    let compact = team_agent::cli::status_port::status(&workspace, true, false)
        .expect("compact status must remain available");
    let full_text = full.to_string();
    let compact_text = compact.to_string();
    let full_safe = !full_text.contains(&marker) && full_text.contains(REDACTED);
    let compact_safe = !compact_text.contains(&marker);
    let diagnostics_preserved = [
        "/agents/worker/restart_error",
        "/teams/current/agents/worker/restart_error",
    ]
    .iter()
    .all(|pointer| {
        full.pointer(pointer)
            .and_then(Value::as_str)
            .is_some_and(|error| {
                error.contains("subprocess exited 37")
                    && error.contains("HTTPS_PROXY")
                    && error.contains("proxy.invalid:8443/path")
                    && error.contains(REDACTED)
            })
    });
    assert!(
        full_safe && compact_safe && diagnostics_preserved,
        "RED3: full/compact status must mask credentials and full detail must retain restart classification/key/host/path; full_safe={full_safe} compact_safe={compact_safe} diagnostics_preserved={diagnostics_preserved}"
    );
    drop(env);
}

#[test]
#[serial(env)]
fn red4_jsonrpc_and_legacy_mcp_status_never_expose_raw_team_credentials() {
    let marker = synthetic_marker();
    let (env, workspace) = redaction_case("red4", &marker);
    let frames = run_mcp_status_frames(&env, &workspace);
    assert_eq!(
        frames.len(),
        2,
        "RED4: real MCP stdio must answer JSON-RPC and legacy requests"
    );

    let outer = frames[0].to_string();
    let content_text = frames[0]
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("JSON-RPC tools/call content text");
    let body: Value = serde_json::from_str(content_text).expect("decoded MCP status body");
    let outer_safe = !outer.contains(&marker);
    let content_safe = !content_text.contains(&marker);
    let body_preserved = body
        .pointer("/teams/current/agents/worker/restart_error")
        .and_then(Value::as_str)
        .is_some_and(|error| error.contains(REDACTED));
    let legacy = frames[1].to_string();
    let legacy_safe = !legacy.contains(&marker);
    let legacy_preserved = frames[1]
        .pointer("/teams/current/agents/worker/restart_error")
        .and_then(Value::as_str)
        .is_some_and(|error| error.contains(REDACTED));
    assert!(
        outer_safe && content_safe && body_preserved && legacy_safe && legacy_preserved,
        "RED4: JSON-RPC outer/text and legacy frames must all mask the synthetic marker while preserving selected raw-team diagnostics; outer_safe={outer_safe} content_safe={content_safe} body_preserved={body_preserved} legacy_safe={legacy_safe} legacy_preserved={legacy_preserved}"
    );
    drop(env);
}

#[test]
#[serial(env)]
fn red5_event_write_and_legacy_tail_are_redacted_and_idempotent() {
    let marker = synthetic_marker();
    let env = hermetic_guard::HermeticTestEnv::enter("redact-red5");
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace("red5");
    let log = EventLog::new(&workspace);
    let diagnostic = restart_diagnostic(&marker);
    let written = [
        log.write(
            "provider.worker.spawn_argv",
            json!({
                "authorization_header": format!("Bearer {marker}"),
                "credential_blob": marker.clone(),
                "config": format!("mcp_servers.demo.env.OPENAI_API_KEY=\"{marker}\""),
                "argv": ["provider", "--api-key", marker.clone()],
            }),
        )
        .unwrap(),
        log.write(
            "provider.worker.spawn_argv",
            json!({"argv": ["provider", diagnostic.clone()]}),
        )
        .unwrap(),
        log.write(
            "start_agent.agent_start",
            json!({"command": ["provider", diagnostic.clone()]}),
        )
        .unwrap(),
        log.write(
            "restart.agent_failed",
            json!({"agent_id": "worker", "error": diagnostic.clone()}),
        )
        .unwrap(),
        log.write(
            "restart.completed",
            json!({"failed_agents": [{"agent_id": "worker", "error": diagnostic}]}),
        )
        .unwrap(),
    ];
    let path = workspace.join(".team/logs/events.jsonl");
    let new_bytes = fs::read_to_string(&path).unwrap();
    let new_bytes_safe = !new_bytes.contains(&marker) && new_bytes.contains(REDACTED);

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        json!({"event": "legacy.restart", "error": restart_diagnostic(&marker)})
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "event": "provider.session.converging",
            "required_missing": ["worker"],
            "pending": ["worker"]
        })
    )
    .unwrap();
    writeln!(file, "legacy malformed {}", restart_diagnostic(&marker)).unwrap();
    let tail = log.tail(0).unwrap();
    let tail_text = Value::Array(tail.clone()).to_string();
    let tail_safe = !tail_text.contains(&marker) && tail_text.contains(REDACTED);
    let double_boundary_idempotent = tail.first() == written.first();
    let legacy_aliases_readable = tail.iter().any(|event| {
        event.get("event").and_then(Value::as_str) == Some("provider.session.converging")
            && event.get("required_missing") == Some(&json!(["worker"]))
            && event.get("pending") == Some(&json!(["worker"]))
    });

    assert!(
        new_bytes_safe && tail_safe && double_boundary_idempotent && legacy_aliases_readable,
        "RED5: EventLog write bytes and legacy/malformed tail output must be masked; applying redaction at write then tail must be idempotent"
    );
}

#[test]
fn eventlog_producers_share_the_central_writer_and_canonical_schema() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let launch = fs::read_to_string(root.join("src/lifecycle/launch.rs")).unwrap();
    let common = fs::read_to_string(root.join("src/lifecycle/restart/common.rs")).unwrap();
    let rebuild = fs::read_to_string(root.join("src/lifecycle/restart/rebuild.rs")).unwrap();
    let helper_calls = [&launch, &common, &rebuild]
        .iter()
        .map(|source| source.matches("provider_worker_spawn_argv_fields(").count())
        .sum::<usize>();
    let convergence_writer = source_section(
        &common,
        "fn write_session_convergence_progress_event(",
        "pub(crate) fn restart_required_missing_session_agent_ids(",
    );
    let resume_writer = source_section(
        &rebuild,
        "fn write_restart_resume_decision_event(",
        "pub fn restart_candidates(",
    );

    assert_eq!(
        helper_calls, 3,
        "launch/restart/fake must share one spawn schema helper"
    );
    assert!(!common.contains("\"required_missing\": progress"));
    assert!(!common.contains("\"pending\": pending_agent_ids"));
    for writer in [convergence_writer, resume_writer] {
        assert!(writer.contains("EventLog::new"));
        assert!(!writer.contains("OpenOptions"));
        assert!(!writer.contains("write_all"));
        assert!(!writer.contains("events.jsonl"));
    }
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("source section start");
    let rest = &source[start..];
    &rest[..rest.find(end).expect("source section end")]
}

#[test]
#[serial(env)]
fn red6_cli_restart_reports_and_top_level_error_logs_are_redacted() {
    let marker = synthetic_marker();
    let diagnostic = restart_diagnostic(&marker);
    let mut regular_reports_safe = true;
    for status in ["partial", "failed"] {
        let output = CmdOutput::Json(json!({
            "ok": false,
            "status": status,
            "reason": "restart_failed",
            "failed_agents": [{"agent_id": "worker", "error": diagnostic.clone()}],
            "action": "inspect the failed spawn and retry"
        }));
        for as_json in [true, false] {
            let rendered = emit(&output, as_json).expect("restart report renders");
            regular_reports_safe &= !rendered.contains(&marker)
                && rendered.contains("restart_failed")
                && rendered.contains("worker")
                && rendered.contains(REDACTED);
        }
    }

    let env = hermetic_guard::HermeticTestEnv::enter("redact-red6");
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace("red6");
    let spec = write_invalid_validate_spec(&workspace, &marker);
    let mut combined = String::new();
    for json_mode in [true, false] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command.arg("validate").arg(&spec);
        if json_mode {
            command.arg("--json");
        }
        let output = command
            .current_dir(&workspace)
            .env("HOME", env.home())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .expect("run top-level CLI error boundary");
        assert!(
            !output.status.success(),
            "invalid spec must reach CLI error"
        );
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let logs = read_cli_error_logs(&workspace);
    let output_safe = !combined.contains(&marker) && combined.contains(REDACTED);
    let logs_safe = !logs.contains(&marker) && logs.contains(REDACTED);
    assert!(
        regular_reports_safe && output_safe && logs_safe,
        "RED6: regular JSON/human reports plus top-level stdout/stderr and physical error logs must mask before emission; regular_reports_safe={regular_reports_safe} output_safe={output_safe} logs_safe={logs_safe} output_had_marker={} logs_had_marker={}",
        combined.contains(&marker),
        logs.contains(&marker)
    );
}

#[test]
#[serial(env)]
fn red7_pane_and_profile_scrubbers_converge_without_losing_existing_policy() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tmux = fs::read_to_string(root.join("src/tmux_backend.rs")).unwrap();
    let profile = fs::read_to_string(root.join("src/lifecycle/profile_smoke.rs")).unwrap();
    let delegates_to_shared =
        tmux.contains("redact_external_text") && profile.contains("redact_external_text");

    let marker = synthetic_marker();
    let profile_token = format!("synthetic-profile-token-{}", std::process::id());
    let tail_sentinel = "synthetic-truncation-tail";
    let body = json!({"error": {"message": format!(
        "HTTPS_PROXY='{}' explicit={} {}{}",
        credential_url(&marker),
        profile_token,
        "x".repeat(600),
        tail_sentinel
    )}})
    .to_string();
    let server = MockLlmServer::new(401, body);
    let env = hermetic_guard::HermeticTestEnv::enter("redact-red7");
    let pane_policy_preserved = pane_excerpt_uses_shared_redaction(&env, &marker);
    let team = write_profile_fixture(env.root(), server.base_url(), &profile_token);
    let result = cmd_preflight(&PreflightArgs { team, json: true })
        .expect("profile smoke returns a diagnostic report");
    let report = match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON preflight output, got {other:?}"),
    };
    let text = report.to_string();
    let profile_policy_preserved = !text.contains(&marker)
        && !text.contains(&profile_token)
        && !text.contains(tail_sentinel)
        && text.contains(REDACTED)
        && text.contains("proxy.invalid:8443/path")
        && server.was_called();

    assert!(
        delegates_to_shared && pane_policy_preserved && profile_policy_preserved,
        "RED7: pane/profile scrubbers must delegate generic env/URL masking to the shared module while retaining pane token/hex policy and profile explicit-token/truncation policy"
    );
}

fn redaction_case(tag: &str, marker: &str) -> (hermetic_guard::HermeticTestEnv, PathBuf) {
    let env = hermetic_guard::HermeticTestEnv::enter(tag);
    env.scrub_tmux();
    env.assert_no_real_tmux();
    let workspace = env.workspace(tag);
    seed_redaction_state(&workspace, marker);
    (env, workspace)
}

fn seed_redaction_state(workspace: &Path, marker: &str) {
    let diagnostic = restart_diagnostic(marker);
    let mixed = mixed_sensitive_value(marker);
    let agent = json!({
        "agent_id": "worker",
        "provider": "codex",
        "model": "gpt-contract",
        "provider_source": "role",
        "model_source": "role",
        "model_stale": true,
        "auth_mode": "subscription",
        "status": "failed",
        "restart_error": diagnostic,
        "session_id": "session-contract",
        "pane_id": "%contract",
    });
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": "current",
            "session_name": "team-redaction-contract",
            "leader": {"id": "leader"},
            "agents": {"worker": agent.clone()},
            "teams": {
                "current": {
                    "agents": {"worker": agent},
                    "diagnostics": mixed,
                    "tasks": []
                }
            },
            "tasks": []
        }),
    )
    .unwrap();
    let _ = MessageStore::open(workspace).unwrap();
}

fn mixed_sensitive_value(marker: &str) -> Value {
    let diagnostic = restart_diagnostic(marker);
    json!({
        "HTTPS_PROXY": marker,
        "http_proxy": marker,
        "Copilot_Github_Token": marker,
        "OPENAI_API_KEY": marker,
        "db_password": marker,
        "client_auth": marker,
        "client_secret": marker,
        "service_credential": marker,
        "ordinary_diagnostic": format!(
            "HTTPS_PROXY='{}' http_proxy=\"{}\" OPENAI_API_KEY={} endpoint={}",
            credential_url(marker), credential_url(marker), marker, credential_url(marker)
        ),
        "nested": [{"values": [diagnostic.clone(), credential_url(marker)]}],
        "team_key": "current",
        "auth_mode": "subscription",
        "provider": "codex",
        "model": "gpt-contract",
        "provider_source": "role",
        "model_source": "role",
        "model_stale": true,
        "session_id": "session-contract",
        "pane_id": "%contract",
        "monkey": "banana",
        "author": "operator",
        "normal_url": "https://proxy.invalid/health",
        "already_redacted": REDACTED,
    })
}

fn synthetic_marker() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "synthetic-redact-marker-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn credential_url(marker: &str) -> String {
    format!("https://demo-user:{marker}@proxy.invalid:8443/path")
}

fn restart_diagnostic(marker: &str) -> String {
    format!(
        "subprocess exited 37: argv=[tmux, new-window, HTTPS_PROXY='{}']; endpoint={}; stderr=synthetic transport failure",
        credential_url(marker),
        credential_url(marker)
    )
}

fn run_mcp_status_frames(env: &hermetic_guard::HermeticTestEnv, workspace: &Path) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["mcp-server", "--workspace"])
        .arg(workspace)
        .current_dir(workspace)
        .env("HOME", env.home())
        .env("TEAM_AGENT_ID", "worker")
        .env("TEAM_AGENT_OWNER_TEAM_ID", "current")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real MCP stdio server");
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "get_team_status", "arguments": {}}
    });
    let legacy = json!({"tool": "get_team_status", "arguments": {}});
    {
        let stdin = child.stdin.as_mut().expect("MCP stdin");
        writeln!(stdin, "{rpc}").unwrap();
        writeln!(stdin, "{legacy}").unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for MCP EOF");
    assert!(output.status.success(), "MCP stdio exits cleanly at EOF");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("MCP response JSON"))
        .collect()
}

fn write_invalid_validate_spec(workspace: &Path, marker: &str) -> PathBuf {
    let path = workspace.join("invalid-provider.spec.yaml");
    fs::write(
        &path,
        format!(
            "version: 1\nteam:\n  id: current\n  name: current\n  session_name: team-redaction-contract\n  workspace: \"{}\"\nleader:\n  provider: \"{}\"\nagents:\n  - id: worker\n    provider: fake\n    model: fake\n    role: Worker\n    window: worker\ntasks: []\n",
            workspace.display(),
            credential_url(marker)
        ),
    )
    .unwrap();
    path
}

fn read_cli_error_logs(workspace: &Path) -> String {
    let dir = workspace.join(".team/logs");
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("cli-error-")
        })
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
fn pane_excerpt_uses_shared_redaction(env: &hermetic_guard::HermeticTestEnv, marker: &str) -> bool {
    let workspace = env.workspace("red7-pane");
    let shim_dir = env.root().join("pane-bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("tmux");
    let submitted = env.root().join("pane-submitted");
    fs::write(
        &shim,
        r#"#!/bin/sh
case " $* " in
  *" capture-pane "*)
    if [ -f "$TEAM_AGENT_TEST_PANE_SUBMITTED" ]; then
      printf '%s\n' "$TEAM_AGENT_TEST_PANE_AFTER"
    else
      printf '%s\n' "$TEAM_AGENT_TEST_PANE_BEFORE"
    fi
    exit 0
    ;;
  *" send-keys "*)
    : > "$TEAM_AGENT_TEST_PANE_SUBMITTED"
    exit 0
    ;;
  *) exit 0 ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shim, permissions).unwrap();
    let real_path = std::env::var_os("PATH").unwrap_or_default();
    let token = "[team-agent-token:synthetic-redact-pane]";
    let diagnostic = restart_diagnostic(marker);
    let before = format!("message {token}\n{diagnostic}");
    let after = format!(
        "{diagnostic}\n{}{}\n{}{}\nBearer {}.part\nhex {}\nplain pane diagnostic",
        "sk-",
        marker,
        "ghp_",
        marker,
        marker,
        "a".repeat(40)
    );
    let _guards = [
        env.with_env(
            "PATH",
            &format!("{}:{}", shim_dir.display(), real_path.to_string_lossy()),
        ),
        env.with_env(
            "TEAM_AGENT_TEST_PANE_SUBMITTED",
            submitted.to_str().unwrap(),
        ),
        env.with_env("TEAM_AGENT_TEST_PANE_BEFORE", &before),
        env.with_env("TEAM_AGENT_TEST_PANE_AFTER", &after),
    ];
    let backend = TmuxBackend::for_workspace(&workspace);
    let report = backend
        .inject(
            &Target::Pane(PaneId::new("%redact-contract")),
            &InjectPayload::Text(format!("message {token}")),
            Key::Enter,
            false,
        )
        .expect("scripted pane injection reaches submit diagnostics");
    let excerpts = report
        .submit_diagnostics
        .into_iter()
        .flat_map(|diagnostics| diagnostics.attempts_detail)
        .map(|observation| observation.pane_tail_excerpt)
        .collect::<Vec<_>>()
        .join("\n");
    !excerpts.contains(marker)
        && excerpts.contains(REDACTED)
        && excerpts.contains("REDACTED_HEX")
        && excerpts.contains("plain pane diagnostic")
}

#[cfg(not(unix))]
fn pane_excerpt_uses_shared_redaction(
    _env: &hermetic_guard::HermeticTestEnv,
    _marker: &str,
) -> bool {
    true
}

fn write_profile_fixture(root: &Path, base_url: &str, token: &str) -> PathBuf {
    let team = root.join("profile-team");
    fs::create_dir_all(team.join("agents")).unwrap();
    fs::create_dir_all(team.join("profiles")).unwrap();
    fs::write(
        team.join("profiles/local.env"),
        format!(
            "ANTHROPIC_BASE_URL={base_url}\nANTHROPIC_AUTH_TOKEN={token}\nANTHROPIC_MODEL=mock-model\n"
        ),
    )
    .unwrap();
    fs::write(
        team.join("TEAM.md"),
        "---\nname: redact-profile\nobjective: Synthetic redaction profile contract.\nprovider: claude\nauth_mode: compatible_api\n---\n\nTeam.\n",
    )
    .unwrap();
    fs::write(
        team.join("agents/worker.md"),
        "---\nname: worker\nrole: Worker\nprovider: claude\nauth_mode: compatible_api\nprofile: local\nmodel: null\ntools:\n  - mcp_team\n---\n\nWorker.\n",
    )
    .unwrap();
    team
}

struct MockLlmServer {
    url: String,
    called: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockLlmServer {
    fn new(status: u16, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let called_in_thread = called.clone();
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        called_in_thread.store(true, Ordering::SeqCst);
                        let mut buffer = [0_u8; 4096];
                        let _ = stream.read(&mut buffer);
                        let response = format!(
                            "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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
