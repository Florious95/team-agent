use super::*;

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

// 0.5.36 supermarket api_error recovery contracts.
//
// User-facing promise: a fresh retryable provider api_error either schedules a
// bounded, visible worker recovery or loudly explains why not. Recovery must not
// run inside the abnormal detector's pre-save window, must not treat a live-pid
// Noop as success, and must never synthesize a hidden provider turn.

#[test]
fn r1_retryable_429_schedules_recovery_and_copyable_command() {
    let case = RecoveryCase::new("r1-retryable");
    case.seed_agents(&[("fe-admin", 429, "rate_limit", "baseline-429")]);
    let coord = case.coord();
    coord.tick().expect("baseline tick");

    case.append_api_error("fe-admin", 429, "rate_limit", "fresh-429");
    coord.tick().expect("fresh retryable tick");

    let events = case.events();
    let abnormal = find_event(&events, "worker.abnormal_exit").expect("abnormal event");
    assert_eq!(abnormal["agent_id"], serde_json::json!("fe-admin"));
    let scheduled = find_event(&events, "worker.abnormal_exit.recovery_scheduled")
        .expect("R1: retryable api_error/429 must emit recovery_scheduled");
    assert_eq!(scheduled["agent_id"], serde_json::json!("fe-admin"));
    assert_eq!(scheduled["apiErrorStatus"], serde_json::json!(429));
    assert_eq!(scheduled["error"], serde_json::json!("rate_limit"));
    assert_eq!(scheduled["attempt"], serde_json::json!(0));
    assert!(
        scheduled
            .get("due_at")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "R1: recovery_scheduled must include a concrete due_at; event={scheduled}"
    );

    let state = case.state();
    let intent = state
        .pointer("/coordinator/abnormal_api_error_recovery/agents/fe-admin")
        .expect("R1: state must persist recovery intent");
    assert_eq!(intent["status"], serde_json::json!("scheduled"));
    assert_eq!(intent["attempts"], serde_json::json!(0));
    assert!(
        intent
            .get("next_retry_at")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "R1: intent must carry next_retry_at; intent={intent}"
    );

    let content = case.latest_leader_notification();
    assert!(
        content.contains("team-agent start-agent fe-admin")
            && content.contains("--workspace")
            && content.contains("--team research")
            && content.contains("--force")
            && content.contains("--json"),
        "R1: leader notification must include one copyable forced start-agent command; content={content}"
    );
    assert!(
        !content.contains("No automatic restart was performed."),
        "R1: old unconditional no-auto-restart text must be replaced; content={content}"
    );
}

#[test]
fn r2_non_retryable_api_error_guides_only_without_intent() {
    let case = RecoveryCase::new("r2-nonretry");
    case.seed_agents(&[("fe-admin", 404, "model_not_found", "baseline-404")]);
    let coord = case.coord();
    coord.tick().expect("baseline tick");

    case.append_api_error("fe-admin", 404, "model_not_found", "fresh-404");
    coord.tick().expect("fresh non-retryable tick");

    let state = case.state();
    assert!(
        state
            .pointer("/coordinator/abnormal_api_error_recovery/agents/fe-admin")
            .is_none(),
        "R2: non-retryable model errors must not schedule auto recovery; state={state}"
    );
    let content = case.latest_leader_notification();
    assert!(
        content.contains("non_retryable_api_error")
            && content.contains("model_not_found")
            && content.contains("team-agent start-agent fe-admin")
            && content.contains("--force"),
        "R2: notification must be guide-only with model/config context and manual command; content={content}"
    );
}

#[test]
fn r3_live_pid_noop_is_never_counted_as_recovery_success() {
    let abnormal = source("src/coordinator/steps/abnormal.rs");
    let lifecycle = source("src/lifecycle/restart/agent.rs");
    let recovery_mentions_noop_block = abnormal.contains("noop_not_recovery")
        && abnormal.contains("worker.abnormal_exit.recovery_blocked");
    let lifecycle_has_stop_then_start_mode =
        abnormal.contains("stop_then_start") || lifecycle.contains("stop_before_start");
    assert!(
        recovery_mentions_noop_block || lifecycle_has_stop_then_start_mode,
        "R3: api_error recovery must either stop-before-start live panes or record recovery_blocked(reason=noop_not_recovery); abnormal.rs/lifecycle.rs missing both guards"
    );
}

