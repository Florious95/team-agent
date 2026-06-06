fn node(id: &str, role: &str, state: &str) -> serde_json::Value {
    serde_json::json!({"node_id": id, "role": role, "state": state})
}

#[test]
fn idle_any_non_idle_node_blocks_ping_with_node_reason() {
    // probe: working → node_working, unknown → node_unknown,
    // blocked_on_human → node_blocked_on_human, abnormal → node_abnormal,
    // missing-state → node_unknown. should_ping=False every time.
    // BLOOD-LINE: Unknown node blocks the ping via NoPingReason::Node(Unknown).
    for (state, expect) in [
        ("working", NoPingReason::Node(TurnState::Working)),
        ("unknown", NoPingReason::Node(TurnState::Unknown)),
        ("blocked_on_human", NoPingReason::Node(TurnState::BlockedOnHuman)),
        ("abnormal", NoPingReason::Node(TurnState::Abnormal)),
    ] {
        let r = evaluate_takeover_reminder(&[node("w1", "worker", state)], None, 100.0, 60.0)
            .expect("evaluate ok");
        assert!(!r.should_ping, "{state} node must block ping");
        assert_eq!(r.reason, expect, "{state} → {}", expect.reason_str());
    }
    // The exact `node_<state>` wire strings the Python `_result` emits.
    assert_eq!(NoPingReason::Node(TurnState::Working).reason_str(), "node_working");
    assert_eq!(NoPingReason::Node(TurnState::Unknown).reason_str(), "node_unknown");
    assert_eq!(
        NoPingReason::Node(TurnState::BlockedOnHuman).reason_str(),
        "node_blocked_on_human"
    );
    assert_eq!(NoPingReason::Node(TurnState::Abnormal).reason_str(), "node_abnormal");
    // missing-state node → node_unknown (state defaults to unknown, idle_predicate.py:49).
    let missing = serde_json::json!({"node_id": "w1", "role": "worker"});
    let r = evaluate_takeover_reminder(&[missing], None, 100.0, 60.0).expect("evaluate ok");
    assert!(!r.should_ping);
    assert_eq!(r.reason, NoPingReason::Node(TurnState::Unknown));
}

#[test]
fn idle_all_idle_but_not_armed_blocks() {
    // probe all_idle_not_armed → should_ping=False, reason=not_armed_no_worker_turn.
    // Worker idle alone never arms; only a DELEGATED state arms the watch (C1).
    let r = evaluate_takeover_reminder(&[node("w1", "worker", "idle")], None, 100.0, 60.0)
        .expect("evaluate ok");
    assert!(!r.should_ping);
    assert_eq!(r.reason, NoPingReason::NotArmedNoWorkerTurn);
}

#[test]
fn idle_armed_debounce_active_then_ping() {
    // probe armed_debounce_active (all_idle_since=100, now=130, debounce=60,
    // elapsed=30 < 60) → should_ping=False, reason=debounce_active.
    let ms = serde_json::json!({
        "opened_worker_turn_since_ack": true,
        "all_idle_since": 100.0,
        "pinged_for_episode": serde_json::Value::Null
    });
    let active = evaluate_takeover_reminder(&[node("w1", "worker", "idle")], Some(&ms), 130.0, 60.0)
        .expect("evaluate ok");
    assert!(!active.should_ping);
    assert_eq!(active.reason, NoPingReason::DebounceActive);
    // probe armed_debounce_elapsed (now=160, elapsed=60 >= 60) →
    // should_ping=True, reason=all_idle_debounce_elapsed.
    let elapsed = evaluate_takeover_reminder(&[node("w1", "worker", "idle")], Some(&ms), 160.0, 60.0)
        .expect("evaluate ok");
    assert!(elapsed.should_ping, "ping must fire at/after debounce");
    assert_eq!(elapsed.reason, NoPingReason::AllIdleDebounceElapsed);
    assert!(elapsed.message.is_some(), "ping carries the stored neutral message");
}

#[test]
fn idle_suppressed_is_acknowledged_and_already_pinged_guard() {
    // probe armed_suppressed_acknowledged → should_ping=False, reason=acknowledged.
    let supp = serde_json::json!({
        "opened_worker_turn_since_ack": true,
        "suppressed": true,
        "all_idle_since": 100.0
    });
    let r = evaluate_takeover_reminder(&[node("w1", "worker", "idle")], Some(&supp), 200.0, 60.0)
        .expect("evaluate ok");
    assert!(!r.should_ping);
    assert_eq!(r.reason, NoPingReason::Acknowledged);
    // probe already_pinged_this_episode (pinged_for_episode == all_idle_since) →
    // should_ping=False, reason=already_pinged_this_episode.
    let pinged = serde_json::json!({
        "opened_worker_turn_since_ack": true,
        "all_idle_since": 100.0,
        "pinged_for_episode": 100.0
    });
    let r2 = evaluate_takeover_reminder(&[node("w1", "worker", "idle")], Some(&pinged), 200.0, 60.0)
        .expect("evaluate ok");
    assert!(!r2.should_ping);
    assert_eq!(r2.reason, NoPingReason::AlreadyPingedThisEpisode);
}

