use super::*;
use serial_test::serial;

struct EnvUnsetGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvUnsetGuard {
    fn unset(keys: &[&'static str]) -> Self {
        let previous = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in keys {
            std::env::remove_var(key);
        }
        Self { previous }
    }
}

impl Drop for EnvUnsetGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct WorkspaceCleanup(std::path::PathBuf);

impl Drop for WorkspaceCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct IsolatedHome {
    prev: Option<String>,
    dir: std::path::PathBuf,
}

impl IsolatedHome {
    fn enter(tag: &str) -> Self {
        let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!(
            "ta-cli-home-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".team-agent").join("leaders")).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &dir);
        Self { prev, dir }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn seed_self_signed_attached_leader(ws: &std::path::Path) {
    std::fs::write(
        ws.join(".team").join("runtime").join("state.json"),
        serde_json::to_vec(&json!({
            "team_key": "current",
            "leader": {"id": "leader"},
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "pane_id": "%1",
                "provider": "codex"
            },
        }))
        .unwrap(),
    )
    .unwrap();
}

fn seed_canonical_healthy_leader(ws: &std::path::Path, endpoint: &str) {
    crate::state::persist::save_runtime_state(
        ws,
        &json!({
            "team_key": "current",
            "workspace": ws,
            "teams": {
                "current": {
                    "team_owner": {
                        "pane_id": "%1",
                        "owner_epoch": 1,
                        "provider": "codex"
                    },
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "pane_id": "%1",
                        "owner_epoch": 1,
                        "provider": "codex",
                        "tmux_socket": endpoint
                    }
                }
            }
        }),
    )
    .unwrap();
    assert_eq!(
        crate::leader::registry::register_binding_from_state_best_effort(
            ws,
            Some("current"),
            "diagnose-healthy-fixture",
        )
        .as_ref()
        .map(|outcome| outcome.status),
        Some("registered")
    );
}

fn seed_leader_registry_entry(ws: &std::path::Path) {
    let entry = crate::leader::registry::build_entry(
        ws,
        "current",
        "direct_tmux",
        json!({"status": "attached", "pane_id": "%1"}),
        1,
        "diagnose-healthy-fixture",
        "2026-08-18T00:00:00Z".to_string(),
    );
    assert!(
        crate::leader::registry::write_entry_best_effort(&entry).is_some(),
        "hermetic HOME must accept a leaders/ registry write"
    );
}

// Retained doctor dispatch and shared status-readiness contracts.

fn cli_argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// ── diagnose ── attached is host-registry authority, not state.json self-sign.
// Seed leader_receiver=attached + a ~/.team-agent/leaders/ row for this workspace
// + NO session_name + NO agents -> issues=[] -> EXIT 0.
#[test]
#[serial(env)]
fn dispatch_routes_diagnose_checks_runtime_beyond_healthy_leader() {
    let _env = EnvUnsetGuard::unset(&[
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_WORKSPACE",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TEAM_AGENT_ACTIVE_TEAM",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let _home = IsolatedHome::enter("diagnose-healthy");
    let ws = tmp_workspace();
    let endpoint = "/tmp/ta-diagnose-healthy.sock";
    seed_canonical_healthy_leader(&ws, endpoint);
    let transport = crate::transport::test_support::OfflineTransport::default()
        .with_tmux_endpoint(endpoint)
        .with_targets(vec![crate::transport::PaneInfo {
            pane_id: crate::transport::PaneId::new("%1"),
            session: crate::transport::SessionName::new("s"),
            window_index: None,
            window_name: None,
            pane_index: None,
            tty: None,
            current_command: Some("codex".to_string()),
            current_path: Some(ws.clone()),
            active: true,
            pane_pid: None,
            leader_env: Default::default(),
        }]);
    crate::transport_factory::with_leader_endpoint_transport(endpoint, transport, || {
        let code = run(
            &cli_argv(&["doctor", "--workspace", &ws.to_string_lossy(), "--json"]),
            &ws,
        );
        assert_eq!(code, ExitCode::Error, "healthy binding alone cannot hide missing coordinator");
        let result = cmd_doctor(&DoctorArgs {
            workspace: ws.clone(), json: true, team: None,
            spec: None, gate: None, comms: false, fix: false,
            fix_schema: false, cleanup_orphans: false, confirm: false,
        }).expect("diagnostic report");
        let CmdOutput::Json(report) = result.output else {
            panic!("expected JSON diagnostic report");
        };
        assert_eq!(report["coordinator"]["ok"], false, "{report}");
        let issues = report["issues"].to_string();
        for binding_failure in ["leader_not_attached", "leader_receiver_unbound", "leader_workspace_mismatch"] {
            assert!(!issues.contains(binding_failure), "healthy binding regressed: {report}");
        }
    });
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
#[serial(env)]
fn dispatch_routes_diagnose_self_signed_attached_without_registry_is_not_ok() {
    // 已废除的行为：旧实现只信 `state.json` 的自签 attached（F2 谎话本体），此断言证明它确实没了。
    // 夹具只种 leader_receiver.status=attached，注册表无该 workspace 条目。
    // 旧 diagnose 会因此报 Ok；新 diagnose 必须以注册表为可投递权威，判非 Ok。
    let _env = EnvUnsetGuard::unset(&[
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_WORKSPACE",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_OWNER_TEAM_ID",
        "TEAM_AGENT_ACTIVE_TEAM",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let _home = IsolatedHome::enter("diagnose-f2-tombstone");
    let ws = tmp_workspace();
    seed_self_signed_attached_leader(&ws);
    let code = run(
        &cli_argv(&["doctor", "--workspace", &ws.to_string_lossy(), "--json"]),
        &ws,
    );
    assert_ne!(
        code,
        ExitCode::Ok,
        "self-signed state.json attached with no leaders/ row must not diagnose Ok (F2 lie is dead); \
         got {code:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// CONTRACT (shared-root, real-machine-driven; golden = correct-behavior baseline): runtime_readiness
// derives cli_prompt_ready from the LIFECYCLE status — an alive worker (status="running") IS
// cli_prompt_ready (golden quick_start.py:173 `status ∈ {running, busy}`). Rust (diagnose.rs:280)
// requires cli_prompt_ready flag / startup_prompts=="complete" / status=="ready" and does NOT accept
// "running" → an alive fake worker never becomes ready → wait_ready times out. Same shared root as the
// deferred_busy regression: lifecycle "running" is the authoritative alive/ready signal, not a
// turn-level flag. (process_started/mcp_ready/task_prompt_delivered are satisfied here so `ready`
// hinges solely on the cli_prompt_ready derivation.)
#[test]
fn contract_alive_worker_running_is_cli_prompt_ready_and_ready() {
    // A-5: missing leader_receiver no longer counts as attached; this contract's
    // subject is the cli_prompt_ready derivation, so the fixture carries an
    // attached receiver to keep `ready` hinging on it alone.
    let state = serde_json::json!({
        "leader_receiver": {"status": "attached", "pane_id": "%9"},
        "agents": {"w1": {
        "status": "running",
        "pane_id": "%1",
        "mcp_ready": true,
        "first_send_at": "2026-01-01T00:00:00Z"
    }}});
    let r = crate::cli::diagnose::runtime_readiness(&state);
    assert_eq!(
            r.get("cli_prompt_ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "CONTRACT: an alive worker (lifecycle status=running) is cli_prompt_ready (golden status∈{{running,busy}}); got {r:?}"
        );
    assert_eq!(
            r.get("ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "status=running + pane_id + mcp_ready + first_send_at → all four readiness signals derive true → ready (no timeout); got {r:?}"
        );
}

// CONTRACT (real-machine wait_ready product FAIL @ 8ea5df5): a live fake quick-start worker times out
// with process_started=false + mcp_ready=false. golden (quick_start.py:172/174):
//   process_started = bool(last["tmux_session_present"])  (the live tmux session exists)
//   mcp_ready       = all(Path(agent["mcp_config"]).exists())  (each agent's mcp_config FILE exists)
// The Rust launch (launch.rs:220-228) persists status="running" + mcp_config (path) but NO pane_id/pid
// and NO mcp_ready flag. runtime_readiness reads per-agent pane_id/pid for process_started (-> false) and an
// mcp_ready FLAG for mcp_ready (-> false), so a live worker never becomes ready. The shared-root fix only
// corrected cli_prompt_ready (status=running); these two signals still read the wrong source. This RED
// uses the REALISTIC post-launch state (no synthetic pane_id / mcp_ready flag).
#[test]
fn contract_wait_ready_derives_process_started_from_session_and_mcp_ready_from_file() {
    let dir = std::env::temp_dir().join(format!(
        "ta-wr-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mcp = dir.join("mcp.json");
    std::fs::write(&mcp, "{}").unwrap();
    let state = serde_json::json!({
        "session_name": "ta-fake",
        "tmux_session_present": true, // golden status() top-level signal: the live tmux session exists
        // A-5: missing leader_receiver no longer counts as attached (see above).
        "leader_receiver": {"status": "attached", "pane_id": "%9"},
        "agents": { "w1": {
            "status": "running",
            "mcp_config": mcp.to_string_lossy(),
            "first_send_at": "2026-01-01T00:00:00Z"
        }}
    });
    let r = crate::cli::diagnose::runtime_readiness(&state);
    assert_eq!(
            r.get("process_started").and_then(serde_json::Value::as_bool),
            Some(true),
            "CONTRACT: process_started derives from tmux_session_present (golden quick_start.py:172), NOT \
             per-agent pane_id/pid (the launch writes neither); got {r:?}"
        );
    assert_eq!(
            r.get("mcp_ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "CONTRACT: mcp_ready derives from each agent's mcp_config FILE existence (golden quick_start.py:174), \
             NOT an mcp_ready flag (never set); got {r:?}"
        );
    assert_eq!(
            r.get("ready").and_then(serde_json::Value::as_bool),
            Some(true),
            "a live fake quick-start worker (session present + status running + mcp_config file + task sent) \
             must be ready, not timeout; got {r:?}"
        );
    let _ = std::fs::remove_dir_all(&dir);
}
