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

