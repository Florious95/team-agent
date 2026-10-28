//! Isolated physical receiver for decoupled send/inbox contract tests.
#![allow(dead_code)]

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct DirectFixture {
    pub root: PathBuf,
    pub socket: String,
    pub pane: String,
}

impl DirectFixture {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ta-oracle-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
        let socket = root.join("tmux.sock").to_string_lossy().into_owned();
        let script = root.join("receiver.py");
        std::fs::write(
            &script,
            format!(
                r#"import os, tty
from pathlib import Path
tty.setraw(0)
os.write(1, b'\x1b[?2004h')
root = Path({root:?})
with (root / 'received').open('ab', buffering=0) as out:
    (root / 'ready').touch()
    while True:
        out.write(os.read(0, 65536))
"#,
                root = root.to_string_lossy()
            ),
        )
        .unwrap();
        let output = Command::new("tmux")
            .args([
                "-S",
                &socket,
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                "oracle",
            ])
            .arg(format!("python3 '{}'", script.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fixture = Self {
            root,
            socket,
            pane: String::from_utf8(output.stdout).unwrap().trim().into(),
        };
        fixture.wait_for(|| fixture.root.join("ready").exists());
        fixture.set_state(&json!({
            "active_team_key": "current",
            "teams": {"current": {
                "team_id": "current", "status": "stopped", "tmux_socket": fixture.socket,
                "leader": {"pane_id": fixture.pane},
                "agents": {"worker-1": {"status": "dead", "pane_id": fixture.pane}}
            }}
        }));
        fixture
    }

    pub fn set_state(&self, state: &Value) {
        std::fs::write(
            self.root.join(".team/runtime/state.json"),
            serde_json::to_vec(state).unwrap(),
        )
        .unwrap();
    }

    pub fn wait_for(&self, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "physical receiver timed out: {:?}",
                std::fs::read(self.root.join("received"))
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn received(&self, text: &str) {
        let expected = format!("\x1b[200~{text}\x1b[201~\r").into_bytes();
        self.wait_for(|| {
            std::fs::read(self.root.join("received"))
                .unwrap_or_default()
                .windows(expected.len())
                .any(|bytes| bytes == expected)
        });
    }

    pub fn inbox(&self, recipient: &str) -> Vec<Value> {
        std::fs::read_to_string(self.root.join(format!(".team/inbox/{recipient}.jsonl")))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for DirectFixture {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", &self.socket, "kill-server"])
            .output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
