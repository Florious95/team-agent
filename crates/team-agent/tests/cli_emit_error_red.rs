//! emit_cli_error byte-lock (lane/emit-cli-human).
//!
//! Golden `_emit_cli_error` (cli/helpers.py:119-134) on a handler exception:
//!   - writes traceback to `<ws>/.team/logs/cli-error-<int(time.time())>.log`
//!   - payload = {ok:false, error:str(exc), action:<CONST>, log:<path>} (+tmux enrich)
//!   - if --json:  print(json.dumps(payload, ensure_ascii=False)) -> COMPACT json to STDOUT, exit 1
//!   - else:       3 lines `error:`/`action:`/`log:` to STDERR (NOT stdout), exit 1
//!
//! Golden bytes captured live (PYTHONPATH=.../src python3 -m team_agent remove-agent ghost [--json]):
//!   human STDERR: "error: <e>\naction: run `team-agent doctor` or inspect the log path shown here\nlog: <p>\n"
//!   json  STDOUT: {"ok": false, "error": "<e>", "action": "run `team-agent doctor`...", "log": "<p>"}\n
//!   both: exit 1, the OTHER stream empty. Log filename: golden cli-error-<int epoch>.log, but we
//!   LOCK the Rust microsecond cli-error-<date>-<time>.<micros>.log as a ✦ §10 divergence (collision-safe).
//!
//! Trigger here: `validate-result notjson` -> CliError::Json -> emit_cli_error (single-line error).
//! Rust impl: cli/emit.rs:191-212 (routing already matches golden) + cli/emit.rs:226-232 (log path).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const ACTION: &str = "run `team-agent doctor` or inspect the log path shown here";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_ws(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-emit-error-red-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin()).args(args).current_dir(cwd).output().unwrap()
}

/// filename stem between `cli-error-` and `.log` of a `log:`/`"log"` path value.
fn log_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_prefix("cli-error-")
        .and_then(|s| s.strip_suffix(".log"))
        .unwrap_or("")
        .to_string()
}

// ── human face: 3 lines to STDERR (stdout empty), exit 1, exact action line ──────────────
#[test]
fn emit_cli_error_human_face_is_stderr_three_lines_exit1() {
    let ws = tmp_ws("human");
    let out = run(&["validate-result", "notjson", "--workspace", ws.to_str().unwrap()], &ws);

    assert_eq!(out.status.code(), Some(1), "golden raises SystemExit(1) (helpers/parser.py:499)");
    assert!(
        out.stdout.is_empty(),
        "human-mode error must NOT print to stdout; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8(out.stderr).unwrap();
    let lines: Vec<&str> = err.strip_suffix('\n').unwrap_or(&err).split('\n').collect();
    assert_eq!(lines.len(), 3, "golden helpers.py:132-134 prints exactly 3 stderr lines; got {lines:?}");
    assert!(lines[0].starts_with("error: "), "line 1 is `error: <msg>`; got {:?}", lines[0]);
    assert_eq!(
        lines[1],
        format!("action: {ACTION}"),
        "line 2 is the EXACT golden action constant (helpers.py:142); got {:?}",
        lines[1]
    );
    assert!(lines[2].starts_with("log: "), "line 3 is `log: <path>`; got {:?}", lines[2]);
}

// ── json face: compact json to STDOUT (stderr empty), exit 1, key order ok,error,action,log ──
#[test]
fn emit_cli_error_json_face_is_stdout_compact_exit1() {
    let ws = tmp_ws("json");
    let out = run(&["validate-result", "notjson", "--workspace", ws.to_str().unwrap(), "--json"], &ws);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stderr.is_empty(),
        "json-mode error must NOT print to stderr; got {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let so = String::from_utf8(out.stdout).unwrap();
    let line = so.strip_suffix('\n').unwrap_or(&so);
    assert!(!line.contains('\n'), "golden json.dumps(no indent) is a SINGLE compact line; got {so:?}");
    // golden key insertion order: ok, error, action, log (NO sort_keys on the error path)
    let v: serde_json::Value = serde_json::from_str(line).expect("compact json parses");
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["ok", "error", "action", "log"],
        "golden helpers.py:139-143 insertion order ok,error,action,log; got {keys:?}"
    );
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["action"], serde_json::json!(ACTION));
    // compact spacing: `, ` between members, `: ` after key (Python json.dumps default)
    assert!(line.starts_with(r#"{"ok": false, "error": "#), "compact `, `/`: ` spacing; got {line}");
}

// ── ✦ INTENTIONAL DIVERGENCE (§10, leader-ruled msg_e5c4af306b50): KEEP the Rust microsecond
//    log filename. Golden helpers.py:126 names it `cli-error-{int(time.time())}.log` (second-resolution
//    unix epoch) which COLLIDES + overwrites when two errors land in the same second. Rust emit.rs:227
//    uses chrono `%Y%m%d-%H%M%S%.6f` -> `cli-error-<YYYYMMDD>-<HHMMSS>.<micros>.log` (microsecond-unique,
//    collision-safe). Rust-superior: we LOCK the Rust shape, NOT golden's. ─────────────────────────────
#[test]
fn emit_cli_error_log_filename_is_microsecond_stem_divergence() {
    let ws = tmp_ws("logname");
    let out = run(&["validate-result", "notjson", "--workspace", ws.to_str().unwrap(), "--json"], &ws);
    let so = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(so.trim_end()).expect("json");
    let log = v["log"].as_str().expect("log field");
    let stem = log_stem(log);
    // ✦ Rust microsecond stem: <8-digit date>-<6-digit time>.<6-digit micros> (collision-safe).
    let micros_stem = stem
        .split_once('.')
        .and_then(|(left, micros)| left.split_once('-').map(|(d, t)| (d, t, micros)))
        .is_some_and(|(date, time, micros)| {
            date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit())
                && time.len() == 6 && time.bytes().all(|b| b.is_ascii_digit())
                && micros.len() == 6 && micros.bytes().all(|b| b.is_ascii_digit())
        });
    assert!(
        micros_stem,
        "✦ §10 divergence: log filename stem must be the collision-safe Rust microsecond form \
         <8date>-<6time>.<6micros> (emit.rs:227), NOT golden's second-resolution int epoch \
         (helpers.py:126, which overwrites same-second errors); got {stem:?} (full log: {log})"
    );
}
