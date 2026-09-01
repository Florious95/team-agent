    #[test]
    fn report_result_infers_agent_from_env_when_not_explicit() {
        // env identity present, no explicit agent_id → envelope.agent_id == env id.
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-env"),
            Some(AgentId::new("worker-7")),
            Some(TeamKey::new("teamA")),
        );
        seed_report_message_for(
            &tools.workspace,
            "msg_env",
            "teamA",
            "worker-7",
            "delivered",
            "2026-07-06T13:24:25.000000+00:00",
        );
        seed_report_scope_state(
            &tools.workspace,
            &json!({
                "active_team_key": "teamA",
                "teams": {"teamA": {
                    "team_key": "teamA",
                    "coordinator": {"turn_open": {
                        "armed": true, "node_id": "worker-7", "turn_id": "msg_env"
                    }},
                    "agents": {"worker-7": {
                        "id": "worker-7", "current_turn_message_id": "msg_env"
                    }}
                }}
            }),
        );
        let ok = tools.report_result(
            None, Some("done it"), ResultStatus::Success,
            None, None, None, None, None,
            None, None, // implicit task and agent
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        // C17/bug-085: env id wins over leader/unknown. agent_id is a guaranteed-present
        // ok-whitelist key (normalize.py:46) — runtime.report_result echoes envelope
        // ["agent_id"], which _infer_agent_id sourced from self.agent_id (the injected
        // TEAM_AGENT_ID). UNCONDITIONAL assert: a vacuous skip here would silently fail
        // to lock the exact invariant this lane exists for.
        assert_eq!(
            v.get("agent_id"),
            Some(&json!("worker-7")),
            "env id wins, never leader/unknown"
        );
    }

    #[test]
    fn report_result_explicit_agent_overrides_env() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-explicit"),
            Some(AgentId::new("worker-7")),
            Some(TeamKey::new("teamA")),
        );
        let ok = tools.report_result(
            None, Some("done"), ResultStatus::Success,
            None, None, None, None, None,
            Some("task-9"), Some("explicit-agent"),
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        // explicit > env: both task_id and agent_id are ok-whitelist keys
        // (normalize.py:45-46) echoed by runtime.report_result, so present on success.
        // UNCONDITIONAL asserts — the override must be proven, not silently skipped.
        assert_eq!(
            v.get("agent_id"),
            Some(&json!("explicit-agent")),
            "explicit agent_id overrides env"
        );
        assert_eq!(
            v.get("task_id"),
            Some(&json!("task-9")),
            "explicit task_id flows through"
        );
    }

    #[test]
    fn report_result_no_env_no_explicit_falls_back_unknown_and_manual() {
        // bug-085: env missing + nothing explicit → agent "unknown", task "manual"
        // (NOT None, NOT "leader"). The envelope-builder is the asserted seam.
        let env = normalize_report_envelope(&json!({"summary": "x"}));
        assert_eq!(env.agent_id, AgentId::new("unknown"));
        assert_eq!(env.task_id, TaskId::new("manual"));
    }

    fn seed_report_message(
        ws: &std::path::Path,
        message_id: &str,
        owner_team_id: &str,
        status: &str,
        created_at: &str,
    ) {
        seed_report_message_for(
            ws,
            message_id,
            owner_team_id,
            "probe-worker",
            status,
            created_at,
        );
    }

    fn seed_report_message_for(
        ws: &std::path::Path,
        message_id: &str,
        owner_team_id: &str,
        recipient: &str,
        status: &str,
        created_at: &str,
    ) {
        let store = MessageStore::open(ws).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        conn.execute(
            "insert into messages(
                message_id, owner_team_id, task_id, sender, recipient, reply_to, requires_ack,
                status, content, artifact_refs, created_at, updated_at, delivered_at,
                acknowledged_at, error, delivery_attempts
            ) values (?1, ?2, null, 'leader', ?3, null, 0,
                ?4, 'task', '[]', ?5, ?5, case when ?4 = 'delivered' then ?5 else null end,
                null, null, 0)",
            rusqlite::params![message_id, owner_team_id, recipient, status, created_at],
        )
        .unwrap();
    }

    fn seed_report_result(
        ws: &std::path::Path,
        result_id: &str,
        owner_team_id: &str,
        task_id: &str,
        created_at: &str,
    ) {
        let store = MessageStore::open(ws).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        let envelope = json!({
            "schema_version": "result_envelope_v1",
            "result_id": result_id,
            "task_id": task_id,
            "agent_id": "probe-worker",
            "status": "success",
            "summary": "seed",
            "changes": [], "tests": [], "risks": [], "artifacts": [], "next_actions": []
        });
        conn.execute(
            "insert into results(
                result_id, owner_team_id, task_id, agent_id, envelope, status, created_at
            ) values (?1, ?2, ?3, 'probe-worker', ?4, 'success', ?5)",
            rusqlite::params![result_id, owner_team_id, task_id, envelope.to_string(), created_at],
        )
        .unwrap();
    }

    fn seed_report_scope_state(ws: &std::path::Path, state: &Value) {
        let cws = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
        let rt = cws.join(".team").join("runtime");
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::write(rt.join("state.json"), serde_json::to_string_pretty(state).unwrap())
            .unwrap();
    }

    #[test]
    fn report_result_prefers_current_turn_message_over_old_delivered_fallback() {
        let ws = unique_ws("report-current-message");
        seed_report_message(
            &ws,
            "msg_old",
            "gate055",
            "delivered",
            "2026-07-06T13:24:25.000000+00:00",
        );
        seed_report_result(
            &ws,
            "res_seed",
            "gate055",
            "task_initial",
            "2026-07-06T13:25:00.000000+00:00",
        );
        seed_report_message(
            &ws,
            "msg_new",
            "gate055",
            "target_resolved",
            "2026-07-06T13:27:35.000000+00:00",
        );
        // 0.5.16 result-attribution-race-locate.md §4/§7.7:
        // target_resolved alone is not physical-submit proof. Current-turn
        // attribution comes from the state pointer armed at physical submit.
        seed_report_scope_state(
            &ws,
            &json!({
                "active_team_key": "gate055",
                "teams": {
                    "gate055": {
                        "team_key": "gate055",
                        "coordinator": {
                            "turn_open": {
                                "armed": true,
                                "node_id": "probe-worker",
                                "turn_id": "msg_new"
                            }
                        },
                        "agents": {
                            "probe-worker": {
                                "id": "probe-worker",
                                "current_turn_message_id": "msg_new"
                            }
                        }
                    }
                }
            }),
        );

        let tools = TeamOrchestratorTools::with_identity(
            &ws,
            Some(AgentId::new("probe-worker")),
            Some(TeamKey::new("gate055")),
        );
        let ok = tools.report_result(
            None, Some("S3_RESTART_TOKEN"), ResultStatus::Success,
            None, None, None, None, None,
            None, None,
        ).expect("report ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(
            v.get("task_id"),
            Some(&json!("msg_new")),
            "current-turn message must beat stale delivered fallback"
        );
    }

    #[test]
    fn implicit_report_after_nested_delivery_stores_original_exactly() {
        let ws = unique_ws("report-nested-original");
        std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
        seed_report_message(&ws, "original", "gate055", "delivered", "2026-07-06T13:24:25.000000+00:00");
        seed_report_message(&ws, "nested", "gate055", "delivered", "2026-07-06T13:25:25.000000+00:00");
        seed_report_scope_state(&ws, &json!({"active_team_key":"gate055","teams":{"gate055":{"team_key":"gate055","coordinator":{"turn_open":{"armed":true,"node_id":"probe-worker","turn_id":"original"}},"agents":{"probe-worker":{"current_turn_message_id":"original"}}}}}));
        let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("probe-worker")), Some(TeamKey::new("gate055")));
        let value = serde_json::to_value(tools.report_result(None, Some("done"), ResultStatus::Success, None, None, None, None, None, None, None).unwrap()).unwrap();
        assert_eq!(value.get("task_id"), Some(&json!("original")));
        assert_eq!(value.get("attributed_message_id"), Some(&json!("original")));
        let conn = crate::db::schema::open_db(MessageStore::open(&ws).unwrap().db_path()).unwrap();
        let (task, envelope): (String, String) = conn.query_row("select task_id,envelope from results", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(task, "original");
        let envelope: Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(envelope["task_id"], json!("original"));
        assert_eq!(envelope["attributed_message_id"], json!("original"));
        assert_eq!(envelope["task_id_source"], json!("current_turn_message"));
        assert_eq!(conn.query_row("select count(*) from results where task_id='nested'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let original_rows = crate::messaging::results::results_for_case(&ws, "original", Some("gate055"), None).unwrap();
        assert_eq!(original_rows.len(), 1);
        assert!(crate::messaging::results::results_for_case(&ws, "nested", Some("gate055"), None).unwrap().is_empty());
        let collected = crate::messaging::collect_for_team(&ws, None, false, Some("gate055")).unwrap();
        assert_eq!(collected["collected_results"].as_array().unwrap().len(), 1);
        assert_eq!(collected["collected_results"][0]["task_id"], json!("original"));
        assert_eq!(collected["collected_results"][0]["scope"], json!("message"));
    }

    #[test]
    fn nested_explicit_flat_and_envelope_task_ids_remain_exact() {
        for (tag, envelope, flat_task) in [
            ("flat", None, Some("explicit-flat")),
            ("envelope", Some(json!({"task_id":"explicit-envelope"})), None),
        ] {
            let ws = unique_ws(&format!("report-nested-explicit-{tag}"));
            seed_report_message(&ws, "original", "gate055", "delivered", "2026-07-06T13:24:25.000000+00:00");
            seed_report_message(&ws, "nested", "gate055", "delivered", "2026-07-06T13:25:25.000000+00:00");
            seed_report_scope_state(&ws, &json!({"active_team_key":"gate055","teams":{"gate055":{"team_key":"gate055","coordinator":{"turn_open":{"armed":true,"node_id":"probe-worker","turn_id":"original"}},"agents":{"probe-worker":{"current_turn_message_id":"original"}}}}}));
            let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("probe-worker")), Some(TeamKey::new("gate055")));
            let value = serde_json::to_value(tools.report_result(envelope.as_ref(), Some("done"), ResultStatus::Success, None, None, None, None, None, flat_task, None).unwrap()).unwrap();
            let expected = flat_task.unwrap_or("explicit-envelope");
            assert_eq!(value["task_id"], json!(expected));
            let conn = crate::db::schema::open_db(MessageStore::open(&ws).unwrap().db_path()).unwrap();
            let (task_id, stored): (String, String) = conn.query_row("select task_id,envelope from results", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
            assert_eq!(task_id, expected);
            assert_eq!(serde_json::from_str::<Value>(&stored).unwrap()["task_id"], json!(expected));
        }
    }

    #[test]
    fn sibling_team_result_does_not_retire_current_turn() {
        let ws = unique_ws("report-team-isolation");
        seed_report_message(&ws, "shared", "teamA", "delivered", "2026-07-06T13:24:25.000000+00:00");
        seed_report_result(&ws, "res-team-b", "teamB", "shared", "9999-01-01T00:00:00+00:00");
        seed_report_scope_state(&ws, &json!({"active_team_key":"teamA","teams":{"teamA":{"team_key":"teamA","coordinator":{"turn_open":{"armed":true,"node_id":"probe-worker","turn_id":"shared"}},"agents":{"probe-worker":{"current_turn_message_id":"shared"}}},"teamB":{"team_key":"teamB","agents":{"probe-worker":{}}}}}));
        let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("probe-worker")), Some(TeamKey::new("teamA")));
        let value = serde_json::to_value(tools.report_result(None, Some("team a"), ResultStatus::Success, None, None, None, None, None, None, None).unwrap()).unwrap();
        assert_eq!(value["task_id"], json!("shared"));
        let conn = crate::db::schema::open_db(MessageStore::open(&ws).unwrap().db_path()).unwrap();
        assert_eq!(conn.query_row("select count(*) from results where task_id='shared' and owner_team_id='teamA'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(conn.query_row("select count(*) from results where task_id='shared' and owner_team_id='teamB'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn ordinary_direct_report_queries_and_collects_exact_message() {
        let ws = unique_ws("report-ordinary-direct");
        std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
        seed_report_message(&ws, "direct", "gate055", "delivered", "2026-07-06T13:24:25.000000+00:00");
        seed_report_scope_state(&ws, &json!({"active_team_key":"gate055","teams":{"gate055":{"team_key":"gate055","coordinator":{"turn_open":{"armed":true,"node_id":"probe-worker","turn_id":"direct"}},"agents":{"probe-worker":{"current_turn_message_id":"direct"}}}}}));
        let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("probe-worker")), Some(TeamKey::new("gate055")));
        tools.report_result(None, Some("ordinary"), ResultStatus::Success, None, None, None, None, None, None, None).unwrap();
        let queried = crate::messaging::results::results_for_case(&ws, "direct", Some("gate055"), None).unwrap();
        assert_eq!(queried.len(), 1);
        assert_eq!(queried[0]["task_id"], json!("direct"));
        assert!(crate::messaging::results::results_for_case(&ws, "nested", Some("gate055"), None).unwrap().is_empty());
        let collected = crate::messaging::collect_for_team(&ws, None, false, Some("gate055")).unwrap();
        assert_eq!(collected["collected_results"].as_array().unwrap().len(), 1);
        assert_eq!(collected["collected_results"][0]["task_id"], json!("direct"));
        assert_eq!(collected["collected_results"][0]["scope"], json!("message"));
    }

    #[test]
    fn implicit_report_missing_authority_stores_no_result() {
        let ws = unique_ws("report-no-authority");
        seed_report_message(&ws, "historical", "gate055", "delivered", "2026-07-06T13:24:25.000000+00:00");
        seed_report_scope_state(&ws, &json!({"active_team_key":"gate055","teams":{"gate055":{"team_key":"gate055","tasks":[{"id":"assigned","assignee":"probe-worker","status":"pending"}],"agents":{"probe-worker":{}}}}}));
        let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("probe-worker")), Some(TeamKey::new("gate055")));
        let error = tools.report_result(None, Some("no authority"), ResultStatus::Success, None, None, None, None, None, None, None).expect_err("implicit attribution must fail closed");
        assert_eq!(error.reason, ToolErrorReason::InvalidToolArguments);
        assert_eq!(error.exc_type, "ResultAttributionUnavailable");
        let conn = crate::db::schema::open_db(MessageStore::open(&ws).unwrap().db_path()).unwrap();
        assert_eq!(conn.query_row("select count(*) from results", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn implicit_report_authority_query_error_fails_closed_without_storage() {
        let ws = unique_ws("report-authority-query-error");
        seed_report_message(&ws, "current", "gate055", "delivered", "2026-07-06T13:24:25.000000+00:00");
        seed_report_scope_state(&ws, &json!({"active_team_key":"gate055","teams":{"gate055":{"team_key":"gate055","coordinator":{"turn_open":{"armed":true,"node_id":"probe-worker","turn_id":"current"}},"agents":{"probe-worker":{"current_turn_message_id":"current"}}}}}));
        let store = MessageStore::open(&ws).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        conn.execute("drop table messages", []).unwrap();
        let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("probe-worker")), Some(TeamKey::new("gate055")));
        let error = tools.report_result(None, Some("query failure"), ResultStatus::Success, None, None, None, None, None, None, None).expect_err("authority query failure must fail closed");
        assert_eq!(error.reason, ToolErrorReason::InvalidToolArguments);
        assert_eq!(error.exc_type, "ResultAttributionUnavailable");
        assert_eq!(conn.query_row("select count(*) from results", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn report_result_target_resolved_without_current_turn_fails_closed() {
        let ws = unique_ws("report-target-resolved-no-current");
        seed_report_message(
            &ws,
            "msg_target_only",
            "gate055",
            "target_resolved",
            "2026-07-06T13:27:35.000000+00:00",
        );
        seed_report_scope_state(
            &ws,
            &json!({
                "active_team_key": "gate055",
                "teams": {
                    "gate055": {
                        "team_key": "gate055",
                        "tasks": [
                            {
                                "id": "task_initial",
                                "assignee": "probe-worker",
                                "status": "pending"
                            }
                        ],
                        "agents": {
                            "probe-worker": {"id": "probe-worker"}
                        }
                    }
                }
            }),
        );

        let tools = TeamOrchestratorTools::with_identity(
            &ws,
            Some(AgentId::new("probe-worker")),
            Some(TeamKey::new("gate055")),
        );
        let error = tools.report_result(
            None, Some("target resolved only"), ResultStatus::Success,
            None, None, None, None, None,
            None, None,
        ).expect_err("implicit attribution must fail closed");
        assert_eq!(error.exc_type, "ResultAttributionUnavailable");
        assert_eq!(error.reason, ToolErrorReason::InvalidToolArguments);
    }

    #[test]
    fn report_result_does_not_backfill_old_delivered_message_after_latest_result() {
        let ws = unique_ws("report-no-old-backfill");
        seed_report_message(
            &ws,
            "msg_old",
            "gate055",
            "delivered",
            "2026-07-06T13:24:25.000000+00:00",
        );
        seed_report_result(
            &ws,
            "res_latest",
            "gate055",
            "task_initial",
            "2026-07-06T13:25:00.000000+00:00",
        );

        let tools = TeamOrchestratorTools::with_identity(
            &ws,
            Some(AgentId::new("probe-worker")),
            Some(TeamKey::new("gate055")),
        );
        let error = tools.report_result(
            None, Some("manual follow-up"), ResultStatus::Success,
            None, None, None, None, None,
            None, None,
        ).expect_err("implicit attribution must fail closed");
        assert_eq!(error.exc_type, "ResultAttributionUnavailable");
        assert_eq!(error.reason, ToolErrorReason::InvalidToolArguments);
    }

    // ════════════════════════════════════════════════════════════════════════
    // CONTROL-PLANE: request_human creates a requires_ack leader message → needs_human
    // (tools.py:342-346). sender = explicit > env > "unknown" (never leader).
    //
    // Post-#230 N31/N32 funnel (cr-approved I-3): request_human routes through the
    // shared leader-delivery primitive (`send_to_leader_receiver`) instead of doing a
    // raw `store.create_message` bypass. Return shape from the caller's perspective is
    // unchanged: `status="needs_human"` + a populated `message_id`. With no leader
    // pane bound in this fixture, the primitive's I-4 rebind_required path STILL
    // persists the message row and returns its `message_id` — audit + rebind replay
    // both depend on it.
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn request_human_returns_needs_human_with_message_id() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("request-human"),
            Some(AgentId::new("worker-3")),
            Some(TeamKey::new("teamA")),
        );
        let ok = tools.request_human("need approval", Some("task-1"), None)
            .expect("request_human ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v.get("status"), Some(&json!("needs_human")));
        assert!(v.get("message_id").and_then(Value::as_str).is_some(),
            "request_human must return the created leader message_id (persisted for rebind audit even on I-4 rebind_required)");
    }

    // ════════════════════════════════════════════════════════════════════════
    // CONTROL-PLANE: update_state appends a note + returns state_file (tools.py:316-325)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn update_state_returns_ok_and_state_file_path() {
        let ws = seed_state_ws(
            "update-state",
            &json!({
                "active_team_key": "teamA",
                "team_key": "teamA",
                "session_name": "teamA",
                "status": "alive",
                "teams": {
                    "teamA": {
                        "team_key": "teamA",
                        "session_name": "teamA",
                        "status": "alive",
                        "agents": {}
                    }
                }
            }),
        );
        let tools = TeamOrchestratorTools::with_identity(
            &ws,
            Some(AgentId::new("leader")),
            Some(TeamKey::new("teamA")),
        );
        let ok = tools.update_state("checkpoint note").expect("update_state ok");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v.get("ok"), Some(&json!(true)));
        assert!(v.get("state_file").and_then(Value::as_str).is_some(),
            "update_state returns the rewritten team_state.md path");
    }

    // ════════════════════════════════════════════════════════════════════════
    // RpcId / RpcResponse byte-stability — null id echoed, error frame shape.
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn rpc_response_error_frame_serializes_without_result_key() {
        // server.py: error frames carry NO result key; result frames carry NO error key.
        let frame = RpcResponse {
            jsonrpc: "2.0".to_string(),
            id: RpcId::Int(7),
            result: None,
            error: Some(RpcError { code: -32601, message: "unknown method 'x'".to_string() }),
        };
        let v = serde_json::to_value(&frame).unwrap();
        assert!(v.get("result").is_none(), "error frame omits result");
        assert_eq!(v["error"]["code"], json!(-32601));
        assert_eq!(v["jsonrpc"], json!("2.0"));
    }

    #[test]
    fn rpc_id_null_roundtrips() {
        // request.get("id") absent/null → echoed back as null
        let frame = RpcResponse {
            jsonrpc: "2.0".to_string(),
            id: RpcId::Null,
            result: Some(json!({"ok": true})),
            error: None,
        };
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["id"], Value::Null);
        assert!(v.get("error").is_none(), "result frame omits error");
    }

    // ════════════════════════════════════════════════════════════════════════
    // STEP-14 DIVERGENCE-FIX RED TESTS (Phase 1). Each encodes the EXACT Python
    // golden v0.2.11 value (probed via PYTHONPATH=.../src python3) and FAILS against
    // current Rust. The P2 porter greens these. Do NOT weaken existing assertions.
    // ════════════════════════════════════════════════════════════════════════

    /// Seed `<ws>/.team/runtime/state.json` and return the CANONICAL workspace path
    /// so `with_identity` (which canonicalizes) reads the same file we wrote.
    fn seed_state_ws(tag: &str, state: &Value) -> std::path::PathBuf {
        let ws = unique_ws(tag);
        let cws = std::fs::canonicalize(&ws).unwrap_or(ws);
        let rt = cws.join(".team").join("runtime");
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::write(rt.join("state.json"), serde_json::to_string_pretty(state).unwrap()).unwrap();
        cws
    }

    // ── #29/#43/#49 compact ok-whitelist: EXACT 23-key golden list + order ──────
    // GOLDEN (probe_mcp_red.py OK-FULL-KEYS): the 23 keys in normalize.py:32-56 order
