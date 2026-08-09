use super::*;

use crate::transport::test_support::OfflineTransport;

// Removal contract for the retired abnormal-exit automatic recovery path.
// Abnormal exits remain visible and keep a copyable manual start-agent hint,
// but coordinator ticks must never schedule or execute lifecycle work.

#[test]
fn abnormal_exit_is_reported_without_auto_recovery_intent_or_action() {
    let case = RecoveryCase::new("reported-no-auto");
    case.seed_agent("baseline-429");
    let transport = OfflineTransport::new().with_session_present(true);
    let coord = case.coord(transport.clone());
    coord.tick().expect("baseline tick");

    case.append_api_error("fresh-429");
    coord.tick().expect("fresh abnormal tick");

    let events = case.events();
    let abnormal = find_event(&events, "worker.abnormal_exit").expect("abnormal event");
    assert_eq!(abnormal["agent_id"], serde_json::json!("fe-admin"));
    assert_eq!(abnormal["signature"], serde_json::json!("api_error"));
    assert_eq!(abnormal["apiErrorStatus"], serde_json::json!(429));
    assert!(
        events.iter().all(|event| {
            let name = event
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            !name.starts_with("worker.abnormal_exit.recovery_")
                && !name.starts_with("worker.abnormal_exit.backpressure_")
        }),
        "abnormal detection must not emit automatic recovery/backpressure events; events={events:?}"
    );

    let state = case.state();
    assert!(
        state
            .pointer("/coordinator/abnormal_api_error_recovery")
            .is_none(),
        "abnormal detection must not persist automatic recovery intent; state={state}"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "abnormal detection must not respawn a worker; spawns={:?}",
        transport.spawn_records()
    );

    let content = case.latest_leader_notification();
    assert!(
        content.contains("No automatic restart was performed.")
            && content.contains("To recover manually, run: team-agent start-agent fe-admin")
            && content.contains("--workspace")
            && content.contains("--team research")
            && content.contains("--force")
            && content.contains("--json"),
        "abnormal notification must remain visible and keep the copyable manual recovery command; content={content}"
    );
}

