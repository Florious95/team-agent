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
                "agents": {"worker-1": {"status": "running", "pane_id": fixture.pane}}
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

    /// Durable inbox records of `recipient` in this fixture's team, read through the store's public
    /// reader: the C3 segment layout (`.team/inbox/<team>/<recipient>/NNNNNNNN.jsonl`) is the
    /// implementation's choice, not part of these oracles.
    pub fn inbox(&self, recipient: &str) -> Vec<Value> {
        team_agent::inbox_store::read(&self.root, recipient, 10_000, Some("current"))
            .unwrap()["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Every inbox file's bytes, keyed by path. Append-only means each earlier value is a prefix
    /// of the later one.
    pub fn inbox_bytes(&self) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn walk(dir: &std::path::Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() { walk(&path, out); } else if path.extension().is_some_and(|e| e == "jsonl") {
                    out.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(&self.root.join(".team/inbox"), &mut out);
        out
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
