//! R12 leader rediscover ambiguity-incident — core cluster (R12-0 two-tier + R12-1/2/3).
//! Offline byte-lock via rediscover_leader_receiver_from_targets_with_owner_identity (injected
//! &[PaneInfo], no real tmux). golden: messaging/leader_panes.py:101-187.
use super::*;

fn r12_ws(tag: &str) -> std::path::PathBuf {
    let ws = std::env::temp_dir().join(format!(
        "ta-r12-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

fn usable_target(pane: &str, cmd: &str) -> crate::transport::PaneInfo {
    crate::transport::PaneInfo {
        pane_id: crate::transport::PaneId::new(pane),
        session: crate::transport::SessionName::new("s"),
        window_index: Some(0),
        window_name: None,
        pane_index: Some(0),
        tty: None,
        current_command: Some(cmd.to_string()),
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: std::collections::BTreeMap::new(),
    }
}

fn r12_events(ws: &std::path::Path) -> Vec<serde_json::Value> {
    crate::event_log::EventLog::new(ws).tail(0).unwrap_or_default()
}
fn find_ev<'a>(evs: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    evs.iter().rev().find(|e| e.get("event").and_then(|v| v.as_str()) == Some(name))
}
fn r12_keys(e: &serde_json::Value) -> std::collections::BTreeSet<String> {
    e.as_object()
        .map(|o| o.keys().filter(|k| *k != "ts" && *k != "event").cloned().collect())
        .unwrap_or_default()
}

// usable target that matches an owner_identity via leader_session_uuid (drives the OWNER-ambiguous
// path D3: golden _rediscover_leader_receiver:133-145, owner present + >1 owner_candidates → broadcast).
fn owner_matching_target(pane: &str, cmd: &str, uuid: &str) -> crate::transport::PaneInfo {
    let mut t = usable_target(pane, cmd);
    t.leader_env
        .insert("TEAM_AGENT_LEADER_SESSION_UUID".to_string(), uuid.to_string());
    t
}
fn count_ev(evs: &[serde_json::Value], name: &str) -> usize {
    evs.iter().filter(|e| e.get("event").and_then(|v| v.as_str()) == Some(name)).count()
}

// R12-0 + R12-3: NO owner_identity + 2 command-usable targets -> golden two-tier AMBIGUOUS (command-usable
// candidates, no owner filter), return key "candidates". Rust unified filter (command ∩ matches_owner_identity)
// with an empty identity matches nothing -> 0 candidates -> "missing" => loses the golden no-owner path.
#[test]
fn r12_no_owner_two_usable_targets_is_ambiguous_not_missing() {
    let ws = r12_ws("noownerambig");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let targets = vec![usable_target("%a", "codex"), usable_target("%b", "codex")];
    let out = crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &serde_json::json!({}), None,
    )
    .unwrap();
    assert_eq!(
        out.get("status").and_then(|v| v.as_str()),
        Some("ambiguous"),
        "R12-0: no owner_identity + 2 command-usable -> golden two-tier AMBIGUOUS (command-usable, no owner \
         filter); Rust unified filter loses the no-owner path -> missing. out={out:?}"
    );
    assert!(
        out.get("candidates").is_some() && out.get("owner_candidates").is_none(),
        "R12-3: ambiguous return key is 'candidates' (golden), not 'owner_candidates'; out={out:?}"
    );
}

// R12-0/R12-1/R12-2: NO owner_identity + 0 command-usable -> golden no-owner MISSING (D7): rediscover_missing
// = {provider, old_target} LEAN; rebind_required = {..., rediscovery_status:"missing"} (no recovery_action/
// owner_identity). Rust attaches owner_identity + candidate_count + recovery_action.
#[test]
fn r12_no_owner_zero_usable_missing_is_lean_payload() {
    let ws = r12_ws("noownermiss");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let targets: Vec<crate::transport::PaneInfo> = vec![];
    let out = crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &serde_json::json!({}), None,
    )
    .unwrap();
    assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("missing"));
    let evs = r12_events(&ws);
    let miss = find_ev(&evs, "leader_receiver.rediscover_missing").expect("rediscover_missing");
    let mk = r12_keys(miss);
    assert!(
        !mk.contains("owner_identity") && !mk.contains("candidate_count"),
        "R12-2: no-owner D7 rediscover_missing must be lean {{provider, old_target}} (golden leader_panes.py:185), \
         not carry owner_identity/candidate_count; got {mk:?}"
    );
    let rebind = find_ev(&evs, "leader_receiver.rebind_required").expect("rebind_required");
    let rk = r12_keys(rebind);
    assert!(
        !rk.contains("recovery_action") && !rk.contains("owner_identity"),
        "R12-1: no-owner D7 rebind_required (golden leader_panes.py:186) has rediscovery_status only, NO \
         recovery_action/owner_identity; got {rk:?}"
    );
    assert_eq!(rebind.get("rediscovery_status").and_then(|v| v.as_str()), Some("missing"));
}

