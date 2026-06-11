//! 测试默认零真-spawn、零真-tmux:lifecycle/CLI spawn 路径必经 *_with_transport 注入离线
//! mock;确需真 tmux 者必须 provider:fake + RAII kill_server 守卫 + #[ignore=real-machine].

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRecord {
    pub kind: String,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone)]
struct OfflineState {
    session_present: bool,
    session_absent_after_spawn_first: bool,
    targets: Vec<PaneInfo>,
    windows: Vec<WindowName>,
    pane_presence: BTreeMap<String, bool>,
    spawn_failures: BTreeMap<String, String>,
    spawned_panes_addressable: bool,
    liveness: BTreeMap<String, PaneLiveness>,
    default_liveness: PaneLiveness,
    calls: Vec<&'static str>,
    spawns: Vec<SpawnRecord>,
    inject_targets: Vec<Target>,
    inject_payloads: Vec<String>,
}

impl Default for OfflineState {
    fn default() -> Self {
        Self {
            session_present: false,
            session_absent_after_spawn_first: false,
            targets: Vec::new(),
            windows: Vec::new(),
            pane_presence: BTreeMap::new(),
            spawn_failures: BTreeMap::new(),
            spawned_panes_addressable: true,
            liveness: BTreeMap::new(),
            default_liveness: PaneLiveness::Unknown,
            calls: Vec::new(),
            spawns: Vec::new(),
            inject_targets: Vec::new(),
            inject_payloads: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OfflineTransport {
    inner: Arc<Mutex<OfflineState>>,
}

impl OfflineTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_session_present(self, present: bool) -> Self {
        self.with_state(|state| state.session_present = present);
        self
    }

    pub fn with_session_absent_after_spawn_first(self) -> Self {
        self.with_state(|state| state.session_absent_after_spawn_first = true);
        self
    }

    pub fn with_targets(self, targets: Vec<PaneInfo>) -> Self {
        self.with_state(|state| state.targets = targets);
        self
    }

    pub fn with_windows(self, windows: Vec<WindowName>) -> Self {
        self.with_state(|state| state.windows = windows);
        self
    }

    pub fn with_default_liveness(self, liveness: PaneLiveness) -> Self {
        self.with_state(|state| state.default_liveness = liveness);
        self
    }

    pub fn with_liveness(self, pane: impl Into<String>, liveness: PaneLiveness) -> Self {
        self.with_state(|state| {
            state.liveness.insert(pane.into(), liveness);
        });
        self
    }

    pub fn with_pane_presence(self, pane: impl Into<String>, present: bool) -> Self {
        self.with_state(|state| {
            state.pane_presence.insert(pane.into(), present);
        });
        self
    }

    pub fn with_spawn_failure(self, window: impl Into<String>, error: impl Into<String>) -> Self {
        self.with_state(|state| {
            state.spawn_failures.insert(window.into(), error.into());
        });
        self
    }

    pub fn with_spawned_panes_addressable(self, present: bool) -> Self {
        self.with_state(|state| state.spawned_panes_addressable = present);
        self
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.with_state(|state| state.calls.clone())
    }

    pub fn spawn_records(&self) -> Vec<(String, Vec<String>)> {
        self.with_state(|state| {
            state
                .spawns
                .iter()
                .map(|record| (record.kind.clone(), record.argv.clone()))
                .collect()
        })
    }

    pub fn inject_targets(&self) -> Vec<Target> {
        self.with_state(|state| state.inject_targets.clone())
    }

    pub fn inject_payloads(&self) -> Vec<String> {
        self.with_state(|state| state.inject_payloads.clone())
    }

    fn record(&self, call: &'static str) {
        self.with_state(|state| state.calls.push(call));
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut OfflineState) -> T) -> T {
        match self.inner.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                f(&mut guard)
            }
        }
    }

    fn spawn_result(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let pane_index = self.with_state(|state| {
            state.calls.push(kind);
            state.spawns.push(SpawnRecord { kind: kind.to_string(), argv: argv.to_vec() });
            if let Some(error) = state.spawn_failures.get(window.as_str()) {
                return Err(TransportError::Spawn {
                    backend: BackendKind::Tmux,
                    source: std::io::Error::other(error.clone()),
                });
            }
            if kind == "spawn_first" && !state.session_absent_after_spawn_first {
                state.session_present = true;
            }
            let pane_index = state.spawns.len().saturating_sub(1);
            state
                .pane_presence
                .insert(format!("%{pane_index}"), state.spawned_panes_addressable);
            Ok(pane_index)
        })?;
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{pane_index}")),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn inject_report() -> InjectReport {
        InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
        }
    }
}

impl Transport for OfflineTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_result("spawn_first", session, window, argv)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_result("spawn_into", session, window, argv)
    }

    fn inject(
        &self,
        target: &Target,
        payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.with_state(|state| {
            state.calls.push("inject");
            state.inject_targets.push(target.clone());
            state.inject_payloads.push(match payload {
                InjectPayload::Empty => String::new(),
                InjectPayload::Text(text) => text.clone(),
            });
        });
        Ok(Self::inject_report())
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        self.record("send_keys");
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        self.record("capture");
        Ok(CapturedText { text: String::new(), range })
    }

    fn query(
        &self,
        _target: &Target,
        _field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        self.record("query");
        Ok(None)
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(self.with_state(|state| {
            state.calls.push("liveness");
            state
                .liveness
                .get(pane.as_str())
                .copied()
                .unwrap_or(state.default_liveness)
        }))
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        Ok(self.with_state(|state| {
            state.calls.push("has_pane");
            state.pane_presence.get(pane.as_str()).copied()
        }))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.with_state(|state| {
            state.calls.push("list_targets");
            state.targets.clone()
        }))
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.with_state(|state| {
            state.calls.push("has_session");
            state.session_present
        }))
    }

    fn list_windows(
        &self,
        _session: &SessionName,
    ) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.with_state(|state| {
            state.calls.push("list_windows");
            if state.windows.is_empty() {
                state.targets.iter().filter_map(|pane| pane.window_name.clone()).collect()
            } else {
                state.windows.clone()
            }
        }))
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        self.record("set_session_env");
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        self.record("kill_session");
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        self.record("kill_window");
        Ok(())
    }

    fn attach_session(
        &self,
        _session: &SessionName,
    ) -> Result<AttachOutcome, TransportError> {
        self.record("attach_session");
        Ok(AttachOutcome::Attached)
    }
}
