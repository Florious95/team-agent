#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const VALID: &str = r#"{"schema_version":"result_envelope_v1","task_id":"t1","agent_id":"a1","status":"success","summary":"done","artifacts":[],"changes":[],"tests":[],"risks":[],"next_actions":[]}"#;
const INVALID: &str = r#"{"schema_version":"result_envelope_v1","task_id":"t1"}"#;

const VALID_JSON_STDOUT: &str = r#"{
  "agent_id": "a1",
  "ok": true,
  "status": "success",
  "task_id": "t1"
}
"#;

const VALID_HUMAN_STDOUT: &str = "ok: True\ntask_id: t1\nagent_id: a1\nstatus: success\n";

const INVALID_ERROR: &str = "result_envelope_v1 validation failed:\\n- /agent_id: missing required field\\n- /artifacts: missing required field\\n- /changes: missing required field\\n- /next_actions: missing required field\\n- /risks: missing required field\\n- /status: missing required field\\n- /summary: missing required field\\n- /tests: missing required field\\n- /status: invalid result status";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_ws(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-validate-result-red-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn run_stdin(args: &[&str], cwd: &Path, stdin_text: &str) -> Output {
    let mut child = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_text.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn assert_success_json(output: &Output) {
    assert!(
        output.status.success(),
        "validate-result valid input must exit 0; stderr={}",
        stderr(output)
    );
    assert_eq!(stdout(output), VALID_JSON_STDOUT);
    assert_eq!(stderr(output), "");
}

fn assert_invalid_json_error(output: &Output, ws: &Path) {
    assert!(!output.status.success(), "invalid envelope must exit 1");
    assert_eq!(stderr(output), "");
    let out = stdout(output);
    assert!(
        out.starts_with(&format!(
            "{{\"ok\": false, \"error\": \"{INVALID_ERROR}\", \"action\": \"run `team-agent doctor` or inspect the log path shown here\", \"log\": \""
        )),
        "invalid --json must emit Python compact error envelope; got {out:?}"
    );
    assert!(
        out.contains(&format!("{}/.team/logs/cli-error-", ws.display())),
        "error envelope must point inside workspace .team/logs: {out:?}"
    );
    assert!(out.ends_with(".log\"}\n"), "error envelope suffix mismatch: {out:?}");
}

#[test]
fn validate_result_accepts_positional_file_and_stdin_valid_envelopes() {
    let ws = tmp_ws("valid");
    let file = ws.join("valid.json");
    std::fs::write(&file, VALID).unwrap();

    assert_success_json(&run(&["validate-result", VALID, "--json"], &ws));
    assert_success_json(&run(
        &["validate-result", "--file", file.to_str().unwrap(), "--json"],
        &ws,
    ));
    assert_success_json(&run_stdin(&["validate-result", "--json"], &ws, VALID));

    let human = run(&["validate-result", VALID], &ws);
    assert!(human.status.success(), "human valid path must exit 0");
    assert_eq!(stdout(&human), VALID_HUMAN_STDOUT);
    assert_eq!(stderr(&human), "");
}

#[test]
fn validate_result_invalid_envelope_errors_are_byte_locked_for_all_input_surfaces() {
    let ws = tmp_ws("invalid");
    let file = ws.join("invalid.json");
    std::fs::write(&file, INVALID).unwrap();

    assert_invalid_json_error(&run(&["validate-result", INVALID, "--json"], &ws), &ws);
    assert_invalid_json_error(
        &run(&["validate-result", "--file", file.to_str().unwrap(), "--json"], &ws),
        &ws,
    );
    assert_invalid_json_error(&run_stdin(&["validate-result", "--json"], &ws, INVALID), &ws);
}

#[test]
fn validate_result_has_no_result_flag_in_python_golden() {
    let ws = tmp_ws("result-flag");
    let output = run(&["validate-result", "--result", VALID, "--json"], &ws);
    assert_eq!(
        output.status.code(),
        Some(2),
        "Python argparse rejects --result; Rust must not accept a non-golden flag"
    );
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).contains("team-agent: error: unrecognized arguments: --result"),
        "argparse error text must reject --result; got {:?}",
        stderr(&output)
    );
}
