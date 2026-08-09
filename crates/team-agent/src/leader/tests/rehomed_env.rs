use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const CALLER_IDENTITY_ENVS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "TEAM_AGENT_LEADER_PANE_ID",
    "TEAM_AGENT_LEADER_SESSION_UUID",
    "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
    "TEAM_AGENT_LEADER_SESSION_NAME",
    "TEAM_AGENT_LEADER_PROVIDER",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
    "TEAM_AGENT_WORKSPACE",
    "TEAM_AGENT_TEAM_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
    "TEAM_AGENT_ACTIVE_TEAM",
    "TEAM_AGENT_ID",
    "TEAM_AGENT_AGENT_ID",
    "TEAM_AGENT_AUTH_MODE",
    "TEAM_AGENT_LEADER_BYPASS",
    "TEAM_AGENT_LEADER_BYPASS_SOURCE",
    "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
    "TEAM_AGENT_LEADER_BYPASS_FLAG",
    "TEAM_AGENT_MCP_AUTO_APPROVE",
    "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
];

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) struct RehomedTestEnv {
    root: PathBuf,
    previous: Vec<(&'static str, Option<String>)>,
}

impl RehomedTestEnv {
    pub(super) fn enter(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ta-leader-rehomed-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("create rehomed test HOME");

        let mut previous = Vec::new();
        for key in std::iter::once("HOME").chain(CALLER_IDENTITY_ENVS.iter().copied()) {
            previous.push((key, std::env::var(key).ok()));
        }
        unsafe {
            std::env::set_var("HOME", &home);
            for key in CALLER_IDENTITY_ENVS {
                std::env::remove_var(key);
            }
        }

        Self { root, previous }
    }

    pub(super) fn workspace(&self, tag: &str) -> PathBuf {
        let path = self.root.join(format!(
            "workspace-{tag}-{}",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create rehomed test workspace");
        path
    }
}

impl Drop for RehomedTestEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
