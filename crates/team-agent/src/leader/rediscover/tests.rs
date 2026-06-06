#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
    PaneField, PaneInfo, PaneLiveness, SetEnvOutcome, SpawnResult, Target, Transport,
    TransportError,
};

fn ws(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ta-rs-redisc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    dir
}

fn receiver(pane: &str, uuid: &str, epoch: u64) -> LeaderReceiver {
    LeaderReceiver {
        mode: ReceiverMode::DirectTmux,
        status: ReceiverStatus::Attached,
        provider: Provider::Codex,
        pane_id: PaneId::new(pane),
        session_name: None,
        window_index: None,
        window_name: None,
        pane_index: None,
        pane_tty: None,
        pane_current_command: None,
        tmux_socket: None,
        fingerprint: None,
        leader_session_uuid: serde_json::from_value(Value::String(uuid.to_string())).ok(),
        owner_epoch: Some(OwnerEpoch(epoch)),
        attached_at: None,
        discovery: None,
        requested_provider: None,
        warning: None,
    }
}

fn owner(pane: &str, uuid: &str, epoch: u64) -> TeamOwner {
    TeamOwner {
        pane_id: PaneId::new(pane),
        provider: Provider::Codex,
        machine_fingerprint: "fp".to_string(),
        leader_session_uuid: serde_json::from_value(Value::String(uuid.to_string())).ok(),
        owner_epoch: OwnerEpoch(epoch),
        claimed_at: "t".to_string(),
        claimed_via: ClaimedVia::ClaimLeader,
        os_user: None,
    }
}

fn pane_json(workspace: &Path, pane: &str, uuid: &str) -> Value {
    json!({
        "pane_id": pane,
        "session_name": "sess",
        "window_name": "leader",
        "pane_current_command": "codex",
        "pane_current_path": workspace.join("sub").to_string_lossy(),
        "leader_session_uuid": uuid,
        "leader_env": {
            "TEAM_AGENT_LEADER_SESSION_UUID": uuid,
            "TEAM_AGENT_MACHINE_FINGERPRINT": "fp",
            "TEAM_AGENT_WORKSPACE": workspace.to_string_lossy(),
            "TEAM_AGENT_TEAM_ID": "current"
        }
    })
}

fn event_named(events: &[Value], name: &str) -> Value {
    events
        .iter()
        .find(|event| event.get("event").and_then(Value::as_str) == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("missing event {name}; got {events:?}"))
}

fn last_event_named(events: &[Value], name: &str) -> Value {
    events
        .iter()
        .rev()
        .find(|event| event.get("event").and_then(Value::as_str) == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("missing event {name}; got {events:?}"))
}

