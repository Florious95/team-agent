use super::*;
use crate::transport::test_support::OfflineTransport;
use std::path::Path;

fn pane(session: &str, tty: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new("%7"),
        session: SessionName::new(session),
        window_index: Some(0),
        window_name: Some(WindowName::new("leader")),
        pane_index: Some(0),
        tty: Some(tty.to_string()),
        current_command: Some("codex".to_string()),
        current_path: None,
        active: true,
        pane_pid: Some(77),
        leader_env: BTreeMap::new(),
    }
}

fn receiver(socket: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": "%7",
        "tmux_socket": socket,
        "session_name": "old-session",
        "window_name": "leader",
        "pane_tty": "/dev/old",
        "fingerprint": "old-session|0|0|/dev/old"
    })
}

#[test]
fn live_direct_channel_accepts_session_and_tty_drift_as_diagnostics() {
    let socket = "/tmp/team-agent-channel-a";
    let workspace = Path::new("/tmp/team-agent-workspace-a");
    let transport = OfflineTransport::new()
        .with_tmux_endpoint(socket)
        .with_targets(vec![pane("new-session", "/dev/new")]);

    let LeaderChannelResolution::Live(LiveLeaderChannel::DirectTmux(channel)) =
        resolve_live_leader_channel(workspace, &receiver(socket), &transport)
    else {
        panic!("same socket and pane must remain a live channel");
    };

    assert_eq!(channel.pane_id, "%7");
    assert_eq!(channel.tmux_socket.as_deref(), Some(socket));
    assert_eq!(
        channel.metadata_drift,
        vec!["session_name", "pane_tty", "fingerprint"]
    );
}

#[test]
fn same_pane_id_on_foreign_socket_is_not_a_live_channel() {
    let workspace = Path::new("/tmp/team-agent-workspace-a");
    let transport = OfflineTransport::new()
        .with_tmux_endpoint("/tmp/team-agent-channel-b")
        .with_targets(vec![pane("old-session", "/dev/old")]);

    assert_eq!(
        resolve_live_leader_channel(
            workspace,
            &receiver("/tmp/team-agent-channel-a"),
            &transport,
        ),
        LeaderChannelResolution::Unbound(LeaderChannelUnbound::EndpointMismatch)
    );
}

#[test]
fn attached_record_without_live_pane_is_not_a_live_channel() {
    let socket = "/tmp/team-agent-channel-a";
    let workspace = Path::new("/tmp/team-agent-workspace-a");
    let transport = OfflineTransport::new().with_tmux_endpoint(socket);

    assert_eq!(
        resolve_live_leader_channel(workspace, &receiver(socket), &transport),
        LeaderChannelResolution::Unbound(LeaderChannelUnbound::PaneNotLive)
    );
}

#[test]
fn live_direct_channel_accepts_a_pane_inside_the_target_workspace() {
    let socket = "/tmp/team-agent-channel-a";
    let workspace = Path::new("/tmp/team-agent-workspace-a");
    let mut observed = pane("new-session", "/dev/new");
    observed.current_path = Some(workspace.join("nested"));
    let transport = OfflineTransport::new()
        .with_tmux_endpoint(socket)
        .with_targets(vec![observed]);

    assert!(matches!(
        resolve_live_leader_channel(workspace, &receiver(socket), &transport),
        LeaderChannelResolution::Live(LiveLeaderChannel::DirectTmux(_))
    ));
}

#[test]
fn same_socket_and_pane_in_a_foreign_workspace_is_not_a_live_leader() {
    let socket = "/tmp/team-agent-channel-a";
    let workspace = Path::new("/tmp/team-agent-workspace-a");
    let mut observed = pane("foreign-session", "/dev/foreign");
    observed.current_path = Some(Path::new("/tmp/team-agent-workspace-b").to_path_buf());
    let transport = OfflineTransport::new()
        .with_tmux_endpoint(socket)
        .with_targets(vec![observed]);

    assert_eq!(
        resolve_live_leader_channel(workspace, &receiver(socket), &transport),
        LeaderChannelResolution::Unbound(LeaderChannelUnbound::PaneWorkspaceMismatch)
    );
}
