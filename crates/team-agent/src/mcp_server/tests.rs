//! ---
//! purpose: MCP no-compaction contract test with fixture-owned workspace and path provenance
//! contract:
//!   provides:
//!     - name: mcp_state_fixture
//!       what: supplies explicit process-owned workspace and state-file paths without mutating process-global environment
//!     - name: mcp_state_path_provenance
//!       what: records raw/canonical workspace, resolved state path, and filesystem ownership facts
//!   depends:
//!     - crate::mcp_server::lifecycle_tools::state_status
//!     - crate::state::selector
//! boundary:
//!   - the owned no-compaction test writes only within its fixture root
//!   - this suite does not alter persistence semantics
//! maturity: wired
//! ---
//! step 14a · mcp_server::tests — WAVE-2 RED contracts (Python v0.2.11 golden).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;
use std::path::{Path, PathBuf};

// ── helpers ──────────────────────────────────────────────────────────────

/// Serialize a serde_json::Value to a string — used to assert byte-stable
/// key ORDER (preserve_order is enabled workspace-wide).
fn s(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}

/// Ordered list of keys as they appear in a JSON object Value.
fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

/// A UNIQUE throwaway workspace dir per test (mirrors the state/coordinator idiom):
/// tests that open the db (MessageStore) or write the filesystem MUST NOT share
/// `/tmp/ws`, or they flake under parallel cargo (sqlite "database is locked" / NotFound).
/// Pure-function / dispatch-shape tests that never touch fs/db keep a dummy fixed path.
fn unique_ws(tag: &str) -> std::path::PathBuf {
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    loop {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("ta-rs-mcp-{tag}-{}-{n}", std::process::id()));
        match std::fs::create_dir(&p) {
            Ok(()) => return p,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create unique workspace {}: {error}", p.display()),
        }
    }
}

static MCP_FIXTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One process-owned root for update_state tests. All paths are passed explicitly
/// to the code under test; the fixture never changes process-global environment.
struct McpStateFixture {
    root: PathBuf,
    workspace: PathBuf,
}

impl McpStateFixture {
    fn new(_tag: &str) -> Self {
        let seq = MCP_FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = if cfg!(target_os = "macos") {
            PathBuf::from("/private/tmp")
        } else {
            PathBuf::from("/tmp")
        };
        let raw_root = base.join(format!("ta-mcp-{}-{seq}", std::process::id()));
        std::fs::create_dir(&raw_root).unwrap();
        let root = std::fs::canonicalize(raw_root).unwrap();
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        let fixture = Self { root, workspace };
        fixture.seed_spec("team_state.md");
        crate::state::persist::save_runtime_state(
            &fixture.workspace,
            &json!({
                "session_name": "mcp-fixture",
                "active_team_key": "current",
                "agents": {},
                "tasks": []
            }),
        )
        .unwrap();
        fixture
    }

    fn seed_spec(&self, state_file: &str) {
        std::fs::write(
            self.workspace.join("team.spec.yaml"),
            format!(
                "team:\n  name: mcp-fixture\n  objective: path contract\nagents: []\ntasks: []\ncontext:\n  state_file: {state_file}\n"
            ),
        )
        .unwrap();
    }

    fn state_file(&self, relative: &str) -> PathBuf {
        self.workspace.join(relative)
    }

    fn record_provenance(&self, raw_workspace: &Path, state_file: &Path) {
        let canonical_workspace =
            crate::model::paths::canonical_run_workspace(raw_workspace).unwrap();
        assert!(self.under_root(raw_workspace));
        assert!(self.under_root(&canonical_workspace));
        assert!(self.under_root(state_file));
        let team_dir = self.workspace.join(".team");
        let runtime_dir = team_dir.join("runtime");
        let paths = [
            ("root", self.root.as_path()),
            ("raw_workspace", raw_workspace),
            ("canonical_workspace", canonical_workspace.as_path()),
            ("team_dir", team_dir.as_path()),
            ("runtime_dir", runtime_dir.as_path()),
            ("state_parent", state_file.parent().unwrap()),
        ];
        let mut facts = paths
            .into_iter()
            .map(|(name, path)| {
                let metadata = std::fs::symlink_metadata(path).unwrap();
                json!({
                    "name": name,
                    "path": path.display().to_string(),
                    "uid": metadata_uid(&metadata),
                    "mode": metadata_mode(&metadata),
                    "device": metadata_device(&metadata)
                })
            })
            .collect::<Vec<_>>();
        if let Ok(metadata) = std::fs::symlink_metadata(state_file) {
            facts.push(json!({
                "name": "state_file",
                "path": state_file.display().to_string(),
                "uid": metadata_uid(&metadata),
                "mode": metadata_mode(&metadata),
                "device": metadata_device(&metadata)
            }));
        }
        let provenance = json!({
            "raw_workspace": raw_workspace.display().to_string(),
            "canonical_workspace": canonical_workspace.display().to_string(),
            "resolved_state_file": state_file.display().to_string(),
            "facts": facts
        });
        std::fs::write(
            self.root.join("mcp-state-provenance.json"),
            serde_json::to_vec_pretty(&provenance).unwrap(),
        )
        .unwrap();
    }

    fn under_root(&self, path: &Path) -> bool {
        let candidate = if path.exists() {
            std::fs::canonicalize(path).unwrap()
        } else {
            path.to_path_buf()
        };
        candidate.starts_with(&self.root)
    }
}

impl Drop for McpStateFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn metadata_uid(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.uid()
}

#[cfg(not(unix))]
fn metadata_uid(_: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn metadata_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(unix))]
fn metadata_mode(_: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn metadata_device(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(not(unix))]
fn metadata_device(_: &std::fs::Metadata) -> u64 {
    0
}

include!("tests/normalize.rs");
include!("tests/wire.rs");
include!("tests/send.rs");
include!("tests/tools.rs");
include!("tests/golden.rs");
include!("tests/scoped.rs");