#[test]
fn try_readopt_writes_owner_receiver_dual_state_and_events() {
    let ws = ws("readopt-ok");
    let mut state = json!({"session_name": "sess"});
    let mut recv = receiver("%old", "OLDUUID123456", 3);
    let owner = owner("%old", "OLDUUID123456", 3);
    let event_log = crate::event_log::EventLog::new(&ws);

    let got = try_readopt_leader_pane(
        &ws,
        &mut state,
        &mut recv,
        &pane_json(&ws, "%new", "NEWUUID123456"),
        &json!({"targets": []}),
        Some(&owner),
        Provider::Codex,
        LeaseSource::Manual,
        &event_log,
    )
    .unwrap()
    .expect("readopt");

    assert_eq!(got.1["ok"], json!(true));
    assert_eq!(got.1["pane"]["pane_id"], json!("%new"));
    assert_eq!(got.1["readopted"], json!(true));
    assert!(got.1.get("warning").is_some_and(Value::is_null));
    assert_eq!(state["team_owner"]["pane_id"], json!("%new"));
    assert_eq!(state["team_owner"]["claimed_via"], json!("attach-leader"));
    assert_eq!(state["team_owner"]["owner_epoch"], json!(4));
    assert_eq!(state["leader_receiver"]["discovery"], json!("attach_readopt"));
    assert!(crate::model::paths::runtime_dir(&ws)
        .join("teams")
        .join("sess")
        .join("state.json")
        .exists());
    let events = event_log.tail(10).unwrap();
    let adopted = event_named(&events, "owner.adopted_on_restart");
    assert_eq!(adopted["reason"], json!("attach_readopt"));
    assert_eq!(adopted["old_pane_id"], json!("%old"));
    assert_eq!(adopted["new_pane_id"], json!("%new"));
    assert_eq!(adopted["owner_epoch"], json!(4));
    assert_eq!(adopted["uuid_prefix"], json!("NEWUUID1"));
    assert_eq!(adopted["team_id"], json!("sess"));
    let rebound = event_named(&events, "leader_receiver.rebind_applied");
    assert_eq!(rebound["reason"], json!("attach_readopt"));
    assert_eq!(rebound["old_pane_id"], json!("%old"));
    assert_eq!(rebound["new_pane_id"], json!("%new"));
    assert!(rebound.get("owner_identity").is_none());
    assert_eq!(rebound["owner_epoch"], json!(4));
    assert_eq!(rebound["uuid_prefix"], json!("NEWUUID1"));
    assert_eq!(rebound["team_id"], json!("sess"));
    let attached = event_named(&events, "leader_receiver.attached");
    assert_eq!(attached["target"], json!("%new"));
    assert_eq!(attached["session_name"], json!("sess"));
    assert_eq!(attached["provider"], json!("codex"));
    assert_eq!(attached["discovery"], json!("attach_readopt"));
    assert_eq!(attached["source"], json!("manual"));
    assert_eq!(attached["owner_epoch"], json!(4));
    assert_eq!(attached["uuid_prefix"], json!("NEWUUID1"));
}

#[test]
fn try_readopt_refuses_when_different_live_owner_uuid_mismatches() {
    let ws = ws("readopt-live-owner");
    let mut state = json!({});
    let mut recv = receiver("%old", "OLDUUID", 3);
    let owner = owner("%old", "OLDUUID", 3);
    let event_log = crate::event_log::EventLog::new(&ws);
    let targets = json!({"targets": [pane_json(&ws, "%old", "OLDUUID")]});

    let got = try_readopt_leader_pane(
        &ws,
        &mut state,
        &mut recv,
        &pane_json(&ws, "%new", "NEWUUID"),
        &targets,
        Some(&owner),
        Provider::Codex,
        LeaseSource::Manual,
        &event_log,
    )
    .unwrap();

    assert!(got.is_none());
    assert!(state.get("team_owner").is_none());
}

