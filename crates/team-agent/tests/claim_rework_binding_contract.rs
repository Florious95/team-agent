//! Claim rework A6 contract tests. Isolated HOME/workspace.
//! Cargo: `--test claim_rework_binding_contract`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
#[path = "support/hermetic.rs"]
mod hermetic;

use hermetic::HermeticTestEnv;
use team_agent::cli::{cmd_doctor, CmdOutput, DoctorArgs};
use team_agent::lifecycle::launch::{
    classify_leader_binding, launched_team_receiver_is_attached,
    seed_launched_owner_from_caller_with_provider_lookup, LeaderBindingClass,
};
use team_agent::lifecycle::{restart_leader_public_bind, CoordinatorStartSummary, RestartReport};
use team_agent::state::owner_gate::CallerIdentity;

static ISOLATION: AtomicU64 = AtomicU64::new(0);

struct IsolatedHome {
    _env: HermeticTestEnv,
}

impl IsolatedHome {
    fn enter() -> Self {
        Self {
            _env: HermeticTestEnv::enter("claim-rework-a9"),
        }
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
    diagnose_json_team(workspace, Some(team))
}

fn diagnose_json_team(workspace: &Path, team: Option<&str>) -> Value {
    let result = cmd_doctor(&DoctorArgs {
        workspace: workspace.to_path_buf(),
        json: true,
        team: team.map(str::to_string),
        spec: None,
        gate: None,
        comms: false,
        fix: false,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
    })
    .expect("cmd_doctor");
    match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("diagnose must be JSON, got {other:?}"),
    }
}

// 262f6cdb (doctor v1): six ordered links plus `next_action`; the issue-id
// inventory is retired.
fn link<'a>(diagnose: &'a Value, id: &str) -> &'a Value {
    let links = diagnose["links"].as_array().unwrap_or_else(|| panic!("doctor v1 links: {diagnose}"));
    assert_eq!(links.len(), 6, "{diagnose}");
    links.iter().find(|link| link["id"] == id).unwrap()
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
fn empty_workspace_diagnose_does_not_claim() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    let diagnose = diagnose_json_team(&workspace, None);
    // 262f6cdb + A3 (ec3ef36e): no runtime is a typed L1 failure whose only
    // next action is the creation hint; a synthetic `current` is never an
    // unbound team, so no claim is induced.
    assert_eq!(link(&diagnose, "L1")["status"], "FAIL", "{diagnose}");
    assert!(link(&diagnose, "L1")["message"].as_str().is_some_and(|message| message.contains("has no runtime state")), "{diagnose}");
    let next = diagnose["next_action"].to_string();
    assert!(
        !next.contains("claim-leader") && !link(&diagnose, "L6")["message"].as_str().unwrap_or("").contains("unbound"),
        "must not induce claim on a workspace with no team; next_action={next}"
    );
    assert!(next.contains("team-agent start AGENT --team"), "{diagnose}");
    assert_eq!(diagnose.get("ok").and_then(Value::as_bool), Some(false));
    assert!(!workspace.join(".team/runtime/state.json").exists());
}

#[test]
#[serial_test::serial(env)]
fn existing_empty_team_diagnose_still_claims() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    // 262f6cdb: doctor v1 reports the first failing link's repair; give the
    // empty team a live host so the unbound leader (L6) owns the repair.
    let (_tmux, _pane) = start_owned_tmux(&workspace);
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({"teams": {"alpha": {"tmux_socket": _tmux.socket.display().to_string()}}}),
    )
    .expect("save empty team");
    let diagnose = diagnose_json(&workspace, "alpha");
    assert_eq!(
        link(&diagnose, "L1")["status"], "PASS",
        "existing team must not be classified missing; diagnose={diagnose}"
    );
    assert_eq!(link(&diagnose, "L6")["status"], "FAIL", "existing empty team stays unbound; diagnose={diagnose}");
    assert!(
        diagnose["next_action"]["argv"].as_array().is_some_and(|argv| argv.iter().any(|arg| arg == "claim-leader")),
        "existing unbound team keeps claim repair; diagnose={diagnose}"
    );
}

#[test]
#[serial_test::serial(env)]
fn isolated_missing_registry_is_index_missing() {
    let _home = IsolatedHome::enter();
    let workspace = unique_workspace();
    // 95cb2f3d: a missing index is IndexMissing only when the underlying
    // binding is live; a dead socket requires an explicit claim (Unbound).
    // Bind the receiver to a real, test-owned live pane.
    let (tmux, pane) = start_owned_tmux(&workspace);
    let socket = tmux.socket.display().to_string();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "team_key": "alpha",
            "teams": {
                "alpha": {
                    "tmux_socket": socket,
                    "team_owner": {"pane_id": pane, "owner_epoch": 1, "tmux_socket": socket},
                    "leader_receiver": {
                        "status": "attached",
                        "pane_id": pane,
                        "owner_epoch": 1,
                        "tmux_socket": socket
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
    // 262f6cdb: the reachable leader with no discovery index is an L6 WARN
    // (index missing/stale), never remapped to an unreachable/unbound FAIL.
    let ret = link(&diagnose, "L6");
    assert_eq!(ret["status"], "WARN", "diagnose={diagnose}");
    assert_eq!(
        ret["message"].as_str().unwrap_or("").matches("discovery index missing").count(),
        1,
        "diagnose={diagnose}"
    );
    assert!(
        !ret["message"].as_str().unwrap_or("").contains("unbound"),
        "canonical diagnose must not remap index-missing to unbound; diagnose={diagnose}"
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
    // 262f6cdb: doctor v1 never certifies an unknown binding: the return link
    // does not pass, and the undecidable registry file is left byte-identical
    // (classification above stays Unknown, not Unbound).
    assert_ne!(link(&diagnose, "L6")["status"], "PASS", "diagnose={diagnose}");
    assert_ne!(diagnose.get("ok").and_then(Value::as_bool), Some(true), "{diagnose}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json", "doctor rewrote the registry");
    assert_eq!(
        classify_leader_binding(&workspace, "alpha"),
        LeaderBindingClass::Unknown,
        "canonical diagnose must not remap unknown to unbound"
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
            tmux_socket: None,
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

    let attached = team_agent::cli::leader_port::claim_leader(&workspace, Some("alpha"), false)
        .expect("claim_leader");
    assert_eq!(attached.get("ok").and_then(Value::as_bool), Some(true), "claim must commit; {attached:?}");

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
