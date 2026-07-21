//! agent-clone-fork · successor RED batch 1 (verifier) — R10 discoverability +
//! R1 latest-role.
//!
//! From locate.md §7 R1/R10 + §3 surface enumeration + §5 observation points 2/4,
//! NOT the §4 root-cause design. Baseline `9e6be51` (v0.5.52): `clone-agent`
//! does not exist and `fork-agent` copies the role from the COMPILED SPEC
//! (`fork_agent.rs` `append_forked_agent(&spec, ...)`), so a later edit to the
//! source role file is not picked up.
//!
//! - **R10 discoverability**: `clone-agent` must be a real CLI verb (help exits
//!   0, not an argparse `invalid choice`), a sibling of `fork-agent`. Baseline:
//!   `clone-agent --help` fails with an invalid-choice error.
//! - **R1 latest-role**: editing the SOURCE role file body (adding a
//!   `ROLE_REV=<marker>`) before fork must make the NEW agent's materialized
//!   role carry the new marker; a stale compiled-spec projection must not win.
//!   Baseline: fork clones from the compiled spec, so the new marker is absent.
//!
//! Offline structural RED (PATH-shim provider, zero tokens): the "does the
//! forked agent REMEMBER the source nonce" semantics is a subscription-E2E
//! concern (locate §4.5) — this batch pins the STRUCTURE (verb exists; latest
//! role file is the source of truth), not provider context semantics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const TEAM_NAME: &str = "cf-batch1";
const SOURCE: &str = "src_worker";
const NEW: &str = "new_worker";

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    socket: Option<PathBuf>,
}

impl Case {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace("ws");
        write_team_docs(&workspace, "SOURCE_ROLE_BODY_ORIGINAL");
        let shim_dir = write_claude_shim(&workspace);
        let shim_path = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").expect("PATH present")
        );
        Self {
            env,
            workspace,
            shim_path,
            socket: None,
        }
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

    /// Seed the SOURCE session tuple so the fork reaches its success path (a
    /// hermetic PATH-shim has no real captured backing). Required after the R1
    /// fixture revision: the success-path shim needs a valid source to copy from,
    /// or the fork refuses with "source session backing missing" before it ever
    /// materializes a role. The RED (compiled-spec vs latest-role) is unchanged.
    fn seed_source_session_tuple(&self) {
        let rollout = self.workspace.join("fixture-source-rollout.jsonl");
        std::fs::write(&rollout, "{\"type\":\"fixture-source\"}\n").expect("write source rollout");
        let Ok(raw) = std::fs::read_to_string(self.state_path()) else {
            return;
        };
        let Ok(mut state) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let tuple = serde_json::json!({
            "session_id": "sess-cf-batch1-source",
            "rollout_path": rollout.to_string_lossy(),
            "captured_at": "2026-07-21T00:00:00Z",
            "captured_via": "contract-fixture"
        });
        let mut patch = |row: &mut serde_json::Value| {
            if let Some(obj) = row.as_object_mut() {
                for (k, v) in tuple.as_object().unwrap() {
                    obj.insert(k.clone(), v.clone());
                }
            }
        };
        if let Some(row) = state.get_mut("agents").and_then(|a| a.get_mut(SOURCE)) {
            patch(row);
        }
        if let Some(teams) = state
            .get_mut("teams")
            .and_then(serde_json::Value::as_object_mut)
        {
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
        // Record the team's own tmux socket (from attach_commands) for precise
        // teardown — quick-start spawns a real tmux server under the shim.
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
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
}

impl Drop for Case {
    fn drop(&mut self) {
        // Kill exactly this team's tmux server (precise socket ownership), then
        // TERM any process whose command line carries this workspace path.
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

fn write_team_docs(workspace: &Path, source_body: &str) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!("---\nname: {TEAM_NAME}\nobjective: clone-fork batch1.\nprovider: claude\n---\n"),
    )
    .expect("write TEAM.md");
    write_source_role(workspace, source_body);
}

fn write_source_role(workspace: &Path, body: &str) {
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{body}\n"
        ),
    )
    .expect("write source role doc");
}

