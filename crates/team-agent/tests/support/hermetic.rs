#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

pub const CALLER_IDENTITY_ENVS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "TEAM_AGENT_LEADER_PANE_ID",
    "TEAM_AGENT_LEADER_SESSION_UUID",
    "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
    "TEAM_AGENT_LEADER_PROVIDER",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
    "TEAM_AGENT_WORKSPACE",
    "TEAM_AGENT_TEAM_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
    "TEAM_AGENT_ACTIVE_TEAM",
    "TEAM_AGENT_ID",
];

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 0.5.43 debt-sweep contract seam. GREEN replaces this stub with a short,
/// process-unique absolute tmux socket path that never depends on long TMPDIR.
pub fn short_tmux_socket(_tag: &str) -> PathBuf {
    panic!("RED 0.5.43: short test-owned tmux socket helper is not implemented")
}

pub struct HermeticTestEnv {
    root: PathBuf,
    home: PathBuf,
    previous: Vec<(&'static str, Option<String>)>,
}

#[derive(Clone, Debug)]
pub struct RegistrySnapshot {
    home: Option<PathBuf>,
    entries: Vec<(PathBuf, String)>,
}

pub struct EnvOverride {
    key: &'static str,
    previous: Option<String>,
}

impl HermeticTestEnv {
    pub fn enter(tag: &str) -> Self {
        let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).expect("create hermetic test tmp root");
        let root = base.join(format!(
            "ta-hermetic-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create hermetic root");
        let root = std::fs::canonicalize(root).expect("canonicalize hermetic root");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".team-agent/leaders"))
            .expect("create hermetic registry root");

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

        Self {
            root,
            home,
            previous,
        }
    }

    pub fn workspace(&self, tag: &str) -> PathBuf {
        let path = self.root.join(format!(
            "workspace-{tag}-{}",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create hermetic workspace");
        std::fs::canonicalize(path).expect("canonicalize hermetic workspace")
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Register an exact test-owned process for Drop cleanup.
    pub fn register_owned_pid(&self, _pid: u32) {}

    /// Register an exact test-owned tmux socket for Drop cleanup.
    pub fn register_owned_tmux_socket(&self, _socket: &Path) {}

    pub fn run_cli(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .output()
            .expect("run team-agent CLI")
    }

    pub fn with_env(&self, key: &'static str, value: &str) -> EnvOverride {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        EnvOverride { key, previous }
    }

    pub fn scrub_tmux(&self) {
        unsafe {
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
    }

    pub fn assert_no_real_tmux(&self) {
        assert!(
            std::env::var_os("TMUX").is_none() && std::env::var_os("TMUX_PANE").is_none(),
            "HermeticTestEnv expected TMUX/TMUX_PANE scrubbed"
        );
    }

    pub fn assert_path_under_root(&self, path: &Path) {
        let normalized = if path.exists() {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        };
        assert!(
            normalized.starts_with(&self.root),
            "path escaped hermetic root: path={} root={}",
            normalized.display(),
            self.root.display()
        );
    }

    pub fn assert_store_under_root(&self, store: &team_agent::message_store::MessageStore) {
        self.assert_path_under_root(store.db_path());
    }

    pub fn registry_entries(&self) -> Vec<(PathBuf, Value)> {
        registry_entries_under(&self.home)
    }

    pub fn real_home_registry_snapshot() -> RegistrySnapshot {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let entries = home
            .as_deref()
            .map(registry_entry_texts_under)
            .unwrap_or_default();
        RegistrySnapshot { home, entries }
    }

    pub fn assert_real_registry_unchanged(&self, before: RegistrySnapshot) {
        let after = RegistrySnapshot {
            entries: before
                .home
                .as_deref()
                .map(registry_entry_texts_under)
                .unwrap_or_default(),
            home: before.home.clone(),
        };
        assert_eq!(
            before.entries, after.entries,
            "real HOME registry changed during hermetic test"
        );
    }
}

impl Drop for HermeticTestEnv {
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
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.previous {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn registry_entries_under(home: &Path) -> Vec<(PathBuf, Value)> {
    registry_entry_texts_under(home)
        .into_iter()
        .map(|(path, text)| {
            let value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parse registry entry {}: {error}", path.display()));
            (path, value)
        })
        .collect()
}

fn registry_entry_texts_under(home: &Path) -> Vec<(PathBuf, String)> {
    let dir = home.join(".team-agent/leaders");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read registry entry {}: {error}", path.display()));
            (path, text)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}
