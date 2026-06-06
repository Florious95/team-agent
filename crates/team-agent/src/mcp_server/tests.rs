//! step 14a · mcp_server::tests — WAVE-2 RED contracts (Python v0.2.11 golden).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

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
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("ta-rs-mcp-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

include!("tests/normalize.rs");
include!("tests/wire.rs");
include!("tests/send.rs");
include!("tests/tools.rs");
include!("tests/golden.rs");
include!("tests/scoped.rs");
