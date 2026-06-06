#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// 隔离的空 workspace。仿 sibling(event_log.rs)风格,不依赖 tempfile dev-dep。
fn temp_ws() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ws = std::env::temp_dir().join(format!("ta_rs_lc_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

fn aid(s: &str) -> AgentId {
    AgentId::new(s)
}
fn sess(s: &str) -> SessionName {
    SessionName(s.to_string())
}

mod core;
mod launch_spawn;
mod agent_ops;
mod lane_ops;
mod main_preserved;
