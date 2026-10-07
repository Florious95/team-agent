#[test]
fn trimmed_provider_builders_preserve_fresh_and_resume_argv() {
    for (provider, base) in [
        (
            Provider::Grok,
            vec!["grok", "--model", "test-model", "--rules", "role"],
        ),
        (
            Provider::CursorAgent,
            vec!["agent", "--model", "test-model", "--workspace", "{workspace}"],
        ),
        (
            Provider::Copilot,
            vec![
                "copilot", "--no-color", "--no-auto-update", "--no-remote",
                "--disable-builtin-mcps", "--allow-tool", "team_orchestrator",
                "--model", "test-model",
            ],
        ),
    ] {
        let adapter = get_adapter(provider);
        let ctx = ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: None,
            system_prompt: Some("role"),
            model: Some("test-model"),
            dangerously_skip_permissions: false,
            profile_launch: None,
            agent_id_hint: Some("worker"),
            effort: None,
        };
        let mut expected = base.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let fresh = adapter.build_command_plan(ctx).expect("fresh plan");
        if provider != Provider::CursorAgent {
            expected.push("--session-id".to_string());
            expected.push(
                fresh.expected_session_id.as_ref().expect("session hint").as_str().to_string(),
            );
        } else {
            assert!(fresh.expected_session_id.is_none());
        }
        assert_eq!(fresh.argv, expected, "{provider:?} fresh argv");
        let session = SessionId::new("existing-session");
        let mut expected = base.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        expected.extend(["--resume".to_string(), session.as_str().to_string()]);
        assert_eq!(
            adapter
                .build_resume_command_plan(Some(&session), ctx)
                .expect("resume plan")
                .argv,
            expected,
            "{provider:?} resume argv"
        );
        if provider == Provider::CursorAgent {
            let mut expected = base.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            expected.insert(1, "--resume".to_string());
            expected.insert(2, session.as_str().to_string());
            assert_eq!(
                adapter
                    .build_resume_command_with_context(
                        Some(&session), ctx.auth_mode, None, ctx.system_prompt, ctx.model, false,
                    )
                    .expect("legacy resume"),
                expected,
                "legacy Cursor resume must retain its distinct flag order"
            );
        }
    }
}

#[test]
fn abnormal_dedup_key_uses_signature_and_optional_turn_id() {
    // probe(/tmp/probe_idle.py read_fault_facts): the C8 dedup key is
    // (Signature, Option<TurnId>). Golden facts below are the exact extraction.

    // claude api_error → signature=api_error, turn_id=sess-9 (sessionId), kind=error.
    let api_err = [serde_json::json!(
        {"type":"system","subtype":"api_error","level":"error","sessionId":"sess-9"}
    )];
    let f = read_fault_facts(&api_err, Provider::Claude);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].signature, Signature::new("api_error"));
    assert_eq!(f[0].turn_id, Some(TurnId::new("sess-9")));
    assert_eq!(f[0].kind, FactKind::Error);

    // claude tool_result is_error → signature=tool_result_is_error,
    // turn_id=parentUuid ("p-1"), kind=error.
    let tool_err = [serde_json::json!(
        {"type":"user","uuid":"u","parentUuid":"p-1",
         "message":{"content":[{"type":"tool_result","is_error":true}]}}
    )];
    let f = read_fault_facts(&tool_err, Provider::Claude);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].signature, Signature::new("tool_result_is_error"));
    assert_eq!(f[0].turn_id, Some(TurnId::new("p-1")));

    // claude api_error with NO ids → key = (api_error, None) — Option must hold None.
    let api_err_noids = [serde_json::json!(
        {"type":"system","subtype":"api_error","level":"error"}
    )];
    let f = read_fault_facts(&api_err_noids, Provider::Claude);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].signature, Signature::new("api_error"));
    assert_eq!(f[0].turn_id, None, "api_error with no ids dedups on (api_error, None)");

    // codex turn_failed → signature=turn_failed, turn_id=ct4, kind=failed.
    let codex_failed = [serde_json::json!(
        {"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"ct4","status":"failed"}}}
    )];
    let f = read_fault_facts(&codex_failed, Provider::Codex);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].signature, Signature::new("turn_failed"));
    assert_eq!(f[0].turn_id, Some(TurnId::new("ct4")));
    assert_eq!(f[0].kind, FactKind::Failed);

    // codex approval → signature=approval_required, turn_id=ct5, kind=approval.
    let codex_approval = [serde_json::json!(
        {"jsonrpc":"2.0","method":"session/requestApproval","params":{"turnId":"ct5"}}
    )];
    let f = read_fault_facts(&codex_approval, Provider::Codex);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].signature, Signature::new("approval_required"));
    assert_eq!(f[0].turn_id, Some(TurnId::new("ct5")));
    assert_eq!(f[0].kind, FactKind::Approval);
}

