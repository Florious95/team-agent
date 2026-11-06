#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::collections::BTreeMap;
use std::path::Path;

// —— 每个 daemon 可达方法 `unimplemented!()` 的 stub:驱动 trait 即 RED ——
struct StubBackend(BackendKind);

impl Transport for StubBackend {
    fn kind(&self) -> BackendKind {
        self.0
    }
    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        let _ = (argv, cwd, env);
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}-{}", session.as_str(), window.as_str())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(1000),
        })
    }
    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        let _ = (argv, cwd, env);
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}-{}", session.as_str(), window.as_str())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(1001),
        })
    }
    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(
        &self,
        target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let _ = target;
        Ok(CapturedText {
            text: normalize_capture("line one  \nbusy\u{a0}marker   \n  \n"),
            range,
        })
    }
    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        if self.0 != BackendKind::Tmux && field == PaneField::PaneMode {
            return Ok(None);
        }
        let value = match field {
            PaneField::PaneMode => "0",
            PaneField::PaneCurrentCommand => "sh",
        };
        Ok(Some(value.to_string()))
    }
    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        if self.0 == BackendKind::ConPty && pane.as_str() == "foreign-leader" {
            return Ok(PaneLiveness::Unknown);
        }
        Ok(PaneLiveness::Live)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(vec![
            PaneInfo {
                pane_id: PaneId::new("%team-sess-win-1"),
                session: SessionName::new("team-sess"),
                window_index: Some(0),
                window_name: Some(WindowName::new("win-1")),
                pane_index: Some(0),
                tty: Some("/dev/ttys001".to_string()),
                current_command: Some("sh".to_string()),
                current_path: Some(PathBuf::from("/tmp/ws")),
                active: true,
                pane_pid: Some(1000),
                leader_env: BTreeMap::new(),
            },
            PaneInfo {
                pane_id: PaneId::new("%team-sess-worker-2"),
                session: SessionName::new("team-sess"),
                window_index: Some(1),
                window_name: Some(WindowName::new("worker-2")),
                pane_index: Some(0),
                tty: Some("/dev/ttys002".to_string()),
                current_command: Some("sh".to_string()),
                current_path: Some(PathBuf::from("/tmp/ws")),
                active: true,
                pane_pid: Some(1001),
                leader_env: BTreeMap::new(),
            },
        ])
    }
    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("win-1"), WindowName::new("worker-2")])
    }
    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }
}

fn tmux() -> StubBackend {
    StubBackend(BackendKind::Tmux)
}
fn wezterm() -> StubBackend {
    StubBackend(BackendKind::WezTerm)
}
fn conpty() -> StubBackend {
    StubBackend(BackendKind::ConPty)
}

mod behavior;
mod wire;
