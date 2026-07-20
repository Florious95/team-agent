//! Batch 9 F8 RED: quick-start's conpty path must persist workers into state.
//!
//! Real-machine finding on batch8-final:
//!
//! > send returns `target_not_in_team` because the Batch 8 F7 seed-state
//! > pattern in launch.rs (writes state.active_team_key + transport.kind=conpty
//! > BEFORE start_coordinator) makes state.json look like an "existing runtime"
//! > to the downstream launch code, which then skips spec compile + worker
//! > spawn.
//!
//! Structural contracts (both platforms, source-code shape):
//!
//! - `lifecycle/launch.rs` must NOT seed `active_team_key` into state before
//!   `start_coordinator`. If a seed is needed for the coord to pick up the
//!   team_key, it must arrive via the coord's CLI (`--team`) argument.
//! - `start_coordinator` must accept and forward a `--team` arg to the
//!   spawned coord daemon.
//! - The coord daemon (`parse_args` for the `coordinator` subcommand) must
//!   accept `--team` and use it for the factory's `team_key` input, not
//!   fall back to `state.active_team_key` derivation (that fell into F8's
//!   trap).
//!
//! The tests are cfg-free (compile on Unix + Windows). Real-machine
//! verification of the six-check green picture lives in the batch report.

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

fn read_src(rel: &str) -> String {
    composite_source::composite_source(&format!("src/{rel}"))
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn launch_no_longer_seeds_active_team_key_before_coord() {
    // Batch 9 F8 lock: the Batch 8 fix2 seed-state pattern in
    // `lifecycle/launch.rs` must be gone. Writing `active_team_key`
    // into state.json before `start_coordinator` triggered the F8
    // "existing runtime, use restart" branch downstream.
    let launch = read_src("lifecycle/launch.rs");
    let code = strip_comments(&launch);
    assert!(
        !code.contains("obj.insert(\n                    \"active_team_key\".to_string(),"),
        "Batch 9 F8 lock: lifecycle/launch.rs must NOT seed \
         `active_team_key` before start_coordinator. Pass team_key \
         to the coordinator via --team CLI instead."
    );
}

#[test]
fn start_coordinator_forwards_team_arg() {
    // Batch 9 F8 lock: `start_coordinator` in `coordinator/health.rs`
    // must accept a team_key parameter and forward it as `--team`
    // to the spawned coord daemon. Without this the daemon still
    // has to derive team_key from state.active_team_key (F8 root
    // cause).
    let health = read_src("coordinator/health.rs");
    let code = strip_comments(&health);
    assert!(
        code.contains("start_coordinator_with_team")
            || (code.contains("start_coordinator") && code.contains("\"--team\"")),
        "Batch 9 F8 lock: coordinator::health must expose a variant \
         of start_coordinator that forwards --team to the daemon \
         command line."
    );
}

#[test]
fn coord_daemon_parses_team_arg() {
    // Batch 9 F8 lock: the `coordinator` subcommand handler must
    // parse a `--team` argument and thread it into `DaemonArgs`
    // (or an equivalent). Otherwise the daemon still has to derive
    // team_key from state at boot time.
    let emit = read_src("cli/emit.rs");
    let code = strip_comments(&emit);
    // Look for the coordinator dispatch that recognizes --team.
    assert!(
        code.contains("run_coordinator") && code.contains("\"--team\""),
        "Batch 9 F8 lock: run_coordinator in cli/emit.rs must \
         recognize a `--team` CLI arg."
    );
}
