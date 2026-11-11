//! Physical send with a persisted legacy session-drift diagnostic.
//!
//! The detector engine is retired. Seed its old diagnostic shape directly and
//! preserve physical delivery, stored-session identity and byte-identical state.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}


#[test]
#[serial_test::serial(env)]
fn observe_marks_codex_session_drift_without_vetoing_physical_send() {
    let _env = hermetic_guard::HermeticTestEnv::enter("drift-direct");
    // A legacy diagnostic is fixture input; no retired detector runs here.
    let fixture = ObserveFixture::new("drift", "codex");
    let mut state = fixture.state();
    state["agents"]["w1"]["session_drift"] = json!({
        "stored_session_id": state["agents"]["w1"]["session_id"].clone(),
        "actual_thread_id": "22222222-2222-4222-8222-222222222222",
        "detected_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        "remediation": "team-agent agent reset w1 --discard-session",
    });
    let drift = state
        .pointer("/agents/w1/session_drift/stored_session_id")
        .or_else(|| state.pointer("/teams/current/agents/w1/session_drift/stored_session_id"))
        .and_then(Value::as_str);
    assert_eq!(
        drift,
        Some("11111111-1111-4111-8111-111111111111"),
        "the session_drift payload must retain stored_session_id for operator diagnosis; state={state}"
    );

    // A drift marker is an observation, never a send veto (original oracle, unchanged).
    let receiver = direct_fixture::DirectFixture::new();
    state["agents"]["w1"]["pane_id"] = json!(receiver.pane);
    state["agents"]["w1"]["tmux_socket"] = json!(receiver.socket);
    // Since M3 retired the observing coordinator, `status` is the lifecycle status (M4 send authority
    // admits only `running`); the drift marker lives in `session_drift`, which must not veto a send.
    state["agents"]["w1"]["status"] = json!("running");
    // 4e8ff838: a bare snapshot is never routing authority for a native leaf
    // (`codex` selects codex_tui), so this raw-snapshot receiver is a legacy,
    // non-native provider row. The persisted drift marker is unchanged.
    state["agents"]["w1"]["provider"] = json!("claude");
    assert!(!team_agent::contract_runtime::registry::recognizes("claude"));
    state["teams"]["current"]["agents"]["w1"] = state["agents"]["w1"].clone();
    std::fs::write(
        team_agent::state::persist::runtime_state_path(&fixture.workspace),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let before = std::fs::read(team_agent::state::persist::runtime_state_path(&fixture.workspace)).unwrap();
    let out = Command::new(bin())
        .args(["send", "--workspace", fixture.workspace.to_str().unwrap(), "--to", "w1", "--json", "hello"])
        .output()
        .expect("run send after drift");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let value = serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null);
    assert!(out.status.success(), "physical send failed: {stdout} {stderr}");
    assert_eq!(value["delivery"], "physical");
    // a68f07e4: an unbound CLI caller is labelled `operator`.
    receiver.received("Team Agent message from operator:\n\nhello");
    // cfd9eb90: physical delivery stamps the recipient's first interaction
    // (`first_send_at`) and the state writer normalizes the document (top-level
    // mirror, explicit nulls). Compare the team entry as msg_sendpath c1 does:
    // the drift marker, session id, pane, socket and status stay unchanged.
    let before: Value = serde_json::from_slice(&before).unwrap();
    let after: Value = serde_json::from_slice(
        &std::fs::read(team_agent::state::persist::runtime_state_path(&fixture.workspace)).unwrap()).unwrap();
    let (observed, mut expected) = (&after["teams"]["current"], before["teams"]["current"].clone());
    let stamp = observed["agents"]["w1"]["first_send_at"].as_str()
        .expect("physical delivery stamps its recipient's first interaction");
    assert!(chrono::DateTime::parse_from_rfc3339(stamp).is_ok(), "{stamp}");
    expected["agents"]["w1"]["first_send_at"] = json!(stamp);
    for (key, value) in observed.as_object().unwrap() {
        match expected.get(key) {
            Some(old) => assert_eq!(old, value, "send must not change {key}"),
            None => assert!(value.is_null() || key == "active_team_key", "send added {key}: {value}"),
        }
    }
    for key in expected.as_object().unwrap().keys() {
        assert!(observed.get(key).is_some(), "send removed {key}");
    }
    let _ = load_runtime_state(&fixture.workspace).expect("state stays readable");
}


struct ObserveFixture {
    workspace: PathBuf,
}

impl ObserveFixture {
    fn new(tag: &str, worker_provider: &str) -> Self {
        let workspace = tmp_dir(tag);
        seed_state(&workspace, worker_provider);
        Self { workspace }
    }

    fn state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("load seeded state")
    }



}

impl Drop for ObserveFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

fn seed_state(workspace: &Path, worker_provider: &str) {
    let worker = json!({
        "provider": worker_provider,
        "status": "running",
        "session_id": "11111111-1111-4111-8111-111111111111",
        "window": "w1",
        "pane_id": "%w1",
        "startup_prompts": "handled"
    });
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": "codex",
        "pane_id": "%leader",
        "owner_epoch": 1
    });
    let owner = json!({"provider": "codex", "pane_id": "%leader", "owner_epoch": 1});
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": "current",
            "session_name": "team-current",
            "leader": {"id": "leader", "provider": "codex"},
            "leader_receiver": receiver,
            "team_owner": owner,
            "agents": {"w1": worker},
            "teams": {"current": {
                "session_name": "team-current",
                "leader_receiver": receiver,
                "team_owner": owner,
                "agents": {"w1": worker}
            }}
        }),
    )
    .expect("seed runtime state");
}



fn tmp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-observe-detectors-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
