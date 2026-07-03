fn line(v: serde_json::Value) -> String {
    v.to_string()
}

// P0 — interrupt marker must match EXACTLY (provider_state/claude.py:75 `== _INTERRUPT_TEXT`),
// not `.contains()`. A transcript that merely QUOTES the marker must stay Unknown
// (ping-blocked); only the exact text is idle-eligible IdleInterrupted. §11 wrong-direction.
#[test]
fn p2_claude_interrupt_requires_exact_marker_text() {
    let quote = classify(
        Provider::ClaudeCode,
        &line(serde_json::json!({"type":"user","uuid":"u1","message":{"content":[
            {"type":"text","text":"prefix [Request interrupted by user] suffix"}]}})),
        ProcessLiveness::Alive,
        0.0,
    )
    .unwrap();
    assert_eq!(quote.state, TurnState::Unknown, "merely quoting the marker must NOT be idle-eligible");
    assert_eq!(quote.reason, "no_turn_lifecycle_fact");

    let exact = classify(
        Provider::ClaudeCode,
        &line(serde_json::json!({"type":"user","uuid":"u1","message":{"content":[
            {"type":"text","text":"[Request interrupted by user]"}]}})),
        ProcessLiveness::Alive,
        0.0,
    )
    .unwrap();
    assert_eq!(exact.state, TurnState::IdleInterrupted);
    assert_eq!(exact.turn_id.as_ref().map(TurnId::as_str), Some("u1"));
}

// P1 — claude api_error fault requires level=="error" (claude.py:54). Missing/other
// level → NO fault (Python count 0).
#[test]
fn p2_claude_api_error_fault_requires_level_error() {
    let no_level = vec![serde_json::json!({"type":"system","subtype":"api_error","sessionId":"s-1"})];
    assert!(read_fault_facts(&no_level, Provider::ClaudeCode).is_empty(), "api_error w/o level=error is not a fault");
    let warn = vec![serde_json::json!({"type":"system","subtype":"api_error","level":"warning","sessionId":"s-1"})];
    assert!(read_fault_facts(&warn, Provider::ClaudeCode).is_empty(), "level=warning is not a fault");

    let err = vec![serde_json::json!({"type":"system","subtype":"api_error","level":"error","sessionId":"s-1"})];
    let facts = read_fault_facts(&err, Provider::ClaudeCode);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].signature.as_str(), "api_error");
    assert_eq!(facts[0].turn_id.as_ref().map(TurnId::as_str), Some("s-1"));
    assert_eq!(facts[0].api_error_status, None);
    assert_eq!(facts[0].error.as_deref(), None);
    assert_eq!(facts[0].request_id.as_deref(), None);
    assert_eq!(facts[0].assistant_uuid.as_deref(), None);
}

// P1 — claude api_error turn_id fallback chain = sessionId -> parentUuid -> uuid
// (claude.py:58), NOT sessionId -> requestId. Collapsing to None breaks C8 dedup.
#[test]
fn p2_claude_api_error_turn_id_fallback_parentuuid_then_uuid() {
    let pu = vec![serde_json::json!({"type":"system","subtype":"api_error","level":"error","parentUuid":"pu-1"})];
    let f = read_fault_facts(&pu, Provider::ClaudeCode);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].turn_id.as_ref().map(TurnId::as_str), Some("pu-1"), "parentUuid is in the chain (requestId is not)");

    let uu = vec![serde_json::json!({"type":"system","subtype":"api_error","level":"error","uuid":"uu-1"})];
    let f2 = read_fault_facts(&uu, Provider::ClaudeCode);
    assert_eq!(f2.len(), 1);
    assert_eq!(f2[0].turn_id.as_ref().map(TurnId::as_str), Some("uu-1"));
}

