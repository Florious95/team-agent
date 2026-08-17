//! ---
//! purpose: 钉死 launched_team_receiver_is_attached 以 host 注册表为可投递性权威，检测失败倒向未接
//! contract:
//!   provides:
//!     - name: LC2-detect-fails-conservative
//!       what: 无注册表条目、运行态读不出、条目授权指向别的 workspace，三者都必须判未接
//!   depends:
//!     - team_agent::lifecycle::launch::launched_team_receiver_is_attached
//!     - team_agent::leader::registry
//!     - ~/.team-agent/leaders/<workspace_hash>__<team_key>.json
//! boundary:
//!   - 不测 claim-leader 写路径，不测投递/信箱
//!   - 不把 state.json 的 leader_receiver 副本当成已接
//!   - discovery=quick_start 的 attached 不是凭据，注册表才是
//! maturity: wired
//! ---
//!
//! 生产侧判据（未经血统审计）。基线实现会把「读不出运行态」和「无注册表」乐观判成已接。

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use hermetic_guard::HermeticTestEnv;
use serde_json::json;
use serial_test::serial;
use std::path::Path;
use team_agent::leader::registry::{build_entry, write_entry_best_effort};
use team_agent::lifecycle::launch::launched_team_receiver_is_attached;
use team_agent::state::persist::save_runtime_state;

const TEAM_KEY: &str = "teamdir";

fn attached_looking_state() -> serde_json::Value {
    json!({
        "active_team_key": TEAM_KEY,
        "is_external_leader": true,
        "agents": {"implementer": {"status": "running"}},
        "teams": {
            TEAM_KEY: {
                "is_external_leader": true,
                "agents": {"implementer": {"status": "running"}},
                "leader_receiver": {
                    "status": "attached",
                    "discovery": "quick_start",
                    "pane_id": "%1",
                    "tmux_socket": "/tmp/does-not-matter"
                }
            }
        }
    })
}

fn write_registry_entry(workspace: &Path, authorized_team_workspace: &Path) {
    let entry = build_entry(
        workspace,
        TEAM_KEY,
        "direct_tmux",
        json!({
            "status": "attached",
            "pane_id": "%1",
            "authorized_team_workspace": authorized_team_workspace.display().to_string(),
        }),
        1,
        "lc2-detect-conservative",
        "2026-08-18T00:00:00Z".to_string(),
    );
    assert!(
        write_entry_best_effort(&entry).is_some(),
        "hermetic HOME must accept a registry write"
    );
}

#[test]
#[serial(env)]
fn registry_missing_is_unbound_even_when_state_copy_looks_attached() {
    let before = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter("lc2-no-reg");
    env.scrub_tmux();
    let workspace = env.workspace("ws");
    save_runtime_state(&workspace, &attached_looking_state()).expect("seed state.json copy");
    assert!(
        env.registry_entries().is_empty(),
        "this case is the missing-registry world"
    );

    assert!(
        !launched_team_receiver_is_attached(&workspace, TEAM_KEY),
        "discovery=quick_start attached in state.json is not credentials; missing registry must be unbound"
    );
    env.assert_real_registry_unchanged(before);
}

#[test]
#[serial(env)]
fn unreadable_runtime_state_is_unbound_not_attached() {
    let before = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter("lc2-no-state");
    env.scrub_tmux();
    let workspace = env.workspace("ws");
    let state_path = workspace.join(".team/runtime/state.json");
    std::fs::create_dir_all(state_path.parent().expect("state parent"))
        .expect("create runtime dir");
    std::fs::write(&state_path, "{this is not json").expect("write corrupt state.json");

    assert!(
        !launched_team_receiver_is_attached(&workspace, TEAM_KEY),
        "unreadable runtime must be unbound/undecidable, never attached"
    );
    env.assert_real_registry_unchanged(before);
}

#[test]
#[serial(env)]
fn registry_entry_authorized_for_other_workspace_is_unbound() {
    let before = HermeticTestEnv::real_home_registry_snapshot();
    let env = HermeticTestEnv::enter("lc2-mismatch");
    env.scrub_tmux();
    let workspace = env.workspace("ws-a");
    let other = env.workspace("ws-b");
    save_runtime_state(&workspace, &attached_looking_state()).expect("seed state.json copy");
    write_registry_entry(&workspace, &other);

    assert!(
        !launched_team_receiver_is_attached(&workspace, TEAM_KEY),
        "registry row whose pane authorization points at another workspace must be unbound"
    );
    env.assert_real_registry_unchanged(before);
}
