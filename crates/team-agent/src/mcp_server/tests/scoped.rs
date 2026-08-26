/// ---
/// purpose: scoped MCP update_state filesystem contracts
/// contract:
///   provides:
///     - name: update_state_path_contracts
///       what: proves writable, canonical-remap, permission, two-outcome, and no-escape cases
///   depends:
///     - name: mcp_state_fixture
///       what: fixture-owned root and provenance from tests.rs
/// boundary:
///   - retain the raw {ok,state_file} result shape
///   - rejected writes preserve both runtime and rendered-state bytes
/// maturity: wired
/// ---
    #[test]
    fn dispatch_send_message_worker_accepted_returned_verbatim() {
        // A-7: accepted requires a REAL stored message_id (no fabricated ids), so the
        // workspace seeds a running worker-1 the delivery layer can actually queue for.
        let ws = unique_ws("dispatch-accepted");
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "session_name": "team-x",
                "agents": {
                    "worker-1": {"status": "running", "agent_id": "worker-1", "window": "worker-1"},
                },
            }),
        )
        .unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &ws,
            Some(AgentId::new("leader")), // legacy single-team bypasses cross-team refusal
            None,
        );
        let ok = dispatch_tool(&tools, McpTool::SendMessage, &json!({
            "to": "worker-1", "content": "do it"
        })).expect("send ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(keys(&v), vec!["status", "delivery_pending", "poll_via", "message_id"],
            "worker-accepted dict returned verbatim (NOT re-compacted)");
        assert_eq!(v.get("status"), Some(&json!("accepted")));
        assert_eq!(v.get("delivery_pending"), Some(&json!(true)));
        let mid = v.get("message_id").and_then(Value::as_str).unwrap();
        assert_eq!(v.get("poll_via"), Some(&json!(format!("team-agent inbox {mid}"))));
    }

    // ── #32/#47 request_human key order ok,message_id,status (no compaction) ────
    // GOLDEN (probe_events_red.py REQUEST_HUMAN-KEYS): ['ok','message_id','status'].
    // Rust compact_tool_result reorders to ok,status,message_id.
    #[test]
    fn request_human_key_order_is_ok_message_id_status() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("reqhuman-order"),
            Some(AgentId::new("worker-3")),
            None,
        );
        let ok = tools.request_human("need approval", Some("task-1"), None).expect("request_human ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(keys(&v), vec!["ok", "message_id", "status"]);
        assert_eq!(v.get("status"), Some(&json!("needs_human")));
    }

    // ── #32 update_state returns RAW {ok, state_file} (NO compaction) ───────────
    // GOLDEN (tools.py:316-325 + probe_passthrough): update_state is NOT compacted;
    // state_file survives. Rust runs compact_tool_result whose ok-whitelist DROPS
    // state_file (not a golden whitelist key), so the key vanishes.
    #[test]
    #[serial_test::serial(env)]
    fn update_state_state_file_survives_no_compaction() {
        let fixture = McpStateFixture::new("writable");
        let expected = fixture.state_file("team_state.md");
        fixture.record_provenance(&fixture.workspace, &expected);
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let ok = tools.update_state("note").expect("update_state ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert!(v.get("state_file").and_then(Value::as_str).is_some(),
            "state_file must survive (update_state is not _compact_tool_result'd)");
        assert_eq!(keys(&v), vec!["ok", "state_file"]);
        assert_eq!(PathBuf::from(v["state_file"].as_str().unwrap()), expected);
        assert!(fixture.under_root(&expected));
        assert!(fixture.under_root(&crate::state::persist::runtime_state_path(&fixture.workspace)));
        assert!(std::fs::read_to_string(&expected).unwrap().contains("- note"));
        let runtime = crate::state::persist::load_runtime_state(&fixture.workspace).unwrap();
        assert!(runtime["notes"].as_array().unwrap().iter().any(|item| item == "note"));
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_canonical_remap_is_observable_and_owned() {
        let fixture = McpStateFixture::new("remap");
        let raw_workspace = fixture.workspace.join(".team");
        let resolved_workspace = crate::model::paths::canonical_run_workspace(&raw_workspace).unwrap();
        let expected = fixture.state_file("team_state.md");
        assert_ne!(raw_workspace, resolved_workspace, "control must exercise canonical remap");
        fixture.record_provenance(&raw_workspace, &expected);
        let tools = TeamOrchestratorTools::with_identity(
            &raw_workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let ok = tools.update_state("remap note").expect("canonical remap update_state ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(PathBuf::from(v["state_file"].as_str().unwrap()), expected);
        assert!(fixture.under_root(&resolved_workspace));
        assert!(fixture.under_root(&expected));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(env)]
    fn update_state_parent_0555_rejects_without_partial_commit() {
        let mut fixture = McpStateFixture::new("parent");
        let relative = "s/team_state.md";
        fixture.seed_spec(relative);
        let parent = fixture.make_parent_readonly(relative);
        let expected = fixture.state_file(relative);
        fixture.record_provenance(&fixture.workspace, &expected);
        let runtime_path = crate::state::persist::runtime_state_path(&fixture.workspace);
        let runtime_before = std::fs::read(&runtime_path).unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let error = tools.update_state("parent failure").expect_err("0555 parent must reject markdown write");
        assert_error_names_paths(&error.message, &fixture.workspace, &fixture.workspace, &expected);
        assert!(error.message.contains("os error 13"), "missing EACCES cause: {}", error.message);
        assert_eq!(metadata_mode(&std::fs::metadata(&parent).unwrap()) & 0o777, 0o555);
        assert!(!expected.exists(), "failed markdown write must not create a file");
        assert_eq!(std::fs::read(&runtime_path).unwrap(), runtime_before);
        assert_runtime_note_absent(&fixture.workspace, "parent failure");
        assert!(fixture.under_root(&expected));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(env)]
    fn update_state_target_0444_rejects_without_partial_commit() {
        let mut fixture = McpStateFixture::new("target");
        let relative = "team_state.md";
        let expected = fixture.make_target_readonly(relative);
        fixture.record_provenance(&fixture.workspace, &expected);
        let runtime_path = crate::state::persist::runtime_state_path(&fixture.workspace);
        let runtime_before = std::fs::read(&runtime_path).unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let error = tools.update_state("target failure").expect_err("0444 target must reject markdown write");
        assert_error_names_paths(&error.message, &fixture.workspace, &fixture.workspace, &expected);
        assert!(error.message.contains("os error 13"), "missing EACCES cause: {}", error.message);
        assert_eq!(std::fs::read_to_string(&expected).unwrap(), "fixture baseline\n");
        assert_eq!(std::fs::read(&runtime_path).unwrap(), runtime_before);
        assert_runtime_note_absent(&fixture.workspace, "target failure");
        assert!(fixture.under_root(&expected));
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_injected_markdown_denial_rejects_both_artifacts() {
        let fixture = McpStateFixture::new("injected-denial");
        let expected = fixture.state_file("team_state.md");
        fixture.record_provenance(&fixture.workspace, &expected);
        let runtime_path = crate::state::persist::runtime_state_path(&fixture.workspace);
        let runtime_before = std::fs::read(&runtime_path).unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let _denial = ScopedEnv::set("TEAM_AGENT_TEST_UPDATE_STATE_DENY_AFTER_PREPARE", "1");
        let error = tools
            .update_state("injected failure")
            .expect_err("injected markdown denial must reject");
        assert_error_names_paths(&error.message, &fixture.workspace, &fixture.workspace, &expected);
        assert!(error.message.contains("injected markdown denial after preparation"), "missing injected cause: {}", error.message);
        assert_eq!(std::fs::read(&runtime_path).unwrap(), runtime_before);
        assert!(!expected.exists(), "rejected update must not create markdown");
        assert_runtime_note_absent(&fixture.workspace, "injected failure");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_missing_nested_parent_commits_both_artifacts() {
        let fixture = McpStateFixture::new("missing-nested-parent");
        let relative = "missing/nested/team_state.md";
        fixture.seed_spec(relative);
        let expected = fixture.state_file(relative);
        assert!(!expected.parent().unwrap().exists());
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let ok = tools.update_state("nested note").expect("nested parent must be created");
        let value = serde_json::to_value(ok).unwrap();
        assert_eq!(PathBuf::from(value["state_file"].as_str().unwrap()), expected);
        assert!(std::fs::read_to_string(&expected).unwrap().contains("- nested note"));
        assert_runtime_note_present(&fixture.workspace, "nested note");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_preflight_failure_cannot_corrupt_existing_bytes() {
        let fixture = McpStateFixture::new("preflight-bytes");
        let expected = fixture.state_file("team_state.md");
        std::fs::write(&expected, b"existing bytes\n").unwrap();
        let artifact_before = std::fs::read(&expected).unwrap();
        let runtime_path = crate::state::persist::runtime_state_path(&fixture.workspace);
        let runtime_before = std::fs::read(&runtime_path).unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let _failure = ScopedEnv::set("TEAM_AGENT_TEST_UPDATE_STATE_FAIL_PREFLIGHT_AFTER_OPEN", "1");
        let error = tools
            .update_state("must reject")
            .expect_err("injected preflight failure must reject");
        assert!(error.message.contains("injected preflight failure after open"));
        assert_eq!(std::fs::read(&expected).unwrap(), artifact_before);
        assert_eq!(std::fs::read(&runtime_path).unwrap(), runtime_before);
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_artifact_promotion_failure_restores_both_bytes() {
        let fixture = McpStateFixture::new("promotion-rollback");
        let expected = fixture.state_file("team_state.md");
        std::fs::write(&expected, b"artifact before\n").unwrap();
        let artifact_before = std::fs::read(&expected).unwrap();
        let runtime_path = crate::state::persist::runtime_state_path(&fixture.workspace);
        let runtime_before = std::fs::read(&runtime_path).unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let _failure = ScopedEnv::set("TEAM_AGENT_TEST_UPDATE_STATE_FAIL_ARTIFACT_PROMOTE", "1");
        let error = tools
            .update_state("rollback note")
            .expect_err("artifact promotion failure must reject");
        assert!(error.message.contains("injected artifact promotion failure"));
        assert_eq!(std::fs::read(&expected).unwrap(), artifact_before);
        assert_eq!(std::fs::read(&runtime_path).unwrap(), runtime_before);
        assert_runtime_note_absent(&fixture.workspace, "rollback note");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_crash_after_runtime_is_recovered_before_load() {
        let fixture = McpStateFixture::new("crash-recovery");
        let expected = fixture.state_file("team_state.md");
        std::fs::write(&expected, b"artifact before\n").unwrap();
        let artifact_before = std::fs::read(&expected).unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        {
            let _crash = ScopedEnv::set("TEAM_AGENT_TEST_UPDATE_STATE_CRASH_AFTER_RUNTIME", "1");
            let error = tools
                .update_state("recovered note")
                .expect_err("test crash interrupts before artifact promotion");
            assert!(
                error.message.contains("injected cra"),
                "unexpected crash error: {}",
                error.message
            );
        }
        assert_eq!(std::fs::read(&expected).unwrap(), artifact_before);
        let transaction = crate::model::paths::runtime_dir(&fixture.workspace)
            .join("update-state.transaction");
        assert!(transaction.exists(), "crash must leave durable recovery intent");
        assert_runtime_note_present(&fixture.workspace, "recovered note");
        assert!(std::fs::read_to_string(&expected).unwrap().contains("- recovered note"));
        assert!(!transaction.exists(), "canonical load must finish recovery");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_holds_canonical_lock_against_concurrent_writer() {
        let fixture = McpStateFixture::new("concurrent-writer");
        let pause = fixture.root.join("transaction-pause");
        let _pause = ScopedEnv::set(
            "TEAM_AGENT_TEST_UPDATE_STATE_PAUSE_AFTER_PREPARE",
            pause.as_os_str(),
        );
        let update_workspace = fixture.workspace.clone();
        let update = std::thread::spawn(move || {
            TeamOrchestratorTools::with_identity(
                &update_workspace,
                Some(AgentId::new("leader")),
                None,
            )
            .update_state("locked note")
        });
        let started = std::time::Instant::now();
        while !pause.join("entered").exists() {
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let writer_workspace = fixture.workspace.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let result = (|| {
                let mut state = crate::state::persist::load_runtime_state(&writer_workspace)?;
                state["concurrent_writer"] = json!("committed");
                crate::state::persist::save_runtime_state(&writer_workspace, &state)
            })();
            sent.send(result).unwrap();
        });
        assert!(matches!(
            received.recv_timeout(std::time::Duration::from_millis(150)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        std::fs::write(pause.join("release"), b"release").unwrap();
        update.join().unwrap().expect("transaction commits");
        received
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .expect("writer commits after transaction");
        writer.join().unwrap();
        let state = crate::state::persist::load_runtime_state(&fixture.workspace).unwrap();
        assert_eq!(state["concurrent_writer"], "committed");
        assert_runtime_note_present(&fixture.workspace, "locked note");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_replaces_existing_destination_and_reads_back_success() {
        let fixture = McpStateFixture::new("replace-existing");
        let expected = fixture.state_file("team_state.md");
        std::fs::write(&expected, b"existing destination\n").unwrap();
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let ok = tools
            .update_state("replacement note")
            .expect("existing destination replacement must succeed");
        let value = serde_json::to_value(ok).unwrap();
        assert_eq!(PathBuf::from(value["state_file"].as_str().unwrap()), expected);
        let artifact = std::fs::read_to_string(&expected).unwrap();
        assert_ne!(artifact, "existing destination\n");
        assert!(artifact.contains("- replacement note"));
        assert_runtime_note_present(&fixture.workspace, "replacement note");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_fixture_destruction_removes_owned_root() {
        let root = {
            let fixture = McpStateFixture::new("destruction");
            let root = fixture.root.clone();
            assert!(root.exists());
            root
        };
        assert!(!root.exists(), "fixture drop must remove its owned root");
    }

    #[test]
    #[serial_test::serial(env)]
    fn update_state_fixture_panic_restores_env_and_allows_next_sibling() {
        let previous_tmpdir = std::env::var_os("TMPDIR");
        let mut panicked_root = None;
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fixture = McpStateFixture::new("panic");
            panicked_root = Some(fixture.root.clone());
            panic!("fixture panic tooth");
        }));
        assert!(panic_result.is_err());
        assert_eq!(std::env::var_os("TMPDIR"), previous_tmpdir);
        assert!(!panicked_root.unwrap().exists());

        let fixture = McpStateFixture::new("after-panic");
        let tools = TeamOrchestratorTools::with_identity(
            &fixture.workspace,
            Some(AgentId::new("leader")),
            None,
        );
        let result = tools
            .update_state("after panic")
            .expect("sibling must retain assertions");
        let result = serde_json::to_value(&result).unwrap();
        assert_eq!(
            PathBuf::from(result["state_file"].as_str().unwrap()),
            fixture.state_file("team_state.md")
        );
    }

    fn assert_error_names_paths(message: &str, raw: &Path, resolved: &Path, state_file: &Path) {
        assert!(message.contains(&format!("raw={}", raw.display())), "raw path missing: {message}");
        assert!(message.contains(&format!("resolved={}", resolved.display())), "resolved path missing: {message}");
        assert!(message.contains(&format!("state={}", state_file.display())), "state path missing: {message}");
    }

    fn assert_runtime_note_absent(workspace: &Path, note: &str) {
        let state = crate::state::persist::load_runtime_state(workspace).unwrap();
        assert!(!state
            .get("notes")
            .and_then(Value::as_array)
            .is_some_and(|notes| notes.iter().any(|item| item == note)),
            "rejected update must not persist runtime note: {state}");
    }

    fn assert_runtime_note_present(workspace: &Path, note: &str) {
        let state = crate::state::persist::load_runtime_state(workspace).unwrap();
        assert!(state
            .get("notes")
            .and_then(Value::as_array)
            .is_some_and(|notes| notes.iter().any(|item| item == note)),
            "committed update must persist runtime note: {state}");
    }

    struct ScopedEnv {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl ScopedEnv {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    // ── #36 report_result setdefault: populated envelope keys WIN over args ─────
    // GOLDEN (probe_setdefault.py): envelope {agent_id:env-agent, task_id:env-task,...}
    // + explicit args agent_id=ARG-agent, task_id=ARG-task → returned dict keeps
    // env-agent / env-task (setdefault). Rust unconditionally insert-overrides.
    #[test]
    fn report_result_setdefault_envelope_wins_over_args() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-setdefault"),
            Some(AgentId::new("env-id")),
            None,
        );
        let ok = tools.report_result(
            Some(&json!({
                "agent_id": "env-agent", "task_id": "env-task",
                "status": "blocked", "summary": "env summary"
            })),
            Some("ARG summary"), ResultStatus::Success,
            None, None, None, None, None,
            Some("ARG-task"), Some("ARG-agent"),
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        // setdefault: the pre-populated envelope values win.
        assert_eq!(v.get("agent_id"), Some(&json!("env-agent")), "envelope agent_id wins (setdefault)");
        assert_eq!(v.get("task_id"), Some(&json!("env-task")), "envelope task_id wins (setdefault)");
    }

    // ── #44 report_result task_id inference from state (_latest_task_for_assignee)
    // GOLDEN (probe_report evidence): env agent worker-7, state tasks=[{id:t-42,
    // assignee:worker-7,status:pending}], report with NO task_id → task_id "t-42".
    // Rust has no _latest_task_for_assignee; hard-codes "manual".
    #[test]
    fn report_result_infers_task_id_from_latest_assigned_task() {
        let cws = seed_state_ws("report-infer-task", &json!({
            "agents": {}, "active_team_key": null,
            "tasks": [{"id": "t-42", "assignee": "worker-7", "status": "pending"}]
        }));
        let tools = TeamOrchestratorTools::with_identity(&cws, Some(AgentId::new("worker-7")), None);
        let ok = tools.report_result(
            None, Some("done it"), ResultStatus::Success,
            None, None, None, None, None,
            None, None,
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v.get("task_id"), Some(&json!("t-42")),
            "task_id inferred from latest non-terminal assigned task, not 'manual'");
    }

    // ── #42 get_visible_peers reads seeded state (sorted live peers) ────────────
    // GOLDEN (probe_peers.py): teamA agents {worker-z(alive),worker-a(working),
    // worker-dead(DEAD),worker-stopped(Stopped),worker-no-status(dict no status),
    // worker-weird(non-dict)} → peers ["worker-a","worker-no-status","worker-weird",
    // "worker-z"] (sorted, dead/stopped filtered, non-dict & no-status INCLUDED);
    // sender_team_id "teamA", scope team. Rust stub returns empty peers.
    #[test]
    fn get_visible_peers_reads_state_sorted_live_filtered() {
        let cws = seed_state_ws("visible-peers", &json!({
            "agents": {}, "active_team_key": null,
            "teams": {
                "teamA": {"status": "alive", "agents": {
                    "worker-z": {"status": "alive"},
                    "worker-a": {"status": "working"},
                    "worker-dead": {"status": "DEAD"},
                    "worker-stopped": {"status": "Stopped"},
                    "worker-no-status": {},
                    "worker-weird": "not-a-dict"
                }},
                "teamB": {"status": "alive", "agents": {"other-bob": {"status": "alive"}}}
            }
        }));
        let tools = TeamOrchestratorTools::with_identity(
            &cws, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
        );
        let vp = tools.get_visible_peers().expect("visible peers");
        let got: Vec<&str> = vp.peers.iter().map(AgentId::as_str).collect();
        assert_eq!(got, vec!["worker-a", "worker-no-status", "worker-weird", "worker-z"]);
        assert_eq!(vp.sender_team_id, Some(TeamKey::new("teamA")));
        assert_eq!(vp.scope, Scope::Team);
    }

    // ── #42 refuse_cross_team_peer ALLOWS a live in-team peer (visible bypass) ──
    // GOLDEN (probe_peers.py): with the same seeded state, refusing worker-a / worker-z
    // / worker-no-status / worker-weird → None (ALLOWED, they are visible peers), while
    // worker-dead / worker-stopped / other-bob → refused. Rust stub refuses ALL of them.
    #[test]
    fn refuse_cross_team_peer_allows_live_in_team_peer() {
        let cws = seed_state_ws("refuse-inteam", &json!({
            "agents": {}, "active_team_key": null,
            "teams": {
                "teamA": {"status": "alive", "agents": {
                    "worker-z": {"status": "alive"},
                    "worker-a": {"status": "working"},
                    "worker-dead": {"status": "DEAD"},
                    "worker-no-status": {},
                    "worker-weird": "not-a-dict"
                }},
                "teamB": {"status": "alive", "agents": {"other-bob": {"status": "alive"}}}
            }
        }));
        let tools = TeamOrchestratorTools::with_identity(
            &cws, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
        );
        // live / no-status / non-dict in-team peers are ALLOWED (None).
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-a".to_string()), None).is_none(),
            "a live in-team peer must be allowed (visible-peer bypass)");
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-z".to_string()), None).is_none());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-no-status".to_string()), None).is_none());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-weird".to_string()), None).is_none());
        // dead / other-team peers are still refused.
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("worker-dead".to_string()), None).is_some());
        assert!(tools.refuse_cross_team_peer(&MessageTarget::Single("other-bob".to_string()), None).is_some());
    }

    // ── #48 refuse_cross_team_peer writes mcp.send_message_refused EventLog ─────
    // GOLDEN (probe_events_red.py): a refusal appends an event with fields
    // event=mcp.send_message_refused, reason=peer_not_in_scope, scope=team,
    // sender_team_id=teamA, hint=<the cross-team hint>. Rust handler writes nothing.
    #[test]
    fn refuse_cross_team_peer_writes_send_message_refused_event() {
        let cws = seed_state_ws("refuse-event", &json!({
            "agents": {}, "active_team_key": null,
            "teams": {"teamA": {"status": "alive", "agents": {"worker-1": {"status": "alive"}}}}
        }));
        let tools = TeamOrchestratorTools::with_identity(
            &cws, Some(AgentId::new("worker-1")), Some(TeamKey::new("teamA")),
        );
        // out-of-scope peer → refusal must emit the audit event.
        let _ = tools.refuse_cross_team_peer(&MessageTarget::Single("other-bob".to_string()), None);
        let events = EventLog::new(&cws).tail(50).expect("read events");
        let refused = events.iter().find(|e| e["event"] == json!("mcp.send_message_refused"))
            .expect("mcp.send_message_refused must be written on refusal");
        assert_eq!(refused["reason"], json!("peer_not_in_scope"));
        assert_eq!(refused["scope"], json!("team"));
        assert_eq!(refused["sender_team_id"], json!("teamA"));
        assert_eq!(refused["hint"],
            json!("the requested peer is not part of your team; worker-origin MCP cannot widen team scope."));
    }
