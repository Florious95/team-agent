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
    pub session: SessionName,
    pub window: WindowName,
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
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
    pane_titles: Vec<(String, String, String, String)>,
    inject_targets: Vec<Target>,
    inject_payloads: Vec<String>,
    tmux_endpoint: Option<String>,
    /// U1-B contract: when set, `list_targets()` returns `TransportError::MuxUnavailable`
    /// — used to model tmux server jitter (subprocess-fork-failed level). The whole
    /// resolve must DEFER, not silently coerce to an empty vec.
    list_targets_error: Option<String>,
    /// U1-C / general: pre-staged `capture()` payload, keyed by target stringification.
    /// `Target::Pane(p)` keys as `p.as_str()`; `Target::SessionWindow{session,window}`
    /// keys as `format!("{session}:{window}")`. A miss returns empty text (current
    /// default behaviour).
    capture_text: BTreeMap<String, String>,
    /// U1-C Tail-peek contract: every `capture()` call records its `CaptureRange`,
    /// in order, so a test can prove the delivery peek site narrowed Full → Tail(80).
    capture_ranges: Vec<CaptureRange>,
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
            pane_titles: Vec::new(),
            inject_targets: Vec::new(),
            inject_payloads: Vec::new(),
            tmux_endpoint: None,
            list_targets_error: None,
            capture_text: BTreeMap::new(),
            capture_ranges: Vec::new(),
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

    pub fn with_tmux_endpoint(self, endpoint: impl Into<String>) -> Self {
        self.with_state(|state| state.tmux_endpoint = Some(endpoint.into()));
        self
    }

    /// U1-B: stage a `list_targets()` error to model tmux server jitter. While set,
    /// every call returns `Err(TransportError::MuxUnavailable{detail})`. Pass an
    /// empty string to clear via re-builder if needed.
    pub fn with_list_targets_error(self, detail: impl Into<String>) -> Self {
        self.with_state(|state| state.list_targets_error = Some(detail.into()));
        self
    }

    /// Pre-stage `capture()` output for a `Target::Pane(pane_id)` key.
    pub fn with_capture_for_pane(
        self,
        pane_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        self.with_state(|state| {
            state.capture_text.insert(pane_id.into(), text.into());
        });
        self
    }

    /// Pre-stage `capture()` output for a `Target::SessionWindow{session,window}` key.
    pub fn with_capture_for_session_window(
        self,
        session: impl Into<String>,
        window: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let key = format!("{}:{}", session.into(), window.into());
        self.with_state(|state| {
            state.capture_text.insert(key, text.into());
        });
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

    pub fn spawn_window_records(&self) -> Vec<(String, String)> {
        self.with_state(|state| {
            state
                .spawns
                .iter()
                .map(|record| (record.kind.clone(), record.window.as_str().to_string()))
                .collect()
        })
    }

    pub fn spawn_cwd_records(&self) -> Vec<std::path::PathBuf> {
        self.with_state(|state| state.spawns.iter().map(|record| record.cwd.clone()).collect())
    }

    pub fn pane_title_records(&self) -> Vec<(String, String, String, String)> {
        self.with_state(|state| state.pane_titles.clone())
    }

    pub fn inject_targets(&self) -> Vec<Target> {
        self.with_state(|state| state.inject_targets.clone())
    }

    pub fn inject_payloads(&self) -> Vec<String> {
        self.with_state(|state| state.inject_payloads.clone())
    }

    /// All `capture()` ranges observed, in call order. Used by U1-C Tail-peek
    /// contracts to prove the delivery peek site requested Tail rather than Full.
    pub fn capture_ranges(&self) -> Vec<CaptureRange> {
        self.with_state(|state| state.capture_ranges.clone())
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
        cwd: &Path,
    ) -> Result<SpawnResult, TransportError> {
        let pane_index = self.with_state(|state| {
            state.calls.push(kind);
            state.spawns.push(SpawnRecord {
                kind: kind.to_string(),
                session: session.clone(),
                window: window.clone(),
                argv: argv.to_vec(),
                cwd: cwd.to_path_buf(),
            });
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
            submit_diagnostics: None,
        }
    }
}

impl Transport for OfflineTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn tmux_endpoint(&self) -> Option<String> {
        self.with_state(|state| state.tmux_endpoint.clone())
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_result("spawn_first", session, window, argv, cwd)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_result("spawn_into", session, window, argv, cwd)
    }

    fn spawn_split_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        _env: &BTreeMap<String, String>,
        _env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_result("spawn_split", session, window, argv, cwd)
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
                InjectPayload::Text(text) | InjectPayload::TextSkipConsumptionPoll(text) => {
                    text.clone()
                }
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
        target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let key = match target {
            Target::Pane(p) => p.as_str().to_string(),
            Target::SessionWindow { session, window } => {
                format!("{}:{}", session.as_str(), window.as_str())
            }
        };
        let text = self.with_state(|state| {
            state.calls.push("capture");
            state.capture_ranges.push(range);
            state.capture_text.get(&key).cloned().unwrap_or_default()
        });
        Ok(CapturedText { text, range })
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
        // U1-B: when `with_list_targets_error` is set, return a real Err so the
        // caller sees server jitter rather than a coerced empty vec.
        if let Some(detail) = self.with_state(|state| {
            state.calls.push("list_targets");
            state.list_targets_error.clone()
        }) {
            return Err(TransportError::MuxUnavailable {
                backend: BackendKind::Tmux,
                detail,
            });
        }
        Ok(self.with_state(|state| state.targets.clone()))
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

    fn configure_adaptive_pane_title(
        &self,
        session: &SessionName,
        window: &WindowName,
        pane: &PaneId,
        title: &str,
    ) -> Result<(), TransportError> {
        self.with_state(|state| {
            state.calls.push("configure_adaptive_pane_title");
            state.pane_titles.push((
                session.as_str().to_string(),
                window.as_str().to_string(),
                pane.as_str().to_string(),
                title.to_string(),
            ));
        });
        Ok(())
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

    fn kill_pane(&self, _pane: &PaneId) -> Result<(), TransportError> {
        self.record("kill_pane");
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
