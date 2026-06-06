//! Bug 3 RED: explicit claim-leader/takeover are operator recovery commands.
//!
//! A live caller pane is enough. The pane must not be rejected because tmux reports
//! `pane_current_command=node` for an npm-launched Codex process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

const CALLER_PANE: &str = "%9";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
fn claim_leader_and_takeover_accept_live_node_caller_pane() {
    let ws = tmp_dir("any-live-pane");
    seed_runtime_state(&ws);
    let fake_tmux = fake_tmux_bin(&ws);
    let _env = EnvGuard::set([
        ("PATH", format!("{}:{}", fake_tmux.display(), std::env::var("PATH").unwrap_or_default())),
        ("TMUX_PANE", CALLER_PANE.to_string()),
        ("TEAM_AGENT_LEADER_PROVIDER", "codex".to_string()),
        ("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-a".to_string()),
    ]);

    let claim = run(
        &[
            "claim-leader",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &ws,
    );
    let mut failures = Vec::new();
    if let Some(failure) = cli_success_failure(
        &claim,
        "claim-leader must claim from any live caller pane, including pane_current_command=node",
    ) {
        failures.push(failure);
    }

    let takeover = run(
        &[
            "takeover",
            "--workspace",
            ws.to_str().unwrap(),
            "--team",
            "current",
            "--confirm",
            "--json",
        ],
        &ws,
    );
    if let Some(failure) = cli_success_failure(
        &takeover,
        "takeover must claim from any live caller pane, including pane_current_command=node",
    ) {
        failures.push(failure);
    }

    assert!(
        failures.is_empty(),
        "explicit claim/takeover live-pane contract failed:\n{}",
        failures.join("\n\n")
    );
}

fn cli_success_failure(output: &Output, label: &str) -> Option<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value = match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return Some(format!(
                "{label}: stdout must be JSON; code={:?} stdout={stdout:?} stderr={stderr:?}",
                output.status.code()
            ));
        }
    };
    if output.status.code() != Some(0) {
        return Some(format!(
            "{label}: expected exit 0; stdout={stdout:?} stderr={stderr:?}"
        ));
    }
    if value["ok"] != json!(true) {
        return Some(format!("{label}: expected ok=true, got {value}"));
    }
    if !matches!(
        value["status"].as_str(),
        Some("claimed") | Some("already_bound")
    ) {
        return Some(format!(
            "{label}: status must be claimed/already_bound, got {value}"
        ));
    }
    if value["reason"] == json!("caller_not_leader_shaped") {
        return Some(format!(
            "{label}: explicit recovery must not inspect leader-shaped command metadata; got {value}"
        ));
    }
    None
}

fn seed_runtime_state(ws: &Path) {
    team_agent::state::persist::save_runtime_state(
        ws,
        &json!({
            "active_team_key": "current",
            "session_name": "current",
            "team_dir": ws.to_string_lossy().to_string(),
            "agents": {},
            "teams": {
                "current": {
                    "session_name": "current",
                    "team_dir": ws.to_string_lossy().to_string(),
                    "agents": {}
                }
            }
        }),
    )
    .unwrap();
}

fn fake_tmux_bin(ws: &Path) -> PathBuf {
    let bin_dir = ws.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let tmux = bin_dir.join("tmux");
    let line = format!(
        "{pane}\tteam-current\t0\tleader\t0\t/dev/ttys001\tnode\t1\t{cwd}\t1\t0\n",
        pane = CALLER_PANE,
        cwd = ws.display(),
    );
    let script = format!(
        r#"#!/bin/sh
case " $* " in
  *" list-panes "*)
    printf '%s' '{line}'
    exit 0
    ;;
  *" display-message "*)
    printf 'node\n'
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#
    );
    std::fs::write(&tmux, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin_dir
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-bug3-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(values: [(&'static str, String); 4]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            unsafe {
                std::env::set_var(key, value);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}
