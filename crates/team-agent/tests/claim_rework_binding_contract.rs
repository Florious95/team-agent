//! Claim rework A4 contract tests. Isolated HOME/workspace.
//! Cargo: `--test claim_rework_binding_contract`.

use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use team_agent::lifecycle::launch::{
    classify_leader_binding, launched_team_receiver_is_attached, LeaderBindingClass,
};

static ISOLATION: AtomicU64 = AtomicU64::new(0);

struct IsolatedHome {
    home: PathBuf,
    previous_home: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn enter() -> Self {
        let n = ISOLATION.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "claim-rework-a4-home-{}-{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::create_dir_all(&home);
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        Self {
            home,
            previous_home,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn unique_workspace() -> PathBuf {
    let n = ISOLATION.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "claim-rework-a4-ws-{}-{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::create_dir_all(&path);
    path
}

#[test]
#[serial_test::serial(env)]
fn isolated_empty_team_is_unbound() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({"teams": {"alpha": {}}}),
    )
    .expect("save empty team");
    assert!(!launched_team_receiver_is_attached(&workspace, "alpha"));
    assert_eq!(
        classify_leader_binding(&workspace, "alpha"),
        LeaderBindingClass::Unbound
    );
}