#[test]
fn rediscover_updates_unique_env_triple_match() {
    let ws = ws("rediscover");
    let mut env = BTreeMap::new();
    env.insert("TEAM_AGENT_LEADER_PANE_ID".to_string(), "%r".to_string());
    env.insert("TEAM_AGENT_LEADER_PROVIDER".to_string(), "codex".to_string());
    env.insert("TEAM_AGENT_MACHINE_FINGERPRINT".to_string(), "fp".to_string());
    let target = PaneInfo {
        pane_id: PaneId::new("%r"),
        session: SessionName::new("sess"),
        window_index: Some(1),
        window_name: Some(WindowName::new("leader")),
        pane_index: Some(0),
        tty: Some("/ttys001".to_string()),
        current_command: Some("bash".to_string()),
        current_path: Some(std::env::temp_dir().join("foreign")),
        active: true,
        pane_pid: None,
        leader_env: env,
    };
    let mut state = json!({
        "session_name": "sess",
        "team_owner": {
            "pane_id": "%r",
            "provider": "codex",
            "machine_fingerprint": "fp",
            "leader_session_uuid": "OWNERUUID123456",
            "owner_epoch": 2,
            "claimed_at": "t",
            "claimed_via": "claim-leader"
        }
    });
    let event_log = crate::event_log::EventLog::new(&ws);

    let out = rediscover_leader_receiver_from_targets(&ws, &mut state, &[target], &event_log).unwrap();

    assert_eq!(out["status"], json!("updated"));
    assert!(out.get("ok").is_none(), "golden rediscover result has no ok wrapper");
    assert_eq!(out["owner_identity"]["pane_id"], json!("%r"));
    assert_eq!(state["leader_receiver"]["pane_id"], json!("%r"));
    let events = event_log.tail(10).unwrap();
    let rediscovered = event_named(&events, "leader_receiver.rediscovered");
    assert_eq!(rediscovered["provider"], json!("codex"));
    assert_eq!(rediscovered["old_target"], json!("%r"));
    assert_eq!(rediscovered["new_target"], json!("%r"));
    assert_eq!(rediscovered["candidate_count"], json!(1));
    assert_eq!(rediscovered["owner_identity"]["provider"], json!("codex"));
    let rebound = event_named(&events, "leader_receiver.rebind_applied");
    assert!(rebound.get("reason").is_some_and(Value::is_null));
    assert_eq!(rebound["old_pane_id"], json!("%r"));
    assert_eq!(rebound["new_pane_id"], json!("%r"));
    assert_eq!(rebound["owner_identity"]["machine_fingerprint"], json!("fp"));
    assert_eq!(rebound["uuid_prefix"], json!("OWNERUUI"));
    assert_eq!(state["leader_receiver"]["discovery"], json!("stale_rediscovery_unique_candidate"));
}

#[test]
fn rediscover_owner_identity_passthrough_sets_discovery_and_reason() {
    let ws = ws("rediscover-owner-identity");
    let target = PaneInfo {
        pane_id: PaneId::new("%raw"),
        session: SessionName::new("sess"),
        window_index: None,
        window_name: None,
        pane_index: None,
        tty: None,
        current_command: Some("node".to_string()),
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::from([
            ("TEAM_AGENT_LEADER_PANE_ID".to_string(), "%raw".to_string()),
            ("TEAM_AGENT_LEADER_PROVIDER".to_string(), "codex".to_string()),
            ("TEAM_AGENT_MACHINE_FINGERPRINT".to_string(), "fp".to_string()),
        ]),
    };
    let raw_identity = json!({
        "provider": "codex",
        "pane_id": "%raw",
        "machine_fingerprint": "fp",
        "leader_session_uuid": "RAWUUID123456",
        "custom": "kept"
    });
    let mut state = json!({"session_name": "sess"});
    let event_log = crate::event_log::EventLog::new(&ws);

    let out = rediscover_leader_receiver_from_targets_with_owner_identity(
        &ws,
        &mut state,
        &[target],
        &event_log,
        &raw_identity,
        Some("stale_receiver"),
    )
    .unwrap();

    assert_eq!(out["status"], json!("updated"));
    assert_eq!(out["owner_identity"], raw_identity);
    assert_eq!(state["leader_receiver"]["discovery"], json!("stale_rediscovery_owner_identity"));
    let rebound = event_named(&event_log.tail(10).unwrap(), "leader_receiver.rebind_applied");
    assert_eq!(rebound["reason"], json!("stale_receiver"));
    assert_eq!(rebound["owner_identity"], raw_identity);
    assert_eq!(rebound["uuid_prefix"], json!("RAWUUID1"));
}

