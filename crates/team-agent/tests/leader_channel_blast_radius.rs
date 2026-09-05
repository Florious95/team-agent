//! ---
//! purpose: 钉死单 workspace 正常 claim 与离线信箱不被 F1 解绑修法改坏
//! contract:
//!   provides:
//!     - name: LC4-normal-delivery-unchanged
//!       what: 不同 pane 的注册表互不解绑；未 attach 的 leader send 仍进离线信箱
//!   depends:
//!     - team_agent::leader::registry
//!     - cli/send/mailbox.rs queued_until_leader_attach
//! boundary:
//!   - 不测跨 workspace 同 pane 覆盖（那是 LC3）
//!   - 不把 mailbox 改成直接失败
//! maturity: wired
//! ---
//!
//! 生产侧判据（未经血统审计）。

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use hermetic_guard::HermeticTestEnv;
use serde_json::json;
use serial_test::serial;
use std::fs;
use team_agent::leader::registry::{
    build_entry, register_binding_from_state_best_effort, write_entry_best_effort,
};
use team_agent::lifecycle::launch::launched_team_receiver_is_attached;
use team_agent::state::persist::save_runtime_state;

fn mailbox_source() -> String {
    fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/send/mailbox.rs"
    ))
    .expect("read mailbox module")
}

fn seed_attached_state(workspace: &std::path::Path, team: &str, pane: &str, socket: &str) {
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": team,
            "teams": {
                team: {
                    "leader_receiver": {
                        "status": "attached",
                        "pane_id": pane,
                        "tmux_socket": socket,
                        "authorized_team_workspace": workspace.display().to_string(),
                    },
                    "agents": {"w": {"status": "running"}}
                }
            },
            "agents": {"w": {"status": "running"}}
        }),
    )
    .expect("seed state");
}

#[test]
fn mailbox_unattached_receipt_shape_is_unchanged() {
    let source = mailbox_source();
    for required in [
        "\"status\": \"deferred\"",
        "\"deferred_reason\": \"never_attached\"",
        "\"delivered\": false",
        "\"message_status\": \"queued_until_leader_attach\"",
        "enqueue_leader_mailbox_until_attach",
        "\"channel\": \"leader_mailbox\"",
    ] {
        assert!(
            source.contains(required),
            "offline mailbox must keep {required}; do not turn never-attached send into a hard fail"
        );
    }
}

#[test]
#[serial(env)]
fn single_workspace_register_stays_attached() {
    let before = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter("lc4-single");
    env.scrub_tmux();
    let workspace = env.workspace("only");
    seed_attached_state(&workspace, "teamdir", "%1", "/tmp/lc4-only");
    let outcome =
        register_binding_from_state_best_effort(&workspace, Some("teamdir"), "claim-leader")
            .expect("register");
    assert_eq!(outcome.status, "registered");
    assert!(
        launched_team_receiver_is_attached(&workspace, "teamdir"),
        "a single workspace claim must remain attached"
    );
    env.assert_real_registry_unchanged(before);
}

#[test]
#[serial(env)]
fn different_pane_entries_do_not_unbind_each_other() {
    let before = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter("lc4-diffpane");
    env.scrub_tmux();
    let ws_a = env.workspace("a");
    let ws_b = env.workspace("b");
    seed_attached_state(&ws_a, "teama", "%1", "/tmp/lc4-a");
    seed_attached_state(&ws_b, "teamb", "%2", "/tmp/lc4-b");
    let a = build_entry(
        &ws_a,
        "teama",
        "direct_tmux",
        json!({"status":"attached","pane_id":"%1","tmux_socket":"/tmp/lc4-a"}),
        1,
        "claim-leader",
        "2026-08-18T00:00:00Z".to_string(),
    );
    let b = build_entry(
        &ws_b,
        "teamb",
        "direct_tmux",
        json!({"status":"attached","pane_id":"%2","tmux_socket":"/tmp/lc4-b"}),
        1,
        "claim-leader",
        "2026-08-18T00:00:00Z".to_string(),
    );
    assert!(write_entry_best_effort(&a).is_some());
    assert!(write_entry_best_effort(&b).is_some());
    register_binding_from_state_best_effort(&ws_b, Some("teamb"), "claim-leader")
        .expect("register b");
    assert!(
        launched_team_receiver_is_attached(&ws_a, "teama"),
        "claiming a different pane must not unbind another workspace"
    );
    assert!(
        launched_team_receiver_is_attached(&ws_b, "teamb"),
        "the claiming workspace must stay attached"
    );
    env.assert_real_registry_unchanged(before);
}

#[test]
#[serial(env)]
fn same_pane_same_socket_entries_remain_attached_across_teams() {
    let before = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter("lc3-same-pane");
    env.scrub_tmux();
    let ws_a = env.workspace("a");
    let ws_b = env.workspace("b");
    let shared_socket = "/tmp/lc3-shared";
    seed_attached_state(&ws_a, "teama", "%1", shared_socket);
    seed_attached_state(&ws_b, "teamb", "%1", shared_socket);
    let a = build_entry(
        &ws_a,
        "teama",
        "direct_tmux",
        json!({"status":"attached","pane_id":"%1","tmux_socket":shared_socket}),
        1,
        "claim-leader",
        "2026-08-18T00:00:00Z".to_string(),
    );
    let b = build_entry(
        &ws_b,
        "teamb",
        "direct_tmux",
        json!({"status":"attached","pane_id":"%1","tmux_socket":shared_socket}),
        1,
        "claim-leader",
        "2026-08-18T00:00:00Z".to_string(),
    );
    let a_path = write_entry_best_effort(&a).expect("write a");
    assert!(write_entry_best_effort(&b).is_some(), "write b");
    register_binding_from_state_best_effort(&ws_b, Some("teamb"), "claim-leader")
        .expect("register b");
    let persisted_a: team_agent::leader::registry::LeaderRegistryEntry =
        serde_json::from_slice(&fs::read(a_path).expect("read a registry entry"))
            .expect("decode a registry entry");
    assert_eq!(
        persisted_a.status, "attached",
        "same leader pane on the same socket may manage an independent Team"
    );
    env.assert_real_registry_unchanged(before);
}
