//! unknown-/missing-subcommand byte-lock RED (lane/unknown-cmd).
//!
//! Golden (cli/parser.py:84 `main` -> argparse subparsers, required) rejects an unknown or
//! missing subcommand with an argparse USAGE error to STDERR and exit code 2:
//!   $ team-agent bogus
//!   usage: team-agent [-h]
//!                     {codex,claude,...,doctor} ...
//!   team-agent: error: argument {...}: invalid choice: 'bogus' (choose from codex, claude, ...)
//!   # exit 2, stdout empty
//!   $ team-agent            # no subcommand
//!   ...team-agent: error: the following arguments are required: {...}   # exit 2
//!
//! Golden bytes captured live (PYTHONPATH=.../src python3 -m team_agent bogus).
//!
//! Rust intentionally diverges at one 0.5.45 boundary: naming-addressing design §8 + CR D-o
//! changed unknown-subcommand from argparse Usage (2) to Error (1), while preserving the usage,
//! invalid-choice, and suggestion diagnostics. Do not restore exit 2 here as golden parity.
//! Missing-subcommand remains aligned with parser.py argparse required/Usage (2).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_cwd(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-unknown-cmd-red-{tag}-{}-{}",
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

#[test]
fn top_level_version_prints_current_package_version() {
    let cwd = tmp_cwd("version");
    let out = run(&["--version"], &cwd);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("team-agent {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        out.stderr.is_empty(),
        "--version must be a zero-noise installer-safe probe; stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── unknown subcommand -> argparse-shaped STDERR diagnostic, D-o Error exit 1 ────────────────
#[test]
fn unknown_subcommand_uses_d_o_error_exit1_with_argparse_diagnostic() {
    let cwd = tmp_cwd("bogus");
    let out = run(&["bogus"], &cwd);
    let err = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "0.5.45 naming-addressing design §8 + CR D-o intentionally changed unknown-subcommand \
         from golden parser.py argparse Usage (2) to Error (1); do not restore exit 2 as a \
         golden-parity regression fix. got {:?}",
        out.status.code()
    );
    assert!(
        out.stdout.is_empty(),
        "unknown-command error must go to stderr, NOT stdout; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        err.starts_with("Usage: team-agent"),
        "golden prints an argparse usage block to stderr; Rust is SILENT. got {err:?}"
    );
    assert!(
        err.contains("Error: Unknown command: 'bogus'"),
        "golden: `team-agent: error: argument {{...}}: invalid choice: 'bogus' (choose from ...)`; \
         got {err:?}"
    );
}

// ── missing subcommand (no args) -> argparse `required` error to STDERR, exit 2 (sibling gap) ──
#[test]
fn no_subcommand_is_pure_root_help_exit0() {
    let cwd = tmp_cwd("noargs");
    let out = run(&[], &cwd);
    let help = run(&["--help"], &cwd);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(help.status.code(), Some(0));
    assert_eq!(out.stdout, help.stdout);
    assert!(out.stderr.is_empty() && help.stderr.is_empty());
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
}
