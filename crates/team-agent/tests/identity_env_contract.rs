//! purpose: 钉死每席生成 env 的 ID/OWNER/AUTH 非空
//! contract:
//!   provides:
//!     - name: J6-owner-auth-not-empty
//!       what: worker_spawn_env 生成的 pane env 三键非空
//! boundary:
//!   - 不改 generic non_empty_string 的其它调用方
//! maturity: wired

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use team_agent::layout::worker_env::worker_spawn_env;

#[test]
fn pane_env_carries_id_owner_and_auth() {
    let pane = worker_spawn_env(
        Vec::<(String, String)>::new(),
        Path::new("/ws"),
        "alpha",
        Some("team-alpha"),
        Some("subscription"),
    );
    for key in [
        "TEAM_AGENT_ID",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TEAM_AGENT_AUTH_MODE",
    ] {
        let value = pane.get(key).map(String::as_str).unwrap_or("");
        assert!(
            !value.trim().is_empty(),
            "{key} must be non-empty on the generated pane env; pane={pane:?}"
        );
    }
}
