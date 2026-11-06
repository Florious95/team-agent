#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::model::enums::Provider;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Runner(Arc<Mutex<Vec<Vec<String>>>>);
impl CommandRunner for Runner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.0.lock().unwrap().push(argv.to_vec());
        let stdout = if argv.iter().any(|arg| arg == "display-message") {
            format!(
                "/tmp/owned-test.sock|%1|{}|session|worker|owned-nonce|0|120\n",
                std::process::id()
            )
        } else if argv.iter().any(|arg| arg == "capture-pane") {
            "old\n────────────────────\n\n────────────────────────\ngpt-6-luna · max · 0%/272k · 0% · $0.00\n".into()
        } else {
            panic!("read-only observation must not issue an input command: {argv:?}")
        };
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }
}

struct Fixture {
    path: std::path::PathBuf,
    backend: TmuxBackend,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ta-native-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            TmuxBackend::with_runner_for_workspace(Box::new(Runner(calls.clone())), &path);
        Self { path, backend, calls }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn native_input_runtime_endpoint_factory_preserves_workspace_and_native_capability() {
    let target = Target::Pane(PaneId::new("%1"));
    assert!(crate::transport::test_support::OfflineTransport::new()
        .observe_native_input(&target, Provider::Pi)
        .unwrap()
        .is_none());
    for field in ["tmux_endpoint", "tmux_socket"] {
        let f = Fixture::new();
        let mut state = serde_json::json!({});
        state[field] = "/tmp/owned-test.sock".into();
        let selection =
            crate::tmux_backend::tmux_backend_with_runner_for_runtime_state_or_workspace(
                Box::new(Runner(f.calls.clone())),
                &f.path,
                Some(&state),
            );
        assert_eq!(selection.backend.event_workspace.as_deref(), Some(f.path.as_path()));
        assert_eq!(selection.tmux_endpoint_used.as_deref(), Some("/tmp/owned-test.sock"));
        // Native observation survives; the injection/acceptance engine does not.
        let frame = selection.backend.observe_native_input(&target, Provider::Pi)
            .unwrap().expect("current Pi input observation");
        assert!(frame.composer.is_empty());
        assert!(!frame.busy);
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 3, "binding, capture, binding revalidation");
        assert!(calls.iter().all(|argv| argv.windows(2)
            .any(|pair| pair == ["-S", "/tmp/owned-test.sock"])));
    }
}

#[test]
fn native_input_required_lock_contention_or_missing_workspace_is_zero_effect() {
    let f = Fixture::new();
    let _lock = acquire_required(PaneInputLockRequest {
        workspace: Some(&f.path),
        target_key: "%1",
        operation: "fixture",
    }).unwrap();
    // Retain the current send_keys lock guarantee, not retired inject-only checks.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            assert!(f.backend
                .send_keys(&Target::Pane(PaneId::new("%1")), &[Key::Enter])
                .is_err())
        });
    });
    assert!(f.calls.lock().unwrap().is_empty());
}
