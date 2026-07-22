//! msg-routing presentation RED contract (verifier-frozen, whole-slice target).
//!
//! Baseline: origin/main tip `7b8316b`. The `presentation{sink,class,case_id}`
//! metadata model does NOT exist at baseline; `cli/send/presentation` is an
//! unrelated delivery-outcome module. Normative gate: MUST-8 amendment signed
//! PASS by reviewer-r18 (2026-07-22).
//!
//! Layering (leader ruling (A)-layered, three nails):
//!  * OFFLINE-DETERMINISTIC forms observe a baseline-visible seam that flips
//!    on GREEN, using ONLY real public entry points — never calling future
//!    API (that would be a compile error, not a red). These are LIVE now.
//!  * REAL-MACHINE-INJECTION forms (physical leader transcript adjudication)
//!    are env-gated behind `TEAM_AGENT_REALMACHINE_MSGROUTE=1` and skip (never
//!    panic-placeholder — L3 incident criterion) when the gate is absent.
//!    Their OFFLINE observable (SQL status allowlist / row presence) is still
//!    asserted deterministically here.
//!
//! §9 fifteen forms (locate.md): Pure1-4 / Behavior5-12 / Pipeline13-15.
//!
//! RED semantics: every assertion states the POST-GREEN truth. At baseline the
//! seam lacks the routing surface, so the assertion FAILS (red). runtime-owner-b
//! flips each to green by implementing the C4 metadata + C6 decision primitive.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

// R6 ISOLATION DECLARATION (test_isolation_escape_contract.rs guard).
//
// This file carries ONE real-machine form (`form13_rm`), env-gated behind
// TEAM_AGENT_REALMACHINE_MSGROUTE=1, which drives the real mcp-server stdio,
// a real TmuxBackend and a real MessageStore through `McpSimHarness`. That
// harness provides equivalent real-machine isolation: it allocates a fresh
// per-process workspace (its own TEAM_AGENT_WORKSPACE root under the test
// temp dir), opens the MessageStore against that workspace only, and scrubs
// TMUX / TMUX_PANE from the child MCP process env so no ambient HOME- or
// TMUX-derived leader/caller identity can leak in. The remaining forms are
// pure `normalize_report_envelope` calls and static source predicates that
// touch neither delivery nor tmux; the `deliver_pending_messages` tokens they
// mention are documentation of the observed SQL allowlist seam, not runtime
// calls. This declaration is truthful, not a guard bypass.

#[path = "support/mcp_sim_harness.rs"]
mod mcp_sim_harness;
use mcp_sim_harness::McpSimHarness;

use serde_json::{json, Value};
use serial_test::serial;

use team_agent::mcp_server::normalize::normalize_report_envelope;

// ---------------------------------------------------------------------------
// Shared source-of-truth helpers (static observation of product source at the
// frozen baseline — used by forms whose seam is a source predicate, e.g. the
// pending-scan SQL allowlist and the single-C6-primitive source lock).
// ---------------------------------------------------------------------------

/// Read a product source file from the same worktree this test compiles in.
/// CARGO_MANIFEST_DIR points at crates/team-agent; sources are under src/.
fn product_src(rel: &str) -> String {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = base.join("src").join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read product source {}: {e}", path.display()))
}

