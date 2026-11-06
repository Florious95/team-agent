//! MED A-batch (1/3): data-panel diff contracts — slices A-1 / A-4 / A-5.
//! A-3 retired with the watch command.
//!
//! Triage doc (sole basis): `.team/artifacts/med-triage-fixed-failure-sweep.md`.
//! Python truth source: 0.2.11 (`~/.team-agent/runtime/0.2.11/src/team_agent/`).
//!
//! A-1 collect/coordinator severed wiring     — results.rs:43 `let _ = ensure_coordinator`
//!     + constant `{ok:false,status:"not_required"}` vs Python results.py:157,176+
//!     `_ensure_coordinator_after_collect` (real start_coordinator result when state has
//!     a session_name; the literal not_required ONLY when ensure=False or no session).
//! A-4 status panel severed                    — status_port.rs:66 `latest_results: []`
//!     constant vs Python queries.py:76; :20 `let _ = detail` vs Python commands.py:99
//!     (`--json --detail` => compact=False, i.e. detail returns the FULL payload);
//!     format_status agent branch returns a bool string vs Python queries.py:130-160
//!     (multi-line agent detail; unknown agent raises).
//! A-5 missing leader_receiver = ready false positive — cli/diagnose.rs:395-407
//!     `is_none_or(...)` reports attached=true when leader_receiver is absent; fixed
//!     expectation (user truthfulness rule + triage adjudication): unreadable/missing
//!     receiver must NOT count as attached.
//!
//! Note on two audited A-5 sub-items NOT contracted here (verified equivalent at 23d8a8b):
//! - doctor_comms_json drops `gate`: Python diagnose/comms.py:30 does `del gate` itself.
//! - doctor --fix/--confirm: RS cli/adapters.rs:1489-1502 wires fix/confirm through
//!   orphan_gate_json exactly like Python cli/commands.py:218-236 (incl. "--fix requires
//!   --gate"); the dropped pair in diagnose/mod.rs doctor_gate_blockers is a packaging
//!   seam not on the user doctor path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::state::persist::save_runtime_state;



/// A-4: current public status must render the registered identity and current brief schema;
/// unknown agents must be refused rather than rendered as fabricated detail.
#[test]
fn a4_format_status_agent_branch_returns_agent_detail() {
    let ws = tmp_ws("a4-format");
    seed_status_state(&ws);
    let status = |agent: &str| std::process::Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["status", agent, "--json", "--workspace"])
        .arg(&ws).output().expect("run public status");
    let known = status("w1");
    assert!(known.status.success(), "known-agent status must succeed: {known:?}");
    let payload: Value = serde_json::from_slice(&known.stdout).expect("status JSON");
    let nodes = payload["nodes"].as_array().expect("current status nodes");
    assert_eq!(nodes.len(), 1, "agent filtering must not synthesize or leak nodes: {payload}");
    let node = &nodes[0];
    for field in ["name", "provider", "model", "effort", "runtime_status", "activity", "health", "session_name", "tmux_command"] {
        assert!(node.get(field).is_some(), "current status schema is missing {field}: {node}");
    }
    assert_eq!(node["name"], "w1");
    assert_eq!(node["provider"], "codex");
    assert_eq!(node["model"], "gpt-5.5");
    assert_eq!(node["runtime_status"], "unknown", "no live evidence must not fabricate readiness: {node}");
    let unknown = status("ghost-agent");
    assert!(!unknown.status.success(), "unknown agent must be refused with nonzero exit: {unknown:?}");
}

/// A-4: detail=true must return the FULL payload (Python commands.py:99 maps
/// `--json --detail` to compact=False), so full agent rows survive even when the
/// caller passed compact=true.
#[test]
fn a4_status_detail_returns_full_payload() {
    let ws = tmp_ws("a4-detail");
    seed_status_state(&ws);

    let status =
        team_agent::cli::status_port::status_scoped(&ws, &team_agent::state::persist::load_runtime_state(&ws).expect("fixture runtime state"), None, true, true).expect("status should succeed");
    assert!(
        status
            .pointer("/agents/w1/spawn_cwd")
            .and_then(Value::as_str)
            .is_some(),
        "A-4: detail=true must yield the full (non-compacted) payload — Python \
`status --json --detail` => compact=False keeps raw agent fields like spawn_cwd; \
agents={:?}",
        status.get("agents")
    );
}

/// A-5: a state WITHOUT leader_receiver must not report all_attached_receiver=true.
/// Today cli/diagnose.rs:395-407 `is_none_or(..)` turns "missing/unreadable" into
/// "attached" — a doctor/wait-ready false positive. Fixed expectation per triage:
/// unreadable means NOT attached.
#[test]
fn a5_missing_leader_receiver_must_not_report_attached() {
    let ws = tmp_ws("a5-receiver");
    seed_status_state(&ws); // note: seeds NO leader_receiver key

    let status =
        team_agent::cli::status_port::status_scoped(&ws, &team_agent::state::persist::load_runtime_state(&ws).expect("fixture runtime state"), None, false, false).expect("status should succeed");
    assert_eq!(
        status.get("all_attached_receiver"),
        Some(&json!(false)),
        "A-5: missing leader_receiver must read as NOT attached (truthfulness rule: \
unreadable is never ready); status={status}"
    );
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn seed_status_state(ws: &Path) {
    save_runtime_state(
        ws,
        &json!({
            "session_name": "team-x",
            "active_team_key": "team-x",
            "agents": {
                "w1": {
                    "status": "running",
                    "provider": "codex",
                    "agent_id": "w1",
                    "window": "w1",
                    "model": "gpt-5.5",
                    "profile": null,
                    "session_id": "sess-w1",
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "spawn_cwd": ws.to_string_lossy(),
                },
            },
        }),
    )
    .unwrap();
}


fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-med-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