#[test]
fn r4_short_window_429_cohort_enters_backpressure_with_one_canary() {
    let case = RecoveryCase::new("r4-backpressure");
    let agents = [
        ("architect", 429, "rate_limit", "baseline-a"),
        ("fe-admin", 429, "rate_limit", "baseline-b"),
        ("fe-staff", 429, "rate_limit", "baseline-c"),
        ("fe-shop", 429, "rate_limit", "baseline-d"),
    ];
    case.seed_agents(&agents);
    let coord = case.coord();
    coord.tick().expect("baseline tick");
    for (agent, _, _, _) in agents {
        case.append_api_error(agent, 429, "rate_limit", &format!("fresh-{agent}"));
    }
    coord.tick().expect("fresh cohort tick");

    let events = case.events();
    let backpressure = find_event(&events, "worker.abnormal_exit.backpressure_started")
        .expect("R4: four same-team/provider 429s inside 120s must start backpressure");
    assert_eq!(backpressure["team_id"], serde_json::json!("research"));
    assert_eq!(backpressure["provider"], serde_json::json!("claude_code"));
    assert_eq!(backpressure["apiErrorStatus"], serde_json::json!(429));
    assert!(
        backpressure
            .get("cooldown_until")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "R4: backpressure event must include shared cooldown_until; event={backpressure}"
    );

    let scheduled_count = events
        .iter()
        .filter(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("worker.abnormal_exit.recovery_scheduled")
        })
        .count();
    assert!(
        scheduled_count <= 1,
        "R4: at most one canary recovery may be scheduled while cohort is backpressured; scheduled_count={scheduled_count}"
    );
    let state = case.state();
    let bp = state
        .pointer("/coordinator/abnormal_api_error_recovery/backpressure")
        .and_then(serde_json::Value::as_object)
        .expect("R4: backpressure state must be persisted");
    assert_eq!(
        bp.len(),
        1,
        "R4: cohort must share one backpressure record; state={state}"
    );
}

#[test]
fn r5_retry_budget_exhausts_and_stays_loud() {
    let abnormal = source("src/coordinator/steps/abnormal.rs");
    for needle in [
        "max_attempts",
        "worker.abnormal_exit.recovery_exhausted",
        "manual_command",
        "attempts",
    ] {
        assert!(
            abnormal.contains(needle),
            "R5: retry budget contract requires `{needle}` in abnormal api_error recovery state/events"
        );
    }
}

#[test]
fn r6_abnormal_detector_does_not_mutate_lifecycle_inside_pre_save_window() {
    let body = detect_abnormal_body();
    for forbidden in [
        "start_agent_at_paths",
        "start_agent(",
        "stop_agent_at_paths",
        "reset_agent",
    ] {
        assert!(
            !body.contains(forbidden),
            "R6: detect_abnormal_exits is pre-save detection only and must not call `{forbidden}`"
        );
    }
}

#[test]
fn r6_recovery_fields_survive_the_next_tick_after_atomic_save() {
    let case = RecoveryCase::new("r6-save-boundary");
    case.seed_agents(&[("fe-admin", 429, "rate_limit", "baseline-429")]);
    case.seed_due_recovery("fe-admin");
    let coord = case.coord();
    coord.tick().expect("recovery tick");

    let events = case.events();
    assert!(
        find_event(&events, "worker.abnormal_exit.recovery_started").is_some()
            || find_event(&events, "worker.abnormal_exit.recovery_blocked").is_some(),
        "R6: due recovery must run after atomic_save and emit started or blocked; events={events:?}"
    );
    let state = case.state();
    let recovery = state
        .pointer("/coordinator/abnormal_api_error_recovery/agents/fe-admin")
        .expect("R6: recovery entry must survive tick");
    assert!(
        recovery.get("last_attempt_at").is_some()
            || recovery.get("last_error").is_some()
            || recovery.get("blocked_reason").is_some(),
        "R6: lifecycle-written recovery result fields must survive next tick save; recovery={recovery}"
    );
}

#[test]
fn r7_recovery_never_synthesizes_hidden_provider_turn_or_sdk_call() {
    let case = RecoveryCase::new("r7-no-hidden-turn");
    case.seed_agents(&[("fe-admin", 429, "rate_limit", "baseline-429")]);
    case.seed_due_recovery("fe-admin");
    let registry = CountingRegistry::new();
    let counters = registry.counters();
    let transport = MockTransport::new(true);
    let calls = std::sync::Arc::clone(&transport.calls);
    let coord = Coordinator::for_test(
        WorkspacePath::new(case.root.clone()),
        Box::new(registry),
        Box::new(transport),
        None,
        None,
    );

    coord.tick().expect("recovery tick");

    assert_eq!(
        counters.adapter_calls.load(Ordering::SeqCst),
        0,
        "R7: recovery must not call provider adapter/client APIs"
    );
    assert!(
        !calls.lock().unwrap().iter().any(|call| *call == "inject"),
        "R7: recovery must not inject a synthetic worker turn; transport calls={:?}",
        calls.lock().unwrap()
    );
    assert_eq!(
        case.non_leader_message_count(),
        0,
        "R7: recovery may notify leader, but must not create hidden worker prompt messages"
    );
}

struct RecoveryCase {
    root: std::path::PathBuf,
}

