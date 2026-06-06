//! Installer doctor self-check must be bounded under Node spawnSync stdout capture.
//!
//! User-visible failure: `npx @team-agent/installer install` prints the install lines and then never
//! exits. The installer runs `team-agent doctor --json` with Node `spawnSync`, so it waits for the
//! doctor process to exit and for the captured stdout pipe to close.
//!
//! This command-level contract keeps the reproducer local and subscription-free: a clean cwd contains
//! a FIFO, and `doctor --json` must not recursively open arbitrary workspace files in a way that can
//! block the installer.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_cwd(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-doctor-spawnsync-red-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp cwd");
    std::fs::canonicalize(dir).expect("canonical temp cwd")
}

#[test]
fn doctor_json_spawn_sync_self_check_does_not_hang_on_clean_cwd_with_fifo() {
    let cwd = tmp_cwd("fifo");
    let home = tmp_cwd("home");
    let fifo = cwd.join("installer-doctor-must-not-block-on-this-fifo");
    make_fifo(&fifo);

    let script = format!(
        r#"
const {{ spawnSync }} = require('node:child_process');
const result = spawnSync({bin:?}, ['doctor', '--json'], {{
  cwd: {cwd:?},
  env: {{
    PATH: '/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin',
    HOME: {home:?},
    TMPDIR: {tmp:?},
    LANG: 'C'
  }},
  encoding: 'utf8',
  timeout: 900
}});
process.stdout.write(JSON.stringify({{
  status: result.status,
  signal: result.signal,
  error: result.error ? String(result.error) : null,
  stdout_len: (result.stdout || '').length,
  stderr: (result.stderr || '').slice(0, 200)
}}));
"#,
        bin = bin(),
        cwd = cwd.to_string_lossy(),
        home = home.to_string_lossy(),
        tmp = std::env::temp_dir().to_string_lossy(),
    );

    let output = Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .expect("run node spawnSync reproducer");
    assert!(
        output.status.success(),
        "node reproducer itself must run; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stdout);

    assert!(
        !report.contains("ETIMEDOUT"),
        "installer self-check contract: `team-agent doctor --json` must not hang under Node \
         spawnSync stdout capture in a clean cwd. Current doctor recursively scans cwd and blocks \
         opening FIFO-like files, so install.mjs can wait forever. spawnSync report: {report}"
    );
    assert!(
        report.contains(r#""status":0"#) && report.contains(r#""stdout_len":"#),
        "doctor should exit 0 and emit JSON for installer to parse; spawnSync report: {report}"
    );
}

fn make_fifo(path: &Path) {
    let output = Command::new("mkfifo")
        .arg(path)
        .output()
        .expect("run mkfifo");
    assert!(
        output.status.success(),
        "mkfifo failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
