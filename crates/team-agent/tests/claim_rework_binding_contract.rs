//! Claim rework A6 contract tests. Isolated HOME/workspace.
//! Cargo: `--test claim_rework_binding_contract`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use team_agent::cli::{cmd_diagnose, CmdOutput, DiagnoseArgs};
use team_agent::lifecycle::launch::{
    classify_leader_binding, launched_team_receiver_is_attached,
    seed_launched_owner_from_caller_with_provider_lookup, LeaderBindingClass,
};
use team_agent::lifecycle::{restart_leader_public_bind, CoordinatorStartSummary, RestartReport};
use team_agent::state::owner_gate::CallerIdentity;
use team_agent::transport::PaneId;

static ISOLATION: AtomicU64 = AtomicU64::new(0);

struct IsolatedHome {
    home: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    previous_tmux: Option<std::ffi::OsString>,
    previous_pane: Option<std::ffi::OsString>,
}

impl IsolatedHome {
    fn enter() -> Self {
        let n = ISOLATION.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "claim-rework-a5-home-{}-{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::create_dir_all(&home);
        let previous_home = std::env::var_os("HOME");
        let previous_tmux = std::env::var_os("TMUX");
        let previous_pane = std::env::var_os("TMUX_PANE");
        std::env::set_var("HOME", &home);
        Self {
            home,
            previous_home,
            previous_tmux,
            previous_pane,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        match &self.previous_tmux {
            Some(value) => std::env::set_var("TMUX", value),
            None => std::env::remove_var("TMUX"),
        }
        match &self.previous_pane {
            Some(value) => std::env::set_var("TMUX_PANE", value),
            None => std::env::remove_var("TMUX_PANE"),
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn unique_workspace() -> PathBuf {
    let n = ISOLATION.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "claim-rework-a5-ws-{}-{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::create_dir_all(&path);
    path
}

fn unique_socket() -> PathBuf {
    let n = ISOLATION.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "/tmp/ta5-{}-{}.sock",
        std::process::id(),
        n
    ))
}

fn diagnose_json(workspace: &Path, team: &str) -> Value {
    let result = cmd_diagnose(&DiagnoseArgs {
        workspace: workspace.to_path_buf(),
        json: true,
        team: Some(team.to_string()),
    })
    .expect("cmd_diagnose");
    match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("diagnose must be JSON, got {other:?}"),
    }
}

fn issue_ids(diagnose: &Value) -> Vec<String> {
    diagnose
        .get("issues")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| {
            item.as_str()
                .map(str::to_string)
                .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_string))
        })
        .collect()
}

#[test]
#[serial_test::serial(env)]
fn isolated_empty_team_is_unbound() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({"teams": {"alpha": {}}}),
    )
    .expect("save empty team");
    assert!(!launched_team_receiver_is_attached(&workspace, "alpha"));
    assert_eq!(
        classify_leader_binding(&workspace, "alpha"),
        LeaderBindingClass::Unbound
    );
}

#[test]
#[serial_test::serial(env)]
fn isolated_missing_registry_is_index_missing() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "team_key": "alpha",
            "teams": {
                "alpha": {
                    "team_owner": {"pane_id": "%1", "owner_epoch": 1},
                    "leader_receiver": {
                        "status": "attached",
                        "pane_id": "%1",
                        "owner_epoch": 1,
                        "tmux_socket": "/tmp/ta5-missing.sock"
                    }
                }
            }
        }),
    )
    .expect("save attached without registry");
    assert_eq!(
        classify_leader_binding(&workspace, "alpha"),
        LeaderBindingClass::IndexMissing
    );
    let diagnose = diagnose_json(&workspace, "alpha");
    let ids = issue_ids(&diagnose);
    assert!(
        ids.iter().any(|id| id == "leader_registry_index_missing"),
        "diagnose issues={ids:?}"
    );
    assert_ne!(diagnose.get("ok").and_then(Value::as_bool), Some(true));
}

#[test]
#[serial_test::serial(env)]
fn isolated_bad_registry_is_unknown() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "team_key": "alpha",
            "teams": {
                "alpha": {
                    "team_owner": {"pane_id": "%1", "owner_epoch": 1}
                }
            }
        }),
    )
    .expect("save owner");
    let dir = team_agent::leader::registry::registry_dir().expect("registry dir");
    std::fs::create_dir_all(&dir).expect("create registry");
    let path = dir.join(format!(
        "{}__alpha.json",
        team_agent::leader::registry::workspace_hash(&workspace)
    ));
    std::fs::write(&path, "{not json").expect("bad registry");
    assert_eq!(
        classify_leader_binding(&workspace, "alpha"),
        LeaderBindingClass::Unknown
    );
    let diagnose = diagnose_json(&workspace, "alpha");
    let ids = issue_ids(&diagnose);
    assert!(
        ids.iter().any(|id| id == "leader_binding_unknown"),
        "diagnose issues={ids:?}"
    );
}