impl RecoveryCase {
    fn new(tag: &str) -> Self {
        let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join(format!(
            "team-agent-0536-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn coord(&self) -> Coordinator {
        Coordinator::for_test(
            WorkspacePath::new(self.root.clone()),
            Box::new(MockRegistry::new(&[], &[])),
            Box::new(MockTransport::new(true)),
            None,
            None,
        )
    }

    fn seed_agents(&self, agents: &[(&str, i64, &str, &str)]) {
        let mut agent_map = serde_json::Map::new();
        for (agent, status, error, uuid) in agents {
            let rollout = self.rollout(agent);
            std::fs::write(&rollout, claude_api_error_line(uuid, *status, error)).unwrap();
            agent_map.insert(
                (*agent).to_string(),
                serde_json::json!({
                    "agent_id": agent,
                    "id": agent,
                    "provider": "claude_code",
                    "model": "sonnet",
                    "status": "running",
                    "window": agent,
                    "rollout_path": rollout,
                    "provider_process_alive": true,
                    "spawn_epoch": 1,
                    "spawned_at": "2026-07-12T10:28:00Z"
                }),
            );
        }
        let state = serde_json::json!({
            "active_team_key": "research",
            "team_key": "research",
            "workspace": self.root,
            "session_name": "team-research",
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "pane_id": "%leader",
                "provider": "codex"
            },
            "agents": serde_json::Value::Object(agent_map),
            "tasks": []
        });
        crate::state::persist::save_runtime_state(&self.root, &state).unwrap();
    }

    fn seed_due_recovery(&self, agent: &str) {
        let mut state = self.state();
        state["coordinator"]["abnormal_api_error_recovery"]["agents"][agent] = serde_json::json!({
            "error_key": format!("worker.abnormal_exit.error:{agent}:rollout:api_error:seed"),
            "cohort_key": "research:claude_code:api_error:429:rate_limit",
            "status": "scheduled",
            "attempts": 0,
            "max_attempts": 2,
            "next_retry_at": "2000-01-01T00:00:00Z",
            "last_attempt_at": null,
            "last_error": null,
            "manual_command": format!("team-agent start-agent {agent} --workspace '{}' --team research --force --json", self.root.display())
        });
        crate::state::persist::save_runtime_state(&self.root, &state).unwrap();
    }

    fn append_api_error(&self, agent: &str, status: i64, error: &str, uuid: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.rollout(agent))
            .unwrap();
        file.write_all(claude_api_error_line(uuid, status, error).as_bytes())
            .unwrap();
    }

    fn rollout(&self, agent: &str) -> std::path::PathBuf {
        self.root.join(format!("rollout-{agent}.jsonl"))
    }

    fn state(&self) -> serde_json::Value {
        crate::state::persist::load_runtime_state(&self.root).unwrap()
    }

    fn events(&self) -> Vec<serde_json::Value> {
        read_event_log_dir(&self.root)
    }

    fn latest_leader_notification(&self) -> String {
        let db = crate::model::paths::runtime_dir(&self.root).join("team.db");
        let conn = crate::db::schema::open_db(&db).unwrap();
        conn.query_row(
            "select content from messages where recipient = 'leader' order by created_at desc limit 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default()
    }

    fn non_leader_message_count(&self) -> i64 {
        let db = crate::model::paths::runtime_dir(&self.root).join("team.db");
        let conn = crate::db::schema::open_db(&db).unwrap();
        conn.query_row(
            "select count(*) from messages where recipient != 'leader'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    }
}

impl Drop for RecoveryCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[derive(Clone)]
struct CounterState {
    adapter_calls: Arc<AtomicU32>,
    error_list_calls: Arc<AtomicU32>,
}

struct CountingRegistry {
    counters: CounterState,
}

impl CountingRegistry {
    fn new() -> Self {
        Self {
            counters: CounterState {
                adapter_calls: Arc::new(AtomicU32::new(0)),
                error_list_calls: Arc::new(AtomicU32::new(0)),
            },
        }
    }

    fn counters(&self) -> CounterState {
        self.counters.clone()
    }
}

impl ProviderRegistry for CountingRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        self.counters.adapter_calls.fetch_add(1, Ordering::SeqCst);
        crate::provider::get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        self.counters
            .error_list_calls
            .fetch_add(1, Ordering::SeqCst);
        ErrorLists::default()
    }
}

fn claude_api_error_line(uuid: &str, status: i64, error: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "parentUuid": format!("parent-{uuid}"),
            "uuid": uuid,
            "requestId": format!("req-{uuid}"),
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": format!("API Error: {status} {error}")}
            ]},
            "error": error,
            "isApiErrorMessage": true,
            "apiErrorStatus": status,
            "sessionId": "session-supermarket",
            "version": "2.1.181"
        })
    )
}

fn find_event<'a>(events: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    events
        .iter()
        .rev()
        .find(|event| event.get("event").and_then(serde_json::Value::as_str) == Some(name))
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .expect("read source")
}

fn detect_abnormal_body() -> String {
    let source = source("src/coordinator/steps/abnormal.rs");
    let start = source
        .find("pub(crate) fn detect_abnormal_exits")
        .expect("detect_abnormal_exits exists");
    let end = source[start..]
        .find("#[derive")
        .map(|offset| start + offset)
        .expect("derive after detect_abnormal_exits");
    source[start..end].to_string()
}
