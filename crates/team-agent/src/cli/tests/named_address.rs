use super::*;
use crate::transport::test_support::OfflineTransport;
use crate::transport::{PaneId, PaneInfo, SessionName, WindowName};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn named_ws(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-named-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join(".team").join("runtime")).unwrap();
    dir
}

fn seed_state(ws: &Path, state: serde_json::Value) {
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    std::fs::write(
        ws.join(".team").join("runtime").join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
}

fn state_with_teams(teams: serde_json::Value) -> serde_json::Value {
    json!({
        "session_name": "projected",
        "tmux_endpoint": "workspace-socket",
        "teams": teams,
    })
}

fn worker_team(session: &str, agent: &str, pane: &str, window: &str) -> serde_json::Value {
    json!({
        "session_name": session,
        "tmux_endpoint": format!("{session}-socket"),
        "agents": {
            agent: {
                "status": "running",
                "pane_id": pane,
                "window": window,
            }
        }
    })
}

fn worker_team_with_status(
    session: &str,
    agent: &str,
    pane: &str,
    window: &str,
    status: &str,
) -> serde_json::Value {
    let mut team = worker_team(session, agent, pane, window);
    team["agents"][agent]["status"] = json!(status);
    team
}

fn leader_team(session: &str, pane: &str, socket: &str) -> serde_json::Value {
    json!({
        "session_name": session,
        "tmux_endpoint": format!("{session}-worker-socket"),
        "agents": {},
        "leader_receiver": {
            "pane_id": pane,
            "session_name": format!("{session}-leader"),
            "window_name": "codex",
            "tmux_socket": socket,
            "status": "attached"
        }
    })
}

fn pane(session: &str, window: &str, pane_id: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new(session),
        window_index: None,
        window_name: Some(WindowName::new(window)),
        pane_index: None,
        tty: None,
        current_command: None,
        current_path: None,
        active: false,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

fn assert_kind(
    result: Result<ResolvedNamedAddress, NamedAddressError>,
    kind: NamedAddressErrorKind,
) {
    let err = result.unwrap_err();
    assert_eq!(err.kind, kind, "{err}");
    let n38 = err.n38_message();
    assert!(n38.contains("Error: "), "{n38}");
    assert!(n38.contains("Action: "), "{n38}");
    assert!(n38.contains("Log: "), "{n38}");
}

fn named_send_args(
    ws: &Path,
    to_name: Option<&str>,
    pane: Option<&str>,
    targets: Option<&str>,
    message: &[&str],
) -> SendArgs {
    SendArgs {
        target: None,
        message: message.iter().map(|part| (*part).to_string()).collect(),
        targets: targets.map(str::to_string),
        workspace: ws.to_path_buf(),
        team: None,
        task: None,
        sender: "leader".to_string(),
        no_ack: false,
        no_wait: false,
        watch_result: false,
        timeout: 0.0,
        confirm_human: false,
        json: true,
        message_id: None,
        pane: pane.map(str::to_string),
        to_name: to_name.map(str::to_string),
    }
}

#[test]
fn resolve_name_current_team_unique() {
    let ws = named_ws("unique");
    seed_state(
        &ws,
        state_with_teams(json!({
            "team-a": worker_team("team-a", "qa", "%old", "qa")
        })),
    );
    let transport = OfflineTransport::new().with_targets(vec![pane("team-a", "qa", "%1")]);

    let resolved = resolve_name_with_transport(&ws, "qa", &transport).unwrap();
    assert_eq!(resolved.pane_id, "%1");
    assert_eq!(resolved.team_key.as_deref(), Some("team-a"));
    assert_eq!(resolved.agent_id.as_deref(), Some("qa"));
    assert_eq!(resolved.target_kind, NamedTargetKind::Worker);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_ambiguous_same_role() {
    let ws = named_ws("ambiguous-role");
    seed_state(
        &ws,
        state_with_teams(json!({
            "team-a": worker_team("team-a", "qa", "%1", "qa"),
            "team-b": worker_team("team-b", "qa", "%2", "qa")
        })),
    );
    let transport = OfflineTransport::new()
        .with_targets(vec![pane("team-a", "qa", "%1"), pane("team-b", "qa", "%2")]);

    let err = resolve_name_with_transport(&ws, "qa", &transport).unwrap_err();
    assert_eq!(err.kind, NamedAddressErrorKind::NameAmbiguous);
    let candidates = serde_json::to_string(&err.candidates).unwrap();
    assert!(candidates.contains("team-a/qa"), "{candidates}");
    assert!(candidates.contains("team-b/qa"), "{candidates}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_team_disambiguated() {
    let ws = named_ws("team-disambiguated");
    seed_state(
        &ws,
        state_with_teams(json!({
            "team-a": worker_team("team-a", "qa", "%1", "qa"),
            "team-b": worker_team("team-b", "qa", "%2", "qa")
        })),
    );
    let transport = OfflineTransport::new()
        .with_targets(vec![pane("team-a", "qa", "%1"), pane("team-b", "qa", "%2")]);

    let resolved = resolve_name_with_transport(&ws, "team-b/qa", &transport).unwrap();
    assert_eq!(resolved.pane_id, "%2");
    assert_eq!(resolved.team_key.as_deref(), Some("team-b"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_stale_pane() {
    let ws = named_ws("stale-pane");
    seed_state(
        &ws,
        state_with_teams(json!({
            "team-a": worker_team("team-a", "qa", "%old", "qa")
        })),
    );
    let transport = OfflineTransport::new().with_targets(vec![pane("team-a", "qa", "%new")]);

    let resolved = resolve_name_with_transport(&ws, "team-a/qa", &transport).unwrap();
    assert_eq!(resolved.pane_id, "%new");
    assert_eq!(resolved.state_pane_id.as_deref(), Some("%old"));
    assert!(resolved.state_pane_stale);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_cross_workspace_socket() {
    let sender = named_ws("sender");
    let target = named_ws("target");
    seed_state(&sender, state_with_teams(json!({})));
    seed_state(
        &target,
        state_with_teams(json!({
            "alpha": worker_team("alpha-session", "qa", "%target-old", "qa")
        })),
    );
    let transport = OfflineTransport::new()
        .with_tmux_endpoint("target-socket")
        .with_targets(vec![pane("alpha-session", "qa", "%target-live")]);

    let resolved = resolve_name_with_transport(
        &sender,
        &format!("{}::alpha/qa", target.display()),
        &transport,
    )
    .unwrap();
    assert_eq!(resolved.sender_workspace, sender);
    assert_eq!(resolved.target_workspace, target);
    assert_eq!(resolved.tmux_endpoint.as_deref(), Some("target-socket"));
    assert_eq!(resolved.pane_id, "%target-live");
    let _ = std::fs::remove_dir_all(&sender);
    let _ = std::fs::remove_dir_all(&target);
}

#[test]
fn send_to_name_mutual_exclusion() {
    let cwd = named_ws("mutual");
    let args = named_send_args(&cwd, Some("team-a/qa"), Some("%1"), None, &["hello"]);
    let err = cmd_send(&args).unwrap_err();
    assert!(
        matches!(err, CliError::Usage(ref message) if message.contains("--to-name and --pane/TARGET/--to are mutually exclusive")),
        "expected --to-name mutual exclusion, got {err:?}"
    );

    let args = named_send_args(&cwd, Some("team-a/qa"), None, Some("worker"), &["hello"]);
    let err = cmd_send(&args).unwrap_err();
    assert!(
        matches!(err, CliError::Usage(ref message) if message.contains("--to-name and --pane/TARGET/--to are mutually exclusive")),
        "expected --to-name/--to mutual exclusion, got {err:?}"
    );

    let args = named_send_args(&cwd, Some("team-a/qa"), None, None, &[]);
    let err = cmd_send(&args).unwrap_err();
    assert!(
        matches!(err, CliError::Usage(ref message) if message == "--to-name requires a non-empty message"),
        "expected empty-message usage error, got {err:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn resolve_leader_name_uses_receiver_socket() {
    let ws = named_ws("leader-socket");
    seed_state(
        &ws,
        state_with_teams(json!({
            "alpha": leader_team("alpha-session", "%leader", "/tmp/leader.sock")
        })),
    );
    let transport = OfflineTransport::new()
        .with_tmux_endpoint("/tmp/leader.sock")
        .with_targets(vec![pane("alpha-session-leader", "codex", "%leader")]);

    let resolved = resolve_name_with_transport(&ws, "alpha/leader", &transport).unwrap();
    assert_eq!(resolved.target_kind, NamedTargetKind::Leader);
    assert_eq!(resolved.pane_id, "%leader");
    assert_eq!(resolved.tmux_endpoint.as_deref(), Some("/tmp/leader.sock"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_leader_name_not_live() {
    let ws = named_ws("leader-not-live");
    seed_state(
        &ws,
        state_with_teams(json!({
            "alpha": leader_team("alpha-session", "%missing", "/tmp/leader.sock")
        })),
    );
    let transport = OfflineTransport::new()
        .with_tmux_endpoint("/tmp/leader.sock")
        .with_targets(vec![pane("other", "codex", "%other")]);

    let err = resolve_name_with_transport(&ws, "alpha/leader", &transport).unwrap_err();
    assert_eq!(err.kind, NamedAddressErrorKind::NameNotLive);
    let n38 = err.n38_message();
    assert!(n38.contains("claim-leader"), "{n38}");
    assert!(n38.contains("attach-leader"), "{n38}");
    assert!(n38.contains("takeover"), "{n38}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_leader_name_after_rebind() {
    let ws = named_ws("leader-rebind");
    seed_state(
        &ws,
        state_with_teams(json!({
            "alpha": leader_team("alpha-session", "%new", "/tmp/leader.sock")
        })),
    );
    let transport = OfflineTransport::new()
        .with_tmux_endpoint("/tmp/leader.sock")
        .with_targets(vec![pane("alpha-session-leader", "codex", "%new")]);

    let resolved = resolve_name_with_transport(&ws, "alpha/leader", &transport).unwrap();
    assert_eq!(resolved.pane_id, "%new");
    assert_eq!(resolved.target_kind, NamedTargetKind::Leader);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_invalid_syntax() {
    let ws = named_ws("invalid");
    let transport = OfflineTransport::new();
    assert_kind(
        resolve_name_with_transport(&ws, "", &transport),
        NamedAddressErrorKind::NameInvalid,
    );
    assert_kind(
        resolve_name_with_transport(&ws, "team//qa", &transport),
        NamedAddressErrorKind::NameInvalid,
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_workspace_not_found() {
    let ws = named_ws("sender-missing-target");
    let missing = std::env::temp_dir().join(format!(
        "ta-named-missing-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let transport = OfflineTransport::new();
    assert_kind(
        resolve_name_with_transport(&ws, &format!("{}::alpha/qa", missing.display()), &transport),
        NamedAddressErrorKind::WorkspaceNotFound,
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_state_missing() {
    let ws = named_ws("state-missing");
    let state_path = ws.join(".team").join("runtime").join("state.json");
    let _ = std::fs::remove_file(state_path);
    let transport = OfflineTransport::new();
    assert_kind(
        resolve_name_with_transport(&ws, "alpha/qa", &transport),
        NamedAddressErrorKind::StateNotFound,
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn resolve_name_same_team_key_across_workspaces() {
    let sender = named_ws("same-team-sender");
    let other = named_ws("same-team-other");
    seed_state(
        &sender,
        state_with_teams(json!({
            "alpha": worker_team("alpha-a", "qa", "%a", "qa")
        })),
    );
    seed_state(
        &other,
        state_with_teams(json!({
            "alpha": worker_team("alpha-b", "qa", "%b", "qa")
        })),
    );
    let sender_transport = OfflineTransport::new().with_targets(vec![pane("alpha-a", "qa", "%a")]);
    let other_transport = OfflineTransport::new().with_targets(vec![pane("alpha-b", "qa", "%b")]);

    let err = resolve_name_with_workspace_transports(
        &sender,
        "alpha/qa",
        &[
            NamedWorkspaceTransport {
                workspace: &sender,
                transport: &sender_transport,
            },
            NamedWorkspaceTransport {
                workspace: &other,
                transport: &other_transport,
            },
        ],
    )
    .unwrap_err();
    assert_eq!(err.kind, NamedAddressErrorKind::NameAmbiguous);
    assert!(err.n38_message().contains("<workspace>::alpha/qa"));
    let _ = std::fs::remove_dir_all(&sender);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn resolve_paused_agent_with_live_pane_succeeds_with_warning() {
    let ws = named_ws("paused-live");
    seed_state(
        &ws,
        state_with_teams(json!({
            "alpha": worker_team_with_status("alpha-session", "qa", "%paused", "qa", "paused")
        })),
    );
    let transport =
        OfflineTransport::new().with_targets(vec![pane("alpha-session", "qa", "%paused")]);

    let resolved = resolve_name_with_transport(&ws, "alpha/qa", &transport).unwrap();
    assert_eq!(resolved.pane_id, "%paused");
    assert_eq!(resolved.agent_status.as_deref(), Some("paused"));
    assert!(resolved.warning.as_deref().unwrap_or("").contains("paused"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn named_address_resolver_does_not_write_leader_receiver_or_owner_fields() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/named_address.rs"
    ))
    .unwrap();
    for forbidden in ["leader_receiver.*=", "team_owner.*=", "owner_epoch.*="] {
        assert!(
            !src.contains(forbidden),
            "resolver must not contain binding write pattern {forbidden}"
        );
    }
    for line in src.lines() {
        let trimmed = line.trim();
        for field in ["leader_receiver", "team_owner", "owner_epoch"] {
            assert!(
                !(trimmed.starts_with(field) && trimmed.contains('=')),
                "resolver must not assign binding field `{field}`: {trimmed}"
            );
            assert!(
                !(trimmed.starts_with(&format!(".{field}")) && trimmed.contains('=')),
                "resolver must not assign binding field `{field}`: {trimmed}"
            );
        }
    }
}
