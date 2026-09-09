//! Claim rework A3 contract tests. Isolated HOME/workspace.
//! Cargo: `--test claim_rework_binding_contract`.

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
            "claim-rework-a3-home-{}-{}",
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
        "claim-rework-a3-ws-{}-{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::create_dir_all(&path);
    path
}

#[test]
#[serial_test::serial(env)]
fn isolated_missing_registry_is_not_attached() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    assert!(!launched_team_receiver_is_attached(&workspace, "alpha"));
    let class = classify_leader_binding(&workspace, "alpha");
    assert_ne!(class, LeaderBindingClass::Attached);
    assert!(
        matches!(
            class,
            LeaderBindingClass::Unbound | LeaderBindingClass::Unknown
        ),
        "isolated empty workspace must be unbound or unknown; class={class:?}"
    );
}