// R12-1/R12-2: owner_identity PRESENT + non-matching + 2 command-usable -> golden owner-MISSING (D4):
// rediscover_missing candidate_count == len(command-usable)=2 + owner_identity; rebind_required carries
// recovery_action (golden string) + owner_identity + uuid_prefix, NO rediscovery_status. Rust: 0 candidates
// (post owner filter) -> candidate_count 0 + unified rebind (recovery_action="run team-agent attach-leader...").
#[test]
fn r12_owner_missing_candidate_count_is_command_usable_len_and_rebind_shape() {
    let ws = r12_ws("ownermiss");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let owner = serde_json::json!({
        "pane_id": "%owner", "leader_session_uuid": "U-abcdef01",
        "machine_fingerprint": "F", "provider": "codex", "team_id": "team-a"
    });
    let targets = vec![usable_target("%a", "codex"), usable_target("%b", "codex")];
    let out = crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &owner, None,
    )
    .unwrap();
    assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("missing"));
    let evs = r12_events(&ws);
    let miss = find_ev(&evs, "leader_receiver.rediscover_missing").expect("rediscover_missing");
    assert_eq!(
        miss.get("candidate_count").and_then(|v| v.as_u64()),
        Some(2),
        "R12-2: owner-missing D4 candidate_count == len(command-usable)=2 (golden leader_panes.py:161, \
         candidate_count is PRE owner-filter), not 0; miss={miss:?}"
    );
    let rebind = find_ev(&evs, "leader_receiver.rebind_required").expect("rebind_required");
    assert_eq!(
        rebind.get("recovery_action").and_then(|v| v.as_str()),
        Some("open the owning leader pane or run team-agent claim-leader --confirm from a matching pane"),
        "R12-1: owner-missing D4 rebind recovery_action == golden string (leader_panes.py:171)"
    );
    assert!(
        rebind.get("rediscovery_status").is_none(),
        "R12-1: owner-missing D4 rebind has NO rediscovery_status (golden D4); rebind={rebind:?}"
    );
}

// R12-5: owner present + 2 owner-matching usable -> OWNER-ambiguous (D3) broadcasts the
// leader_receiver.ambiguous_candidates event. golden payload (probe-captured, sort_keys):
//   {candidates:[sorted pane_ids], debounce_bucket, incident_id, old_pane_id, provider,
//    reason:"force_confirm_required", team_id, uuid_prefix}.
// Rust emit_ambiguous_candidates (rediscover.rs:650) emits key "pane_ids" (not "candidates") and
// embeds owner_identity + invalidation_reason + queued[] (golden has none of these in this event;
// the per-candidate queue is R12-6's separate ambiguous_candidate_queued events).
#[test]
fn r12_5_ambiguous_candidates_event_is_full_golden_candidates_key() {
    let ws = r12_ws("ambigbroadcast");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let owner = serde_json::json!({
        "pane_id": "%owner", "leader_session_uuid": "U-abcdef01",
        "machine_fingerprint": "F", "provider": "codex", "team_id": "team-a"
    });
    let targets = vec![
        owner_matching_target("%b", "codex", "U-abcdef01"),
        owner_matching_target("%a", "codex", "U-abcdef01"),
    ];
    let out = crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &owner, None,
    )
    .unwrap();
    assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("ambiguous"));
    let evs = r12_events(&ws);
    let bc = find_ev(&evs, "leader_receiver.ambiguous_candidates")
        .expect("OWNER-ambiguous must broadcast leader_receiver.ambiguous_candidates");
    let k = r12_keys(bc);
    assert!(
        k.contains("candidates") && !k.contains("pane_ids"),
        "R12-5: broadcast key is 'candidates' (golden _broadcast_ambiguous_candidates:270), not 'pane_ids'; got {k:?}"
    );
    assert!(
        !k.contains("queued") && !k.contains("owner_identity") && !k.contains("invalidation_reason"),
        "R12-5: golden ambiguous_candidates carries NO queued[]/owner_identity/invalidation_reason (per-candidate \
         queue is the separate ambiguous_candidate_queued event, R12-6); got {k:?}"
    );
    assert_eq!(
        bc.get("candidates").cloned(),
        Some(serde_json::json!(["%a", "%b"])),
        "R12-5: candidates == sorted pane_ids (golden candidate_ids = sorted); bc={bc:?}"
    );
    assert_eq!(bc.get("reason").and_then(|v| v.as_str()), Some("force_confirm_required"));
}