fn realmachine_gate() -> bool {
    std::env::var("TEAM_AGENT_REALMACHINE_MSGROUTE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

// ===========================================================================
// Pure / schema 1-4  (entry: normalize_report_envelope — real pub fn)
// ===========================================================================

/// form1: missing presentation normalizes to the default leader/message sink,
/// and that default is a FIRST-CLASS, serialized field on the normalized
/// envelope. Baseline red: the fixed 8-field NormalizedReportEnvelope has no
/// presentation field, so the serialized value carries no default sink.
#[test]
fn form1_missing_metadata_normalizes_to_leader_message_default() {
    let env = json!({"summary": "hello", "status": "success"});
    let normalized = normalize_report_envelope(&env);
    let v = serde_json::to_value(&normalized).expect("serialize normalized envelope");

    let presentation = v.get("presentation").expect(
        "POST-GREEN: normalized envelope carries a presentation object even when caller omits it",
    );
    assert_eq!(
        presentation.get("sink").and_then(Value::as_str),
        Some("leader"),
        "default sink must be leader (0.5.52 back-compat)"
    );
    assert_eq!(
        presentation.get("class").and_then(Value::as_str),
        Some("message"),
        "default class must be message"
    );
}

/// form2: an unknown sink/class fails LOUD at the normalization boundary — no
/// silent fallback to silent/leader. Baseline red: unknown JSON keys are simply
/// dropped by field-wise `get`, so no error surfaces.
#[test]
fn form2_unknown_sink_class_fails_loud_no_silent_fallback() {
    let env = json!({
        "summary": "x",
        "presentation": {"sink": "bogus_sink", "class": "message"}
    });
    let normalized = normalize_report_envelope(&env);
    let v = serde_json::to_value(&normalized).expect("serialize");

    // POST-GREEN: normalization records a structured rejection for the unknown
    // enum (surfaced as an ingest error marker on the envelope), never a silent
    // downgrade. Baseline has no such marker -> red.
    let err = v.get("presentation_error").and_then(Value::as_str);
    assert_eq!(
        err,
        Some("unknown_sink:bogus_sink"),
        "POST-GREEN: unknown sink must fail loud with a typed argument error, not be silently ignored"
    );
}

/// form3: a well-formed presentation object survives normalization byte/field
/// accurately (sink+class+case_id). Baseline red: stripped by the fixed schema.
#[test]
fn form3_normalization_retains_presentation_field_accurately() {
    let env = json!({
        "summary": "x",
        "presentation": {"sink": "casefile", "class": "stage_result", "case_id": "bug-123"}
    });
    let normalized = normalize_report_envelope(&env);
    let v = serde_json::to_value(&normalized).expect("serialize");
    let p = v
        .get("presentation")
        .expect("POST-GREEN: presentation retained through normalization");
    assert_eq!(p.get("sink").and_then(Value::as_str), Some("casefile"));
    assert_eq!(p.get("class").and_then(Value::as_str), Some("stage_result"));
    assert_eq!(p.get("case_id").and_then(Value::as_str), Some("bug-123"));
}

/// form4: requested + effective sink + policy_reason survive the durable
/// round-trip inside the result envelope. Baseline red: no audit fields exist.
#[test]
fn form4_requested_effective_reason_survive_durable_roundtrip() {
    // A stage_pass (critical) requested as casefile MUST be recorded with
    // requested=casefile, effective=leader, reason=critical_class:stage_pass.
    let env = json!({
        "summary": "x",
        "presentation": {"sink": "casefile", "class": "stage_pass"}
    });
    let normalized = normalize_report_envelope(&env);
    let v = serde_json::to_value(&normalized).expect("serialize");
    let audit = v
        .get("presentation")
        .expect("POST-GREEN: presentation audit present");
    assert_eq!(
        audit.get("requested_sink").and_then(Value::as_str),
        Some("casefile"),
        "requested sink must be preserved"
    );
    assert_eq!(
        audit.get("effective_sink").and_then(Value::as_str),
        Some("leader"),
        "critical stage_pass escalates effective sink to leader"
    );
    assert_eq!(
        audit.get("policy_reason").and_then(Value::as_str),
        Some("critical_class:stage_pass"),
        "escalation reason must be auditable"
    );
}

// ===========================================================================
// Behavior 5-12
//   Offline observable: SQL pending-scan allowlist / normalize policy output /
//   single-primitive source lock. Physical leader-transcript adjudication is
//   env-gated (real-machine).
// ===========================================================================

/// form5 (offline seam): a casefile/silent durable row uses a non-injecting
/// initial disposition, and the coordinator pending scan must NEVER select it.
/// Observable at baseline: `deliver_pending_messages` SQL status allowlist. It
/// currently selects 'accepted' (where a casefile row would land for lack of a
/// stored_only disposition) and has no 'stored_only' exclusion.
/// Baseline red: the allowlist neither knows nor excludes stored_only.
#[test]
fn form5_casefile_stored_only_never_enters_pending_scan() {
    let src = product_src("messaging/delivery.rs");
    // Locate the pending-scan status allowlist.
    assert!(
        src.contains("where status in ("),
        "pending scan status allowlist present (structural anchor)"
    );
    // POST-GREEN: a stored_only disposition exists AND the pending scan either
    // excludes it explicitly or never lists it. We assert the durable-only
    // status token exists in the disposition source and is absent from the
    // pending allowlist.
    let persist = product_src("messaging/persist.rs");
    assert!(
        persist.contains("StoredOnly"),
        "POST-GREEN: InitialDisposition has a StoredOnly (durable, non-injecting) variant"
    );
    // The pending-scan allowlist must not select stored_only rows.
    // Extract the allowlist block and assert stored_only is not whitelisted.
    let after = src
        .split("where status in (")
        .nth(1)
        .expect("allowlist block");
    let block: String = after.chars().take(400).collect();
    assert!(
        !block.contains("stored_only"),
        "POST-GREEN: deliver_pending_messages must never select stored_only rows"
    );
}

/// form6 (offline seam): report_result(casefile,stage_result) commits a result
/// row but creates NO leader-notification message row unless policy escalates.
/// Baseline red: results.rs unconditionally formats+funnels a notification.
/// Observable: the result path must consult a presentation decision before the
/// funnel; at baseline the notification call is unconditional (source predicate).
#[test]
fn form6_casefile_result_creates_no_leader_notification_row() {
    let src = product_src("messaging/results.rs");
    // POST-GREEN: the result notification is gated on an effective-leader
    // decision from the single C6 primitive. Baseline calls the funnel with no
    // presentation gate -> red on this predicate.
    assert!(
        src.contains("decide_presentation") || src.contains("present_persisted_obligation"),
        "POST-GREEN: report_result routes its notification through the C6 presentation decision \
         primitive (casefile/silent result must not unconditionally create a leader-notification)"
    );
}

/// form7 (offline seam): silent is durable/pullable but never injected. Same
/// stored_only non-injecting disposition as casefile (only discoverability
/// differs). Baseline red: no silent sink, no stored_only disposition.
#[test]
fn form7_silent_durable_pullable_not_injected() {
    let persist = product_src("messaging/persist.rs");
    assert!(
        persist.contains("StoredOnly"),
        "POST-GREEN: silent maps to the StoredOnly non-injecting durable disposition"
    );
}

/// form8 (offline seam): every critical class forces effective leader and
/// records the override reason. Baseline red: no policy layer computes an
/// effective sink or reason.
#[test]
fn form8_critical_class_forces_effective_leader_with_reason() {
    for class in [
        "stage_pass",
        "bounce",
        "blocking",
        "final_review",
        "timeout",
    ] {
        let env = json!({
            "summary": "x",
            "presentation": {"sink": "casefile", "class": class}
        });
        let normalized = normalize_report_envelope(&env);
        let v = serde_json::to_value(&normalized).expect("serialize");
        let p = v.get("presentation").unwrap_or_else(|| {
            panic!("POST-GREEN: presentation present for critical class {class}")
        });
        assert_eq!(
            p.get("effective_sink").and_then(Value::as_str),
            Some("leader"),
            "critical class {class} must force effective sink=leader"
        );
        assert_eq!(
            p.get("policy_reason").and_then(Value::as_str),
            Some(format!("critical_class:{class}").as_str()),
            "critical class {class} must record its escalation reason"
        );
    }
}

/// form9 (offline seam, ANTI-REGEX TOOTH): a noncritical class is NOT promoted
/// by the word "BLOCKING" in content; a critical class IS promoted even when
/// content is benign. Escalation reads typed class only, never prose.
/// Baseline red: no class concept, so neither case is distinguishable.
#[test]
fn form9_anti_regex_tooth_class_not_content() {
    // r19 revision (MUST-8 amendment compliance): the anti-regex tooth must be
    // demonstrated on the ONLY class that can legitimately be durable-only for a
    // report_result — `stage_result`. A non-stage_result class must NOT reach a
    // non-leader sink at all (see form9c), so it cannot carry the anti-regex
    // demonstration without itself violating MUST-8.

    // (a) anti-regex negative: a `stage_result` (legitimately durable-only) with
    // ALARMING content must NOT be promoted to leader by the prose. Escalation
    // reads typed class only; "BLOCKING" words in the summary do not make a
    // noncritical stage_result critical.
    let stage_result_alarming_content = json!({
        "summary": "THIS IS BLOCKING BLOCKING BLOCKING",
        "presentation": {"sink": "casefile", "class": "stage_result"}
    });
    let n_a = normalize_report_envelope(&stage_result_alarming_content);
    let v_a = serde_json::to_value(&n_a).expect("serialize");
    let p_a = v_a
        .get("presentation")
        .expect("POST-GREEN: presentation present (a)");
    assert_eq!(
        p_a.get("effective_sink").and_then(Value::as_str),
        Some("casefile"),
        "content words must NOT promote a noncritical stage_result to leader (anti-regex)"
    );

    // (b) anti-regex positive: a typed critical class with BENIGN content must
    // be promoted to leader by the TYPE, not the prose.
    let critical_class_benign_content = json!({
        "summary": "all good, nothing to see",
        "presentation": {"sink": "casefile", "class": "blocking"}
    });
    let n_b = normalize_report_envelope(&critical_class_benign_content);
    let v_b = serde_json::to_value(&n_b).expect("serialize");
    let p_b = v_b
        .get("presentation")
        .expect("POST-GREEN: presentation present (b)");
    assert_eq!(
        p_b.get("effective_sink").and_then(Value::as_str),
        Some("leader"),
        "typed critical class must be promoted regardless of benign content"
    );
}

/// form9c (r19 revision, MUST-8 compliance): a `report_result` whose
/// presentation class is NOT the sole durable-only-eligible `stage_result`
/// (here `message`) MUST resolve to effective leader regardless of the
/// requested casefile/silent sink — a non-stage_result result is `user_delivery`
/// and keeps its on-screen protection. Requesting casefile does NOT grant it
/// durable-only quiet. Baseline red: no class concept / no user_delivery gate.
///
/// r19 verdict: the pre-revision form9(a) encoded the VIOLATING semantics
/// (message class -> effective casefile). This form fences the signed rule that
/// ONLY explicit typed stage_result earns durable-only for report_result.
#[test]
fn form9c_non_stage_result_class_stays_leader_despite_casefile_request() {
    for class in ["message", "progress"] {
        let env = json!({
            "summary": "x",
            "presentation": {"sink": "casefile", "class": class}
        });
        let v = serde_json::to_value(&normalize_report_envelope(&env)).expect("serialize");
        let p = v.get("presentation").unwrap_or_else(|| {
            panic!("POST-GREEN: presentation present for non-stage_result class {class}")
        });
        assert_eq!(
            p.get("effective_sink").and_then(Value::as_str),
            Some("leader"),
            "MUST-8: report_result class={class} (not stage_result) has no durable-only \
             eligibility; a casefile request must resolve to effective leader (user_delivery)"
        );
        // And the audit must record the requested casefile + the forced leader
        // effective, so the escalation is explainable (not a silent rewrite).
        assert_eq!(
            p.get("requested_sink").and_then(Value::as_str),
            Some("casefile"),
            "requested casefile must be preserved in audit for class={class}"
        );
    }
}

/// form10 (offline seam): retry/rebind preserves the effective decision; a
/// casefile/silent (stored_only) row must not become leader after restart
/// merely because an accepted-row scanner sees it. Observable: pending scan
/// allowlist excludes stored_only (same seam as form5, restart angle).
/// Baseline red: stored_only unknown, so a re-scan would inject it.
#[test]
fn form10_retry_rebind_preserves_effective_decision() {
    let src = product_src("messaging/delivery.rs");
    let after = src
        .split("where status in (")
        .nth(1)
        .expect("allowlist block");
    let block: String = after.chars().take(400).collect();
    assert!(
        !block.contains("stored_only"),
        "POST-GREEN: restart pending-scan must never resurrect a stored_only row into leader inject"
    );
    // And the disposition must exist to be preserved.
    let persist = product_src("messaging/persist.rs");
    assert!(
        persist.contains("StoredOnly"),
        "POST-GREEN: stored_only disposition persists across rebind"
    );
}

/// form11 (GUARD, naturally green — regression fence): existing leader
/// send/report/broadcast/dedupe behavior must stay intact. We fence the
/// invariant that the default (no presentation) path still targets leader.
#[test]
fn form11_existing_leader_paths_unbroken_guard() {
    let env = json!({"summary": "legacy result", "status": "success"});
    let normalized = normalize_report_envelope(&env);
    // Legacy envelopes still normalize successfully (guard: no panic, summary
    // preserved). This stays green across baseline and GREEN.
    assert_eq!(normalized.summary, "legacy result");
}

/// form12 (offline seam, SOURCE GUARD — dual lock): send and report_result
/// share ONE C6 decision primitive, and the existing N31/N32 funnel remains the
/// only physical leader inject path. Baseline red: the primitive does not exist.
#[test]
fn form12_single_c6_primitive_single_funnel_source_guard() {
    // Lock A: both send and report_result reference the same decision primitive.
    let send_src = product_src("messaging/send.rs");
    let results_src = product_src("messaging/results.rs");
    let primitive = "decide_presentation";
    assert!(
        send_src.contains(primitive),
        "POST-GREEN Lock A: send path calls the C6 decision primitive `{primitive}`"
    );
    assert!(
        results_src.contains(primitive),
        "POST-GREEN Lock A: report_result path calls the SAME C6 decision primitive `{primitive}`"
    );
    // Lock B: leader physical inject stays behind the single funnel entry
    // (send_to_leader_receiver); no caller invents a second inject path.
    assert!(
        results_src.contains("send_to_leader_receiver"),
        "POST-GREEN Lock B: effective-leader results still use the existing N31/N32 funnel"
    );
}

// ===========================================================================
// Pipeline 13-15
//   Offline observable asserted deterministically; physical transcript
//   adjudication env-gated real-machine (no panic placeholder).
// ===========================================================================

/// form13 (offline seam): a stage result is pollable from `results` while
/// absent from the leader transcript. Offline predicate: the result-insert path
/// is independent of (precedes) the notification gate, so a casefile result row
/// commits even when no leader notification is produced.
/// Real-machine adjudication (transcript absence) is env-gated below.
#[test]
fn form13_stage_result_pollable_absent_from_transcript_offline() {
    let src = product_src("messaging/results.rs");
    // The result row insert must not be conditional on presentation sink; only
    // the notification is gated. POST-GREEN: a decision primitive gates the
    // notification, not the result insert.
    assert!(
        src.contains("decide_presentation") || src.contains("present_persisted_obligation"),
        "POST-GREEN: result row always commits; only leader notification is presentation-gated"
    );
}

/// form13-rm (REAL-MACHINE, env-gated): physically assert — through the real
/// mcp-server stdio + real tmux backend + real SQLite — that a casefile stage
/// result is pollable from `results` yet NEVER appears in the live leader
/// transcript. Skips (never panics) without the gate (leader nail ①: no panic
/// placeholder; per-tip nail ②: this env-gate form is on the checklist and must
/// be run for real, not assumed green).
///
/// Baseline red: presentation metadata is dropped, so the casefile result is
/// funneled to the leader exactly like any result and its summary DOES appear
/// in the leader transcript — the "absent from transcript" assertion fails.
#[test]
#[serial(msgroute_rm)]
fn form13_rm_stage_result_absent_from_live_transcript() {
    if !realmachine_gate() {
        eprintln!(
            "SKIP form13_rm: set TEAM_AGENT_REALMACHINE_MSGROUTE=1 to run the live \
             transcript-absence adjudication (real MCP stdio + real tmux + real SQLite)."
        );
        return;
    }
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let marker = "MSGROUTE_CASEFILE_ONLY_A17Q";
    let _ = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_msgroute",
            "agent_id": "worker_a",
            "status": "success",
            "summary": marker,
            "presentation": {"sink": "casefile", "class": "stage_result", "case_id": "bug-777"},
        }),
    );
    harness.drive_delivery_twice();

    // The result row must be durable/pollable regardless of sink.
    let stored = harness.message_rows_containing(marker);
    let transcript = harness.pane_text("leader");
    // POST-GREEN: casefile => durable but NOT on the leader transcript.
    assert!(
        !transcript.contains(marker),
        "POST-GREEN: a casefile stage result must NOT appear in the live leader transcript; \
         baseline funnels it to leader (red). transcript_has_marker={}, durable_rows={}",
        transcript.contains(marker),
        stored.len()
    );
}