struct OwnedTmux {
    socket: PathBuf,
}

impl Drop for OwnedTmux {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", &self.socket.display().to_string(), "kill-server"])
            .output();
        for _ in 0..20 {
            if !self.socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn start_owned_tmux(workspace: &Path) -> (OwnedTmux, String) {
    let socket = unique_socket();
    let _ = std::fs::remove_file(&socket);
    let output = Command::new("tmux")
        .args([
            "-S",
            &socket.display().to_string(),
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-n",
            "leader",
            "-c",
            &workspace.display().to_string(),
            "/bin/cat",
        ])
        .output()
        .expect("start tmux");
    assert!(
        output.status.success(),
        "tmux start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let owned = OwnedTmux { socket: socket.clone() };
    let list = Command::new("tmux")
        .args([
            "-S",
            &socket.display().to_string(),
            "list-panes",
            "-t",
            "alpha:leader",
            "-F",
            "#{pane_id}",
        ])
        .output()
        .expect("list pane");
    assert!(list.status.success(), "pane lookup failed");
    let pane = String::from_utf8_lossy(&list.stdout).trim().to_string();
    assert!(!pane.is_empty(), "tmux pane id required");
    (owned, pane)
}

fn dummy_coordinator() -> CoordinatorStartSummary {
    CoordinatorStartSummary {
        ok: true,
        status: "started".to_string(),
        pid: None,
        binary_path: None,
        binary_version: None,
        rotation_reason: None,
        binary_identity_relation: "same".to_string(),
    }
}

#[test]
#[serial_test::serial(env)]
fn isolated_production_seed_attach_register_restart_ok() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    let (tmux, pane) = start_owned_tmux(&workspace);
    let endpoint = tmux.socket.display().to_string();
    std::env::set_var("TMUX", format!("{endpoint},1,0"));
    std::env::set_var("TMUX_PANE", &pane);

    let mut state = json!({
        "team_key": "alpha",
        "session_name": "team-alpha",
        "workspace": workspace.display().to_string(),
        "teams": { "alpha": {} }
    });
    assert!(seed_launched_owner_from_caller_with_provider_lookup(
        &mut state,
        CallerIdentity {
            pane_id: pane.clone(),
            provider: "fake".to_string(),
            machine_fingerprint: "fp".to_string(),
            leader_session_uuid: "uuid-a5".to_string(),
            leader_session_uuid_source: "env".to_string(),
        },
        |_| None
    ));
    let pending = state
        .pointer("/teams/alpha/leader_receiver")
        .cloned()
        .expect("seed pending receiver");
    assert_eq!(
        pending.get("status").and_then(Value::as_str),
        Some("pending")
    );
    team_agent::state::persist::save_runtime_state(&workspace, &state).expect("persist seed");
    let pending_state = state.clone();

    let attached = team_agent::leader::attach_leader(
        &workspace,
        Some("alpha"),
        Some(&PaneId::new(pane.clone())),
        team_agent::provider::Provider::Fake,
    )
    .expect("attach_leader");
    assert!(attached.ok, "attach_leader must commit; {attached:?}");

    let outcome = team_agent::leader::registry::register_binding_from_state_best_effort(
        &workspace,
        Some("alpha"),
        "isolated-success",
    );
    assert_eq!(
        outcome.as_ref().map(|value| value.status),
        Some("registered")
    );

    assert_eq!(
        classify_leader_binding(&workspace, "alpha"),
        LeaderBindingClass::Attached
    );
    let (failures, ok, reason) =
        restart_leader_public_bind(&workspace, Some("alpha"), &pending_state);
    assert_eq!(
        (ok, reason.as_deref()),
        (true, None),
        "load_restart_bind_state must read attached disk over in-memory pending"
    );
    let public = team_agent::cli::lifecycle_port::restart_value(
        RestartReport::Restarted {
            session_name: team_agent::transport::SessionName::new("team-alpha"),
            agents: Vec::new(),
            coordinator_started: true,
            coordinator: dummy_coordinator(),
            next_actions: Vec::new(),
            attach_commands: Vec::new(),
            attach_window_failures: failures,
            leader_bind_ok: ok,
            leader_bind_reason: reason,
        },
        Some("alpha"),
    );
    assert_eq!(public.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        public.get("reason").is_none() || public.get("reason") == Some(&Value::Null),
        "successful Restarted must not carry a bind failure reason; got {public}"
    );

    let diagnose = diagnose_json(&workspace, "alpha");
    let ids = issue_ids(&diagnose);
    assert!(
        !ids.iter().any(|id| id == "leader_registry_index_missing"),
        "attached success must not be diagnosed as index-missing; issues={ids:?}"
    );
}
