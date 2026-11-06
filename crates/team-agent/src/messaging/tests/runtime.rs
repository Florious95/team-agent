use super::*;

#[test]
fn send_message_target_not_in_team_is_refused() {
    // send.py:259-261 — non-leader, non-team target → refused. M1 (station::send) refuses
    // before storing anything.
    let ws = tmp_ws("sendrefuse");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({"session_name": "team-x", "agents": {"w1": {"provider": "copilot"}}}),
    )
    .unwrap();
    let out = crate::station::send::execute(
        crate::station::send::Request {
            workspace: &ws,
            team: None,
            to: "ghost",
            content: "hi",
            task_id: None,
            message_id: None,
            mailbox: false,
            presentation: &crate::messaging::presentation::PresentationRequest::default(),
            envelope: None,
        },
        crate::station::send::Authority::Operator { sender: "leader" },
    );
    // M5 two-phase admission: an unresolvable target is one typed refusal, not a routing error.
    let refusal = typed_refusal(&out);
    assert_eq!(refusal["reason"], "name_not_resolvable", "{refusal}");
    assert_eq!(refusal["invalid_targets"], serde_json::json!(["ghost"]), "{refusal}");
    let inbox = crate::inbox_store::read(&ws, "ghost", 10, None).unwrap();
    assert!(inbox["messages"].as_array().unwrap().is_empty(), "a refused send stores nothing");
}

#[test]
fn send_message_broadcast_empty_team_skips_no_recipients() {
    // send.py:391-393 — "*" with no team recipients →
    //   {"ok": False, "status": "failed", "reason": "no_team_recipients", "to": "*"}.
    // Post-#230 N31/N32 funnel implementation (cr-approved): broadcast now expands the
    // recipient set via `broadcast_recipients(state, sender, team)` and routes each
    // recipient through the SAME primitives as a single send (leader → primitive,
    // peer → send_message). The assertions stay the same: with no agents seeded and
    // sender="leader" (default opts.sender), `broadcast_recipients` returns an empty
    // list — outcome is Failed/no-recipients with channel="*". The "*" channel label
    // is preserved through the new `fanout_send(..., channel_label="*")` parameter so
    // legacy consumers can still tell broadcast (`*`) apart from explicit fanout list.
    // M1 (station::send): a broadcast that expands to no recipient fails before storing.
    let ws = tmp_ws("sendbcast");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({"session_name": "team-x", "agents": {}}),
    )
    .unwrap();
    let out = crate::station::send::execute(
        crate::station::send::Request {
            workspace: &ws,
            team: None,
            to: "*",
            content: "hi",
            task_id: None,
            message_id: None,
            mailbox: false,
            presentation: &crate::messaging::presentation::PresentationRequest::default(),
            envelope: None,
        },
        crate::station::send::Authority::Operator { sender: "leader" },
    );
    let refusal = typed_refusal(&out);
    assert_eq!(refusal["reason"], "empty_target_list", "{refusal}");
    assert_eq!(refusal["invalid_targets"], serde_json::json!(["*"]), "{refusal}");
}

#[test]
fn send_message_fanout_empty_recipients_fails() {
    // send.py:456-457 — fanout with no usable recipients → ok False,
    // no_fanout_recipients. (Dedup-then-deliver happy path needs team fixtures.)
    let ws = tmp_ws("sendfanout");
    let out = crate::station::send::execute(
        crate::station::send::Request {
            workspace: &ws,
            team: None,
            to: "",
            content: "hi",
            task_id: None,
            message_id: None,
            mailbox: false,
            presentation: &crate::messaging::presentation::PresentationRequest::default(),
            envelope: None,
        },
        crate::station::send::Authority::Operator { sender: "leader" },
    );
    assert_eq!(typed_refusal(&out)["reason"], "empty_target_list");
}

#[test]
fn run_comms_selftest_zero_provider_sdk_passes_and_locks_scope() {
    let ws = tmp_ws("selftestok");
    let driver = ZeroSdkDriver {
        run_id: Some("fixedrunid01".to_string()),
        calls: ProviderSdkCalls::default(),
    };
    let report = run_comms_selftest(&ws, None, &driver).unwrap();
    // diagnose/comms.py:44 scope == "binding_consistency".
    assert_eq!(report.scope, "binding_consistency");
    assert_eq!(report.run_id, "fixedrunid01");
    // The mechanical gate: provider_sdk_calls check is a Pass with all-zero evidence.
    assert_eq!(report.provider_sdk_calls.status, CheckStatus::Pass);
    match &report.provider_sdk_calls.evidence {
        CheckEvidence::ProviderSdkCalls(calls) => assert!(calls.is_zero()),
        other => panic!("expected ProviderSdkCalls evidence, got {other:?}"),
    }
    assert!(report.ok);
}

