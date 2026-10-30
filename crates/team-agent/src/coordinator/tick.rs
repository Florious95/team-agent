//! Retired coordinator tests pending the dedicated test-excision PR.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod u1_tests {
    use super::*;
    use std::io::Write as _;

    struct CaptureFailureRegistry;

    impl ProviderRegistry for CaptureFailureRegistry {
        fn adapter_for(
            &self,
            provider: crate::provider::Provider,
        ) -> Box<dyn crate::provider::ProviderAdapter> {
            Box::new(
                crate::session_capture::test_support::CaptureCandidatesAdapter::new(
                    provider,
                    Some("w1"),
                    "capture exploded",
                ),
            )
        }
    }

    #[test]
    fn tick_logs_capture_missing_failure_and_continues() {
        let dir = std::env::temp_dir().join(format!(
            "team-agent-u1-capture-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        crate::state::persist::save_runtime_state(
            &dir,
            &serde_json::json!({
                "agents": {
                    "w1": {
                        "provider": "codex",
                        "status": "running",
                        "spawn_cwd": dir.to_string_lossy()
                    }
                }
            }),
        )
        .unwrap();
        let coordinator = Coordinator::for_test(
            WorkspacePath::new(dir.clone()),
            Box::new(CaptureFailureRegistry),
            Box::new(crate::transport::test_support::OfflineTransport::new()),
            None,
            None,
        );

        let report = coordinator
            .tick()
            .expect("capture_missing failure must be logged and not abort the tick");

        assert!(report.ok, "tick should continue to a successful report");
        let events_path = crate::model::paths::logs_dir(&dir).join("events.jsonl");
        let events = std::fs::read_to_string(events_path).unwrap();
        let has_capture_failure = events.lines().any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|event| {
                    event
                        .get("event")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .as_deref()
                == Some("coordinator.tick.capture_missing_failed")
        });
        assert!(
            has_capture_failure,
            "capture_missing failure must be visible in events.jsonl; got {events}"
        );
    }

    #[test]
    fn daemon_team_key_projection_beats_stale_root_active_team_key() {
        let dir = std::env::temp_dir().join(format!(
            "team-agent-daemon-team-key-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = serde_json::json!({
            "active_team_key": "stale-b",
            "session_name": "team-stale-b",
            "teams": {
                "fresh-a": {
                    "active_team_key": "fresh-a",
                    "team_key": "fresh-a",
                    "session_name": "team-fresh-a",
                    "agents": {}
                },
                "stale-b": {
                    "active_team_key": "stale-b",
                    "team_key": "stale-b",
                    "session_name": "team-stale-b",
                    "agents": {}
                }
            }
        });
        crate::state::persist::save_runtime_state(&dir, &raw).unwrap();

        let selected =
            coordinator_team_scoped_state(&dir, &raw, Some("fresh-a")).expect("selected team");

        assert_eq!(
            selected.get("session_name").and_then(Value::as_str),
            Some("team-fresh-a")
        );
    }
}
