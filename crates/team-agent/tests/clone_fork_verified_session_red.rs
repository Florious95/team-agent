//! agent-clone-fork · successor RED batch 2 (verifier) — R2 verified truth +
//! R3 false-success teeth.
//!
//! From locate.md §7 R2/R3 + §1.2 (Codex N=2: one seat spawned but 90s no final
//! output — spawn is not success) + §3 success/readback surface, NOT the §4
//! design. Baseline `9e6be51` (v0.5.52): fork reports `ok:true` and
//! `session_id:null` (locate §3 report_type), and the success gate only checks
//! registration (pane/window), never the NEW provider backing (locate §3
//! success/readback fork_agent.rs:312-463 violates MUST-17/N36).
//!
//! - **R2 verified truth**: a fork that returns ok MUST carry a non-null NEW
//!   `session_id` that differs from the source, and its backing must be readable.
//!   Baseline red: fork returns ok with session_id null.
//! - **R3 false-success teeth**: a provider shim that only SPAWNS (sleeps) and
//!   never produces a NEW transcript/backing must NOT be reported as a forked
//!   context — the new implementation must refuse/rollback
//!   (`context_fork_unverified`). Baseline red: fork reports ok anyway (spawn ==
//!   success), which is exactly the 2026-07-17 false-success family (locate §2).
//!
//! Offline structural RED (PATH-shim provider, zero tokens): the source session
//! backing is seeded as fixture prep (a hermetic shim has no real captured
//! session); this batch pins that ok requires a VERIFIED NEW backing, not that
//! the fork "remembers" the nonce (that is subscription-E2E, locate §4.5).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const TEAM_NAME: &str = "cf-batch2";
const SOURCE: &str = "src_worker";
const NEW: &str = "new_worker";

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    socket: Option<PathBuf>,
}

impl Case {
    /// `emit_transcript=false` is the R3 teeth shim: the provider only spawns
    /// (sleeps) and never writes a session transcript/backing.
    fn start(tag: &str, emit_transcript: bool) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace("ws");
        write_team_docs(&workspace);
        let shim_dir = write_claude_shim(&workspace, emit_transcript);
        let shim_path = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").expect("PATH present")
        );
        let mut case = Self {
            env,
            workspace,
            shim_path,
            socket: None,
        };
        case.quick_start();
        case.seed_source_session_tuple();
        case
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().expect("ws utf8")
    }

    fn state_path(&self) -> PathBuf {
        self.workspace
            .join(".team")
            .join("runtime")
            .join("state.json")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env
            .run_cli_env(&self.workspace, args, &[("PATH", self.shim_path.as_str())])
    }

    fn quick_start(&mut self) {
        let out = self.run(&[
            "quick-start",
            self.ws(),
            "--workspace",
            self.ws(),
            "--name",
            TEAM_NAME,
            "--yes",
            "--json",
        ]);
        if let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) {
            if let Some(cmd) = v
                .get("attach_commands")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.as_str())
                .or_else(|| v.get("leader_attach_command").and_then(|c| c.as_str()))
            {
                let toks: Vec<&str> = cmd.split_whitespace().collect();
                if let Some(i) = toks.iter().position(|t| *t == "-S") {
                    if let Some(sock) = toks.get(i + 1) {
                        self.socket = Some(PathBuf::from(*sock));
                    }
                }
            }
        }
    }

    /// Seed the SOURCE backing tuple (a hermetic shim has no real captured
    /// session) so the fork has a valid source to copy from. The RED is about
    /// the NEW backing, not the source's.
    fn seed_source_session_tuple(&self) {
        let rollout = self.workspace.join("fixture-source-rollout.jsonl");
        std::fs::write(&rollout, "{\"type\":\"fixture-source\"}\n").expect("write source rollout");
        let Ok(raw) = std::fs::read_to_string(self.state_path()) else {
            return;
        };
        let Ok(mut state) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        let tuple = json!({
            "session_id": "sess-cf-batch2-source",
            "rollout_path": rollout.to_string_lossy(),
            "captured_at": "2026-07-21T00:00:00Z",
            "captured_via": "contract-fixture"
        });
        let mut patch = |row: &mut Value| {
            if let Some(obj) = row.as_object_mut() {
                for (k, v) in tuple.as_object().unwrap() {
                    obj.insert(k.clone(), v.clone());
                }
            }
        };
        if let Some(row) = state.get_mut("agents").and_then(|a| a.get_mut(SOURCE)) {
            patch(row);
        }
        if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
            for team in teams.values_mut() {
                if let Some(row) = team.get_mut("agents").and_then(|a| a.get_mut(SOURCE)) {
                    patch(row);
                }
            }
        }
        let _ = std::fs::write(
            self.state_path(),
            serde_json::to_string_pretty(&state).unwrap(),
        );
    }

    fn fork(&self) -> Value {
        let out = self.run(&[
            "fork-agent",
            SOURCE,
            "--as",
            NEW,
            "--workspace",
            self.ws(),
            "--team",
            TEAM_NAME,
            "--no-display",
            "--json",
        ]);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "fork-agent --json must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if let Some(sock) = &self.socket {
            let _ = std::process::Command::new("tmux")
                .args(["-S", sock.to_str().unwrap_or(""), "kill-server"])
                .output();
        }
        let ws = self.workspace.to_string_lossy().to_string();
        if let Ok(out) = std::process::Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains(&ws) {
                    if let Some(pid) = line.split_whitespace().next() {
                        let _ = std::process::Command::new("kill")
                            .args(["-TERM", pid])
                            .output();
                    }
                }
            }
        }
        let _ = &self.env;
    }
}

