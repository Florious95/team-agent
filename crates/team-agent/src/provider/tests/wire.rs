

#[test]
fn process_liveness_wire_strings_are_exact_python() {
    // provider_state/common.py:109 three-valued.
    assert_eq!(ser(&ProcessLiveness::Alive), "\"alive\"");
    assert_eq!(ser(&ProcessLiveness::Dead), "\"dead\"");
    assert_eq!(ser(&ProcessLiveness::Unverifiable), "\"unverifiable\"");
}

#[test]
fn capture_via_wire_strings_are_exact_python() {
    // claude.py:101/371, codex.py:84
    assert_eq!(ser(&CaptureVia::FsWatch), "\"fs_watch\"");
    assert_eq!(ser(&CaptureVia::FsMtimeFallback), "\"fs_mtime_fallback\"");
    assert_eq!(ser(&CaptureVia::FsRepair), "\"fs_repair\"");
}

#[test]
fn confidence_wire_strings_are_exact_python() {
    assert_eq!(ser(&Confidence::High), "\"high\"");
    assert_eq!(ser(&Confidence::Medium), "\"medium\"");
    assert_eq!(ser(&Confidence::Low), "\"low\"");
}



#[test]
fn approval_kind_and_auth_hint_wire_strings() {
    // parsing.py:30/55/65
    assert_eq!(ser(&ApprovalKind::McpTool), "\"mcp_tool\"");
    assert_eq!(ser(&ApprovalKind::Command), "\"command\"");
    assert_eq!(ser(&ApprovalKind::Unknown), "\"unknown\"");
    // adapter.py:38
    assert_eq!(ser(&AuthHintStatus::Present), "\"present\"");
    assert_eq!(ser(&AuthHintStatus::Missing), "\"missing\"");
    assert_eq!(
        ser(&AuthHintStatus::MissingOrUnknown),
        "\"missing_or_unknown\""
    );
    assert_eq!(ser(&AuthHintStatus::Unknown), "\"unknown\"");
}


#[test]
fn provider_aliases_share_one_wire_family_and_gemini_cli_renamed() {
    // model::enums::Provider re-export: claude / claude_code are distinct
    // wire keys (providers.py:38-44 ADAPTERS) but reader-side normalize to
    // claude (provider_state/__init__.py:88) — encoded behaviorally below.
    assert_eq!(ser(&Provider::Claude), "\"claude\"");
    assert_eq!(ser(&Provider::ClaudeCode), "\"claude_code\"");
    assert_eq!(ser(&Provider::Codex), "\"codex\"");
    assert_eq!(ser(&Provider::GeminiCli), "\"gemini_cli\"");
    assert_eq!(ser(&Provider::Fake), "\"fake\"");
    // auth_mode wire (profiles/constants.py:6)
    assert_eq!(ser(&AuthMode::CompatibleApi), "\"compatible_api\"");
    assert_eq!(ser(&AuthMode::Subscription), "\"subscription\"");
    assert_eq!(ser(&AuthMode::OfficialApi), "\"official_api\"");
}

// -------------------------------------------------------------------
// TIER 1 · PREDICATE CONTRACTS (§11 unknown≠idle / _CLOSING) — pure, locked
// -------------------------------------------------------------------



// -------------------------------------------------------------------
// TIER 1 · NEWTYPE / PAYLOAD shape contracts (bug-085 None穿透) — pure
// -------------------------------------------------------------------

#[test]
fn newtypes_are_serde_transparent() {
    // §3 id-混传 newtypes must serialize as the bare scalar (transparent).
    assert_eq!(ser(&SessionId::new("abc-123")), "\"abc-123\"");
    assert_eq!(
        ser(&ApprovalFingerprint::new("700dc5c0a9e4e3e8")),
        "\"700dc5c0a9e4e3e8\""
    );
    // RolloutPath transparent over the path string.
    assert_eq!(
        ser(&RolloutPath::new(PathBuf::from("/x/y.jsonl"))),
        "\"/x/y.jsonl\""
    );
}

#[test]
fn captured_session_bug085_fallback_shape_roundtrips_with_nulls() {
    // bug-085 (claude.py:365-372): compatible_api fallback yields
    // session_id=None, captured_via=fs_mtime_fallback, confidence=low,
    // rollout_path SET. None must serialize as JSON null (穷尽 Option).
    let cs = CapturedSession {
        session_id: None,
        rollout_path: Some(RolloutPath::new(PathBuf::from("/p/s.jsonl"))),
        captured_via: CaptureVia::FsMtimeFallback,
        attribution_confidence: Confidence::Low,
        spawn_cwd: PathBuf::from("/cwd"),
    };
    let j: serde_json::Value = serde_json::to_value(&cs).expect("to_value");
    assert!(j["session_id"].is_null(), "session_id must be JSON null");
    assert_eq!(j["captured_via"], "fs_mtime_fallback");
    assert_eq!(j["attribution_confidence"], "low");
    assert_eq!(j["rollout_path"], "/p/s.jsonl");
    // round trip back to the same struct.
    let back: CapturedSession = serde_json::from_value(j).expect("from_value");
    assert_eq!(back, cs);
}

// -------------------------------------------------------------------
// TIER 2 · BEHAVIORAL — drive through get_adapter(..) (unimplemented → RED)
// Golden semantics annotated; each test panics today at get_adapter and only
// greens when the adapter + classify/idle/approval pipeline is implemented.
// -------------------------------------------------------------------

// ---- (a) turn-state classify: rollout_path=None / unreadable → Unknown ----

// Golden probes (PYTHONPATH=…/src python3 /tmp/probe_classify.py against v0.2.11
// truth source): every state/reason/turn_id/annotations value below is the
// exact dict returned by provider_state.read_turn_state.

// Fixture builders — minimal JSONL transcripts that the readers recognize.
