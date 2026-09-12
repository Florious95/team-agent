#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic;

use std::path::Path;
use serde_json::{json, Value};
use crate::cli::{cmd_send, CmdOutput, SendArgs};
use crate::message_store::MessageStore;
use crate::messaging::{DeliveryOutcome, DeliveryStatus, MessageTarget, SendOptions, TrustedSender};
use crate::model::ids::{AgentId, TeamKey};

fn no_injection_path(env: &hermetic::HermeticTestEnv) -> hermetic::EnvOverride {
    let bin = env.workspace("bin");
    let log = env.root().join("tmux-calls");
    let quoted_log = log.to_string_lossy().replace('\'', "'\\''");
    std::fs::write(bin.join("tmux"), format!(
        "#!/bin/sh\nprintf 'unexpected tmux call\\n' >> '{quoted_log}'\nexit 1\n"
    )).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("tmux"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    env.with_env("PATH", bin.to_str().unwrap())
}

fn seed_team(ws: &Path, team: &str, blocked: bool) {
    let agent = |id| json!({"agent_id": id, "provider": "fake", "status": "running"});
    let agents = json!({"sender": agent("sender"), "a": agent("a"), "b": agent("b")});
    let mut state = crate::state::persist::load_runtime_state(ws).unwrap_or(json!({}));
    state["active_team_key"] = json!(team);
    state["agents"] = agents.clone();
    state["teams"][team] = json!({"status": "alive", "agents": agents});
    crate::state::persist::save_runtime_state(ws, &state).unwrap();
    MessageStore::open(ws).unwrap();
    if blocked {
        let workspace = crate::coordinator::WorkspacePath::new(ws.to_path_buf());
        let pid = crate::coordinator::Pid::new(99_999_999);
        crate::coordinator::write_coordinator_metadata(&workspace, pid,
            crate::coordinator::MetadataSource::Boot).unwrap();
        std::fs::write(crate::coordinator::coordinator_pid_path(&workspace), pid.to_string()).unwrap();
    }
}

fn args(ws: &Path, target: String, mailbox: bool) -> SendArgs {
    SendArgs {
        target: Some(target), message: vec!["synthetic-body-do-not-echo".into()], targets: None,
        workspace: ws.to_path_buf(), team: Some("one".into()), task: None,
        sender: TrustedSender::from_runtime_identity(AgentId::new("sender")),
        no_ack: false, no_wait: true, watch_result: false, timeout: 0.0,
        confirm_human: false, json: true, message_id: None,
        presentation: crate::messaging::presentation::PresentationRequest {
            sink: if mailbox { crate::messaging::presentation::PresentationSink::Silent }
                else { crate::messaging::presentation::PresentationSink::Leader },
            ..Default::default()
        },
        pane: None, to_name: None, to_leader: None,
    }
}

fn assert_row(ws: &Path, team: &str, agent: &str, expected: &str) -> String {
    let store = MessageStore::open(ws).unwrap();
    let rows = store.inbox(agent, 10, Some(team)).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row["status"], expected, "{row}");
    assert!(row["delivered_at"].is_null(), "{row}");
    assert_eq!(row["delivery_attempts"], 0, "{row}");
    if expected == "queued_coordinator_unavailable" {
        assert_eq!(row["error"], "coordinator_unavailable", "{row}");
    }
    let id = row["message_id"].as_str().unwrap().to_string();
    if expected == "stored_only" {
        assert!(!store.claim_for_delivery(&id).unwrap());
    }
    eprintln!("T03 row: team={team} agent={agent} id={id} status={expected} delivered_at=null");
    id
}

fn cli_scope_matrix(mailbox: bool, blocked: bool) {
    let env = hermetic::HermeticTestEnv::enter("rules-t03-cli");
    let _path = no_injection_path(&env);
    for scope in ["same", "team", "workspace"] {
        let ws = env.workspace(scope);
        let remote = if scope == "workspace" { env.workspace("remote") } else { ws.clone() };
        seed_team(&ws, "one", blocked);
        let second_team = if scope == "same" { "one" } else { "two" };
        if scope != "same" { seed_team(&remote, second_team, blocked); }
        let target = format!("{}::one/a,{}::{second_team}/b", ws.display(), remote.display());
        let result = cmd_send(&args(&ws, target, mailbox)).unwrap();
        let CmdOutput::Json(value) = result.output else { panic!("JSON response required"); };
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["delivered"], false, "{value}");
        let expected = if mailbox { "stored_only" } else if blocked { "blocked" } else { "queued" };
        assert_eq!(value["status"], expected, "{value}");
        assert!(value["delivery_status"] == expected || (!mailbox && !blocked && value["delivery_status"] == "pending"));
        if blocked && !mailbox { assert_eq!(value["reason"], "coordinator_unavailable"); }
        if mailbox { assert!(!value["reminder"].as_str().unwrap().contains("queued")); }
        assert!(!value.to_string().contains("synthetic-body-do-not-echo"));
        if scope != "same" { assert_eq!(value["results"].as_array().unwrap().len(), 2); }
        let row_status = if mailbox { "stored_only" } else if blocked { "queued_coordinator_unavailable" } else { "accepted" };
        let first_id = assert_row(&ws, "one", "a", row_status);
        let second_id = assert_row(&remote, second_team, "b", row_status);
        assert_ne!(first_id, second_id);
        assert_eq!(value["message_id"], second_id);
        eprintln!("T03 scope={scope} receipt={value}");
        assert!(!env.root().join("tmux-calls").exists(), "no physical transport calls permitted");
        // No target session or provider exists. Physical-delivery events would
        // contradict the persisted receipt.
        for workspace in [&ws, &remote] {
            let events = crate::event_log::EventLog::new(workspace).tail(100).unwrap();
            assert!(!events.iter().any(|event| event["event"].as_str()
                .is_some_and(|name| name.starts_with("delivery.inject") || name == "message.delivered")));
        }
    }
}

