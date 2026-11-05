    #[test]
    fn report_result_infers_agent_from_env_when_not_explicit() {
        // env identity present, no explicit agent_id → envelope.agent_id == env id.
        let tools = TeamOrchestratorTools::with_identity(
            &seed_state_ws("report-env", &json!({"teams": {"teamA": {"agents": {}}}})),
            Some(AgentId::new("worker-7")),
            Some(TeamKey::new("teamA")),
        );
        let ok = tools.report_result_with_presentation(
            None, Some("done it"), Some("success"),
            None, None, None, None, None,
            None, None, // no explicit task/agent
            None,
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
    fn report_result_rejects_explicit_agent_mismatch() {
        let tools = TeamOrchestratorTools::with_identity(
            &unique_ws("report-explicit-mismatch"),
            Some(AgentId::new("worker-7")),
            Some(TeamKey::new("teamA")),
        );
        let error = tools
            .report_result_with_presentation(
                None, Some("done"), Some("success"),
                None, None, None, None, None,
                Some("task-9"), Some("explicit-agent"), None)
            .expect_err("a request agent_id cannot override the captured identity");
        assert_eq!(error.reason, ToolErrorReason::McpScopeRefused);
        assert!(error.message.contains("identity_mismatch"));
        assert_eq!(error.extra.get("expected_agent_id"), Some(&json!("worker-7")));
        assert_eq!(error.extra.get("provided_agent_id"), Some(&json!("explicit-agent")));
    }

    #[test]
    fn report_result_accepts_explicit_captured_agent() {
        let tools = TeamOrchestratorTools::with_identity(
            &seed_state_ws("report-explicit-match", &json!({"teams": {"teamA": {"agents": {}}}})),
            Some(AgentId::new("worker-7")),
            Some(TeamKey::new("teamA")),
        );
        let ok = tools
            .report_result_with_presentation(
                None, Some("done"), Some("success"),
                None, None, None, None, None,
                Some("task-9"), Some("worker-7"), None)
            .expect("matching explicit agent_id remains compatible");
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v.get("agent_id"), Some(&json!("worker-7")));
        assert_eq!(v.get("task_id"), Some(&json!("task-9")));
    }

    #[test]
    fn report_result_no_env_no_explicit_falls_back_unknown_and_manual() {
        // bug-085: env missing + nothing explicit → agent "unknown", task "manual"
        // (NOT None, NOT "leader"). The envelope-builder is the asserted seam.
        let env = normalize_report_envelope(&json!({"summary": "x"}));
        assert_eq!(env.agent_id, AgentId::new("unknown"));
        assert_eq!(env.task_id, TaskId::new("manual"));
    }



    fn seed_report_scope_state(ws: &std::path::Path, state: &Value) {
        let cws = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
        let rt = cws.join(".team").join("runtime");
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::write(rt.join("state.json"), serde_json::to_string_pretty(state).unwrap())
            .unwrap();
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
