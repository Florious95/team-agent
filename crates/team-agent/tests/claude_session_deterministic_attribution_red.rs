//! RED contract for deterministic session attribution.
//!
//! Requirement anchors:
//! - `.team/artifacts/PERF-DESIGN-FINAL-20260730.md` P1-P9.
//! - `ARB-20260730-12` P7b `.team` non-session exclusion.
//!
//! Claude Code workers run only through the contract leaf, so the legacy
//! Claude scanner contracts (P1, P3-P7a, P7c, P9) left with that engine; the
//! provider-neutral contracts below stay.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::provider::session::capture::capture_missing_provider_sessions_once;
use team_agent::provider::{get_adapter, CaptureSessionContext, Provider, SessionId};

const AGENT: &str = "attr_worker";
const TARGET: &str = "22222222-2222-4222-8222-222222222222";
const OLD_TS: &str = "2020-01-01T00:00:00Z";

#[test]
#[serial(env)]
fn p2_expected_codex_session_keeps_the_codex_scanner() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p2");
    let cwd = env.workspace("p2");
    let root = env.root().join("codex-sessions");
    let path = root.join(format!("rollout-2026-07-30T00-00-00-{TARGET}.jsonl"));
    write_codex(&path, TARGET, &cwd);

    let candidates = scan(Provider::Codex, context(&cwd, Some(TARGET), Some(root)));
    assert_eq!(
        candidate_paths(&candidates),
        vec![path],
        "P2_RED_PROVIDER_GATE: Codex+expected must keep the Codex rollout scanner; \
a Claude-only expected-path shortcut would return the wrong shape or no candidate"
    );
}

#[test]
#[serial(env)]
fn p7b_non_provider_session_team_files_stay_excluded_for_all_provider_families() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p7b");
    let cwd = env.workspace("p7b");

    let codex_root = env.root().join("p7b-codex-sessions");
    let codex_valid = codex_root.join(format!("rollout-2026-07-30T00-00-00-{TARGET}.jsonl"));
    write_codex(&codex_valid, TARGET, &cwd);
    let codex_junk = cwd.join(".team/artifacts/codex-junk.jsonl");
    write_codex(&codex_junk, TARGET, &cwd);
    let codex = scan(
        Provider::Codex,
        context(&cwd, Some(TARGET), Some(codex_root)),
    );

    let copilot_junk = cwd.join(".team/runtime/provider-env/copilot-junk.jsonl");
    write_jsonl(
        &copilot_junk,
        &[json!({"id": TARGET, "cwd": cwd.to_string_lossy()})],
    );
    let copilot_home = env.root().join("p7b-copilot-home");
    let copilot_valid = write_copilot_store(&copilot_home, TARGET, &cwd);
    let _copilot_home = env.with_env(
        "COPILOT_HOME",
        copilot_home.to_str().expect("utf8 fixture path"),
    );
    let copilot = scan(Provider::Copilot, context(&cwd, Some(TARGET), None));

    for (provider, valid, junk, candidates) in [
        ("codex", codex_valid, codex_junk, codex),
        ("copilot", copilot_valid, copilot_junk, copilot),
    ] {
        let paths = candidate_paths(&candidates);
        assert_eq!(
            paths,
            vec![valid],
            "P7B_RED_TEAM_SHIELD_REMOVED: {provider} must retain its legal \
provider-session positive control while excluding non-session .team junk; \
narrowing the shield may exempt only an explicit provider session root, never \
runtime/artifact junk={junk:?}; paths={paths:?}"
        );
    }
}

#[test]
#[serial(env)]
fn p8_terminal_capture_state_stops_repeat_scanning() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p8");
    let cwd = env.workspace("p8");
    // The expected session exists, so only the terminal state can stop the scan.
    let copilot_home = env.root().join("p8-copilot-home");
    write_copilot_store(&copilot_home, TARGET, &cwd);
    let _copilot_home = env.with_env(
        "COPILOT_HOME",
        copilot_home.to_str().expect("utf8 fixture path"),
    );
    let mut state = pending_state(&cwd, "attribution_ambiguous");

    let report =
        capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0).unwrap();
    assert!(
        report.assigned.is_empty()
            && !report.candidate_count_by_agent.contains_key(AGENT)
            && state
                .pointer(&format!("/agents/{AGENT}/session_id"))
                .is_none()
            && state.pointer(&format!("/agents/{AGENT}/capture_state"))
                == Some(&json!("attribution_ambiguous")),
        "P8_RED_TERMINAL_STATE_IGNORED: attribution_ambiguous is a terminal \
capture decision and must suppress the next scan; report={report:?} state={state}"
    );
}

fn scan(
    provider: Provider,
    context: CaptureSessionContext,
) -> Vec<team_agent::provider::CapturedSessionCandidate> {
    get_adapter(provider)
        .capture_session_candidates(&context, 0)
        .expect("provider scan")
}

fn context(
    cwd: &Path,
    expected: Option<&str>,
    provider_projects_root: Option<PathBuf>,
) -> CaptureSessionContext {
    CaptureSessionContext {
        agent_id: AGENT.to_string(),
        spawn_cwd: cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: None,
        expected_session_id: expected.map(SessionId::new),
        provider_projects_root,
    }
}

fn candidate_paths(candidates: &[team_agent::provider::CapturedSessionCandidate]) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .captured
                .rollout_path
                .as_ref()
                .map(|path| path.as_path().to_path_buf())
        })
        .collect()
}

fn write_codex(path: &Path, session_id: &str, cwd: &Path) {
    write_jsonl(
        path,
        &[json!({
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "cwd": cwd.to_string_lossy(),
            }
        })],
    );
}

fn write_copilot_store(home: &Path, session_id: &str, cwd: &Path) -> PathBuf {
    std::fs::create_dir_all(home).unwrap();
    let path = home.join("session-store.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "create table sessions (id text primary key, cwd text, updated_at integer)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into sessions (id, cwd, updated_at) values (?1, ?2, 1)",
        rusqlite::params![session_id, cwd.to_string_lossy()],
    )
    .unwrap();
    path
}

fn write_jsonl(path: &Path, records: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let text = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(path, text).unwrap();
}

fn pending_state(cwd: &Path, capture_state: &str) -> Value {
    json!({
        "session_name": "team-attr-red",
        "active_team_key": "attr-red",
        "agents": {
            AGENT: {
                "agent_id": AGENT,
                "owner_team_id": "attr-red",
                "status": "running",
                "provider": "copilot",
                "window": AGENT,
                "pane_id": "%71",
                "spawn_epoch": 1,
                "spawned_at": OLD_TS,
                "first_send_at": OLD_TS,
                "spawn_cwd": cwd.to_string_lossy(),
                "_pending_session_id": TARGET,
                "capture_state": capture_state,
            }
        }
    })
}
