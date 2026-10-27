#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::model::enums::Provider;
use crate::transport::{PaneId, SessionName};
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

struct Case {
    ws: PathBuf,
    state: Value,
    target: Target,
    panes: Vec<PaneInfo>,
    started: DateTime<Utc>,
}

impl Case {
    fn new() -> Self {
        let ws = std::env::temp_dir().join(format!(
            "ta-mcp-handshake-{}-{}",
            std::process::id(),
            crate::provider::adapter::next_session_token()
        ));
        std::fs::create_dir(&ws).unwrap();
        let ws = ws.canonicalize().unwrap();
        let config = ws.join(".team/runtime/mcp/worker.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        std::fs::write(&config, serde_json::to_vec(&json!({
            "mcpServers": {"team_orchestrator": {
                "command": executable,
                "env": {"TEAM_AGENT_WORKSPACE": ws, "TEAM_AGENT_ID": "worker", "TEAM_AGENT_OWNER_TEAM_ID": "owner"}
            }}
        })).unwrap()).unwrap();
        // Existing managed configurations are 0644, unlike private receipts.
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
        let carrier = crate::platform::process::current_parent_pid().unwrap();
        assert!(carrier > 1);
        let started = Utc::now();
        let state = json!({"agents": {"worker": {
            "provider": "copilot", "mcp_config": config,
            "spawned_at": (started - chrono::Duration::seconds(10)).to_rfc3339(),
            "pane_id": "%1", "pane_pid": carrier
        }}});
        let panes = vec![PaneInfo {
            pane_id: PaneId::new("%1"),
            session: SessionName::new("owned"),
            window_index: None,
            window_name: None,
            pane_index: None,
            tty: None,
            current_command: None,
            current_path: None,
            active: true,
            pane_pid: Some(carrier),
            leader_env: BTreeMap::new(),
        }];
        Self {
            ws,
            state,
            target: Target::Pane(PaneId::new("%1")),
            panes,
            started,
        }
    }

    fn path(&self) -> PathBuf {
        self.sibling(&std::process::id().to_string())
    }

    /// `<key>.<suffix>.json`; an empty suffix names the pre-#372 single slot.
    fn sibling(&self, suffix: &str) -> PathBuf {
        let name = key("owner", "worker");
        let file = if suffix.is_empty() {
            format!("{name}.json")
        } else {
            format!("{name}.{suffix}.json")
        };
        self.ws.join(".team/runtime/mcp-handshakes").join(file)
    }

    fn write_private(&self, path: &Path, value: &Value) {
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn receipt(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.path()).unwrap()).unwrap()
    }

    fn publish(&self) {
        let outcome = publish(&self.ws, "owner", "worker", self.started);
        if outcome.is_err() {
            for event in crate::event_log::EventLog::new(&self.ws).tail(0).unwrap() {
                if event["event"] == "mcp.handshake_evidence_failed" {
                    eprintln!("CONTROLLED_MCP_HANDSHAKE_FAILURE {event}");
                }
            }
        }
        outcome.unwrap();
    }

    fn admission(&self) -> Result<Option<Admission>> {
        preflight(&self.ws, Some("owner"), "worker", &self.state)
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.ws);
    }
}

#[test]
fn issue345_missing_mcp_config_does_not_disable_eager_admission() {
    // The Case seat is Copilot, the surviving eager-handshake legacy worker.
    let mut case = Case::new();
    case.state["agents"]["worker"]
        .as_object_mut()
        .unwrap()
        .remove("mcp_config");
    assert!(
        case.admission().is_err(),
        "missing config cannot disable copilot admission"
    );
    for value in [Value::Null, json!(false), json!(""), json!(" ")] {
        case.state["agents"]["worker"]["mcp_config"] = value;
        assert!(
            case.admission().is_err(),
            "invalid config cannot disable copilot admission"
        );
    }
}

#[test]
fn missing_handshake_defers_then_a_live_scoped_receipt_admits() {
    let case = Case::new();
    assert!(case.admission().is_err());
    case.publish();
    let admission = case
        .admission()
        .unwrap()
        .expect("eager client must require a receipt");
    revalidate(&admission, &case.target, &case.panes).unwrap();
}

