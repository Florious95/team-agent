#![allow(dead_code)] // Used by T2-tier cases that land in the next batch.
//! Scripted provider shim for T2 tier E2E cases.
//!
//! The Team Agent runtime invokes external `codex` / `claude` / `copilot`
//! binaries to spawn provider sessions. T2 tests want to exercise CLI paths
//! that branch on provider behavior (resume decision, session backing,
//! activity classifier) WITHOUT touching real subscription auth or the
//! network.
//!
//! This shim writes a temp `bin/` under the workspace and prepends it to
//! `PATH`, so `team-agent`'s shellout to `codex resume ...` lands on a
//! deterministic shell script instead of the real binary.
//!
//! Use via `ScriptedProvider::with_codex(ws, behavior)`; pass the resulting
//! `extra_env()` vec into `run_ta_env` (sibling helper in framework) when
//! invoking team-agent so the override is per-command, not global
//! `std::env::set_var` (keeps tests parallel-safe).

use std::path::{Path, PathBuf};

use crate::framework::TestWorkspace;

#[derive(Clone, Copy, Debug)]
pub enum ScriptedBehavior {
    /// Sleep until killed; print a heartbeat line; exit 0 on SIGTERM.
    SleepUntilKilled,
    /// Echo argv and exit 0 immediately.
    EchoExit0,
    /// Echo argv and exit 1 immediately.
    EchoExit1,
}

pub struct ScriptedProvider {
    bin_dir: PathBuf,
}

impl ScriptedProvider {
    /// Install a temp `codex` script in `<ws>/.bin/`. Returns the shim
    /// handle. Drop the workspace to clean it up.
    pub fn with_codex(ws: &TestWorkspace, behavior: ScriptedBehavior) -> Self {
        Self::install(ws, "codex", behavior)
    }

    pub fn with_claude(ws: &TestWorkspace, behavior: ScriptedBehavior) -> Self {
        Self::install(ws, "claude", behavior)
    }

    fn install(ws: &TestWorkspace, name: &str, behavior: ScriptedBehavior) -> Self {
        let bin_dir = ws.path().join(".bin");
        std::fs::create_dir_all(&bin_dir).expect("create scripted provider bin dir");
        let script_path = bin_dir.join(name);
        let body = script_body(behavior);
        std::fs::write(&script_path, body).expect("write scripted provider script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)
                .expect("stat script")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).expect("chmod script");
        }
        Self { bin_dir }
    }

    /// Env overrides to splice into the CLI command. The caller is
    /// responsible for combining with the existing `PATH` (do this before
    /// passing into `Command::env`).
    pub fn path_prepend(&self) -> &Path {
        &self.bin_dir
    }

    /// Convenience: build a `PATH=<bin_dir>:<existing>` string for
    /// `Command::env("PATH", ...)`.
    pub fn path_env(&self) -> String {
        let existing = std::env::var("PATH").unwrap_or_default();
        format!("{}:{existing}", self.bin_dir.display())
    }
}

/// Pre-create a fake Codex rollout/backing file at `path` so the runtime's
/// resume gate sees an existing file. The body is a minimal jsonl with a
/// session-meta line.
pub fn write_codex_backing(path: &Path, session_id: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create rollout parent");
    }
    let line = format!(
        "{{\"type\":\"session_meta\",\"session_id\":\"{session_id}\",\"created\":\"2026-01-01T00:00:00Z\"}}\n"
    );
    std::fs::write(path, line).expect("write codex backing file");
}

fn script_body(behavior: ScriptedBehavior) -> String {
    let core = match behavior {
        ScriptedBehavior::SleepUntilKilled => {
            "trap 'exit 0' TERM INT\necho \"scripted provider $0 args=$@ pid=$$\"\nwhile true; do sleep 5; done\n"
        }
        ScriptedBehavior::EchoExit0 => {
            "echo \"scripted provider $0 args=$@\"\nexit 0\n"
        }
        ScriptedBehavior::EchoExit1 => {
            "echo \"scripted provider $0 args=$@\" >&2\nexit 1\n"
        }
    };
    format!("#!/bin/sh\n{core}")
}