// ---- (e) session capture payload + bug-085 fallback (confidence=low) ----

#[test]
fn capture_session_id_success_payload_fields() {
    // claude.py:73-106 success → CapturedSession high confidence, fs_watch,
    // session_id Some, rollout_path Some. Drive the real fn against a temp cwd
    // that contains a discoverable transcript; assert ALL 5 typed fields.
    let dir = std::env::temp_dir().join(format!(
        "ta-cap-success-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let transcript = dir.join("session.jsonl");
    let _ = std::fs::write(
        &transcript,
        r#"{"type":"user","sessionId":"sess-1","cwd":"PLACEHOLDER","message":{"content":"hi"}}"#,
    );
    let adapter = get_adapter(Provider::ClaudeCode);
    let cs = adapter
        .capture_session_id("agentX", &dir, 3)
        .expect("capture ok")
        .expect("transcript found → Some");
    assert_eq!(cs.captured_via, CaptureVia::FsWatch);
    assert_eq!(cs.attribution_confidence, Confidence::High);
    assert!(cs.session_id.is_some(), "found transcript yields a session_id");
    assert!(cs.rollout_path.is_some(), "found transcript yields a rollout_path");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_session_id_bug085_compatible_api_fallback_is_low_confidence_none_session() {
    // probe(claude fallback): compatible_api transcript w/o a session_id →
    // session_id=None, captured_via=fs_mtime_fallback, confidence=low,
    // rollout_path SET. The half-state is LEGAL and must not panic (bug-085).
    let dir = std::env::temp_dir().join(format!(
        "ta-cap-fallback-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let transcript = dir.join("nosession.jsonl");
    let _ = std::fs::write(&transcript, r#"{"type":"user","message":{"content":"hi"}}"#);
    let adapter = get_adapter(Provider::ClaudeCode);
    let cs = adapter
        .capture_session_id("agentX", &dir, 3)
        .expect("capture ok")
        .expect("fallback transcript → Some half-state");
    assert_eq!(cs.session_id, None, "bug-085: fallback session_id is None");
    assert_eq!(cs.captured_via, CaptureVia::FsMtimeFallback);
    assert_eq!(cs.attribution_confidence, Confidence::Low);
    assert!(cs.rollout_path.is_some(), "fallback still pins rollout_path");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_session_id_no_match_returns_none() {
    // claude.py:111-113 deadline with no match → Ok(None), explicitly NOT Err.
    let empty = std::env::temp_dir().join(format!("ta-cap-empty-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&empty);
    let adapter = get_adapter(Provider::ClaudeCode);
    let res = adapter.capture_session_id("agentX", &empty, 0);
    // ProviderError 非 PartialEq(携 Io)→ 用 matches! 而非 ==。
    assert!(matches!(res, Ok(None)), "no transcript + timeout 0 → Ok(None), never Err");
    let _ = std::fs::remove_dir_all(&empty);
}

// ---- resume / fork / build_command / caps — command-build core ----

#[test]
fn claude_caps_resume_and_fork_and_native_mcp() {
    // doc §59 + claude.py: claude supports resume + fork (static cap true,
    // runtime auth-gated) + native --mcp-config; does NOT write a global settings file.
    let adapter = get_adapter(Provider::ClaudeCode);
    let caps = adapter.caps();
    assert_eq!(
        caps,
        ProviderCaps {
            resume: true,
            fork: true,
            native_mcp_config: true,
            writes_global_settings: false,
        }
    );
}

#[test]
fn gemini_writes_global_settings_cap() {
    // gemini.py:40-78 — install_mcp writes ~/.gemini/settings.json →
    // caps.writes_global_settings=true, native_mcp_config=false.
    let adapter = get_adapter(Provider::GeminiCli);
    let caps = adapter.caps();
    assert!(caps.writes_global_settings, "gemini writes a global settings file");
    assert!(!caps.native_mcp_config, "gemini has no native --mcp-config flag");
}

#[test]
fn claude_fork_blocked_under_compatible_api() {
    // claude.py:54 supports_session_fork == (auth_mode != compatible_api).
    // fork under CompatibleApi must Err (never silently empty) — capability path.
    let adapter = get_adapter(Provider::ClaudeCode);
    let sid = SessionId::new("src-1");
    let res = adapter.fork(Some(&sid), AuthMode::CompatibleApi, None);
    assert!(
        matches!(
            res,
            Err(ProviderError::CapabilityUnsupported(_)) | Err(ProviderError::ResumeUnavailable(_))
        ),
        "fork under compatible_api must Err, got {res:?}"
    );
}

#[test]
fn build_resume_command_without_session_id_is_resume_unavailable() {
    // claude.py:41-42 — no session_id → ResumeUnavailable, never a bare crash.
    let adapter = get_adapter(Provider::ClaudeCode);
    let res = adapter.build_resume_command(None, AuthMode::Subscription, None);
    assert!(
        matches!(res, Err(ProviderError::ResumeUnavailable(_))),
        "resume without session_id must be ResumeUnavailable, got {res:?}"
    );
}

#[test]
fn session_is_resumable_none_session_is_false_not_panic() {
    // claude.py:116-118 — session_id falsy → not resumable; bug-085 None穿透.
    let adapter = get_adapter(Provider::ClaudeCode);
    let res = adapter.session_is_resumable(None, AuthMode::CompatibleApi);
    assert!(matches!(res, Ok(false)), "None session → Ok(false), never panic (bug-085)");
}

/// true iff `needle` (e.g. ["--model","opus"]) appears as a contiguous run in `hay`.
fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    if needle.is_empty() {
        return true;
    }
    hay.windows(needle.len())
        .any(|w| w.iter().zip(needle).all(|(a, b)| a == b))
}

#[test]
fn build_command_includes_model_and_system_prompt() {
    // claude.py:175-199 — _base_command appends --model <m> + --append-system-prompt <p>;
    // --strict-mcp-config only appears when mcp_config is present. With mcp_config=None,
    // assert both flag pairs present adjacently AND --strict-mcp-config ABSENT.
    let adapter = get_adapter(Provider::ClaudeCode);
    let argv = adapter
        .build_command(AuthMode::Subscription, None, Some("be helpful"), Some("opus"))
        .expect("build_command ok");
    assert!(
        argv_contains_adjacent(&argv, &["--model", "opus"]),
        "argv must contain `--model opus` adjacency: {argv:?}"
    );
    assert!(
        argv_contains_adjacent(&argv, &["--append-system-prompt", "be helpful"]),
        "argv must contain `--append-system-prompt 'be helpful'`: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--strict-mcp-config"),
        "no mcp_config → --strict-mcp-config must be absent: {argv:?}"
    );
}

#[test]
fn claude_mcp_config_does_not_enable_strict_mode() {
    let adapter = get_adapter(Provider::ClaudeCode);
    let config = adapter
        .mcp_config(AuthMode::Subscription)
        .expect("mcp config");
    let argv = adapter
        .build_command(
            AuthMode::Subscription,
            Some(&config),
            Some("worker"),
            Some("opus"),
        )
        .expect("build command with mcp config");

    assert!(
        argv_contains_adjacent(&argv, &["--mcp-config"]),
        "Claude worker argv must still pass Team Agent MCP config: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "--strict-mcp-config"),
        "--mcp-config must merge with user-level Claude MCP; strict mode hides user MCP: {argv:?}"
    );
}

#[test]
fn unsupported_provider_capability_error_message_shape() {
    // unsupported.py:31 ProviderCapabilityError — placeholder providers reject
    // on call. The skeleton Provider enum has no Copilot/Opencode variant, so
    // the unsupported path is exercised through ProviderError::CapabilityUnsupported
    // construction (message contract) here; full plug dispatch deferred.
    let e = ProviderError::CapabilityUnsupported("opencode:start".to_string());
    assert_eq!(
        e.to_string(),
        "provider capability unsupported: opencode:start"
    );
    let e2 = ProviderError::ResumeUnavailable("claude resume requires session_id".to_string());
    assert_eq!(
        e2.to_string(),
        "resume unavailable: claude resume requires session_id"
    );
}

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════
// Lock the CORRECT Python v0.2.11 behavior the strengthened contracts missed.
// Golden re-probed via /tmp/probe_p2_provider.py vs team-agent-public @ 439bef8.

// ═══════════════ 0.5.67 provider-adapter (Grok + CursorAgent) ═══════════════
// 仿 claude build_command tests 结构 (见 build_command_includes_model_and_system_prompt)。
// 不跑 cargo (禁 cargo 红线), 静态编码 + leader verify 时执行。

#[test]
fn test_grok_build_command_includes_rules_flag_when_system_prompt_set() {
    let adapter = get_adapter(Provider::Grok);
    let argv = adapter
        .build_command(AuthMode::Subscription, None, Some("be helpful"), Some("grok-4"))
        .expect("build_command ok");
    assert!(
        argv_contains_adjacent(&argv, &["--model", "grok-4"]),
        "argv must contain `--model grok-4` adjacency: {argv:?}"
    );
    assert!(
        argv_contains_adjacent(&argv, &["--rules", "be helpful"]),
        "grok append-system-prompt must be `--rules 'be helpful'`: {argv:?}"
    );
}

#[test]
fn test_grok_build_command_bypass_flag_when_dangerous() {
    let adapter = get_adapter(Provider::Grok);
    let dangerous = adapter
        .build_command_with_permissions(
            AuthMode::Subscription,
            None,
            None,
            Some("grok-4"),
            true,
        )
        .expect("dangerous build_command ok");
    assert!(
        argv_contains_adjacent(&dangerous, &["--always-approve"]),
        "dangerous grok must include `--always-approve`: {dangerous:?}"
    );
    let safe = adapter
        .build_command(AuthMode::Subscription, None, None, Some("grok-4"))
        .expect("safe build_command ok");
    assert!(
        !safe.iter().any(|a| a == "--always-approve"),
        "non-dangerous grok must not include --always-approve: {safe:?}"
    );
}

#[test]
fn test_grok_build_command_omits_missing_model() {
    let adapter = get_adapter(Provider::Grok);
    let argv = adapter
        .build_command(AuthMode::Subscription, None, None, None)
        .expect("missing model preserves the provider-native default");
    assert!(
        !argv.iter().any(|arg| arg == "--model"),
        "missing model must not fabricate a --model argument: {argv:?}"
    );
    assert!(
        argv_contains_adjacent(
            &adapter
                .build_command(AuthMode::Subscription, None, None, Some("grok-4.6"))
                .expect("explicit model must build"),
            &["--model", "grok-4.6"]
        ),
        "explicit model must still become adjacent `--model grok-4.6`"
    );
}

#[test]
fn test_cursor_agent_build_command_includes_workspace_flag() {
    let adapter = get_adapter(Provider::CursorAgent);
    let argv = adapter
        .build_command(AuthMode::Subscription, None, Some("be helpful"), Some("sonnet-4-thinking"))
        .expect("build_command ok");
    // system_prompt 不入 argv (方案 1 变体走 workspace rules 文件), argv 只带
    // --workspace {workspace} placeholder (spawn 时 fill_spawn_placeholders 替换)。
    assert!(
        argv_contains_adjacent(&argv, &["--workspace", "{workspace}"]),
        "cursor argv must carry `--workspace {{workspace}}`: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "--trust" || arg == "--sandbox"),
        "safe Cursor command must not opt into trust or disabled sandbox: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--rules" || a == "--append-system-prompt"),
        "cursor append-system-prompt must NOT go on argv (workspace rules file): {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--system-prompt" || a == "--allowed-tools"),
        "undocumented flags must stay off the main path: {argv:?}"
    );
}

#[test]
fn test_cursor_agent_build_command_bypass_flag_when_dangerous() {
    let adapter = get_adapter(Provider::CursorAgent);
    let dangerous = adapter
        .build_command_with_permissions(
            AuthMode::Subscription,
            None,
            None,
            None,
            true,
        )
        .expect("dangerous build_command ok");
    assert!(
        argv_contains_adjacent(&dangerous, &["--force"]),
        "dangerous cursor must include `--force`: {dangerous:?}"
    );
    let safe = adapter
        .build_command(AuthMode::Subscription, None, None, None)
        .expect("safe build_command ok");
    assert!(
        !safe.iter().any(|a| a == "--force"),
        "non-dangerous cursor must not include --force: {safe:?}"
    );
}

#[test]
fn test_cursor_agent_accepts_mcp_config_without_putting_it_on_argv() {
    let adapter = get_adapter(Provider::CursorAgent);
    let cfg = McpConfig {
        raw: serde_json::json!({
            "team_orchestrator": {
                "command": "/bin/team-agent",
                "args": ["mcp-server"]
            }
        }),
    };
    let argv = adapter
        .build_command(AuthMode::Subscription, Some(&cfg), None, None)
        .expect("cursor MCP is launch overlay, not an argv capability hole");
    assert!(
        !argv.iter().any(|a| a == "--mcp-config" || a.contains("mcp")),
        "cursor has no --mcp-config flag; overlay writes .cursor/mcp.json: {argv:?}"
    );
    assert!(
        argv_contains_adjacent(&argv, &["--workspace", "{workspace}"]),
        "workspace flag must survive an mcp_config argument: {argv:?}"
    );
}

#[test]
fn test_grok_build_command_plan_binds_expected_session_id_to_argv() {
    let adapter = get_adapter(Provider::Grok);
    let plan = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: None,
            system_prompt: None,
            model: Some("grok-4.6"),
            dangerously_skip_permissions: false,
            profile_launch: None,
            agent_id_hint: Some("w1"),
            effort: None,
        })
        .expect("grok plan");
    let sid = plan
        .expected_session_id
        .as_ref()
        .map(|s| s.as_str())
        .expect("grok fresh plan must carry expected_session_id");
    assert!(
        !sid.is_empty(),
        "expected_session_id must be a non-empty uuid"
    );
    assert!(
        argv_contains_adjacent(&plan.argv, &["--session-id", sid]),
        "argv --session-id must match expected_session_id; argv={:?}",
        plan.argv
    );
    assert!(
        !plan.argv.iter().any(|a| a == "--resume"),
        "fresh grok plan must not include --resume; argv={:?}",
        plan.argv
    );
}

#[test]
fn test_cursor_auth_hint_subscription_is_unknown_not_present() {
    let adapter = get_adapter(Provider::CursorAgent);
    let hint = adapter.auth_hint(AuthMode::Subscription);
    assert_ne!(
        hint,
        crate::provider::types::AuthHintStatus::Present,
        "U-17 前不得把未观测写成 Present"
    );
    assert_ne!(hint, crate::provider::types::AuthHintStatus::PresentWeak);
    assert_eq!(hint, crate::provider::types::AuthHintStatus::Unknown);
}

#[test]
fn test_cursor_fresh_plan_has_no_session_id_and_no_resume() {
    let adapter = get_adapter(Provider::CursorAgent);
    let plan = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: None,
            system_prompt: None,
            model: Some("sonnet-4-thinking"),
            dangerously_skip_permissions: false,
            profile_launch: None,
            agent_id_hint: Some("w1"),
            effort: None,
        })
        .expect("cursor plan");
    assert!(
        plan.expected_session_id.is_none(),
        "U-01 未过不得造假 pending; got {:?}",
        plan.expected_session_id
    );
    assert!(
        !plan.argv.iter().any(|a| a == "--session-id" || a == "--resume" || a == "--continue"),
        "fresh cursor argv must not invent session flags; argv={:?}",
        plan.argv
    );
    assert!(
        !plan
            .argv
            .iter()
            .any(|arg| arg == "--trust" || arg == "--sandbox"),
        "default Cursor plan must not opt into trust or disabled sandbox; argv={:?}",
        plan.argv
    );
}

#[test]
fn test_cursor_resume_plan_requires_chat_id_and_never_emits_empty_resume() {
    let adapter = get_adapter(Provider::CursorAgent);
    let ctx = ProviderCommandContext {
        auth_mode: AuthMode::Subscription,
        mcp_config: None,
        system_prompt: None,
        model: Some("sonnet-4-thinking"),
        dangerously_skip_permissions: false,
        profile_launch: None,
        agent_id_hint: Some("w1"),
        effort: None,
    };
    let missing = adapter.build_resume_command_plan(None, ctx);
    assert!(
        matches!(missing, Err(ProviderError::ResumeUnavailable(_))),
        "no chatId must not emit empty --resume; got {missing:?}"
    );
    let sid = SessionId::new("502896a1-72ba-4c53-9a86-b2da28780806");
    let plan = adapter
        .build_resume_command_plan(Some(&sid), ctx)
        .expect("resume plan");
    assert!(
        argv_contains_adjacent(&plan.argv, &["--resume", sid.as_str()]),
        "resume plan must hit CursorAgent --resume arm; argv={:?}",
        plan.argv
    );
    assert!(
        !plan.argv.iter().any(|a| a == "--session-id" || a == "--continue"),
        "resume must not mint --session-id/--continue; argv={:?}",
        plan.argv
    );
}

#[test]
fn pi_wire_roundtrip_requires_backing() {
    use crate::provider::wire::{
        aliases, command_name, parse_canonical_provider, parse_provider, provider_wire,
        requires_resume_backing,
    };

    let provider = Provider::Pi;
    assert_eq!(provider_wire(provider), "pi");
    assert_eq!(parse_provider("pi"), Some(provider));
    assert_eq!(parse_canonical_provider("pi"), Some(provider));
    assert_eq!(aliases(provider), &["pi"]);
    assert_eq!(command_name(provider), "pi");
    assert!(requires_resume_backing(provider));
}

#[test]
fn pi_caps_resume_true_fork_false_native_mcp_false_auth_unknown() {
    let adapter = get_adapter(Provider::Pi);
    assert_eq!(
        adapter.caps(),
        ProviderCaps {
            resume: true,
            fork: false,
            native_mcp_config: false,
            writes_global_settings: false,
        }
    );
    assert_eq!(
        adapter.auth_hint(AuthMode::Subscription),
        AuthHintStatus::Unknown
    );
}

#[test]
fn pi_fork_is_typed_capability_unsupported() {
    let adapter = get_adapter(Provider::Pi);
    let source = SessionId::new("7dcfed9a-88ce-45bf-b1e3-161696acfe89");
    let result = adapter.fork(Some(&source), AuthMode::Subscription, None);
    assert!(
        matches!(result, Err(ProviderError::CapabilityUnsupported(_))),
        "Pi fork must be a typed capability refusal, got {result:?}"
    );
    if let Err(error) = result {
        let text = error.to_string();
        assert!(text.contains("pi") && text.contains("fork"), "got {text}");
        assert!(!text.to_ascii_lowercase().contains("clone"), "got {text}");
    }
}

#[test]
fn pi_classify_has_no_lifecycle_facts() {
    let transcript = concat!(
        r#"{"type":"turn_started","turn_id":"turn-1"}"#,
        "\n",
        r#"{"type":"turn_completed","turn_id":"turn-1"}"#,
    );
    let classified = crate::provider::classify::classify(
        Provider::Pi,
        transcript,
        ProcessLiveness::Alive,
        60.0,
    )
    .expect("Pi classification must preserve unknown without a turn reader");
    assert_eq!(classified.state, TurnState::Unknown);
    assert_eq!(classified.reason, "no_turn_lifecycle_fact");

    let records = [serde_json::json!({
        "type": "system",
        "subtype": "api_error",
        "level": "error",
        "sessionId": "pi-session"
    })];
    assert!(
        crate::provider::faults::read_fault_facts(&records, Provider::Pi).is_empty(),
        "Pi has no JSONL lifecycle/fault reader"
    );
}