#[test]
fn rediscover_missing_and_ambiguous_use_golden_status_names() {
    let ws = ws("rediscover-status");
    let event_log = crate::event_log::EventLog::new(&ws);
    let mut state = json!({
        "session_name": "sess",
        "team_owner": {
            "pane_id": "%owner",
            "provider": "codex",
            "machine_fingerprint": "fp",
            "leader_session_uuid": "OWNERUUID",
            "owner_epoch": 2,
            "claimed_at": "t",
            "claimed_via": "claim-leader"
        }
    });
    let missing = rediscover_leader_receiver_from_targets(&ws, &mut state, &[], &event_log).unwrap();
    assert_eq!(missing["status"], json!("missing"));
    assert!(missing.get("ok").is_none());
    assert_eq!(missing["owner_identity"]["pane_id"], json!("%owner"));
    let missing_event = event_named(&event_log.tail(10).unwrap(), "leader_receiver.rediscover_missing");
    assert_eq!(missing_event["owner_identity"]["pane_id"], json!("%owner"));
    assert_eq!(missing_event["provider"], json!("codex"));
    assert_eq!(missing_event["old_target"], json!("%owner"));
    assert_eq!(missing_event["candidate_count"], json!(0));
    let missing_rebind = last_event_named(&event_log.tail(10).unwrap(), "leader_receiver.rebind_required");
    assert!(missing_rebind.get("reason").is_some_and(Value::is_null));
    assert_eq!(missing_rebind["provider"], json!("codex"));
    assert_eq!(missing_rebind["team_id"], json!("sess"));
    assert_eq!(missing_rebind["uuid_prefix"], json!("OWNERUUI"));
    assert_eq!(missing_rebind["owner_identity"]["pane_id"], json!("%owner"));
    assert_eq!(
        missing_rebind["recovery_action"],
        json!("run team-agent attach-leader or claim-leader")
    );
    assert!(missing_rebind.get("invalidation_reason").is_none());

    let mut env = BTreeMap::new();
    env.insert("TEAM_AGENT_LEADER_PANE_ID".to_string(), "%owner".to_string());
    env.insert("TEAM_AGENT_LEADER_PROVIDER".to_string(), "codex".to_string());
    env.insert("TEAM_AGENT_MACHINE_FINGERPRINT".to_string(), "fp".to_string());
    let mk = |pane: &str, window: u32, pane_index: u32, tty: &str| PaneInfo {
        pane_id: PaneId::new(pane),
        session: SessionName::new("sess"),
        window_index: Some(window),
        window_name: None,
        pane_index: Some(pane_index),
        tty: Some(tty.to_string()),
        current_command: Some("codex".to_string()),
        current_path: Some(std::env::temp_dir().join(format!("rediscover-{pane}"))),
        active: true,
        pane_pid: None,
        leader_env: env.clone(),
    };
    let ambiguous = rediscover_leader_receiver_from_targets(
        &ws,
        &mut state,
        &[mk("%b", 2, 1, "/ttys002"), mk("%a", 1, 0, "/ttys001")],
        &event_log,
    )
    .unwrap();
    assert_eq!(ambiguous["status"], json!("ambiguous"));
    assert!(ambiguous.get("ok").is_none());
    assert_eq!(ambiguous["owner_candidates"].as_array().map(Vec::len), Some(2));
    assert_eq!(ambiguous["owner_candidates"][0]["pane_id"], json!("%a"));
    assert_eq!(ambiguous["owner_candidates"][0]["window_index"], json!(1));
    assert_eq!(ambiguous["owner_candidates"][0]["pane_index"], json!(0));
    assert_eq!(ambiguous["owner_candidates"][0]["pane_tty"], json!("/ttys001"));
    assert_eq!(ambiguous["owner_candidates"][0]["current_command"], json!("codex"));
    assert_eq!(ambiguous["owner_candidates"][0]["fingerprint"], json!("fp"));
    assert!(ambiguous["owner_candidates"][0]["current_path"]
        .as_str()
        .is_some_and(|path| path.contains("rediscover-%a")));
    assert!(ambiguous.get("incident").is_none());
    assert!(ambiguous["incident_id"].as_str().is_some_and(|id| id.starts_with("rediscover_")));
    assert_eq!(ambiguous["deduped"], json!(false));
    let events = event_log.tail(20).unwrap();
    let ambiguous_event = event_named(&events, "leader_receiver.rediscover_ambiguous");
    assert_eq!(ambiguous_event["candidates"], json!(["%a", "%b"]));
    assert_eq!(ambiguous_event["incident_id"], ambiguous["incident_id"]);
    assert_eq!(ambiguous_event["deduped"], json!(false));
    let candidates_event = event_named(&events, "leader_receiver.ambiguous_candidates");
    assert_eq!(candidates_event["pane_ids"], json!(["%a", "%b"]));
    assert_eq!(candidates_event["provider"], json!("codex"));
    assert_eq!(candidates_event["team_id"], json!("sess"));
    assert_eq!(candidates_event["uuid_prefix"], json!("OWNERUUI"));
    assert_eq!(candidates_event["debounce_bucket"], ambiguous["incident_id"]);
    assert_eq!(candidates_event["reason"], json!("force_confirm_required"));
    assert_eq!(candidates_event["queued"].as_array().map(Vec::len), Some(2));
}

