//! MCP server unit-test fixtures and focused stdio/wire contract tests.
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

/// A UNIQUE throwaway workspace dir per test. Filesystem and database tests
/// must not share `/tmp/ws` under parallel cargo.
fn unique_ws(tag: &str) -> PathBuf {
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

include!("tests/normalize.rs");
include!("tests/wire.rs");
include!("tests/send.rs");
include!("tests/tools.rs");
include!("tests/golden.rs");
include!("tests/scoped.rs");
