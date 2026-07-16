#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::Value;

pub const CALLER_IDENTITY_ENVS: &[&str] = &[
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

/// 0.5.43 debt-sweep (debt-sweep-locate.md §4.2): short, process-unique
/// absolute tmux socket path for tests that exec real `tmux -S`. Root
/// cause of the Gate2 flake is macOS `AF_UNIX sun_path` >104 bytes when
/// tests inherit `/var/folders/...` from `temp_dir()`; this helper
/// keeps the returned path deterministically < 100 bytes so
/// `sun_path` (incl. NUL) stays safe with headroom.
///
/// Basename shape: `ta43-<short-tag>-<pid>-<counter>-<hash>.sock` under
/// `/private/tmp` (macOS/BSD-friendly short root). Never falls back to
/// the default tmux socket. Long tags are truncated + hashed to
/// preserve traceability while capping length. Panics if the resulting
/// path is still >=100 bytes (fail-closed vs. fail-open silent long
/// path).
pub fn short_tmux_socket(tag: &str) -> PathBuf {
    // Short root: macOS uses /private/tmp (12 chars); other Unix
    // (incl. Linux CI) uses /tmp (4 chars). Both leave ~90+ bytes
    // for basename — plenty for tag+pid+counter+hash. Root is
    // intentionally NOT the hermetic HOME workspace because sockets
    // must survive across `HermeticTestEnv` Drops when the test needs
    // to hand-off. Sockets are cleaned by the owning fixture's Drop
    // (see `TestWorkspace::cleanup_owned_tmux` / `TmuxServer::drop`).
    // Mirrors te's `short_test_base` in `support/debt_sweep_0543.rs`
    // (4b13e9f portable-fixtures patch): macOS→/private/tmp,
    // other Unix→/tmp, non-Unix→temp_dir.
    let root = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    // Truncate the tag to 24 bytes; append 4-hex-char hash of the
    // original tag so distinct long tags remain distinct.
    let short_tag: String = tag.chars().take(24).collect();
    let hash = short_hash4(tag);
    let pid = std::process::id();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let basename = format!("ta43-{short_tag}-{pid}-{counter}-{hash}.sock");
    let path = root.join(&basename);
    let byte_len = path.as_os_str().to_string_lossy().len();
    assert!(
        byte_len < 100,
        "short_tmux_socket produced {}-byte path (target <100, sun_path cap 104): {}",
        byte_len,
        path.display()
    );
    path
}

fn short_hash4(input: &str) -> String {
    // Cheap deterministic 4-hex-char hash (FNV-1a 32-bit truncated).
    // Not cryptographic; only for uniqueness spread across long tags.
    let mut hash: u32 = 0x811c9dc5;
    for byte in input.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    format!("{:04x}", hash & 0xffff)
}

pub struct HermeticTestEnv {
    root: PathBuf,
    home: PathBuf,
    previous: Vec<(&'static str, Option<String>)>,
    /// 0.5.43 debt-sweep (§6.1): test-owned resource ledger. Drop
    /// cleanup walks these EXACT entries (never a host-wide scan).
    /// Order matters: pids first (stop coordinator/child tree), then
    /// sockets (kill exact tmux server + delete socket file), then the
    /// hermetic root (workspace removal).
    owned: Mutex<OwnedResources>,
}

#[derive(Default)]
struct OwnedResources {
    pids: Vec<u32>,
    tmux_sockets: Vec<PathBuf>,
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
            owned: Mutex::new(OwnedResources::default()),
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

    /// 0.5.43 debt-sweep (§6.1): register an exact test-owned process
    /// pid so Drop stops just that pid (never a `pgrep target/debug`
    /// scan). Multiple pids allowed for tests with coordinator +
    /// helper processes.
    pub fn register_owned_pid(&self, pid: u32) {
        if let Ok(mut owned) = self.owned.lock() {
            owned.pids.push(pid);
        }
    }

    /// 0.5.43 debt-sweep (§6.1): register an exact test-owned tmux
    /// socket. Drop runs `tmux -S <socket> kill-server` on JUST that
    /// path, then removes the socket file. Never scans `/private/tmp/
    /// tmux-*` or `ta-*` for stragglers — foreign sockets remain
    /// untouched (verified by RED
    /// `hermetic_drop_cleans_exact_owned_resources_and_preserves_foreign_server`).
    pub fn register_owned_tmux_socket(&self, socket: &Path) {
        assert_fixture_owned_tmux_socket(&self.root, socket);
        if let Ok(mut owned) = self.owned.lock() {
            owned.tmux_sockets.push(socket.to_path_buf());
        }
    }

    pub fn run_cli(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run_cli_env(cwd, args, &[])
    }

    pub fn run_cli_env(&self, cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command.args(args).current_dir(cwd).env("HOME", &self.home);
        for key in CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.output().expect("run team-agent CLI")
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

fn assert_fixture_owned_tmux_socket(root: &Path, socket: &Path) {
    let ambient = std::env::var_os("TMUX").and_then(|value| {
        let socket = value.to_str()?.split(',').next()?;
        (!socket.is_empty()).then(|| PathBuf::from(socket))
    });
    assert_ne!(
        ambient.as_deref(),
        Some(socket),
        "refusing to register ambient TMUX endpoint as test-owned: {}",
        socket.display()
    );
    let fixture_named = socket
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("ta43-"));
    assert!(
        socket.is_absolute() && socket.exists() && (socket.starts_with(root) || fixture_named),
        "tmux endpoint must already exist and have fixture provenance: socket={} root={}",
        socket.display(),
        root.display()
    );
}

impl Drop for HermeticTestEnv {
    fn drop(&mut self) {
        // 0.5.43 debt-sweep (§6.1) Drop order — exact-owned only, no
        // host scan: (1) stop exact registered pids, (2) kill exact
        // registered tmux servers + delete socket files, (3) restore
        // env, (4) remove hermetic root. `TEAM_AGENT_KEEP_TEST_PROCESSES`
        // + `TEAM_AGENT_KEEP_TEST_TMP` are the loud debug escapes;
        // both print a stderr breadcrumb so skipped cleanup is
        // observable (the ledger explicitly forbids silent skip).
        let keep_procs = std::env::var("TEAM_AGENT_KEEP_TEST_PROCESSES").as_deref() == Ok("1");
        let keep_tmp = std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() == Ok("1");
        let mut owned = self
            .owned
            .lock()
            .ok()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        if keep_procs {
            eprintln!(
                "TEAM_AGENT_KEEP_TEST_PROCESSES=1 — skipping exact-owned pid/tmux cleanup for {} pids, {} sockets",
                owned.pids.len(),
                owned.tmux_sockets.len()
            );
        } else {
            for pid in owned.pids.drain(..) {
                let _ = Command::new("kill").arg(pid.to_string()).output();
            }
            for socket in owned.tmux_sockets.drain(..) {
                if let Some(socket_str) = socket.to_str() {
                    let _ = Command::new("tmux")
                        .args(["-S", socket_str, "kill-server"])
                        .output();
                }
                let _ = std::fs::remove_file(&socket);
            }
        }
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
        if keep_tmp {
            eprintln!(
                "TEAM_AGENT_KEEP_TEST_TMP=1 — preserving hermetic root: {}",
                self.root.display()
            );
        } else {
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
