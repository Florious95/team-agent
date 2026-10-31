#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;

#[test]
#[serial(env)]
fn launch_writes_worker_owner_team_id_from_runtime_spec_key_not_stale_active_team() {
    let case = RealLaunchCase::new("u0b-owner-canonical");
    let _path = install_copilot_stub(&case.root);
    seed_stale_active_multiteam_state(&case.root);
    let spec_path = write_runtime_spec(&case.root, "bravo", "worker_a");

    team_agent::lifecycle::launch_with_transport_in_workspace(
        &case.root,
        &spec_path,
        false,
        true,
        true,
        &case.backend,
    )
    .expect("real launch should spawn the bravo worker");

    let state = load_runtime_state(&case.root).expect("runtime state after launch");
    let mcp_config = state
        .pointer("/teams/bravo/agents/worker_a/mcp_config")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("bravo worker must be persisted under teams.bravo; state={state}")
        });
    let owner = worker_mcp_owner_team_id(Path::new(mcp_config));
    assert_eq!(
        owner.as_deref(),
        Some("bravo"),
        "worker MCP env must use the launched runtime team key, not stale active_team_key=alpha; state={state}"
    );
}


struct RealLaunchCase {
    root: PathBuf,
    backend: TmuxBackend,
}

impl RealLaunchCase {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let backend = TmuxBackend::for_workspace(&root);
        Self { root, backend }
    }
}

impl Drop for RealLaunchCase {
    fn drop(&mut self) {
        let _ = self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ta-rs-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

fn seed_stale_active_multiteam_state(root: &Path) {
    let state = json!({
        "active_team_key": "alpha",
        "session_name": "team-alpha",
        "agents": {},
        "tasks": [{"id": "task_initial", "assignee": "worker_a", "status": "pending", "summary": "Initial task"}],
        "teams": {
            "alpha": {
                "status": "alive",
                "session_name": "team-alpha",
                "spec_name": "shared-template",
                "legacy_aliases": ["shared-template"],
                "agents": {},
                "tasks": [{"id": "task_initial", "assignee": "worker_a", "status": "pending", "summary": "Initial task"}]
            },
            "bravo": {
                "status": "alive",
                "session_name": "team-bravo",
                "spec_name": "shared-template",
                "legacy_aliases": ["shared-template"],
                "agents": {},
                "tasks": [{"id": "task_initial", "assignee": "worker_a", "status": "pending", "summary": "Initial task"}]
            }
        }
    });
    save_runtime_state(root, &state).unwrap();
    // M4: no SQLite message store fixture (MessageStore::open is retired).
}


fn write_runtime_spec(root: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let spec_path = root
        .join(".team")
        .join("runtime")
        .join(team_key)
        .join("team.spec.yaml");
    std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
    let spec_text = format!(
        r#"version: 1
team:
  name: "shared-template"
  mode: "supervisor_worker"
  objective: "U0B owner-team canonical contract"
  workspace: "{workspace}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  tools: ["mcp_team"]
agents:
  - id: "{agent_id}"
    role: "Worker"
    provider: "copilot"
    auth_mode: "subscription"
    working_directory: "{workspace}"
    system_prompt:
      inline: "Worker."
      file: null
    tools: ["mcp_team"]
    permission_mode: "restricted"
routing:
  default_assignee: "{agent_id}"
  rules:
    - id: "route-{agent_id}"
      match:
        assignee: ["{agent_id}"]
      assign_to: "{agent_id}"
      priority: 10
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-{team_key}"
  auto_launch: true
  startup_order: ["{agent_id}"]
  dangerously_skip_permissions: false
tasks:
  - id: "task_initial"
    title: "Initial task"
    type: "implementation"
    assignee: "{agent_id}"
    status: "pending"
    acceptance: ["Worker reports valid result_envelope_v1"]
"#,
        workspace = root.display(),
    );
    std::fs::write(&spec_path, &spec_text).unwrap();
    std::fs::write(root.join("team.spec.yaml"), &spec_text).unwrap();
    spec_path
}

/// A real-CLI-shaped legacy worker binary (the spawn-time `copilot mcp list`
/// probe reads EOF and exits cleanly).
fn install_copilot_stub(root: &Path) -> EnvGuard {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let copilot = bin.join("copilot");
    std::fs::write(
        &copilot,
        "#!/bin/sh\nstty -echo 2>/dev/null || true\nexec cat\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&copilot).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&copilot, perms).unwrap();
    let old = std::env::var("PATH").ok();
    let mut next = bin.to_string_lossy().to_string();
    if let Some(old_path) = old.as_deref().filter(|path| !path.is_empty()) {
        next.push(':');
        next.push_str(old_path);
    }
    std::env::set_var("PATH", next);
    EnvGuard { key: "PATH", old }
}

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.old {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn worker_mcp_owner_team_id(path: &Path) -> Option<String> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    value
        .pointer("/mcpServers/team_orchestrator/env/TEAM_AGENT_OWNER_TEAM_ID")
        .and_then(Value::as_str)
        .map(str::to_string)
}