// R12-5 dedup: golden _broadcast_ambiguous_candidates:263 skips re-broadcast when a prior
// ambiguous_candidates with the same incident_id is in event_log.tail(200) (returns deduped:true).
// Rust incident_id is deterministic (✦ R12-4), so a repeat drive yields the SAME incident_id; Rust
// hardcodes deduped:false (rediscover.rs:245/255) and re-emits every time -> RED (2 events, golden 1).
#[test]
fn r12_5_ambiguous_candidates_dedups_on_repeat_incident() {
    let ws = r12_ws("ambigdedup");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let owner = serde_json::json!({
        "pane_id": "%owner", "leader_session_uuid": "U-abcdef01",
        "machine_fingerprint": "F", "provider": "codex", "team_id": "team-a"
    });
    let targets = vec![
        owner_matching_target("%a", "codex", "U-abcdef01"),
        owner_matching_target("%b", "codex", "U-abcdef01"),
    ];
    for _ in 0..2 {
        crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
            &ws, &mut state, &targets, &log, &owner, None,
        )
        .unwrap();
    }
    let evs = r12_events(&ws);
    assert_eq!(
        count_ev(&evs, "leader_receiver.ambiguous_candidates"),
        1,
        "R12-5: repeat drive with the same incident_id must dedup the broadcast (golden tail(200) skip, \
         leader_panes.py:263); Rust re-emits every time. ambiguous_candidates count={}",
        count_ev(&evs, "leader_receiver.ambiguous_candidates")
    );
}

// R12-6: golden _broadcast_ambiguous_candidates:279-294 loops candidates and emits a SEPARATE
// leader_receiver.ambiguous_candidate_queued event per candidate (probe-captured keys
// {error, event, incident_id, ok, pane_id}), tagged with the broadcast's incident_id. Rust embeds a
// queued:[{pane_id,...}] array INSIDE the single ambiguous_candidates event (rediscover.rs:663) and
// emits ZERO per-candidate events. NOTE (surfaced): the ok/error VALUES come from the tmux inject
// (real-tmux); the offline from_targets seam can assert structure (separate events, incident_id,
// pane_id) but not the inject result — the porter/leader decide the offline ok/error semantics.
#[test]
fn r12_6_per_candidate_emits_separate_ambiguous_candidate_queued_events() {
    let ws = r12_ws("percand");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let owner = serde_json::json!({
        "pane_id": "%owner", "leader_session_uuid": "U-abcdef01",
        "machine_fingerprint": "F", "provider": "codex", "team_id": "team-a"
    });
    let targets = vec![
        owner_matching_target("%a", "codex", "U-abcdef01"),
        owner_matching_target("%b", "codex", "U-abcdef01"),
    ];
    crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &owner, None,
    )
    .unwrap();
    let evs = r12_events(&ws);
    let n = count_ev(&evs, "leader_receiver.ambiguous_candidate_queued");
    assert_eq!(
        n, 2,
        "R12-6: each candidate must emit a SEPARATE leader_receiver.ambiguous_candidate_queued event \
         (golden leader_panes.py:288); Rust embeds queued[] in the single ambiguous_candidates event and \
         emits none. count={n}"
    );
    let qpanes: std::collections::BTreeSet<String> = evs
        .iter()
        .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("leader_receiver.ambiguous_candidate_queued"))
        .filter_map(|e| e.get("pane_id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    assert_eq!(
        qpanes,
        ["%a".to_string(), "%b".to_string()].into_iter().collect(),
        "R12-6: per-candidate queued events cover each candidate pane_id; got {qpanes:?}"
    );
    let bc = find_ev(&evs, "leader_receiver.ambiguous_candidates").expect("broadcast");
    let iid = bc.get("incident_id").cloned();
    let q = find_ev(&evs, "leader_receiver.ambiguous_candidate_queued").expect("queued");
    assert_eq!(
        q.get("incident_id").cloned(),
        iid,
        "R12-6: each ambiguous_candidate_queued carries the broadcast incident_id (golden :290); q={q:?}"
    );
}