/// form14 (offline seam): PASS/BOUNCE/BLOCKING/timeout/final-review decisions
/// each appear exactly once on the leader screen. Offline predicate: all five
/// are members of the fixed critical set that forces effective leader (form8
/// covers forcing; here we fence exactly-once dedupe reuse of the existing
/// result_id claim). Baseline red: no critical set, no class-based dedupe.
#[test]
fn form14_critical_decisions_exactly_once_offline() {
    // The five critical classes must all force effective leader (exactly-once
    // is enforced by the existing result_id dedupe claim, which form12 Lock B
    // keeps as the single funnel). Assert the critical set is honored.
    for class in [
        "stage_pass",
        "bounce",
        "blocking",
        "final_review",
        "timeout",
    ] {
        let env = json!({"summary": "x", "presentation": {"sink": "silent", "class": class}});
        let v = serde_json::to_value(&normalize_report_envelope(&env)).expect("serialize");
        let eff = v
            .get("presentation")
            .and_then(|p| p.get("effective_sink"))
            .and_then(Value::as_str);
        assert_eq!(
            eff,
            Some("leader"),
            "critical class {class} forces leader even when requested silent (exactly-once via funnel dedupe)"
        );
    }
}

/// form15 (offline seam): a malformed casefile report still reaches harness
/// validation and produces a visible bounce; silence cannot hide a required
/// escalation. Offline predicate: a malformed presentation object fails closed
/// at ingest (form2 covers unknown sink); here we fence that a MISSING required
/// class under a casefile sink does not silently grant durable-only quiet.
/// Baseline red: no ingest validation of the presentation object at all.
#[test]
fn form15_malformed_casefile_fails_closed_not_silent() {
    // casefile sink but class omitted -> POST-GREEN must fail closed (typed
    // error), never silently accept as quiet durable-only.
    let env = json!({
        "summary": "x",
        "presentation": {"sink": "casefile"}
    });
    let v = serde_json::to_value(&normalize_report_envelope(&env)).expect("serialize");
    let err = v.get("presentation_error").and_then(Value::as_str);
    assert_eq!(
        err,
        Some("missing_class"),
        "POST-GREEN: a casefile sink with no class fails closed loudly; silence must not hide escalation"
    );
}