#[test]
fn receipt_cannot_cross_workspace_owner_or_seat() {
    let case = Case::new();
    case.publish();
    assert!(preflight(&case.ws, None, "worker", &case.state).is_err());
    assert!(preflight(&case.ws, Some("other"), "worker", &case.state).is_err());
    let original = std::fs::read(case.path()).unwrap();
    for field in ["workspace", "owner", "agent"] {
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        value[field] = json!("foreign");
        std::fs::write(case.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(case.admission().is_err(), "foreign {field} cannot admit");
    }
}

#[test]
fn malformed_stale_birth_ancestry_and_image_receipts_refuse() {
    let case = Case::new();
    case.publish();
    let original: Value = serde_json::from_slice(&std::fs::read(case.path()).unwrap()).unwrap();
    let mut changed = Vec::new();
    let mut v = original.clone();
    v["schema"] = json!(2);
    changed.push(v);
    let mut v = original.clone();
    v["server"]["identity"]["birth_identity"] = json!("wrong-birth");
    changed.push(v);
    let mut v = original.clone();
    v["server"]["identity"]["executable"] = json!("/not-authorized");
    changed.push(v);
    let mut v = original.clone();
    v["server"]["image"]["inode"] = json!(0);
    changed.push(v);
    let mut v = original.clone();
    v["ancestors"][0]["birth"] = json!("reused-parent");
    changed.push(v);
    let mut v = original.clone();
    v["server"]["parent"] = json!(1);
    changed.push(v);
    let mut v = original.clone();
    v["started_at"] = json!("2000-01-01T00:00:00Z");
    changed.push(v);
    for value in changed {
        std::fs::write(case.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(case.admission().is_err());
    }
    std::fs::write(case.path(), b"{incomplete").unwrap();
    assert!(case.admission().is_err());
}

#[test]
fn changed_current_target_and_new_spawn_do_not_reuse_a_handshake() {
    let mut case = Case::new();
    case.publish();
    let admission = case.admission().unwrap().unwrap();
    let mut other = case.panes.clone();
    other[0].pane_pid = Some(std::process::id());
    assert!(revalidate(&admission, &case.target, &other).is_err());
    assert!(revalidate(&admission, &Target::Pane(PaneId::new("%2")), &case.panes).is_err());
    case.state["agents"]["worker"]["spawned_at"] =
        json!((Utc::now() + chrono::Duration::seconds(1)).to_rfc3339());
    assert!(case.admission().is_err());
}

// Receipt pids beyond any real pid_max, so they never collide with this test.
const EXITED: &str = "4000000001";
const FOREIGN: &str = "4000000002";

/// A server outside the seat pane (inherited TEAM_AGENT_ID) or an exited one
/// may hold a newer receipt; neither can mask the seat's live server (#372).
fn foreign_and_exited(case: &Case) -> (Value, Value) {
    let newer = (case.started + chrono::Duration::seconds(5)).to_rfc3339();
    let mut foreign = case.receipt();
    foreign["started_at"] = json!(newer);
    foreign["flushed_at"] = json!(newer);
    foreign["ancestors"] = json!([{"pid": 1_000_000_001_u32, "birth": "fixture-tmux"}]);
    let mut exited = case.receipt();
    exited["started_at"] = json!(newer);
    exited["flushed_at"] = json!(newer);
    exited["server"]["identity"]["birth_identity"] = json!("exited-server");
    (foreign, exited)
}

#[test]
fn issue372_foreign_or_exited_receipts_cannot_mask_the_live_seat_server() {
    let case = Case::new();
    case.publish();
    let (foreign, exited) = foreign_and_exited(&case);
    case.write_private(&case.sibling(EXITED), &exited);
    case.write_private(&case.sibling(FOREIGN), &foreign);
    let admission = case.admission().unwrap().expect("live seat receipt admits");
    assert_eq!(admission.server.identity.pid, std::process::id());
    revalidate(&admission, &case.target, &case.panes).unwrap();

    std::fs::remove_file(case.path()).unwrap();
    assert_eq!(case.admission().unwrap_err(), "mcp_handshake_server_not_live");
    std::fs::remove_file(case.sibling(EXITED)).unwrap();
    assert_eq!(case.admission().unwrap_err(), "mcp_handshake_ancestry");
    std::fs::remove_file(case.sibling(FOREIGN)).unwrap();
    assert_eq!(case.admission().unwrap_err(), "mcp_handshake_missing");
}

#[test]
fn issue372_pre_upgrade_single_slot_receipt_still_admits() {
    let case = Case::new();
    case.publish();
    std::fs::rename(case.path(), case.sibling("")).unwrap();
    assert!(case.admission().unwrap().is_some());
}

#[test]
fn issue372_publish_prunes_exited_receipts_and_keeps_live_ones() {
    let case = Case::new();
    case.publish();
    let (foreign, exited) = foreign_and_exited(&case);
    case.write_private(&case.sibling(""), &exited);
    case.write_private(&case.sibling(EXITED), &exited);
    case.write_private(&case.sibling(FOREIGN), &foreign);
    case.publish();
    assert!(!case.sibling("").exists());
    assert!(!case.sibling(EXITED).exists());
    assert!(case.sibling(FOREIGN).exists(), "a live foreign receipt is only ignored");
    assert!(case.admission().unwrap().is_some());
}

#[test]
fn issue372_publish_resumes_only_this_seats_parked_messages() {
    let case = Case::new();
    let store = crate::message_store::MessageStore::open(&case.ws).unwrap();
    let parked = store
        .create_message(None, "leader", "worker", "parked", None, false, Some("owner"))
        .unwrap();
    let other = store
        .create_message(None, "leader", "worker", "other team", None, false, Some("other"))
        .unwrap();
    for id in [&parked, &other] {
        assert!(store.claim_for_delivery(id).unwrap());
        assert!(store.park_delivery_before_input(id, 1).unwrap());
    }
    case.publish();
    let row = |id: &str| -> (String, Option<String>) {
        rusqlite::Connection::open(store.db_path())
            .unwrap()
            .query_row(
                "select status, error from messages where message_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(row(parked.as_str()), ("accepted".to_string(), None));
    assert_eq!(
        row(other.as_str()),
        (
            "queued_until_start".to_string(),
            Some("mcp_handshake_timeout".to_string())
        )
    );
}

#[test]
fn symlink_and_writable_evidence_are_not_adopted_or_overwritten() {
    let case = Case::new();
    case.publish();
    std::fs::set_permissions(case.path(), std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(case.admission().is_err());
    std::fs::remove_file(case.path()).unwrap();
    let outside = case.ws.join("outside");
    std::fs::write(&outside, b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, case.path()).unwrap();
    assert!(case.admission().is_err());
    assert!(publish(&case.ws, "owner", "worker", case.started).is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"untouched");
    assert!(std::fs::symlink_metadata(case.path())
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn config_scope_and_authorized_executable_cannot_be_substituted() {
    let case = Case::new();
    case.publish();
    let path = case.ws.join(".team/runtime/mcp/worker.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for field in [
        "TEAM_AGENT_ID",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TEAM_AGENT_WORKSPACE",
    ] {
        let mut value = original.clone();
        value["mcpServers"]["team_orchestrator"]["env"][field] = json!("foreign");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(case.admission().is_err());
    }
    let mut value = original;
    value["mcpServers"]["team_orchestrator"]["command"] = json!("/bin/sh");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(case.admission().is_err());
}

#[test]
fn fake_and_pi_keep_their_existing_non_eager_or_runtime_registry_admission() {
    assert!(!crate::provider::get_adapter(Provider::Fake).requires_worker_mcp_handshake());
    assert!(!crate::provider::get_adapter(Provider::Pi).requires_worker_mcp_handshake());
    assert!(crate::provider::get_adapter(Provider::Copilot).requires_worker_mcp_handshake());
    let case = Case::new();
    for provider in ["fake", "pi"] {
        let mut state = case.state.clone();
        state["agents"]["worker"]["provider"] = json!(provider);
        assert!(preflight(&case.ws, Some("owner"), "worker", &state)
            .unwrap()
            .is_none());
        state["agents"]["worker"]
            .as_object_mut()
            .unwrap()
            .remove("mcp_config");
        assert!(preflight(&case.ws, Some("owner"), "worker", &state)
            .unwrap()
            .is_none());
    }
}