// helper: pull the candidate-raw array from the ambiguous return regardless of the R12-3 key rename.
fn r12_return_candidates(out: &serde_json::Value) -> &Vec<serde_json::Value> {
    out.get("candidates")
        .or_else(|| out.get("owner_candidates"))
        .and_then(|v| v.as_array())
        .expect("ambiguous return must carry a candidate array")
}

// R12-7: golden returns the candidate RAW as the passthrough tmux target dict (probe-captured keys
// {leader_session_uuid, pane_current_command, pane_current_path, pane_id}). Rust reconstructs via
// pane_info_value (rediscover.rs:568) with "current_command"/"current_path". Assert golden spelling.
#[test]
fn r12_7_candidate_raw_uses_golden_pane_current_spelling() {
    let ws = r12_ws("rawkeys");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old", "provider": "codex"}});
    let owner = serde_json::json!({
        "pane_id": "%owner", "leader_session_uuid": "U-abcdef01",
        "machine_fingerprint": "F", "provider": "codex", "team_id": "team-a"
    });
    let targets = vec![
        owner_matching_target("%a", "codex", "U-abcdef01"),
        owner_matching_target("%b", "codex", "U-abcdef01"),
    ];
    let out = crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &owner, None,
    )
    .unwrap();
    let c0 = &r12_return_candidates(&out)[0];
    let k = r12_keys(c0);
    assert!(
        k.contains("pane_current_command") && k.contains("pane_current_path"),
        "R12-7: candidate raw uses golden tmux spelling pane_current_command/pane_current_path (passthrough); \
         got {k:?}"
    );
    assert!(
        !k.contains("current_command") && !k.contains("current_path"),
        "R12-7: Rust pane_info_value spelling current_command/current_path must not leak; got {k:?}"
    );
}

// R12-7: golden provider = str(receiver.provider or "codex") (leader_panes.py:108) — DEFAULTS to "codex"
// when absent, applied to every emitted event's provider field. Rust uses identity.provider (null when
// absent). Drive with no provider anywhere -> golden event provider "codex", Rust null.
#[test]
fn r12_7_event_provider_defaults_to_codex_when_absent() {
    let ws = r12_ws("providerdefault");
    let log = crate::event_log::EventLog::new(&ws);
    let mut state = serde_json::json!({"leader_receiver": {"pane_id": "%old"}});
    let owner = serde_json::json!({"leader_session_uuid": "U-abcdef01", "team_id": "team-a"});
    let targets = vec![
        owner_matching_target("%a", "codex", "U-abcdef01"),
        owner_matching_target("%b", "codex", "U-abcdef01"),
    ];
    let out = crate::leader::rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws, &mut state, &targets, &log, &owner, None,
    )
    .unwrap();
    assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("ambiguous"));
    let evs = r12_events(&ws);
    let amb = find_ev(&evs, "leader_receiver.rediscover_ambiguous").expect("rediscover_ambiguous");
    assert_eq!(
        amb.get("provider").and_then(|v| v.as_str()),
        Some("codex"),
        "R12-7: event provider defaults to \"codex\" when absent (golden leader_panes.py:108 \
         receiver.provider or 'codex'); Rust emits identity.provider=null. amb={amb:?}"
    );
}
