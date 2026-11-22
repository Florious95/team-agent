//! RED gate: no process-lifetime heap copy of the runtime state.
//!
//! Long-lived hosts (the stdio `mcp-server`, the managed-leader launcher) call
//! `load_runtime_state` and the MCP tool handlers. On 0.6.0,
//! `RUNTIME_STATE_CACHE` keeps a full deep clone of state.json for the life of
//! the process: about 4.5 MB live for a 1.1 MB file, 97% of the server's live
//! heap after one `get_team_status`. The cache is only read when the file is
//! missing, so this is retained memory that grows with state size.
//!
//! A counting global allocator measures live heap bytes before and after each
//! operation, once all results are dropped. Warm-up on a tiny workspace pays
//! one-time lazy initialisation first, so only state-proportional retention
//! is counted. This file must keep exactly one #[test] so the counters see no
//! concurrent test threads.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};
use team_agent::mcp_server::{handle_mcp, TeamOrchestratorTools};
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::load_runtime_state;

use hermetic_guard::HermeticTestEnv;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            LIVE.fetch_add(new_size, Ordering::Relaxed);
        }
        new
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const KIB: usize = 1024;
const STATE_BYTES: usize = 1024 * 1024;
/// Live heap allowed to remain after 3x `load_runtime_state` on a 1 MiB state.
const LOAD_RETAINED_MAX: usize = 64 * KIB;
/// Live heap allowed to remain after MCP tool calls on a 1 MiB state.
const TOOL_RETAINED_MAX: usize = 128 * KIB;

fn live() -> usize {
    LIVE.load(Ordering::SeqCst)
}

fn tools(ws: &std::path::Path) -> TeamOrchestratorTools {
    TeamOrchestratorTools::with_identity(
        ws,
        Some(AgentId::new(memory_fixture::SENDER)),
        Some(TeamKey::new(memory_fixture::ACTIVE_TEAM)),
    )
}

fn call(tools: &TeamOrchestratorTools, id: u64, name: &str, arguments: Value) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    });
    let response = handle_mcp(tools, &request).expect("handle_mcp");
    assert!(
        response.is_some(),
        "{name}: tools/call must produce a frame"
    );
}

fn exercise(tools: &TeamOrchestratorTools) {
    call(
        tools,
        1,
        "send_message",
        json!({"to": "leader", "content": "retention probe", "mailbox": true}),
    );
    call(tools, 2, "get_team_status", json!({}));
    call(
        tools,
        3,
        "send_message",
        json!({"to": "leader", "content": "retention probe 2", "mailbox": true}),
    );
}

fn kib(bytes: isize) -> String {
    format!("{} KiB", bytes / KIB as isize)
}

#[test]
#[serial_test::serial(env)]
fn long_lived_hosts_retain_no_heap_proportional_to_runtime_state() {
    let env = HermeticTestEnv::enter("state-heap-retention");
    let warm = env.workspace("warm");
    // Separate workspaces so each phase starts with no cache entry for its path.
    let big_load = env.workspace("big-load");
    let big_tools = env.workspace("big-tools");
    memory_fixture::seed_runtime_state(&warm, 0);
    let state_bytes = memory_fixture::seed_runtime_state(&big_load, STATE_BYTES);
    memory_fixture::seed_runtime_state(&big_tools, STATE_BYTES);

    // Warm-up: lazy statics, regex caches, first-touch paths, tiny cache entries.
    for _ in 0..2 {
        drop(load_runtime_state(&warm).expect("warm load"));
        exercise(&tools(&warm));
    }

    let before_load = live();
    for _ in 0..3 {
        drop(load_runtime_state(&big_load).expect("load fixture state"));
    }
    let load_retained = live() as isize - before_load as isize;

    let before_tools = live();
    {
        let handlers = tools(&big_tools);
        exercise(&handlers);
        exercise(&handlers);
    }
    let tool_retained = live() as isize - before_tools as isize;

    eprintln!(
        "[heap-retention] state={state_bytes}B load_retained={} tool_retained={}",
        kib(load_retained),
        kib(tool_retained)
    );
    let mut failures = Vec::new();
    if load_retained > LOAD_RETAINED_MAX as isize {
        failures.push(format!(
            "3x load_runtime_state retained {} (> {}) of live heap after every result was dropped",
            kib(load_retained),
            kib(LOAD_RETAINED_MAX as isize)
        ));
    }
    if tool_retained > TOOL_RETAINED_MAX as isize {
        failures.push(format!(
            "send_message/get_team_status retained {} (> {}) of live heap after the tools were dropped",
            kib(tool_retained),
            kib(TOOL_RETAINED_MAX as isize)
        ));
    }
    assert!(
        failures.is_empty(),
        "heap retained in proportion to a {state_bytes} byte state.json:\n  {}",
        failures.join("\n  ")
    );
}