fn write_team_docs(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!("---\nname: {TEAM_NAME}\nobjective: clone-fork batch2.\nprovider: claude\n---\n"),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{SOURCE}.\n"
        ),
    )
    .expect("write source role doc");
}

fn write_claude_shim(workspace: &Path, emit_transcript: bool) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
    // R3 teeth: with emit_transcript=false the shim ONLY spawns (sleeps) and
    // never writes any session backing — a spawn-without-transcript that the old
    // implementation would wrongly accept as a forked context.
    let body = if emit_transcript {
        "#!/bin/sh\nmkdir -p \"$HOME/.claude/projects/shim\"\necho '{\"type\":\"shim\"}' > \"$HOME/.claude/projects/shim/session.jsonl\"\necho 'claude shim ready'\nexec sleep 3600\n"
    } else {
        "#!/bin/sh\necho 'claude shim ready'\nexec sleep 3600\n"
    };
    std::fs::write(&shim, body).expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude shim");
    }
    bin_dir
}

/// R2 — a fork that returns ok MUST carry a non-null NEW session id distinct
/// from the source. Baseline red: fork returns ok with a null session id
/// (locate §3 report_type: `ForkAgentReport.session_id: Option`, None at
/// baseline), so "ok" does not prove a forked context.
#[test]
fn r2_fork_ok_carries_a_verified_new_session_id() {
    let case = Case::start("cf-r2", true);
    let fork = case.fork();

    // The case phenomenon: baseline reports ok. R2 pins that ok is not enough —
    // it must carry a verified NEW session id.
    if fork.get("ok").and_then(Value::as_bool) != Some(true) {
        // If the product refuses (a legitimate non-ok), R2 does not apply; but
        // baseline reports ok, so we assert the verified-session requirement.
        return;
    }
    let new_session = fork
        .get("new_session_id")
        .or_else(|| fork.get("session_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    assert!(
        new_session.is_some(),
        "a fork reporting ok MUST carry a non-null NEW session id (verified context proof), not \
         session_id=null; a spawned window is not a forked context (locate §2/§3). fork={fork}"
    );
    assert_ne!(
        new_session,
        Some("sess-cf-batch2-source"),
        "the NEW session id must DIFFER from the source session id; fork={fork}"
    );
}

/// R3 false-success teeth — a provider shim that only spawns (no transcript /
/// backing) must NOT be reported as a forked context. Baseline red: fork reports
/// ok even though no NEW backing was produced (spawn == success, the 2026-07-17
/// false-success family). The new implementation must refuse/rollback with a
/// discriminable `context_fork_unverified`.
#[test]
fn r3_spawn_without_transcript_is_not_a_forked_context() {
    let case = Case::start("cf-r3", false);
    let fork = case.fork();

    let ok = fork.get("ok").and_then(Value::as_bool) == Some(true);
    let has_verified_new_backing = fork
        .get("new_session_id")
        .or_else(|| fork.get("session_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .is_some();

    // The teeth: a spawn that produced NO transcript/backing must NOT be a green
    // fork. The old implementation accepts spawn-as-success — it returns ok:true
    // with no verified NEW backing. The new implementation must refuse/rollback
    // with a discriminable `context_fork_unverified` (ok:false), OR at minimum
    // must not report ok while carrying no verified NEW backing.
    assert!(
        !(ok && !has_verified_new_backing),
        "a provider that only spawned (no transcript/backing) must NOT be reported ok as a forked \
         context: the old implementation accepts spawn-as-success (ok:true, no NEW backing). \
         Expected refuse/rollback (context_fork_unverified). got ok={ok} \
         verified_new_backing={has_verified_new_backing}. fork={fork}"
    );
}