/// REVISION (implement interlock, 2026-07-21): the original R1 shim only spawned
/// (`echo; exec sleep 3600`) and never produced a session transcript. During
/// batch1/2 implement, runtime-owner surfaced a genuine contract conflict: the
/// successor implementation, per R3/MUST-17, must obtain a `ContextForkProof`
/// (NEW session_id + readable changed backing) or return `context_fork_unverified`
/// and ROLL BACK the NEW managed role/spec/state/window. A shim that never emits a
/// transcript therefore forces a rollback, so R1's "NEW materialized role exists
/// and carries the marker" assertion would go red for the WRONG reason (the role
/// was correctly rolled back), and keeping the role green would leave a half-seat
/// (violating R3/R5 atomicity). Per leader ruling (L1 r2 precedent) the fix is a
/// single-point fixture revision: the shim now writes a legitimate session
/// backing (success path) so the fork completes without rollback and the NEW
/// managed role is retained — while R1's RED cause is UNCHANGED: baseline does not
/// consult backing and clones the role from the COMPILED SPEC, so the marker
/// edited into the source role file after quick-start is still absent.
fn write_claude_shim(workspace: &Path) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
    // Success-path shim: emit a session transcript into the hermetic HOME so the
    // implementation can capture a verified NEW backing and NOT roll back the
    // fork (matching batch2's emit_transcript=true success shim).
    std::fs::write(
        &shim,
        "#!/bin/sh\nmkdir -p \"$HOME/.claude/projects/shim\"\necho '{\"type\":\"shim\"}' > \"$HOME/.claude/projects/shim/session.jsonl\"\necho 'claude shim ready'\nexec sleep 3600\n",
    )
    .expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude shim");
    }
    bin_dir
}

/// R10 — `clone-agent` is a discoverable CLI verb. Baseline red: argparse has no
/// `clone-agent` choice, so `--help` exits non-zero with an invalid-choice error.
#[test]
fn r10_clone_agent_is_a_discoverable_cli_verb() {
    let case = Case::start("cf-r10");
    let out = case.run(&["clone-agent", "--help"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "`clone-agent --help` must exit 0 (the verb exists); got exit={:?} stderr={stderr} stdout={stdout}",
        out.status.code()
    );
    assert!(
        !stderr.contains("invalid choice") && !stdout.contains("invalid choice"),
        "`clone-agent` must not be an argparse invalid choice; stderr={stderr}"
    );
    // It must present the same SOURCE --as NEW shape as fork-agent.
    let help = format!("{stdout}{stderr}");
    assert!(
        help.contains("clone-agent") && help.contains("--as"),
        "clone-agent help must document `clone-agent SOURCE --as NEW`; help={help}"
    );
}

/// R1 — fork materializes the LATEST source role file, not a stale compiled
/// spec. We edit the source role body to carry a fresh marker AFTER quick-start
/// (which compiled the original), then fork; the NEW agent's materialized role
/// must contain the new marker. Baseline red: fork clones from the compiled
/// spec, so the marker added after compilation is absent.
#[test]
fn r1_fork_materializes_latest_source_role_file() {
    let mut case = Case::start("cf-r1");
    case.quick_start();
    // Seed the source backing so the revised success-path shim lets the fork
    // complete (without a valid source tuple the fork refuses before it ever
    // materializes a role). The RED cause is unchanged by this.
    case.seed_source_session_tuple();

    // Edit the SOURCE role file AFTER quick-start compiled the original body.
    let marker = "ROLE_REV_9F3K_LATEST";
    write_source_role(&case.workspace, &format!("SOURCE_ROLE_BODY_WITH {marker}"));

    let fork = case.run(&[
        "fork-agent",
        SOURCE,
        "--as",
        NEW,
        "--workspace",
        case.ws(),
        "--team",
        TEAM_NAME,
        "--no-display",
        "--json",
    ]);
    // The command need not be green at baseline; what R1 pins is that IF a NEW
    // role is materialized, it reflects the LATEST source role file. With the
    // revised success-path shim the fork completes (exit 0) yet clones the role
    // from the stale COMPILED SPEC, so the marker is absent — the RED cause is the
    // compiled-spec projection, not a rollback. Read the materialized role for NEW
    // and assert the fresh marker is present.
    let _ = fork;

    let new_role_marker_present = materialized_role_contains(&case.workspace, NEW, marker);
    assert!(
        new_role_marker_present,
        "fork must materialize NEW from the LATEST source role file (marker {marker} present); \
         a stale compiled-spec projection must not win. NEW materialized role did not carry the \
         marker edited into the source role file after quick-start."
    );
}

/// Look for the NEW agent's materialized role text carrying `marker`, across the
/// managed dynamic-role file and the compiled spec system prompt — whichever the
/// product uses as the role source of truth. Absence in ALL of them is the RED.
fn materialized_role_contains(workspace: &Path, agent: &str, marker: &str) -> bool {
    // Managed dynamic role file (locate §4.2 target: <ws>/.team/dynamic-role-files/<NEW>.md).
    let dyn_role = workspace
        .join(".team")
        .join("dynamic-role-files")
        .join(format!("{agent}.md"));
    if std::fs::read_to_string(&dyn_role)
        .map(|s| s.contains(marker))
        .unwrap_or(false)
    {
        return true;
    }
    // Fallback: any compiled spec / state file under .team/runtime that carries
    // the NEW agent's role body with the marker.
    let runtime = workspace.join(".team").join("runtime");
    let mut found = false;
    if let Ok(entries) = walk(&runtime) {
        for path in entries {
            if std::fs::read_to_string(&path)
                .map(|s| s.contains(agent) && s.contains(marker))
                .unwrap_or(false)
            {
                found = true;
                break;
            }
        }
    }
    found
}

fn walk(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    Ok(out)
}
