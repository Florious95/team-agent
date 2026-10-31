//! RED contract for deterministic Claude session attribution.
//!
//! Requirement anchors:
//! - `.team/artifacts/PERF-DESIGN-FINAL-20260730.md` P1-P9.
//! - `ARB-20260730-12` replaces the old P7 compatible-api exclusion with
//!   P7a positive capture, P7b `.team` non-session exclusion, and P7c
//!   non-silent failure.
//!
//! Codex workers run only through their contract leaf, so the legacy Codex
//! scanner contracts (P2 and the Codex P7b row) left with that engine.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::provider::session::capture::capture_missing_provider_sessions_once;
use team_agent::provider::{
    get_adapter, CaptureSessionContext, Provider, SessionId,
};

const AGENT: &str = "claude_compat";
const TARGET: &str = "22222222-2222-4222-8222-222222222222";
const OLD_TS: &str = "2020-01-01T00:00:00Z";

#[test]
#[serial(env)]
fn p1_expected_claude_session_is_addressed_without_candidate_enumeration_interference() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p1");
    let cwd = env.workspace("p1");
    let root = env.root().join("provider-projects");
    let target = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&target, TARGET, &cwd, None);
    set_mtime(&target, SystemTime::UNIX_EPOCH + Duration::from_secs(1));

    for index in 0..301 {
        let path = cwd.join(format!("{AGENT}-enumeration-decoy-{index}.jsonl"));
        write_claude_user(
            &path,
            &format!("11111111-1111-4111-8111-{index:012}"),
            &cwd,
            None,
        );
    }

    let candidates = scan(
        Provider::ClaudeCode,
        context(&cwd, Some(TARGET), Some(root)),
    );
    let paths = candidate_paths(&candidates);
    assert_eq!(
        paths,
        vec![target],
        "P1_RED_ENUMERATION_INTERFERENCE: Claude+expected must address exactly \
R/<slug>/<U>.jsonl; a >300 newer decoy pool must not evict or replace U; paths={paths:?}"
    );
}

#[test]
#[serial(env)]
fn p3_claude_without_expected_session_keeps_existing_multi_candidate_behavior() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p3");
    let cwd = env.workspace("p3");
    let root = env.root().join("provider-projects");
    let first = root.join("first.jsonl");
    let second = root.join("second.jsonl");
    write_claude_user(&first, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", &cwd, None);
    write_claude_user(&second, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", &cwd, None);

    let candidates = scan(Provider::ClaudeCode, context(&cwd, None, Some(root)));
    let paths = candidate_paths(&candidates);
    assert!(
        paths.contains(&first) && paths.contains(&second),
        "P3_RED_NONE_BEHAVIOR_DRIFT: expected_session_id=None must retain the \
pre-change scanner and both ambiguous candidates; paths={paths:?}"
    );
}

#[test]
#[serial(env)]
fn p4_expected_path_is_not_double_prefixed_with_claude_projects() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p4");
    let cwd = env.workspace("p4");
    let root = env.root().join("provider-projects");
    let correct = expected_claude_path(&root, &cwd, TARGET);
    let doubled = root
        .join(".claude")
        .join("projects")
        .join(claude_slug(&cwd))
        .join(format!("{TARGET}.jsonl"));
    write_claude_user(&correct, TARGET, &cwd, None);
    write_claude_user(&doubled, TARGET, &cwd, None);

    let candidates = scan(Provider::Claude, context(&cwd, Some(TARGET), Some(root)));
    assert_eq!(
        candidate_paths(&candidates),
        vec![correct],
        "P4_RED_DOUBLE_PREFIX: R already denotes the provider projects root; \
the selected path must be R/<slug>/<U>.jsonl, never R/.claude/projects/<slug>/<U>.jsonl"
    );
}

#[test]
#[serial(env)]
fn p5_expected_claude_path_still_rejects_a_leader_transcript() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p5");
    let cwd = env.workspace("p5");
    let root = env.root().join("provider-projects");
    let path = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&path, TARGET, &cwd, Some("Claude Leader"));

    let candidates = scan(
        Provider::ClaudeCode,
        context(&cwd, Some(TARGET), Some(root)),
    );
    assert!(
        candidates.is_empty(),
        "P5_RED_LEADER_MARKER_BYPASS: exact session-id addressing must not capture \
a transcript carrying the Claude leader marker; candidates={candidates:?}"
    );
}

