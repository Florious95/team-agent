//! Claim rework A2 contract tests. These are registered Cargo integration
//! tests (`--test claim_rework_binding_contract`). They exercise public
//! producer/consumer surfaces, not hand-built BindingFact enums.

use serde_json::json;
use std::path::PathBuf;
use team_agent::lifecycle::launch::launched_team_receiver_is_attached;
use team_agent::messaging::leader_channel::{
    resolve_live_leader_channel, LeaderChannelResolution, LeaderChannelUnbound,
};
use team_agent::transport::test_support::OfflineTransport;

#[test]
fn pending_receiver_is_not_a_live_channel() {
    let workspace = PathBuf::from("/tmp/claim-rework-a2-pending");
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "pending",
        "pane_id": "%1"
    });
    let transport = OfflineTransport::default();
    match resolve_live_leader_channel(&workspace, &receiver, &transport) {
        LeaderChannelResolution::Unbound(LeaderChannelUnbound::ReceiverNotAttached) => {}
        other => panic!("pending seed must not be live; got {other:?}"),
    }
}

#[test]
fn missing_registry_is_not_attached() {
    let workspace = PathBuf::from("/tmp/claim-rework-a2-missing-registry");
    assert!(
        !launched_team_receiver_is_attached(&workspace, "alpha"),
        "absent registry must not count as attached"
    );
}

#[test]
fn attached_status_without_live_pane_is_unbound_not_conflict() {
    let workspace = PathBuf::from("/tmp/claim-rework-a2-dead-pane");
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": "%missing",
        "tmux_socket": "/tmp/does-not-exist.sock"
    });
    let transport = OfflineTransport::default();
    match resolve_live_leader_channel(&workspace, &receiver, &transport) {
        LeaderChannelResolution::Unbound(_) | LeaderChannelResolution::ProbeFailed(_) => {}
        LeaderChannelResolution::Live(_) => {
            panic!("dead pane must not resolve as live conflict/takeover evidence")
        }
    }
}