struct FailingListTargetsTransport;

impl FailingListTargetsTransport {
    fn unsupported(&self) -> TransportError {
        TransportError::MuxUnavailable {
            backend: BackendKind::Tmux,
            detail: "unsupported in test".to_string(),
        }
    }
}

impl Transport for FailingListTargetsTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Err(self.unsupported())
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Err(self.unsupported())
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Err(self.unsupported())
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Err(self.unsupported())
    }

    fn capture(
        &self,
        _target: &Target,
        _range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Err(self.unsupported())
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Err(self.unsupported())
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Err(self.unsupported())
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Err(TransportError::MuxUnavailable {
            backend: BackendKind::Tmux,
            detail: "scan failed".to_string(),
        })
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Err(self.unsupported())
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Err(self.unsupported())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Err(self.unsupported())
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Err(self.unsupported())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Err(self.unsupported())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Err(self.unsupported())
    }
}

#[test]
fn rediscover_failed_uses_golden_status_and_rebind_shape() {
    let ws = ws("rediscover-failed");
    let event_log = crate::event_log::EventLog::new(&ws);
    let mut state = json!({
        "session_name": "sess",
        "leader_receiver": {
            "pane_id": "%old",
            "provider": "codex"
        }
    });
    let raw_identity = json!({
        "provider": "codex",
        "pane_id": "%owner",
        "machine_fingerprint": "fp",
        "leader_session_uuid": "OWNERUUID123456",
        "team_id": "sess"
    });

    let out = rediscover_leader_receiver_with_owner_identity(
        &ws,
        &mut state,
        &FailingListTargetsTransport,
        &event_log,
        &raw_identity,
        Some("stale_receiver"),
    )
    .unwrap();

    assert_eq!(out["status"], json!("failed"));
    assert!(out["error"].as_str().is_some_and(|error| error.contains("scan failed")));
    assert!(out.get("ok").is_none());
    let events = event_log.tail(10).unwrap();
    let failed = event_named(&events, "leader_receiver.rediscover_failed");
    assert_eq!(failed["provider"], json!("codex"));
    assert!(failed["error"].as_str().is_some_and(|error| error.contains("scan failed")));
    let rebind = event_named(&events, "leader_receiver.rebind_required");
    assert_eq!(rebind["old_pane_id"], json!("%old"));
    assert_eq!(rebind["reason"], json!("stale_receiver"));
    assert_eq!(rebind["provider"], json!("codex"));
    assert_eq!(rebind["team_id"], json!("sess"));
    assert_eq!(rebind["rediscovery_status"], json!("failed"));
    assert!(rebind["error"].as_str().is_some_and(|error| error.contains("scan failed")));
    assert!(rebind.get("owner_identity").is_none());
    assert!(rebind.get("uuid_prefix").is_none());
    assert!(rebind.get("invalidation_reason").is_none());
}