#[test]
fn first_turn_death_cannot_enter_auto_respawn_loop_and_manual_start_still_recovers() {
    let case = RecoveryCase::new("first-turn-death-no-loop");
    case.seed_agent("baseline-429");
    let transport = OfflineTransport::new().with_session_present(true);
    let coord = case.coord(transport.clone());
    coord.tick().expect("baseline tick");

    // A-29 shape: the replacement worker dies before completing its first
    // turn, with a fresh explicit provider error. The following tick reports
    // the death; a persisted due row models the next 6.5-second loop turn.
    case.mark_first_turn_dead();
    case.append_api_error("fresh-first-turn-death");
    coord.tick().expect("fresh first-turn death tick");
    case.seed_legacy_due_recovery();
    coord.tick().expect("tick with legacy due intent");

    let events = case.events();
    let abnormal = find_event(&events, "worker.abnormal_exit").expect("abnormal event");
    assert_eq!(abnormal["dead_process"], serde_json::json!(true));
    assert_eq!(abnormal["fresh_error"], serde_json::json!(true));
    assert!(
        events.iter().all(|event| {
            let name = event
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            !name.starts_with("worker.abnormal_exit.recovery_")
                && !name.starts_with("worker.abnormal_exit.backpressure_")
        }),
        "first-turn death must report only; automatic recovery events make the A-29 loop possible; events={events:?}"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "first-turn death and legacy due intents must never respawn; spawns={:?}",
        transport.spawn_records()
    );
    assert!(
        transport.calls().iter().all(|call| !matches!(
            *call,
            "spawn_first" | "spawn_into" | "kill_pane" | "kill_window"
        )),
        "automatic loop prevention must be structural: no lifecycle transport action; calls={:?}",
        transport.calls()
    );

    case.seed_healthy_coordinator();
    let manual_transport = OfflineTransport::new().with_session_present(true);
    let outcome = crate::lifecycle::start_agent_with_transport(
        &case.root,
        &crate::model::ids::AgentId::new("fe-admin"),
        false,
        false,
        false,
        None,
        &manual_transport,
    );

    assert!(
        matches!(outcome, Ok(crate::lifecycle::StartAgentOutcome::Running { .. })),
        "manual start-agent path must remain functional after an abnormal exit; outcome={outcome:?}"
    );
    assert_eq!(
        manual_transport.spawn_records().len(),
        1,
        "manual start-agent must spawn exactly one replacement worker; spawns={:?}",
        manual_transport.spawn_records()
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
            "team-agent-remove-auto-recovery-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn coord(&self, transport: OfflineTransport) -> Coordinator {
        Coordinator::for_test(
            WorkspacePath::new(self.root.clone()),
            Box::new(MockRegistry::new(&[], &[])),
            Box::new(transport),
            None,
            None,
        )
    }

    fn seed_agent(&self, uuid: &str) {
        let rollout = self.rollout();
        std::fs::write(&rollout, claude_api_error_line(uuid)).unwrap();
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
            "agents": {
                "fe-admin": {
                    "agent_id": "fe-admin",
                    "id": "fe-admin",
                    "provider": "claude_code",
                    "model": "sonnet",
                    "status": "running",
                    "window": "fe-admin",
                    "rollout_path": rollout,
                    "session_id": "session-supermarket",
                    "provider_process_alive": true,
                    "spawn_epoch": 1,
                    "spawned_at": "2026-07-12T10:28:00Z"
                }
            },
            "tasks": []
        });
        crate::state::persist::save_runtime_state(&self.root, &state).unwrap();
    }

    fn seed_legacy_due_recovery(&self) {
        let mut state = self.state();
        state["coordinator"]["abnormal_api_error_recovery"]["agents"]["fe-admin"] = serde_json::json!({
            "error_key": "worker.abnormal_exit.error:fe-admin:rollout:api_error:seed",
            "cohort_key": "research:claude_code:api_error:429:rate_limit",
            "status": "scheduled",
            "attempts": 0,
            "max_attempts": 2,
            "next_retry_at": "2000-01-01T00:00:00Z",
            "manual_command": format!(
                "team-agent start-agent fe-admin --workspace '{}' --team research --force --json",
                self.root.display()
            )
        });
        crate::state::persist::save_runtime_state(&self.root, &state).unwrap();
    }

    fn mark_first_turn_dead(&self) {
        let mut state = self.state();
        let agent = &mut state["agents"]["fe-admin"];
        agent["provider_process_alive"] = serde_json::json!(false);
        agent["process_liveness"] = serde_json::json!("dead");
        agent["last_output_at"] = serde_json::Value::Null;
        agent["first_send_at"] = serde_json::Value::Null;
        crate::state::persist::save_runtime_state(&self.root, &state).unwrap();
    }

    fn seed_healthy_coordinator(&self) {
        let workspace = WorkspacePath::new(self.root.clone());
        let _ = crate::message_store::MessageStore::open(&self.root).unwrap();
        let pid = Pid::new(std::process::id());
        write_coordinator_metadata(&workspace, pid, MetadataSource::Boot).unwrap();
        std::fs::write(coordinator_pid_path(&workspace), pid.to_string()).unwrap();
    }

    fn append_api_error(&self, uuid: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.rollout())
            .unwrap();
        file.write_all(claude_api_error_line(uuid).as_bytes())
            .unwrap();
    }

    fn rollout(&self) -> std::path::PathBuf {
        self.root.join("rollout-fe-admin.jsonl")
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
}

impl Drop for RecoveryCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn claude_api_error_line(uuid: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "parentUuid": format!("parent-{uuid}"),
            "uuid": uuid,
            "requestId": format!("req-{uuid}"),
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "API Error: 429 rate_limit"}
            ]},
            "error": "rate_limit",
            "isApiErrorMessage": true,
            "apiErrorStatus": 429,
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
