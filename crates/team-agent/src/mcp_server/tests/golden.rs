    #[test]
    fn compact_ok_whitelist_is_exact_23_key_golden_set_and_order() {
        let r = json!({
            "ok": true, "status": "accepted", "message_id": "m1", "to": "alice",
            "targets": ["a", "b"], "delivered_count": 2, "failed_count": 1, "submitted": true,
            "visible": true, "queued": 3, "durably_stored": true, "result_id": "r1",
            "task_id": "t1", "agent_id": "ag1", "new_agent_id": "ag2", "source_agent_id": "ag3",
            "role_file_sha": "sha", "session_id": "s1", "leader_notified": true,
            "notification_message_id": "nm", "notification_status": "sent",
            "notification_channel": "ch", "notification_event_id": "ev",
            // non-whitelist (golden DROPS these — Rust currently INVENTS the first three):
            "delivery_pending": true, "poll_via": "x", "state_file": "/p", "secret": "z"
        });
        let v = serde_json::to_value(compact_tool_result(&r).expect("ok compaction")).unwrap();
        assert_eq!(keys(&v), vec![
            "ok", "status", "message_id", "to", "targets", "delivered_count", "failed_count",
            "submitted", "visible", "queued", "durably_stored", "result_id", "task_id",
            "agent_id", "new_agent_id", "source_agent_id", "role_file_sha", "session_id",
            "leader_notified", "notification_message_id", "notification_status",
            "notification_channel", "notification_event_id",
        ]);
        // The invented Rust keys must NOT appear (golden has no such whitelist entries).
        assert!(v.get("delivery_pending").is_none(), "delivery_pending not in golden ok-whitelist");
        assert!(v.get("poll_via").is_none(), "poll_via not in golden ok-whitelist");
        assert!(v.get("state_file").is_none(), "state_file not in golden ok-whitelist");
        // golden drops everything but ok/status when only the invented keys are present:
        let only_extra = json!({"ok": true, "status": "accepted",
            "delivery_pending": true, "poll_via": "x", "state_file": "/p"});
        let v2 = serde_json::to_value(compact_tool_result(&only_extra).unwrap()).unwrap();
        assert_eq!(s(&v2), r#"{"ok":true,"status":"accepted"}"#);
    }

    // ── #30/#43/#50 compact error-whitelist: EXACT 16-key golden list + order ───
    // GOLDEN (probe_mcp_red.py ERR-FULL-KEYS): normalize.py:8-25 order, all 16 kept.
    #[test]
    fn compact_error_whitelist_is_exact_16_key_golden_set_and_order() {
        let r = json!({
            "ok": false, "status": "failed", "reason": "boom", "error": "boom detail",
            "message_id": "m1", "agent_id": "ag1", "new_agent_id": "ag2", "source_agent_id": "ag3",
            "role_file_sha": "sha", "session_id": "s1", "to": "alice", "targets": ["a"],
            "delivered_count": 0, "failed_count": 2, "fallback_path": "/fb", "suggestion": "try x",
            // not in error-whitelist → dropped:
            "result_id": "r1", "extra": "drop"
        });
        let v = serde_json::to_value(compact_tool_result(&r).expect("error compaction")).unwrap();
        assert_eq!(keys(&v), vec![
            "ok", "status", "reason", "error", "message_id", "agent_id", "new_agent_id",
            "source_agent_id", "role_file_sha", "session_id", "to", "targets",
            "delivered_count", "failed_count", "fallback_path", "suggestion",
        ]);
        assert!(v.get("result_id").is_none(), "result_id not in golden error-whitelist");
    }

    // ── #52 acknowledged_count is OK-PATH ONLY (normalize.py:62 inside else) ────
    // GOLDEN (probe_mcp_red.py ACK-ON-ERR): an ok:false result with acknowledged_messages
    // → {"ok":false,"status":"x"} (NO acknowledged_count). Rust adds it unconditionally.
    #[test]
    fn compact_acknowledged_count_not_added_on_error_path() {
        let r = json!({"ok": false, "status": "x", "acknowledged_messages": ["a", "b"]});
        let v = serde_json::to_value(compact_tool_result(&r).expect("error compaction")).unwrap();
        assert!(v.get("acknowledged_count").is_none(),
            "acknowledged_count is added only on the ok branch (normalize.py:62)");
        assert_eq!(s(&v), r#"{"ok":false,"status":"x"}"#);
    }

    // ── #34/#45/#58 normalize_change_kind: golden alias map + keyword set ───────
    // GOLDEN (probe_mcp_red.py CK ...): verified/verify→modified (NOT observed!);
    // inspected/inspect/observe→observed; edited/edit→modified; desc 'inspected'→observed.
    #[test]
    fn change_kind_golden_alias_and_keyword_set() {
        // verified/verify are NOT in the golden alias map → fall to keyword inference.
        // With empty desc, keyword scan finds no match → Modified (golden), NOT Observed.
        assert_eq!(normalize_change_kind(Some("verified"), ""), ChangeKind::Modified);
        assert_eq!(normalize_change_kind(Some("verify"), ""), ChangeKind::Modified);
        // inspected/inspect ARE in the golden alias map → Observed.
        assert_eq!(normalize_change_kind(Some("inspected"), ""), ChangeKind::Observed);
        assert_eq!(normalize_change_kind(Some("inspect"), ""), ChangeKind::Observed);
        assert_eq!(normalize_change_kind(Some("observe"), ""), ChangeKind::Observed);
        // edited/edit→Modified.
        assert_eq!(normalize_change_kind(Some("edited"), ""), ChangeKind::Modified);
        assert_eq!(normalize_change_kind(Some("edit"), ""), ChangeKind::Modified);
        // description keyword scan for observed includes 'inspected'.
        assert_eq!(normalize_change_kind(None, "inspected it"), ChangeKind::Observed);
    }

    // ── #40 normalize_result_status: 'partiallydone' (no underscore) → Partial ──
    // Re-anchored per cr verdict (refined, 2026-06-10): the old Python golden mapped
    // an unmatched literal to success (probe_mcp_red.py STATUS); RS deliberately
    // normalizes a NON-EMPTY unknown literal to Partial (MUST-NOT-13, P7-type fix).
    #[test]
    fn result_status_partiallydone_no_underscore_is_partial() {
        assert_eq!(normalize_result_status(Some("partiallydone")), ResultStatus::Partial);
        assert_eq!(normalize_result_status(Some("PartiallyDone")), ResultStatus::Partial);
    }

    // ── #35/#46/#53 normalize_changes: full alias set + empty-path SKIP ─────────
    // GOLDEN (probe_mcp_red.py CHG ...): path={path,file,filepath,filename};
    // kind={kind,type,action}; desc={description,summary,detail,details,message};
    // no-path item is SKIPPED (not a phantom path:"").
    #[test]
    fn normalize_changes_full_aliases_and_skip_empty_path() {
        // filepath alias honored; kind 'type-ignored' is not a real alias → keyword
        // inference on desc 'fb' → Modified.
        let v = normalize_changes(Some(&json!([{"filepath": "x.rs", "kind": "type-ignored"}])), "fb");
        assert_eq!(v, vec![NormalizedChange {
            path: "x.rs".to_string(), kind: ChangeKind::Modified, description: "fb".to_string()
        }]);
        // 'type' alias → kind resolution.
        let v2 = normalize_changes(Some(&json!([{"path": "p", "type": "create"}])), "fb");
        assert_eq!(v2[0].kind, ChangeKind::Created);
        // filename + detail aliases.
        let v3 = normalize_changes(Some(&json!([{"filename": "c.rs", "detail": "deet"}])), "fb");
        assert_eq!(v3, vec![NormalizedChange {
            path: "c.rs".to_string(), kind: ChangeKind::Modified, description: "deet".to_string()
        }]);
        // no path at all → SKIPPED (golden returns []).
        assert!(normalize_changes(Some(&json!([{"kind": "created", "description": "d"}])), "fb").is_empty());
    }

    // ── #35/#46/#54 normalize_tests: full aliases + scalar coercion + skip ──────
    // GOLDEN (probe_mcp_red.py TST ...): command={command,cmd,name,test};
    // detail={detail,output,stdout,stderr,summary,message} (NO 'details');
    // non-dict scalar (incl int) → {command:str(v),status:not_run}; no-command dict SKIPPED;
    // a bare non-list scalar value is wrapped via _items.
    #[test]
    fn normalize_tests_full_aliases_scalar_coerce_and_skip() {
        // name alias + output alias.
        let v = normalize_tests(Some(&json!([{"name": "t", "output": "O"}])));
        assert_eq!(v, vec![NormalizedTest {
            command: "t".to_string(), status: TestStatus::NotRun, detail: Some("O".to_string())
        }]);
        // int scalar coerced to "123".
        let vi = normalize_tests(Some(&json!([123])));
        assert_eq!(vi, vec![NormalizedTest {
            command: "123".to_string(), status: TestStatus::NotRun, detail: None
        }]);
        // dict with no command key → SKIPPED.
        assert!(normalize_tests(Some(&json!([{"status": "pass"}]))).is_empty());
        // non-list scalar value wrapped (_items): "x" → one test.
        let vw = normalize_tests(Some(&json!("x")));
        assert_eq!(vw, vec![NormalizedTest {
            command: "x".to_string(), status: TestStatus::NotRun, detail: None
        }]);
    }

    // ── #35/#46/#55 normalize_risks: aliases + 'level' + scalar coerce + skip ───
    // GOLDEN (probe_mcp_red.py RISK ...): desc={description,summary,detail,message};
    // severity={severity,level}; int scalar→{severity:low,description:str(v)};
    // no-description dict SKIPPED.
    #[test]
    fn normalize_risks_level_alias_scalar_coerce_and_skip() {
        // 'detail' description alias + 'level' severity alias (HIGH → high).
        let v = normalize_risks(Some(&json!([{"detail": "risky", "level": "HIGH"}])));
        assert_eq!(v, vec![NormalizedRisk {
            severity: RiskSeverity::High, description: "risky".to_string()
        }]);
        // int scalar coerced.
        let vi = normalize_risks(Some(&json!([5])));
        assert_eq!(vi, vec![NormalizedRisk {
            severity: RiskSeverity::Low, description: "5".to_string()
        }]);
        // dict with no description → SKIPPED.
        assert!(normalize_risks(Some(&json!([{"severity": "high"}]))).is_empty());
    }

    // ── #35/#46/#56 normalize_artifacts: aliases + scalar + skip ────────────────
    // GOLDEN (probe_mcp_red.py ART ...): path={path,file,filepath,filename};
    // desc={description,summary,detail} (default=path); no-path dict SKIPPED.
    #[test]
    fn normalize_artifacts_full_aliases_and_skip() {
        // 'file' path alias + 'detail' description alias.
        let v = normalize_artifacts(Some(&json!([{"file": "a.bin", "detail": "art"}])));
        assert_eq!(v, vec![NormalizedArtifact {
            path: "a.bin".to_string(), description: "art".to_string()
        }]);
        // dict with no path → SKIPPED.
        assert!(normalize_artifacts(Some(&json!([{"description": "d"}]))).is_empty());
    }

    // ── #35/#46/#57 normalize_next_actions: action/todo/message aliases ─────────
    // GOLDEN (probe_mcp_red.py NA ...): dict desc={description,summary,action,todo,message}.
    #[test]
    fn normalize_next_actions_action_todo_message_aliases() {
        let v = normalize_next_actions(Some(&json!([
            {"action": "a"}, {"todo": "t"}, {"message": "m"}
        ])));
        assert_eq!(v, vec![
            NormalizedNextAction { description: "a".to_string() },
            NormalizedNextAction { description: "t".to_string() },
            NormalizedNextAction { description: "m".to_string() },
        ]);
    }

    // ── #41 text_field: stringify non-string scalars (Python str()) ─────────────
    // GOLDEN (probe_mcp_red.py ENV-NUMERIC): numeric task_id/agent_id/summary
    // stringify to "123"/"456"/"42" (NOT the manual/unknown/completed fallbacks).
    #[test]
    fn report_envelope_stringifies_numeric_scalar_fields() {
        let env = normalize_report_envelope(&json!({"task_id": 123, "agent_id": 456, "summary": 42}));
        assert_eq!(env.task_id, TaskId::new("123"));
        assert_eq!(env.agent_id, AgentId::new("456"));
        assert_eq!(env.summary, "42");
    }

    // ── #28 handle_mcp initialize result KEY ORDER (server.py:55-59) ────────────
    // GOLDEN (probe_init evidence): protocolVersion, capabilities, serverInfo.
    // Rust currently emits protocolVersion, serverInfo, capabilities.
    #[test]
    fn handle_mcp_initialize_result_key_order_is_golden() {
        let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
        let resp = handle_mcp(&tools, &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "X"}
        })).unwrap().expect("initialize frame");
        let result = resp.result.unwrap();
        assert_eq!(keys(&result), vec!["protocolVersion", "capabilities", "serverInfo"]);
    }

    // ── #31 tools/call text field: json.dumps DEFAULT separators (spaces) ───────
    // GOLDEN (probe_mcp_red.py DUMPS-DEFAULT): text has a space after ':' and ','.
    // Rust serde_json::to_string is compact (no spaces).
    #[test]
    fn tool_call_text_field_uses_json_dumps_default_spacing() {
        let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
        let resp = handle_mcp(&tools, &json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "nope", "arguments": {}}
        })).unwrap().unwrap();
        let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        // The raw bytes are compared by MCP clients: golden has ": " and ", ".
        assert!(text.contains(r#""ok": false"#), "golden json.dumps puts a space after ':'");
        assert!(text.contains(r#""reason": "unknown_tool""#), "golden has space after ':' and ','");
        assert!(text.starts_with(r#"{"ok": false, "reason": "unknown_tool""#),
            "byte-stable golden prefix; got: {text}");
    }

    // ── #33/#39 dispatch: empty/missing tool → 'unknown tool None'; method fallback;
    //    quote tool → Python repr double-quote switch ───────────────────────────
    // GOLDEN (probe_dispatch_red.py): empty/missing tool message == "unknown tool None"
    // (Python repr of None, not ''); method fallback resolves the tool name; a tool
    // name containing a single quote reprs with DOUBLE quotes.
    #[test]
    fn dispatch_empty_and_missing_tool_message_is_unknown_tool_none() {
        let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
        // empty-string tool is falsy in Python → falls through to method (None).
        let e1 = dispatch(&tools, &json!({"tool": "", "arguments": {}})).expect_err("empty tool ⇒ Err");
        assert_eq!(e1.message, "unknown tool None");
        // no tool, no method at all.
        let e2 = dispatch(&tools, &json!({"arguments": {}})).expect_err("missing tool ⇒ Err");
        assert_eq!(e2.message, "unknown tool None");
    }

    #[test]
    fn dispatch_falls_back_to_method_key_for_tool_name() {
        // tool absent, method present and unknown → 'unknown tool 'nope'' (the method
        // value, not 'unknown tool None'). Rust currently only falls back to 'name'.
        let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
        let e = dispatch(&tools, &json!({"method": "nope", "params": {}})).expect_err("unknown ⇒ Err");
        assert_eq!(e.message, "unknown tool 'nope'");
    }

    #[test]
    fn dispatch_unknown_tool_with_quote_uses_python_repr_double_quotes() {
        // GOLDEN QUOTE-TOOL: repr("a'b") switches to double quotes → unknown tool "a'b".
        let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
        let e = dispatch(&tools, &json!({"tool": "a'b", "arguments": {}})).expect_err("unknown ⇒ Err");
        assert_eq!(e.message, "unknown tool \"a'b\"");
    }

    // ── #37 rpc id echoed VERBATIM: float 1.5 and bigint beyond i64 ─────────────
    // GOLDEN (probe_rpcid_red.py): id 1.5 → echoed 1.5; 99999999999999999999 → verbatim.
    // Rust collapses both to null (RpcId has only Int(i64)/Str/Null).
    #[test]
    fn rpc_id_float_echoed_verbatim() {
        let tools = TeamOrchestratorTools::with_identity(Path::new("/tmp/ws"), None, None);
        let resp = handle_mcp(&tools, &json!({
            "jsonrpc": "2.0", "id": 1.5, "method": "initialize", "params": {}
        })).unwrap().unwrap();
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["id"], json!(1.5), "float id echoed verbatim, not null");
    }

    // ── #51 dispatch_tool(SendMessage) WorkerAccepted returned VERBATIM ─────────
    // GOLDEN (probe_sendoutcome evidence + tools.py:176-183): worker-accepted dict is
    // returned DIRECTLY: keys [status, delivery_pending, poll_via, message_id]. Rust
    // dispatch_tool re-runs compact_tool_result, dropping delivery_pending/poll_via.
