//! Swallow-family batch 3: CLI/diagnose exit-layer fake-greens.
//!
//! Basis: `.team/artifacts/swallow-family-slices.md` batch 3 (6 items; one family
//! contract with the three adjudicated probes folded together, per the "族级断言,
//! 不逐条铺用例" rule). Python parity: cmd_doctor raises on an unknown gate
//! (cli/commands.py:234-235 `unknown doctor gate`), send failures carry a reason
//! field, and an unreadable state must never read as "ready".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use team_agent::messaging::send::{send_message, MessageTarget, SendOptions};

/// Batch 3 family contract: the three exit-layer fake-greens.
/// ① `doctor --gate bogus` must refuse with `unknown_gate` + ok=false (today the
///    unknown gate silently falls through to the default doctor — empty green).
/// ② an empty `--to` target list must fail WITH `reason="empty_target_list"`
///    (today: failed with no reason).
/// ③ The canonical state reader must reject unreadable state.json, not substitute
///    an empty ready state.
#[test]
fn b3_exit_layer_failures_are_explained_not_fake_green() {
    let mut failures = Vec::new();

    // ① unknown doctor gate
    let ws = tmp_ws("b3-doctor");
    let doctor = run_cli(&ws, &["doctor", "--gate", "bogus", "--json"]);
    if !doctor.contains("unknown_gate")
        || doctor.contains("\"ok\": true")
        || doctor.contains("\"ok\":true")
    {
        failures.push(format!(
            "①: `doctor --gate bogus` must refuse with status unknown_gate + ok=false \
(Python cli/commands.py:234-235 raises `unknown doctor gate`); got {doctor}"
        ));
    }

    // ② empty --to list
    let ws = tmp_ws("b3-send");
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    let outcome = send_message(
        &ws,
        &MessageTarget::Fanout(Vec::new()),
        "batch3 probe",
        &SendOptions::default(),
    );
    match outcome {
        Ok(out) => {
            let rendered = format!("{out:?}");
            if !rendered.contains("empty_target_list") {
                failures.push(format!(
                    "②: an empty target list must fail with reason `empty_target_list` \
(Python send error reason style); got {rendered}"
                ));
            }
        }
        Err(error) => {
            if !error.to_string().contains("empty_target_list") {
                failures.push(format!(
                    "②: the empty-target failure must name `empty_target_list`; got Err({error})"
                ));
            }
        }
    }

    // ③ canonical reader over unreadable state
    let ws = tmp_ws("b3-unreadable-state");
    let state_path = ws.join(".team/runtime/state.json");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, "{ definitely not json").unwrap();
    if team_agent::state::persist::load_runtime_state(&ws).is_ok() {
        failures.push("③: unreadable state must fail instead of becoming an empty ready state".to_string());
    }

    assert!(
        failures.is_empty(),
        "batch3 exit-layer observability contract failed:\n{}",
        failures.join("\n")
    );
}

fn run_cli(ws: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(args)
        .args(["--workspace", &ws.to_string_lossy()])
        .current_dir(ws)
        .output()
        .expect("run team-agent");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-swallow-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