#[test]
#[serial(env)]
fn p6_expected_claude_path_waits_for_user_or_assistant_lifecycle_record() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p6");
    let cwd = env.workspace("p6");
    let root = env.root().join("provider-projects");
    let path = expected_claude_path(&root, &cwd, TARGET);
    write_jsonl(
        &path,
        &[json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "sessionId": TARGET,
            "cwd": cwd.to_string_lossy(),
        })],
    );

    let candidates = scan(Provider::Claude, context(&cwd, Some(TARGET), Some(root)));
    assert!(
        candidates.is_empty(),
        "P6_RED_PREMATURE_TUPLE: queue-operation-only backing is not a usable \
provider session; capture must stay pending until type=user|assistant; candidates={candidates:?}"
    );
}

#[test]
#[serial(env)]
fn p7a_compatible_api_claude_provider_root_is_capturable_inside_team_runtime() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p7a");
    let cwd = env.workspace("p7a");
    let root = cwd
        .join(".team/runtime/provider-config")
        .join(AGENT)
        .join("claude/projects");
    let target = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&target, TARGET, &cwd, None);

    let candidates = scan(
        Provider::ClaudeCode,
        context(&cwd, Some(TARGET), Some(root)),
    );
    assert_eq!(
        candidate_paths(&candidates),
        vec![target],
        "P7A_RED_COMPATIBLE_ROOT_BLOCKED: a provider session root explicitly \
selected for a compatible_api Claude worker must be capturable even under .team"
    );
}

#[test]
#[serial(env)]
fn p7b_non_provider_session_team_files_stay_excluded_for_all_provider_families() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p7b");
    let cwd = env.workspace("p7b");

    let claude_root = env.root().join("p7b-claude-projects");
    let claude_valid = expected_claude_path(&claude_root, &cwd, TARGET);
    write_claude_user(&claude_valid, TARGET, &cwd, None);
    let claude_junk = cwd.join(".team/runtime/provider-env/claude-junk.jsonl");
    write_claude_user(&claude_junk, TARGET, &cwd, None);
    let claude = scan(Provider::ClaudeCode, context(&cwd, None, Some(claude_root)));

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

    let claude_paths = candidate_paths(&claude);
    assert_eq!(
        claude_paths,
        vec![claude_valid],
        "P7B_RED_TEAM_SHIELD_REMOVED: claude must retain its legal provider-session \
positive control while excluding non-session .team junk={claude_junk:?}; paths={claude_paths:?}"
    );
    let paths = candidate_paths(&copilot);
    assert_eq!(
        paths,
        vec![copilot_valid],
        "P7B_RED_TEAM_SHIELD_REMOVED: copilot must retain its legal \
provider-session positive control while excluding non-session .team junk; \
narrowing the shield may exempt only an explicit provider session root, never \
runtime/artifact junk={copilot_junk:?}; paths={paths:?}"
    );
}

#[test]
#[serial(env)]
fn p8_terminal_capture_state_stops_repeat_scanning() {
    let env = hermetic_guard::HermeticTestEnv::enter("attr-p8");
    let cwd = env.workspace("p8");
    let root = env.root().join("provider-projects");
    let target = expected_claude_path(&root, &cwd, TARGET);
    write_claude_user(&target, TARGET, &cwd, None);
    let mut state = pending_state(&cwd, &root, "attribution_ambiguous");

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

fn claude_slug(cwd: &Path) -> String {
    let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    canonical
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn expected_claude_path(root: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    root.join(claude_slug(cwd))
        .join(format!("{session_id}.jsonl"))
}

fn write_claude_user(path: &Path, session_id: &str, cwd: &Path, title: Option<&str>) {
    let mut records = Vec::new();
    if let Some(title) = title {
        records.push(json!({
            "type": "custom-title",
            "customTitle": title,
            "sessionId": session_id,
        }));
    }
    records.push(json!({
        "type": "user",
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
        "message": {"role": "user", "content": "fixture"},
    }));
    write_jsonl(path, &records);
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

fn set_mtime(path: &Path, when: SystemTime) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(when).unwrap();
}

fn pending_state(cwd: &Path, root: &Path, capture_state: &str) -> Value {
    json!({
        "session_name": "team-attr-red",
        "active_team_key": "attr-red",
        "agents": {
            AGENT: {
                "agent_id": AGENT,
                "owner_team_id": "attr-red",
                "status": "running",
                "provider": "claude_code",
                "auth_mode": "compatible_api",
                "window": AGENT,
                "pane_id": "%71",
                "spawn_epoch": 1,
                "spawned_at": OLD_TS,
                "first_send_at": OLD_TS,
                "spawn_cwd": cwd.to_string_lossy(),
                "_pending_session_id": TARGET,
                "claude_projects_root": root.to_string_lossy(),
                "capture_state": capture_state,
            }
        }
    })
}
