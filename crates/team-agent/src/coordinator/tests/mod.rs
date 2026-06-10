#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

// ─────────────────────────────────────────────────────────────────────────
// helpers — concrete golden fixtures (Python v0.2.11 @ 439bef8)
// ─────────────────────────────────────────────────────────────────────────

/// `MessageStore.SCHEMA_VERSION == 3` (both Python & Rust db/schema.rs:13). metadata 三元之一。
const GOLDEN_SCHEMA_VERSION: i64 = 3;

fn ws() -> WorkspacePath {
    WorkspacePath::new("/tmp/team-agent-coord-test")
}

fn meta(pid: u32, proto: u32, schema: i64) -> CoordinatorMetadata {
    CoordinatorMetadata {
        pid: Pid(pid),
        protocol_version: proto,
        message_store_schema_version: schema,
        source: MetadataSource::Boot,
        updated_at: "2026-06-02T00:00:00+00:00".to_string(),
    }
}

/// `ProviderRegistry` mock — 断言**零** provider-client 调用 (MUST-NOT-13 / §84).
/// `adapter_for`/`error_lists` 各记一次调用计数;abnormal-track 只允许触碰 `error_lists`,
/// 绝不触碰 `adapter_for`(那会走真实 provider client crate)。
struct MockRegistry {
    whitelist: Vec<String>,
    blacklist: Vec<String>,
    adapter_calls: std::cell::Cell<u32>,
    error_list_calls: std::cell::Cell<u32>,
}

impl MockRegistry {
    fn new(whitelist: &[&str], blacklist: &[&str]) -> Self {
        Self {
            whitelist: whitelist.iter().map(|s| s.to_string()).collect(),
            blacklist: blacklist.iter().map(|s| s.to_string()).collect(),
            adapter_calls: std::cell::Cell::new(0),
            error_list_calls: std::cell::Cell::new(0),
        }
    }
}

impl ProviderRegistry for MockRegistry {
    fn adapter_for(&self, _provider: Provider) -> Box<dyn ProviderAdapter> {
        // 任何对它的调用都违反 §84 zero-injection;计数,断言保持 0。
        self.adapter_calls.set(self.adapter_calls.get() + 1);
        crate::provider::get_adapter(_provider)
    }
    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        self.error_list_calls.set(self.error_list_calls.get() + 1);
        ErrorLists {
            whitelist: self.whitelist.clone(),
            blacklist: self.blacklist.clone(),
        }
    }
}