#[test]
fn idle_interrupted_counts_as_idle_and_appears_in_interrupted_nodes() {
    // probe interrupted_counts_idle_ping → should_ping=True AND
    // interrupted_nodes=["w1"] (C12: idle_interrupted is idle but annotated).
    let armed = serde_json::json!({
        "opened_worker_turn_since_ack": true,
        "all_idle_since": 100.0,
        "pinged_for_episode": serde_json::Value::Null
    });
    let r = evaluate_takeover_reminder(
        &[node("w1", "worker", "idle_interrupted")],
        Some(&armed),
        200.0,
        60.0,
    )
    .expect("evaluate ok");
    assert!(r.should_ping);
    assert_eq!(r.reason, NoPingReason::AllIdleDebounceElapsed);
    assert_eq!(r.interrupted_nodes, vec!["w1".to_string()]);
}

#[test]
fn idle_leader_activity_never_arms_but_leader_idle_allows_ping() {
    // probe leader_working_does_not_arm: leader-only working never arms; the
    // working node still BLOCKS the ping → reason=node_working, should_ping=False.
    let r = evaluate_takeover_reminder(&[node("leader", "leader", "working")], None, 100.0, 60.0)
        .expect("evaluate ok");
    assert!(!r.should_ping);
    assert_eq!(r.reason, NoPingReason::Node(TurnState::Working));
    // probe leader_and_worker_idle_armed_ping: once a WORKER opened a turn the
    // watch is armed; leader+worker both idle past debounce → should_ping=True.
    let armed = serde_json::json!({
        "opened_worker_turn_since_ack": true,
        "all_idle_since": 100.0,
        "pinged_for_episode": serde_json::Value::Null
    });
    let nodes = [node("leader", "leader", "idle"), node("w1", "worker", "idle")];
    let r2 = evaluate_takeover_reminder(&nodes, Some(&armed), 200.0, 60.0).expect("evaluate ok");
    assert!(r2.should_ping);
    assert_eq!(r2.reason, NoPingReason::AllIdleDebounceElapsed);
}

// ---- (c) trust-prompt recognizer (REAL fixtures, own-vs-foreign) ----
//
// NOTE: the own-vs-foreign trust recognizer lives in messaging/leader_panes.py
// (step 9/10 owns it per card §42). The provider.rs skeleton exposes only
// `status_patterns()` (idle/processing/trust regex set) — driven RED below.
// Full own-vs-foreign realpath judgement + truncated-workspace logic deferred.

// Real fixtures (mirrored into the rust workspace from team-agent-public).
const CLAUDE_IDLE_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../snapshot/fixtures/idle_prompts/claude_code_idle.txt"
));
const CODEX_IDLE_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../snapshot/fixtures/idle_prompts/codex_idle.txt"
));
const CODEX_WORKING_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../snapshot/fixtures/idle_prompts/codex_working.txt"
));

fn fixture_matches(re: &regex::Regex, fixture: &str) -> bool {
    fixture.lines().any(|l| re.is_match(l))
}

#[test]
fn claude_status_patterns_compile() {
    // provider_cli/claude.py:225 status_patterns idle=r"[>❯]\s".
    // The real fixture has prompt lines like "❯ /compact" → idle MUST match.
    // The processing pattern r"[✶✢✽✻✳·].*…" must NOT match those idle prompt lines.
    let adapter = get_adapter(Provider::ClaudeCode);
    let pats = adapter.status_patterns().expect("status_patterns ok");
    assert!(
        fixture_matches(&pats.idle, CLAUDE_IDLE_FIXTURE),
        "claude idle pattern must match a '❯ ' prompt line in the idle fixture"
    );
    assert!(
        pats.idle.is_match("❯ /compact"),
        "claude idle pattern matches the canonical prompt line"
    );
    assert!(
        !pats.processing.is_match("❯ /compact"),
        "claude processing pattern must NOT match an idle prompt line"
    );
}

#[test]
fn codex_status_patterns_compile() {
    // provider_cli/codex.py:140 idle=r"(›|❯|codex>)" processing=r"•.*esc to interrupt".
    // codex_idle.txt has a "› Find and fix a bug…" prompt → idle matches.
    // codex_working.txt has a "• …esc to interrupt" spinner → processing matches.
    let adapter = get_adapter(Provider::Codex);
    let pats = adapter.status_patterns().expect("status_patterns ok");
    assert!(
        fixture_matches(&pats.idle, CODEX_IDLE_FIXTURE),
        "codex idle pattern must match a '›' prompt line"
    );
    assert!(
        CODEX_WORKING_FIXTURE.lines().any(|l| pats.processing.is_match(l)),
        "codex processing pattern must match the 'esc to interrupt' spinner"
    );
    // The discriminator: a pure working-status footer line carries no prompt char.
    assert!(
        !pats.idle.is_match("  gpt-5.5 medium · /private/tmp/working"),
        "codex idle pattern must NOT match a bare status footer"
    );
}

// ---- (d) abnormal dedup key (signature, Option<TurnId>) ----
//
// NOTE: read_fault_facts lives in provider_state; not on the skeleton trait.
// Golden dedup keys recorded below; flagged deferred for the fault-facts entry.

