//! Read-only runtime observation detectors (compaction, Codex session drift, leader API error).
//!
//! Re-homed from `tick_detectors_red.rs` by the M3 excision. The resident coordinator tick that
//! used to collect captures is retired (Final Synthesis §4 duty 13: detection becomes
//! query-time). The detectors themselves remain: these contracts call the retained
//! `team_agent::coordinator::observe` entry point directly, with the same captures the tick used
//! to collect, and keep every original oracle that does not depend on the tick.
//!
//! Dropped with the tick (no longer meaningful): "the tick persists the detector result into
//! state.json" and "the detector never asks the tick's ProviderRegistry for an adapter".
//! `observe` has no registry parameter, so it cannot ask for one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::coordinator::{observe, CapturedRuntimeFact, LeaderCaptureFact};
use team_agent::event_log::EventLog;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::provider::Provider;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{PaneId, SessionName, WindowName};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
fn observe_reports_compaction_from_real_worker_capture_not_empty_placeholder() {
    let worker_capture = "\
        previous output\n\
        /compact\n\
        Context compacted. Compaction completed; continue from the summary.\n\
        ❯\n";
    let fixture = ObserveFixture::new("compaction", "fake");
    let mut state = fixture.state();
    let captures = fixture.worker_fact(Provider::Fake, worker_capture, &state);
    let results = observe(&fixture.workspace, &mut state, captures, None);
    let events = fixture.events();

    assert!(
        !results.compaction.is_empty(),
        "observe().compaction must be populated from the captured worker pane when compaction \
         markers are visible; it must not fabricate an empty vec. results={results:?} events={events:?}"
    );
    assert!(
        events.iter().any(|event| {
            event_name(event).is_some_and(|name| {
                name == "coordinator.compaction_observed"
                    || name == "coordinator.compaction_threshold"
                    || name.contains("compaction")
            })
        }),
        "observe must audit the observed compaction/threshold event from pane capture; events={events:?}"
    );
}

#[test]
#[serial_test::serial(env)]
fn observe_marks_codex_session_drift_without_vetoing_physical_send() {
    let _env = hermetic_guard::HermeticTestEnv::enter("drift-direct");
    let worker_capture =
        "Codex resumed. Switched to thread 22222222-2222-4222-8222-222222222222\n❯\n";
    // The session-drift detector reads only Codex rows (its Codex TUI thread marker), so this
    // pre-upgrade worker row stays `codex`.
    let fixture = ObserveFixture::new("drift", "codex");
    let mut state = fixture.state();
    let captures = fixture.worker_fact(Provider::Codex, worker_capture, &state);
    let results = observe(&fixture.workspace, &mut state, captures, None);

    assert!(
        !results.session_drift.is_empty(),
        "observe().session_drift must include the codex worker whose captured thread id differs \
         from stored session_id; results={results:?} state={state}"
    );
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
    state["agents"]["w1"]["status"] = json!("session_drift");
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
    receiver.received("Team Agent message from leader:\n\nhello");
    assert_eq!(std::fs::read(team_agent::state::persist::runtime_state_path(&fixture.workspace)).unwrap(), before);
    let _ = load_runtime_state(&fixture.workspace).expect("state stays readable");
}

#[test]
fn observe_records_leader_api_error_once_per_fingerprint_from_leader_capture() {
    let leader_capture = "\
        Working...\n\
        API Error: Overloaded\n\
        529 too many requests, please retry later\n";
    let fixture = ObserveFixture::new("api-error", "fake");
    let mut state = fixture.state();

    let (captures, leader) = (fixture.worker_fact(Provider::Fake, "❯\n", &state), fixture.leader_fact(leader_capture, &state));
    let first = observe(&fixture.workspace, &mut state, captures, Some(leader));
    let events_after_first = fixture.events();
    assert!(
        !first.api_errors.is_empty(),
        "observe().api_errors must contain the provider-neutral leader API error fact from the \
         leader pane capture; results={first:?} events={events_after_first:?}"
    );
    let first_count = leader_api_error_count(&events_after_first);
    assert_eq!(
        first_count, 1,
        "the first observation must emit exactly one leader.api_error for the captured overloaded \
         fingerprint; events={events_after_first:?}"
    );

    let (captures, leader) = (fixture.worker_fact(Provider::Fake, "❯\n", &state), fixture.leader_fact(leader_capture, &state));
    let second = observe(&fixture.workspace, &mut state, captures, Some(leader));
    let events_after_second = fixture.events();
    assert_eq!(
        leader_api_error_count(&events_after_second),
        first_count,
        "the same leader API error fingerprint must be deduped on the next observation; \
         results={second:?} events={events_after_second:?}"
    );
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

    /// The capture fact the retired tick built for worker `w1` from its pane.
    fn worker_fact(&self, provider: Provider, capture: &str, state: &Value) -> BTreeMap<AgentId, CapturedRuntimeFact> {
        let row = state.pointer("/agents/w1").cloned().unwrap_or(Value::Null);
        let fact = CapturedRuntimeFact {
            team_key: Some(TeamKey::new("current")),
            agent_id: AgentId::new("w1"),
            provider: Some(provider),
            session_name: Some(SessionName::new("team-current")),
            window: Some(WindowName::new("w1")),
            pane_id: Some(PaneId::new("%w1")),
            scrollback_tail: capture.to_string(),
            pane_info: None,
            agent_state_snapshot: row.clone(),
            stored_session_id: row.get("session_id").and_then(Value::as_str).map(str::to_string),
            last_output_at: None,
            rollout_path: None,
            process_liveness: None,
        };
        BTreeMap::from([(AgentId::new("w1"), fact)])
    }

    /// The capture fact the retired tick built for the attached leader pane.
    fn leader_fact(&self, capture: &str, state: &Value) -> LeaderCaptureFact {
        LeaderCaptureFact {
            team_key: Some(TeamKey::new("current")),
            leader_receiver: state.get("leader_receiver").cloned(),
            pane_id: Some(PaneId::new("%leader")),
            scrollback_tail: capture.to_string(),
        }
    }

    fn events(&self) -> Vec<Value> {
        EventLog::new(&self.workspace).tail(100).expect("read events")
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

fn event_name(event: &Value) -> Option<&str> {
    event.get("event").and_then(Value::as_str)
}

fn leader_api_error_count(events: &[Value]) -> usize {
    events.iter().filter(|event| event_name(event) == Some("leader.api_error")).count()
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
