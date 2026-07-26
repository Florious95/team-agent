//! 0.5.61 RED: `mailbox=true` is target-neutral.
//!
//! Requirement anchors:
//! - `wiki/功能/send 单信箱与状态事实穿透.md` B02/B07, C02/C09
//! - `wiki/功能/F4 消息寻址、受理与可靠送达.md`
//!
//! A mailbox send must still create a stable, pullable message row, but that row
//! must never become eligible for live delivery. The same invariant applies to a
//! direct worker recipient and to every recipient expanded from a broadcast.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::mcp_server::{SendOutcome, TeamOrchestratorTools};
use team_agent::message_store::MessageStore;
use team_agent::messaging::MessageTarget;
use team_agent::model::ids::{AgentId, TeamKey};

#[test]
fn mailbox_to_worker_is_pullable_but_never_live_delivery_eligible() {
    let ws = seeded_workspace("worker");
    let tools = worker_tools(&ws);
    let mut failures = Vec::new();

    let mailbox = tools
        .send_message_with_presentation(
            &MessageTarget::Single("worker-b".to_string()),
            "worker mailbox red token",
            None,
            None,
            None,
            Some(&json!(true)),
            None,
        )
        .expect("mailbox send must be durably accepted");
    let mailbox_value = mailbox.to_value();
    let mailbox_id = required_message_id(&mailbox_value, "worker mailbox receipt");
    if mailbox_value.get("status") != Some(&json!("stored_only")) {
        failures.push(format!(
            "RED tooth worker: mailbox=true must not return the ordinary worker \
             accepted/queued shape; receipt={mailbox_value}"
        ));
    }
    if mailbox_value.get("verification") != Some(&json!("durable_without_live_inject")) {
        failures.push(format!(
            "RED tooth worker: receipt must distinguish durable-only from live delivery; \
             receipt={mailbox_value}"
        ));
    }

    let store = MessageStore::open(&ws).expect("open message store");
    let mailbox_row = inbox_row(&store, "worker-b", &mailbox_id);
    if mailbox_row.get("status") != Some(&json!("stored_only")) {
        failures.push(format!(
            "RED tooth worker: pullable durable row must carry the non-injecting disposition; \
             row={mailbox_row}"
        ));
    }
    if store
        .claim_for_delivery(&mailbox_id)
        .expect("claim stored-only row")
    {
        failures.push(format!(
            "RED tooth worker: mailbox row must never enter the worker delivery queue/claim \
             funnel; message_id={mailbox_id}"
        ));
    }

    let ordinary = tools
        .send_message(
            &MessageTarget::Single("worker-b".to_string()),
            "worker ordinary positive control",
            None,
            None,
            None,
        )
        .expect("ordinary worker send remains accepted");
    let ordinary_value = ordinary.to_value();
    if ordinary_value.get("status") != Some(&json!("accepted")) {
        failures.push(format!(
            "positive control: omitting mailbox must preserve the ordinary worker queue \
             surface; receipt={ordinary_value}"
        ));
    }
    let ordinary_id = required_message_id(&ordinary_value, "ordinary worker receipt");
    let ordinary_row = inbox_row(&store, "worker-b", &ordinary_id);
    if ordinary_row.get("status") == Some(&json!("stored_only")) {
        failures.push(format!(
            "positive control: ordinary worker send must not be silently converted to mailbox; \
             row={ordinary_row}"
        ));
    }
    if !store
        .claim_for_delivery(&ordinary_id)
        .expect("claim ordinary worker row")
    {
        failures.push(format!(
            "positive control: ordinary worker send must retain live-delivery eligibility; \
             message_id={ordinary_id}"
        ));
    }

    assert!(
        failures.is_empty(),
        "worker mailbox universal-path contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn mailbox_broadcast_makes_every_recipient_pullable_and_non_injecting() {
    let ws = seeded_workspace("broadcast");
    let tools = worker_tools(&ws);
    let mut failures = Vec::new();

    let mailbox = tools
        .send_message_with_presentation(
            &MessageTarget::Broadcast,
            "broadcast mailbox red token",
            None,
            None,
            None,
            Some(&json!(true)),
            None,
        )
        .expect("mailbox broadcast must be durably accepted");
    let mailbox_value = mailbox.to_value();
    let returned_id = required_message_id(&mailbox_value, "mailbox broadcast receipt");

    let store = MessageStore::open(&ws).expect("open message store");
    let mailbox_rows = rows_for_content(
        &store,
        &["leader", "worker-b"],
        "broadcast mailbox red token",
    );
    if recipients(&mailbox_rows) != BTreeSet::from(["leader".to_string(), "worker-b".to_string()]) {
        failures.push(format!(
            "RED tooth broadcast: sender-excluding broadcast must durably fan out to leader \
             and peer; rows={mailbox_rows:?}"
        ));
    }
    let mailbox_ids = message_ids(&mailbox_rows);
    if !mailbox_ids.contains(&returned_id) {
        failures.push(format!(
            "RED tooth broadcast: returned stable message_id must name one of the durable fanout \
             rows; returned={returned_id}, rows={mailbox_rows:?}"
        ));
    }
    for row in &mailbox_rows {
        let message_id = row["message_id"].as_str().expect("message_id");
        if row.get("status") != Some(&json!("stored_only")) {
            failures.push(format!(
                "RED tooth broadcast: every expanded recipient must use the same non-injecting \
                 disposition; row={row}"
            ));
        }
        if store
            .claim_for_delivery(message_id)
            .expect("claim mailbox broadcast row")
        {
            failures.push(format!(
                "RED tooth broadcast: no mailbox branch may enter leader/worker injection; \
                 row={row}"
            ));
        }
    }

    let ordinary = tools
        .send_message(
            &MessageTarget::Broadcast,
            "broadcast ordinary positive control",
            None,
            None,
            None,
        )
        .expect("ordinary broadcast remains accepted");
    if !matches!(&ordinary, SendOutcome::Direct(_)) {
        failures.push(
            "positive control: ordinary broadcast keeps its aggregate direct response".to_string(),
        );
    }
    let ordinary_value = ordinary.to_value();
    required_message_id(&ordinary_value, "ordinary broadcast receipt");
    let ordinary_rows = rows_for_content(
        &store,
        &["leader", "worker-b"],
        "broadcast ordinary positive control",
    );
    if recipients(&ordinary_rows) != BTreeSet::from(["leader".to_string(), "worker-b".to_string()])
    {
        failures.push(format!(
            "positive control: ordinary broadcast fanout remains intact; rows={ordinary_rows:?}"
        ));
    }
    if !ordinary_rows
        .iter()
        .all(|row| row.get("status") != Some(&json!("stored_only")))
    {
        failures.push(format!(
            "positive control: omitting mailbox must not silently suppress the ordinary \
             broadcast; rows={ordinary_rows:?}"
        ));
    }
    let worker_row = ordinary_rows
        .iter()
        .find(|row| row.get("recipient") == Some(&json!("worker-b")))
        .expect("ordinary broadcast worker branch");
    if !store
        .claim_for_delivery(worker_row["message_id"].as_str().expect("message_id"))
        .expect("claim ordinary broadcast worker row")
    {
        failures.push(
            "positive control: ordinary broadcast worker branch retains live-delivery eligibility"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "broadcast mailbox universal-path contract failed:\n{}",
        failures.join("\n")
    );
}

fn seeded_workspace(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let ws = root.join(format!(
        "ta-send-mailbox-universal-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&ws).expect("create isolated workspace");
    let agent = |id: &str| {
        json!({
            "agent_id": id,
            "provider": "codex",
            "status": "running",
            "window": id,
        })
    };
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "leader": {"id": "leader"},
            "agents": {
                "worker-a": agent("worker-a"),
                "worker-b": agent("worker-b"),
            },
            "teams": {
                "team-a": {
                    "status": "alive",
                    "leader": {"id": "leader"},
                    "agents": {
                        "worker-a": agent("worker-a"),
                        "worker-b": agent("worker-b"),
                    },
                },
            },
        }),
    )
    .expect("seed runtime state");
    ws
}

fn worker_tools(workspace: &std::path::Path) -> TeamOrchestratorTools {
    TeamOrchestratorTools::with_identity(
        workspace,
        Some(AgentId::new("worker-a")),
        Some(TeamKey::new("team-a")),
    )
}

fn required_message_id(value: &Value, context: &str) -> String {
    value
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("msg_"))
        .unwrap_or_else(|| {
            panic!("{context} must expose a stable persisted message_id; got {value}")
        })
        .to_string()
}

fn inbox_row(store: &MessageStore, recipient: &str, message_id: &str) -> Value {
    store
        .inbox(recipient, 20, Some("team-a"))
        .expect("read inbox")
        .into_iter()
        .find(|row| row.get("message_id") == Some(&json!(message_id)))
        .unwrap_or_else(|| {
            panic!(
                "durable positive backing missing: recipient={recipient}, message_id={message_id}"
            )
        })
}

fn rows_for_content(store: &MessageStore, recipients: &[&str], content: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    for recipient in recipients {
        rows.extend(
            store
                .inbox(recipient, 20, Some("team-a"))
                .expect("read recipient inbox")
                .into_iter()
                .filter(|row| row.get("recipient") == Some(&json!(recipient)))
                .filter(|row| row.get("content") == Some(&json!(content))),
        );
    }
    rows
}

fn recipients(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.get("recipient").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

fn message_ids(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.get("message_id").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}