#[test]
fn run_comms_selftest_nonzero_provider_sdk_fails_gate() {
    // Any non-zero SDK call count → provider_sdk_calls check FAILS, report not ok.
    let ws = tmp_ws("selftestbad");
    let driver = ZeroSdkDriver {
        run_id: Some("r2".to_string()),
        calls: ProviderSdkCalls {
            anthropic: 1,
            openai: 0,
            httpx: 0,
        },
    };
    let report = run_comms_selftest(&ws, None, &driver).unwrap();
    assert_eq!(report.provider_sdk_calls.status, CheckStatus::Fail);
    assert!(!report.ok);
}

#[test]
fn u1_multi_team_send_does_not_backfill_top_level_leader_binding() {
    let ws = tmp_ws("u1backfill");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "leader_receiver": {"status": "unbound", "pane_id": "__team_agent_unbound__"},
            "team_owner": {"pane_id": "__team_agent_unbound__"},
            "teams": {
                "team-a": {
                    "session_name": "team-a",
                    "leader_receiver": {"status": "unbound", "pane_id": "__team_agent_unbound__"},
                    "agents": {}
                },
                "team-b": {
                    "session_name": "team-b",
                    "agents": {}
                }
            },
            "agents": {}
        }),
    )
    .unwrap();


    let out = crate::station::send::execute(
        crate::station::send::Request {
            workspace: &ws,
            team: Some("team-b"),
            to: "leader",
            content: "hello team-b",
            task_id: None,
            message_id: None,
            mailbox: false,
            presentation: &crate::messaging::presentation::PresentationRequest::default(),
            envelope: None,
        },
        crate::station::send::Authority::Operator { sender: "leader" },
    )
    .unwrap();

    assert_eq!(out["ok"], true, "{out:?}");
    let message_id = out["message_id"].as_str().expect("stored leader message id");
    // M1 inboxes are team-scoped (red_m1_r10): the record belongs to team-b only.
    let team_b = crate::inbox_store::read(&ws, "leader", 10, Some("team-b")).unwrap();
    let records = team_b["messages"].as_array().unwrap();
    assert_eq!(records.len(), 1, "team-b leader send must land in team-b's inbox: {team_b}");
    assert_eq!(records[0]["message_id"], message_id);
    assert_eq!(records[0]["team"], "team-b");
    let team_a = crate::inbox_store::read(&ws, "leader", 10, Some("team-a")).unwrap();
    assert!(
        team_a["messages"].as_array().unwrap().is_empty(),
        "team-b leader send must not inherit team-a/top-level unbound leader binding: {team_a}"
    );
}

#[test]
fn casefile_leader_send_is_durable_without_entering_leader_funnel() {
    let ws = tmp_ws("casefile-send");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {}
        }),
    )
    .unwrap();
    let presentation = crate::messaging::presentation::PresentationRequest {
        sink: crate::messaging::presentation::PresentationSink::Casefile,
        class: crate::messaging::presentation::PresentationClass::Progress,
        case_id: Some("case-1".to_string()),
    };
    let out = crate::station::send::execute(
        crate::station::send::Request {
            workspace: &ws,
            team: Some("team-a"),
            to: "leader",
            content: "internal progress",
            task_id: None,
            message_id: None,
            mailbox: false,
            presentation: &presentation,
            envelope: None,
        },
        crate::station::send::Authority::Operator { sender: "leader" },
    )
    .unwrap();
    assert_eq!(out["status"], "stored");
    assert_eq!(out["stored"], true);
    // M1: the durable record is the inbox; a casefile send never attempts submission.
    let receipt = &out;
    assert_eq!(receipt["live"], false);
    assert!(receipt.get("submission").is_none(), "{receipt}");
    let inbox = crate::inbox_store::read(&ws, "leader", 10, Some("team-a")).unwrap();
    let record = &inbox["messages"][0];
    assert_eq!(record["message_id"], out["message_id"].as_str().unwrap());
    assert_eq!(record["presentation"]["sink"], "casefile");
}

fn typed_refusal(out: &Result<serde_json::Value, String>) -> serde_json::Value {
    let out = out.as_ref().expect("an admission refusal is a typed outcome");
    assert_eq!(out["ok"], false, "{out:?}");
    assert_eq!(out["status"], "refused", "{out:?}");
    assert!(out.get("message_id").is_none(), "a refused send stores nothing: {out:?}");
    assert_eq!(out["stored"], false, "{out}");
    assert_eq!(out["not_attempted"], serde_json::json!([]), "{out}");
    out.clone()
}