#[test]
#[serial_test::serial(env)]
fn rules_t03_cli_queued_fanout_has_only_queue_evidence_in_all_scopes() {
    cli_scope_matrix(false, false);
}

#[test]
#[serial_test::serial(env)]
fn rules_t03_cli_stored_only_fanout_never_waits_for_live_delivery() {
    cli_scope_matrix(true, false);
}

#[test]
#[serial_test::serial(env)]
fn rules_t03_cli_blocked_fanout_preserves_db_blockers_and_reasons() {
    cli_scope_matrix(false, true);
}

#[test]
#[serial_test::serial(env)]
fn rules_t03_mcp_fanout_consumes_same_queued_blocked_and_mailbox_decision() {
    let env = hermetic::HermeticTestEnv::enter("rules-t03-mcp");
    let _path = no_injection_path(&env);
    for (mailbox, blocked, expected, row_status) in [
        (false, false, "queued", "accepted"),
        (false, true, "blocked", "queued_coordinator_unavailable"),
        (true, false, "stored_only", "stored_only"),
    ] {
        let ws = env.workspace(expected);
        seed_team(&ws, "one", blocked);
        let tools = crate::mcp_server::TeamOrchestratorTools::with_identity(
            &ws, Some(AgentId::new("sender")), Some(TeamKey::new("one")));
        let value = tools.send_message_with_presentation(
            &MessageTarget::Fanout(vec!["a".into(), "b".into()]), "synthetic-mcp-body",
            None, None, None, Some(&json!(mailbox)), None,
        ).unwrap().to_value();
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["status"], expected, "{value}");
        if blocked {
            // MCP's successful compact envelope carries this through warning;
            // CLI retains its existing reason field.
            assert_eq!(value["warning"], "coordinator_unavailable");
        }
        assert!(!value.to_string().contains("synthetic-mcp-body"));
        assert_row(&ws, "one", "a", row_status);
        assert_eq!(value["message_id"], assert_row(&ws, "one", "b", row_status));
        eprintln!("T03 MCP receipt={value}");
        assert!(!env.root().join("tmux-calls").exists(), "no physical transport calls permitted");
    }
}

fn outcome(status: DeliveryStatus, ok: bool) -> DeliveryOutcome {
    DeliveryOutcome {
        ok, status, ack_forced_off: false,
        message_status: crate::messaging::helpers::MessageStatusShadow(
            if status == DeliveryStatus::Queued { "accepted".into() }
            else { crate::messaging::helpers::status_wire(status).into() }),
        message_id: Some("msg_synthetic".into()), verification: None, stage: None,
        reason: None, channel: None, turn_verification: None,
    }
}

#[test]
fn rules_t03_aggregate_preserves_receipts_mixed_states_and_partial_failure() {
    use DeliveryStatus::*;
    for (children, expected_ok, expected) in [
        (vec![(true, Delivered), (true, AlreadyDelivered)], true, FanoutDelivered),
        (vec![(true, Delivered), (true, Queued)], true, Queued),
        (vec![(true, Queued), (true, Queued)], true, Queued),
        (vec![(true, Blocked), (true, Queued)], true, Blocked),
        (vec![(true, StoredOnly), (true, StoredOnly)], true, StoredOnly),
        (vec![(true, StoredOnly), (true, Queued)], true, FanoutMixed),
        (vec![(true, StoredOnly), (true, Delivered)], true, FanoutMixed),
        (vec![(true, Delivered), (false, Refused)], false, FanoutPartial),
        (vec![(true, Delivered), (false, Failed)], false, FanoutPartial),
        (vec![(true, FallbackLog), (true, Queued)], true, Queued),
        (vec![], false, Failed),
    ] {
        let children = children.into_iter().map(|(ok, status)| outcome(status, ok)).collect::<Vec<_>>();
        let evidence = children.iter().map(|out| (out.ok, out.status)).collect::<Vec<_>>();
        let (ok, status) = crate::messaging::send::aggregate_fanout_status(&evidence);
        assert_eq!((ok, status), (expected_ok, expected), "{children:?}");
        let result = outcome(status, ok);
        let value = super::super::presentation::delivery_outcome_json(&result,
            &MessageTarget::Fanout(vec!["a".into(), "b".into()]), "body", &SendOptions::default());
        assert_eq!(value["delivered"], expected == FanoutDelivered);
        assert_eq!(crate::mcp_server::delivery_outcome_value(&result)["status"], value["status"]);
    }
}

#[test]
fn rules_t03_single_receipt_projection_keeps_evidence_levels() {
    for status in [DeliveryStatus::Queued, DeliveryStatus::Blocked, DeliveryStatus::Refused,
        DeliveryStatus::StoredOnly, DeliveryStatus::Delivered, DeliveryStatus::AlreadyDelivered] {
        let out = outcome(status, status != DeliveryStatus::Refused);
        let value = super::super::presentation::delivery_outcome_json(&out,
            &MessageTarget::Single("a".into()), "body", &SendOptions::default());
        assert_eq!(value["delivered"], matches!(status, DeliveryStatus::Delivered | DeliveryStatus::AlreadyDelivered));
        assert_eq!(value["status"], crate::messaging::helpers::status_wire(status));
        assert_eq!(crate::mcp_server::delivery_outcome_value(&out)["status"], value["status"]);
    }
}