#[test]
fn p2_claude_assistant_api_error_fault_uses_uuid_and_structured_details() {
    let records = vec![serde_json::json!({
        "type": "assistant",
        "parentUuid": "parent-1",
        "uuid": "assistant-1",
        "requestId": "req_011CceNfWj2aPY5gtCdakULt",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "There's an issue with the selected model."}
        ]},
        "error": "model_not_found",
        "isApiErrorMessage": true,
        "apiErrorStatus": 404,
        "sessionId": "session-1",
        "version": "2.1.181"
    })];

    let facts = read_fault_facts(&records, Provider::ClaudeCode);

    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].signature.as_str(), "api_error");
    assert_eq!(facts[0].turn_id.as_ref().map(TurnId::as_str), Some("assistant-1"));
    assert_eq!(facts[0].api_error_status, Some(404));
    assert_eq!(facts[0].error.as_deref(), Some("model_not_found"));
    assert_eq!(facts[0].request_id.as_deref(), Some("req_011CceNfWj2aPY5gtCdakULt"));
    assert_eq!(facts[0].assistant_uuid.as_deref(), Some("assistant-1"));
}

// P1 — codex requestApproval turn_id = params.turnId OR params.turn_id (codex.py:79).
#[test]
fn p2_codex_approval_turn_id_accepts_snake_case() {
    let snake = vec![serde_json::json!({"jsonrpc":"2.0","method":"session/requestApproval","params":{"turn_id":"snake1"}})];
    let f = read_fault_facts(&snake, Provider::Codex);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].turn_id.as_ref().map(TurnId::as_str), Some("snake1"), "snake-case turn_id must be honored");
}

// P1 — codex app-server turn/completed status completed/interrupted/inProgress map to
// idle/idle_interrupted/working (codex.py:69-74), not Unknown.
#[test]
fn p2_codex_app_server_status_completed_interrupted_in_progress() {
    let app = |status: &str| {
        line(serde_json::json!({"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"t1","status":status}}}))
    };
    let c = classify(Provider::Codex, &app("completed"), ProcessLiveness::Alive, 0.0).unwrap();
    assert_eq!((c.state, c.reason.as_str()), (TurnState::Idle, "completed"));
    assert_eq!(c.turn_id.as_ref().map(TurnId::as_str), Some("t1"));

    let i = classify(Provider::Codex, &app("interrupted"), ProcessLiveness::Alive, 0.0).unwrap();
    assert_eq!((i.state, i.reason.as_str()), (TurnState::IdleInterrupted, "interrupted"));

    let p = classify(Provider::Codex, &app("inProgress"), ProcessLiveness::Alive, 0.0).unwrap();
    assert_eq!((p.state, p.reason.as_str()), (TurnState::Working, "open_turn"));
    assert_eq!(p.turn_id.as_ref().map(TurnId::as_str), Some("t1"));
}

// P1 — codex event_msg task_started → open turn → working(alive) (codex.py:30-31).
#[test]
fn p2_codex_event_msg_task_started_is_open_turn() {
    let txt = line(serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"ts1"}}));
    let r = classify(Provider::Codex, &txt, ProcessLiveness::Alive, 0.0).unwrap();
    assert_eq!((r.state, r.reason.as_str()), (TurnState::Working, "open_turn"));
    assert_eq!(r.turn_id.as_ref().map(TurnId::as_str), Some("ts1"));
}

// SPAWN+FAKE-WORKER RED — Provider::Fake::build_command must invoke the fake-worker backing program
// (the single-binary `fake-worker` subcommand), NOT the bare placeholder vec!["fake"] (no backing
// binary). This is what makes launch(dry_run=false)'s spawn path exercisable with NO subscription
// provider. Golden intent: fake_worker.py + provider_cli/fake.py — a subscription-free backing worker.
#[test]
fn fake_build_command_invokes_fake_worker_not_bare_fake() {
    let adapter = get_adapter(Provider::Fake);
    let argv = adapter
        .build_command(AuthMode::Subscription, None, None, None)
        .expect("fake build_command");
    assert_ne!(
        argv,
        vec!["fake".to_string()],
        "Provider::Fake::build_command must not be the bare placeholder vec![\"fake\"] (no backing binary)"
    );
    assert!(
        argv.iter().any(|a| a == "fake-worker"),
        "Provider::Fake::build_command must invoke the `fake-worker` subcommand so the spawn path runs the fake backing worker; got {argv:?}"
    );
}
