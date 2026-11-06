    #[test]
    fn result_status_alias_chain() {
        // cr verdict (refined, 2026-06-10) three-way split: missing/null/empty stays
        // the parity-locked implicit success (Python :107); a NON-EMPTY unknown
        // literal is Partial, never silent success (MUST-NOT-13, diverges from :123).
        assert_eq!(normalize_result_status(None), ResultStatus::Success);
        assert_eq!(normalize_result_status(Some("")), ResultStatus::Success);
        assert_eq!(normalize_result_status(Some("weird")), ResultStatus::Partial);
        // ok/done/complete/completed/passed/pass → success
        for a in ["ok", "DONE", "complete", "completed", "passed", "pass"] {
            assert_eq!(normalize_result_status(Some(a)), ResultStatus::Success, "alias {a}");
        }
        assert_eq!(normalize_result_status(Some("blocked")), ResultStatus::Blocked);
        assert_eq!(normalize_result_status(Some("block")), ResultStatus::Blocked);
        assert_eq!(normalize_result_status(Some("failed")), ResultStatus::Failed);
        assert_eq!(normalize_result_status(Some("fail")), ResultStatus::Failed);
        assert_eq!(normalize_result_status(Some("error")), ResultStatus::Failed);
        assert_eq!(normalize_result_status(Some("partial")), ResultStatus::Partial);
        assert_eq!(normalize_result_status(Some("partially_done")), ResultStatus::Partial);
        // case/space/hyphen fold: "  Partially-Done " → partial
        assert_eq!(normalize_result_status(Some("  Partially-Done ")), ResultStatus::Partial);
    }

    // ════════════════════════════════════════════════════════════════════════
    // normalize_change_kind — alias map + description keyword fallback (normalize.py:145)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn change_kind_alias_and_keyword_inference() {
        // None + empty desc → Modified (final fallback)
        assert_eq!(normalize_change_kind(None, ""), ChangeKind::Modified);
        // exact canonical passes through
        assert_eq!(normalize_change_kind(Some("created"), "whatever"), ChangeKind::Created);
        // alias map
        assert_eq!(normalize_change_kind(Some("add"), ""), ChangeKind::Created);
        assert_eq!(normalize_change_kind(Some("updated"), ""), ChangeKind::Modified);
        // keyword inference from description when value absent
        assert_eq!(normalize_change_kind(None, "created a new file"), ChangeKind::Created);
        assert_eq!(normalize_change_kind(Some(""), "removed the thing"), ChangeKind::Deleted);
        assert_eq!(normalize_change_kind(None, "verified output"), ChangeKind::Observed);
        // unknown value + plain desc → Modified
        assert_eq!(normalize_change_kind(Some("zzz"), "plain text"), ChangeKind::Modified);
    }

    // ════════════════════════════════════════════════════════════════════════
    // normalize_test_status — alias + unknown→not_run (normalize.py:199)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn test_status_alias_chain() {
        assert_eq!(normalize_test_status(None), TestStatus::NotRun);
        assert_eq!(normalize_test_status(Some("weird")), TestStatus::NotRun);
        for a in ["pass", "OK", "success"] {
            assert_eq!(normalize_test_status(Some(a)), TestStatus::Passed, "alias {a}");
        }
        assert_eq!(normalize_test_status(Some("fail")), TestStatus::Failed);
        assert_eq!(normalize_test_status(Some("error")), TestStatus::Failed);
        assert_eq!(normalize_test_status(Some("notrun")), TestStatus::NotRun);
        assert_eq!(normalize_test_status(Some("skip")), TestStatus::Skipped);
        assert_eq!(normalize_test_status(Some("passed")), TestStatus::Passed);
    }

    // ════════════════════════════════════════════════════════════════════════
    // normalize_risk_severity — out-of-set → Low (normalize.py:226)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn risk_severity_out_of_set_is_low() {
        assert_eq!(normalize_risk_severity(None), RiskSeverity::Low);
        assert_eq!(normalize_risk_severity(Some("CRITICAL")), RiskSeverity::Low);
        assert_eq!(normalize_risk_severity(Some("low")), RiskSeverity::Low);
        assert_eq!(normalize_risk_severity(Some("medium")), RiskSeverity::Medium);
        assert_eq!(normalize_risk_severity(Some("high")), RiskSeverity::High);
    }

    // ════════════════════════════════════════════════════════════════════════
    // normalize_report_envelope — fixed schema_version + manual/unknown fallbacks
    // + child-list regularization (normalize.py:67)
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn report_envelope_empty_uses_fixed_fallbacks() {
        let env = normalize_report_envelope(&json!({}));
        assert_eq!(env.schema_version, "result_envelope_v1");
        assert_eq!(env.task_id, TaskId::new("manual")); // bug-085: NOT None
        assert_eq!(env.agent_id, AgentId::new("unknown"));
        assert_eq!(env.status, ResultStatus::Success);
        assert_eq!(env.summary, "completed"); // empty summary → "completed"
        assert!(env.changes.is_empty());
        assert!(env.tests.is_empty());
        assert!(env.risks.is_empty());
        assert!(env.artifacts.is_empty());
        assert!(env.next_actions.is_empty());
    }

    #[test]
    fn report_envelope_regularizes_children_and_blank_ids() {
        // blank summary/task_id/agent_id collapse to fallbacks; status alias "done"→success;
        // change uses file/action/summary aliases + keyword; bare test str → not_run.
        let env = normalize_report_envelope(&json!({
            "summary": "   ",
            "status": "done",
            "task_id": "  ",
            "agent_id": "",
            "changes": [{"file": "a.rs", "action": "add", "summary": "added a.rs"}],
            "tests": ["cargo test"],
        }));
        assert_eq!(env.summary, "completed");
        assert_eq!(env.task_id, TaskId::new("manual"));
        assert_eq!(env.agent_id, AgentId::new("unknown"));
        assert_eq!(env.status, ResultStatus::Success);
        assert_eq!(env.changes.len(), 1);
        assert_eq!(env.changes[0], NormalizedChange {
            path: "a.rs".to_string(),
            kind: ChangeKind::Created,        // "add" alias
            description: "added a.rs".to_string(),
        });
        assert_eq!(env.tests.len(), 1);
        assert_eq!(env.tests[0], NormalizedTest {
            command: "cargo test".to_string(),
            status: TestStatus::NotRun,       // bare string default
            detail: None,
        });
    }

    // ════════════════════════════════════════════════════════════════════════
    // compact_tool_result — ok whitelist + INSERTION ORDER (preserve_order on)
    // Golden: input {delivered_count,ok,message_id,status,to} (scrambled) emits
    // keys in WHITELIST order: ok,status,message_id,to,delivered_count.
    // ════════════════════════════════════════════════════════════════════════





    // ════════════════════════════════════════════════════════════════════════
    // ToolError envelope — REDUNDANT keys are load-bearing (server.py:98-106)
    // reason==error_code AND message==error, byte-for-byte.
    // ════════════════════════════════════════════════════════════════════════
    #[test]
    fn tool_error_envelope_redundant_keys_byte_stable() {
        let te = ToolError::new(ToolErrorReason::UnknownTool, "unknown tool 'foo'", "UnknownTool");
        let env = te.to_envelope();
        // exact golden from server._tool_error_result
        assert_eq!(env.get("ok"), Some(&json!(false)));
        assert_eq!(env.get("reason"), Some(&json!("unknown_tool")));
        assert_eq!(env.get("error_code"), Some(&json!("unknown_tool"))); // == reason
        assert_eq!(env.get("exc_type"), Some(&json!("UnknownTool")));
        assert_eq!(env.get("message"), Some(&json!("unknown tool 'foo'")));
        assert_eq!(env.get("error"), Some(&json!("unknown tool 'foo'"))); // == message
        // full byte-stable order: ok,reason,error_code,exc_type,message,error
        assert_eq!(keys(&env), vec!["ok", "reason", "error_code", "exc_type", "message", "error"]);
    }

    #[test]
    fn public_exception_message_scrub() {
        // newline→space, trim
        assert_eq!(
            ToolError::public_exception_message("  line1\nline2  ", "ValueError"),
            "line1 line2"
        );
        // empty → exc_type name
        assert_eq!(ToolError::public_exception_message("", "ValueError"), "ValueError");
        // truncate to 200 chars
        let long = "x".repeat(250);
        assert_eq!(ToolError::public_exception_message(&long, "ValueError").len(), 200);
    }

    // ════════════════════════════════════════════════════════════════════════
    // McpTool / RpcMethod wire mapping
    // ════════════════════════════════════════════════════════════════════════