/// in-memory `MarkerStore` —— durable marker 落盘探针。
struct MapMarkerStore {
    markers: std::collections::BTreeMap<String, Value>,
    fail: bool,
}
impl MapMarkerStore {
    fn ok() -> Self {
        Self { markers: Default::default(), fail: false }
    }
    fn failing() -> Self {
        Self { markers: Default::default(), fail: true }
    }
}
impl MarkerStore for MapMarkerStore {
    fn set_marker(&mut self, name: &str, value: Value) -> bool {
        if self.fail {
            return false;
        }
        self.markers.insert(name.to_string(), value);
        true
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Mock `Transport` — records every method call (name) + returns canned values.
// The §84/no-tmux/no-panic tick contracts inject THIS so a Coordinator can be
// constructed in tests. `has_session` answer drives the tmux-session-missing gate.
// Recorded `calls` let a future tick-order test assert which control-plane probes
// ran. capture/query return canned text so tick's readonly detectors stay
// provider-neutral (no real subprocess, no provider client). MUST-NOT-13: this
// mock NEVER reaches a provider client crate.
// ─────────────────────────────────────────────────────────────────────────
use crate::transport::{
    test_support::OfflineTransport, AttachOutcome, BackendKind, CaptureRange, CapturedText,
    InjectPayload, InjectReport, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, Target, Transport, TransportError, WindowName,
};

struct MockTransport {
    inner: OfflineTransport,
    calls: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
}

impl MockTransport {
    fn new(session_present: bool) -> Self {
        Self {
            inner: OfflineTransport::new().with_session_present(session_present),
            calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    fn record(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

impl Transport for MockTransport {
    fn kind(&self) -> BackendKind {
        self.inner.kind()
    }
    fn spawn_first(
        &self,
        s: &SessionName,
        w: &WindowName,
        argv: &[String],
        cwd: &std::path::Path,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.record("spawn_first");
        self.inner.spawn_first(s, w, argv, cwd, env)
    }
    fn spawn_into(
        &self,
        s: &SessionName,
        w: &WindowName,
        argv: &[String],
        cwd: &std::path::Path,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.record("spawn_into");
        self.inner.spawn_into(s, w, argv, cwd, env)
    }
    fn inject(
        &self,
        t: &Target,
        p: &InjectPayload,
        submit: Key,
        bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.record("inject");
        self.inner.inject(t, p, submit, bracketed)
    }
    fn send_keys(&self, t: &Target, keys: &[Key]) -> Result<(), TransportError> {
        self.record("send_keys");
        self.inner.send_keys(t, keys)
    }
    fn capture(
        &self,
        t: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        self.record("capture");
        self.inner.capture(t, range)
    }
    fn query(
        &self,
        t: &Target,
        f: PaneField,
    ) -> Result<Option<String>, TransportError> {
        self.record("query");
        self.inner.query(t, f)
    }
    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        self.record("liveness");
        self.inner.liveness(pane)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.record("list_targets");
        self.inner.list_targets()
    }
    fn has_session(&self, s: &SessionName) -> Result<bool, TransportError> {
        self.record("has_session");
        self.inner.has_session(s)
    }
    fn list_windows(
        &self,
        s: &SessionName,
    ) -> Result<Vec<WindowName>, TransportError> {
        self.record("list_windows");
        self.inner.list_windows(s)
    }
    fn set_session_env(
        &self,
        s: &SessionName,
        k: &str,
        v: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        self.record("set_session_env");
        self.inner.set_session_env(s, k, v)
    }
    fn kill_session(&self, s: &SessionName) -> Result<(), TransportError> {
        self.record("kill_session");
        self.inner.kill_session(s)
    }
    fn kill_window(&self, t: &Target) -> Result<(), TransportError> {
        self.record("kill_window");
        self.inner.kill_window(t)
    }
    fn attach_session(
        &self,
        s: &SessionName,
    ) -> Result<AttachOutcome, TransportError> {
        self.record("attach_session");
        self.inner.attach_session(s)
    }
}

/// Construct a `Coordinator` over a fresh temp workspace with injected mocks.
/// Returns `(coord, transport_calls_handle)` so tick tests can assert control-plane
/// call order and that `inject` (an exploratory prompt) never fired (§84).
/// `session_present` drives the tmux-session-missing gate; `save_hook` injects a
/// forced save failure (bug-084); `recorder` captures tick side-effect ORDER.
fn coord_for_test(
    session_present: bool,
    save_hook: Option<SaveHook>,
    recorder: Option<OrderRecorder>,
) -> (Coordinator, std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>) {
    let dir = std::env::temp_dir().join(format!(
        "team-agent-coord-tick-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let ws = WorkspacePath::new(dir);
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let transport = MockTransport::new(session_present);
    let calls = std::sync::Arc::clone(&transport.calls);
    let coord = Coordinator::for_test(
        ws,
        reg,
        Box::new(transport),
        save_hook,
        recorder,
    );
    (coord, calls)
}

/// Like [`coord_for_test`] but seeds a TRUTHY `session_name` into the workspace
/// state.json first, so the tmux-session gate actually runs. The session-missing
/// STOP path requires a truthy session_name (Python lifecycle.py:276
/// `if session_name and not _tmux_session_exists(...)`); a null/empty name skips
/// the gate entirely (see `p2_tick_skips_tmux_gate_when_session_name_absent`).
fn coord_for_test_with_session(
    session_present: bool,
    session_name: &str,
) -> (Coordinator, std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>) {
    let dir = std::env::temp_dir().join(format!(
        "team-agent-coord-tick-sess-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::state::persist::save_runtime_state(
        &dir,
        &serde_json::json!({ "session_name": session_name }),
    )
    .unwrap();
    let ws = WorkspacePath::new(dir);
    let reg: Box<dyn ProviderRegistry> = Box::new(MockRegistry::new(&[], &[]));
    let transport = MockTransport::new(session_present);
    let calls = std::sync::Arc::clone(&transport.calls);
    let coord = Coordinator::for_test(ws, reg, Box::new(transport), None, None);
    (coord, calls)
}

/// A save hook that always fails (bug-084 forced persistence failure).
fn failing_save_hook() -> SaveHook {
    Box::new(|_ws, _state| {
        Err(crate::state::StateError::SaveFailed(
            "injected tick-end save failure".to_string(),
        ))
    })
}

fn read_event_log_dir(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let path = crate::model::paths::logs_dir(dir).join("events.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
        Err(_) => Vec::new(),
    }
}


mod basics;
mod abnormal;
mod watch;
mod tick_core;
mod spine;
mod health_sync;
mod takeover;
mod daemon;
mod main_preserved;
mod a0_lostupdate;
