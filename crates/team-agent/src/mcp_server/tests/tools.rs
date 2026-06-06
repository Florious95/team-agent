    #[test]
    fn report_result_infers_agent_from_env_when_not_explicit() {
        // env identity present, no explicit agent_id → envelope.agent_id == env id.
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-env"),
            Some(AgentId::new("worker-7")),
            Some(TeamKey::new("teamA")),
        );
        let ok = tools.report_result(
            None, Some("done it"), ResultStatus::Success,
            None, None, None, None, None,
            None, None, // no explicit task/agent
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
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("update-state"),
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
