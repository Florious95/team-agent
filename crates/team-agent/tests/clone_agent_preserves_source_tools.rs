//! clone-agent tools preservation — regression RED→GREEN (P1).
//!
//! Contract: `team-agent clone-agent SOURCE --as NEW` must preserve the source
//! seat's FULL `tools` set. Baseline defect (0.5.66 = d1289f81, host-proven):
//! a source whose role declares `fs_read fs_list fs_write execute_bash mcp_team
//! provider_builtin` clones out with `fs_list fs_read mcp_team` — silently, no
//! warning/event/help flag (the seat is mid-task before it notices it cannot
//! write or exec).
//!
//! Root cause: `clamp_materialized_role_to_leader`
//! (crates/team-agent/src/lifecycle/launch/role_source.rs) filters the role's
//! declared tools down to the leader's hardcoded 3-tool ceiling
//! `[fs_read, fs_list, mcp_team]` (compiler.rs `compile_team`). The sibling
//! `add-agent` path does NOT clamp — it compiles the role with all declared
//! tools — so the clamp is not a coherent "no worker above the leader"
//! invariant; it is a silent tool drop confined to the clone/fork path, and no
//! test or doc pins it.
//!
//! Baseline RED: the NEW materialized role (and the compiled runtime spec row)
//! carry the clamped `fs_list fs_read mcp_team` instead of the source's tools.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use team_agent::model::yaml::Value;

const TEAM_NAME: &str = "cp-tools";
const SOURCE: &str = "src_worker";
const NEW: &str = "new_worker";

/// Mirrors the defect-report source seat: the full 6-tool worker set that the
/// baseline clone silently strips to `fs_list fs_read mcp_team`.
const SOURCE_TOOLS: &[&str] = &[
    "fs_read",
    "fs_list",
    "fs_write",
    "execute_bash",
    "mcp_team",
    "provider_builtin",
];

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
        write_team_docs(&workspace);
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

    /// Seed the SOURCE session tuple so the clone reaches its success path (a
    /// hermetic PATH-shim has no real captured backing). Same fixture as the
    /// R1 latest-role test.
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
            "session_id": "sess-cp-tools-source",
            "rollout_path": rollout.to_string_lossy(),
            "captured_at": "2026-08-15T00:00:00Z",
            "captured_via": "contract-fixture"
        });
        let patch = |row: &mut serde_json::Value| {
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

fn write_team_docs(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!("---\nname: {TEAM_NAME}\nobjective: clone-agent tools preservation.\nprovider: claude\n---\n"),
    )
    .expect("write TEAM.md");
    let tools_yaml = SOURCE_TOOLS
        .iter()
        .map(|tool| format!("  - {tool}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n{tools_yaml}\n---\n\n{SOURCE} body.\n"
        ),
    )
    .expect("write source role doc");
}

/// Success-path shim: emit a session transcript into the hermetic HOME so the
/// clone's spawn does not hang and the NEW seat registers (same mechanism as
/// the R1 latest-role test).
fn write_claude_shim(workspace: &Path) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
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

/// Parse the `tools` front-matter list of a role doc as a set.
fn role_tools(path: &Path) -> BTreeSet<String> {
    let (meta, _) = team_agent::compiler::read_front_matter(path)
        .unwrap_or_else(|e| panic!("read front matter {}: {e}", path.display()));
    let items = meta
        .get("tools")
        .and_then(Value::as_list)
        .unwrap_or_else(|| panic!("role {} must declare a tools list", path.display()));
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// Find the NEW agent's compiled `tools` in the runtime spec (the seat truth
/// the spawned worker actually runs with).
fn runtime_spec_tools_for(workspace: &Path, agent: &str) -> Option<BTreeSet<String>> {
    let runtime = workspace.join(".team").join("runtime");
    let spec = std::fs::read_dir(&runtime)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("team.spec.yaml"))
        .find(|path| path.is_file())?;
    let text = std::fs::read_to_string(&spec).ok()?;
    let parsed = team_agent::model::yaml::loads(&text).ok()?;
    let agents = parsed.get("agents")?.as_list()?;
    for row in agents {
        if row.get("id").and_then(Value::as_str) != Some(agent) {
            continue;
        }
        let items = row.get("tools")?.as_list()?;
        return Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        );
    }
    None
}

/// Contract: a clone of a worker whose role declares the full tool set must
/// carry the SAME tools — the spawned seat must be able to write and exec, not
/// a stripped read-only shell.
#[test]
fn clone_agent_preserves_source_tools() {
    let mut case = Case::start("cp-tools");
    case.quick_start();
    case.seed_source_session_tuple();

    let source_role = case.workspace.join("agents").join(format!("{SOURCE}.md"));
    let source_tools = role_tools(&source_role);
    assert_eq!(source_tools.len(), SOURCE_TOOLS.len(), "fixture source tools");

    let out = case.run(&[
        "clone-agent",
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
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "clone-agent must exit 0; exit={:?} stderr={stderr}",
        out.status.code()
    );

    // The NEW materialized role is the direct product of clone (materialize →
    // clamp → compile). Baseline clamps it to the leader's 3-tool ceiling.
    let new_role = case
        .workspace
        .join(".team")
        .join("dynamic-role-files")
        .join(format!("{NEW}.md"));
    let new_tools = role_tools(&new_role);
    assert_eq!(
        new_tools, source_tools,
        "clone must preserve the source seat's FULL tools set; \
         source={source_tools:?} new={new_tools:?} \
         (baseline silently clamps to the leader ceiling fs_read/fs_list/mcp_team)"
    );

    // The compiled runtime spec is what the spawned seat actually runs with —
    // it must carry the same preserved tools (seat truth, not just the role file).
    let new_spec_tools = runtime_spec_tools_for(&case.workspace, NEW)
        .unwrap_or_else(|| panic!("NEW agent must be present in the compiled runtime spec"));
    assert_eq!(
        new_spec_tools, source_tools,
        "runtime spec NEW tools must match the source seat's tools; \
         source={source_tools:?} spec={new_spec_tools:?}"
    );
}
