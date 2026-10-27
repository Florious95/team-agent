use crate::cli::spec::{CommandKind, COMMAND_SPECS};
use crate::lifecycle::launch::pi_mcp::parse_pi_leader_args;
use crate::model::enums::{Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::transport::test_support::OfflineTransport;
use crate::transport::{Transport, WindowName};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const PI_DUAL_ENTRY_CHILD: &str = "TEAM_AGENT_TEST_PI_DUAL_ENTRY_CHILD";
const PI_DUAL_ENTRY_PARENT_PID: &str = "TEAM_AGENT_TEST_PI_DUAL_ENTRY_PARENT_PID";
const PI_DUAL_ENTRY_TEST: &str = concat!(
    "lifecycle::tests::pi_dual_entry_red::",
    "pi_leader_and_teammate_share_provider_plan_but_are_separately_launchable"
);

fn run_process_isolated(marker: &str, test_name: &str, body: impl FnOnce()) {
    if std::env::var_os(marker).is_some() {
        body();
        return;
    }

    let output = Command::new(std::env::current_exe().expect("current lib-test executable"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(marker, "1")
        .env(PI_DUAL_ENTRY_PARENT_PID, std::process::id().to_string())
        .output()
        .expect("run Pi send_message fixture in isolated child test process");
    assert!(
        output.status.success(),
        "isolated Pi child test failed: test={test_name} status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn dynamic_add_fixture(root: &Path, label: &str, role_doc: &str) -> (PathBuf, PathBuf) {
    let team = root.join(label);
    std::fs::create_dir_all(team.join("agents")).expect("create dynamic team fixture");
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {label}\nobjective: Dynamic add fixture.\nprovider: fake\n---\n\nfixture\n"
        ),
    )
    .expect("write dynamic TEAM.md");
    std::fs::write(
        team.join("agents/implementer.md"),
        "---\nname: implementer\nrole: Existing Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nexisting\n",
    )
    .expect("write existing role");
    let role = team.join("mate-role.md");
    std::fs::write(&role, role_doc).expect("write dynamic role");
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": format!("team-{label}"),
            "active_team_key": label,
            "team_dir": team,
            "agents": {
                "implementer": {
                    "status": "running",
                    "provider": "fake",
                    "role": "Existing Worker",
                    "model": "fake",
                    "auth_mode": "subscription",
                    "window": "implementer"
                }
            }
        }),
    )
    .expect("seed dynamic runtime state");
    super::launch_spawn::seed_healthy_coordinator(&team);
    (team, role)
}

#[test]
fn pi_leader_and_teammate_share_provider_plan_but_are_separately_launchable() {
    run_process_isolated(PI_DUAL_ENTRY_CHILD, PI_DUAL_ENTRY_TEST, || {
        let hermetic = HermeticTestEnv::enter("pi-dual-entry-send");
        let parent_pid = std::env::var(PI_DUAL_ENTRY_PARENT_PID)
            .expect("isolated child receives the parent test process id");
        assert_ne!(
            std::process::id().to_string(),
            parent_pid,
            "Pi send_message fixture must not run in the parent lib-test process"
        );
        pi_leader_and_teammate_body(&hermetic);
    });
}

fn pi_leader_and_teammate_body(hermetic: &HermeticTestEnv) {
    let spec = COMMAND_SPECS
        .iter()
        .find(|spec| spec.name == "pi")
        .expect("team-agent pi must be a registered leader command");
    assert_eq!(spec.kind, CommandKind::LeaderPassthrough { provider: "pi" });
    assert_eq!(
        crate::cli::leader::leader_passthrough_provider("pi"),
        Some(Provider::Pi)
    );

    let leader_argv = [
        "pi".to_string(),
        "--".to_string(),
        "--model".to_string(),
        "team-agent/qwen3.8-27b".to_string(),
        "--thinking".to_string(),
        "max".to_string(),
    ];
    assert!(
        crate::cli::emit::is_leader_passthrough_command(&leader_argv[0]),
        "the real CLI dispatch table must route team-agent pi before generic subcommand parsing"
    );
    assert!(
        crate::cli::emit::default_help()
            .lines()
            .any(|line| { line.split_whitespace().next() == Some("pi") }),
        "default help must advertise the dispatchable Pi launcher"
    );
    let leader = parse_pi_leader_args(&leader_argv[2..])
        .expect("leader exact model and effort must compile into the shared plan input");
    assert_eq!(leader.model.as_deref(), Some("team-agent/qwen3.8-27b"));
    assert_eq!(leader.effort, Some(ProviderEffort::Max));

    let defaults = parse_pi_leader_args(&[]).expect("leader provider defaults");
    assert_eq!(defaults.model, None);
    assert_eq!(defaults.effort, None);
    assert_eq!(
        parse_pi_leader_args(&["--thinking".to_string(), "medium".to_string()])
            .expect("explicit thinking only")
            .effort,
        Some(ProviderEffort::Medium)
    );
    assert_eq!(
        parse_pi_leader_args(&["--model".to_string(), "team-agent/qwen3.8-27b".to_string(),])
            .expect("explicit model only")
            .model
            .as_deref(),
        Some("team-agent/qwen3.8-27b")
    );

    for invalid in [
        vec![
            "--model".to_string(),
            "qwen3.8-27b".to_string(),
            "--thinking".to_string(),
            "medium".to_string(),
        ],
        vec![
            "--model".to_string(),
            "team-agent/qwen3.8-27b".to_string(),
            "--thinking".to_string(),
            "medium".to_string(),
            "--mcp-config".to_string(),
            "/tmp/ambient.json".to_string(),
        ],
    ] {
        assert!(
            parse_pi_leader_args(&invalid).is_err(),
            "leader input must refuse ambiguous or materializer-owned fields: {invalid:?}"
        );
    }

    let dynamic_root = hermetic.workspace("teammate-dual");
    let fake_role = "---\nname: mate\nrole: Dynamic Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\ndynamic\n";

    let (success_team, success_role) =
        dynamic_add_fixture(&dynamic_root, "dynamic-success", fake_role);
    let success_agents = crate::state::persist::load_runtime_state(&success_team)
        .expect("load selected-team fixture")
        .get("agents")
        .cloned()
        .expect("fixture agents");
    let receiver_socket = "/private/tmp/tmux-501/ta-leader-receiver";
    let owning_session = "team-agent-leader-pi-owning-team";
    let worker_socket = "/private/tmp/tmux-501/ta-worker-persisted";
    crate::state::persist::save_runtime_state(
        &success_team,
        &json!({
            "active_team_key": "dynamic-success",
            "tmux_endpoint": worker_socket,
            "tmux_socket": worker_socket,
            "teams": {
                "dynamic-success": {
                    "agents": success_agents,
                    "team_dir": success_team,
                    "session_name": null,
                    "tmux_endpoint": worker_socket,
                    "tmux_socket": worker_socket,
                    "leader_receiver": {
                        "status": "attached",
                        "session_name": owning_session,
                        "tmux_socket": receiver_socket
                    }
                }
            }
        }),
    )
    .expect("seed nested selected-team owner");
    let selected_transport =
        crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
            &success_team,
            Some("dynamic-success"),
        )
        .expect("resolve selected worker transport");
    assert_eq!(
        selected_transport.tmux_endpoint().as_deref(),
        Some(worker_socket),
        "lifecycle worker transport must use persisted worker endpoint, not receiver endpoint"
    );
    let mut annotation_state =
        crate::state::persist::load_runtime_state(&success_team).expect("load annotation fixture");
    let wrong_transport = OfflineTransport::new().with_tmux_endpoint(worker_socket);
    crate::lifecycle::launch::annotate_runtime_tmux_endpoint(
        &mut annotation_state,
        &wrong_transport,
        &success_team,
    );
    assert_eq!(
        annotation_state
            .get("tmux_endpoint")
            .and_then(serde_json::Value::as_str),
        Some(worker_socket),
        "worker-derived annotation must preserve the persisted worker endpoint"
    );
    assert_eq!(
        annotation_state
            .pointer("/teams/dynamic-success/leader_receiver/tmux_socket")
            .and_then(serde_json::Value::as_str),
        Some(receiver_socket)
    );
    let success_transport = OfflineTransport::new()
        .with_session_present(true)
        .with_tmux_endpoint(worker_socket);
    crate::lifecycle::add_agent_with_transport(
        &success_team,
        &AgentId::new("mate"),
        &success_role,
        false,
        Some("dynamic-success"),
        &success_transport,
    )
    .expect("fake dynamic add must spawn through the shared start path");
    let spawn = success_transport.spawn_records();
    assert_eq!(spawn.len(), 1, "dynamic add must spawn exactly once");
    let events = crate::event_log::EventLog::new(&success_team)
        .tail(50)
        .expect("read dynamic add events");
    let start_event = events
        .iter()
        .find(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("start_agent.agent_start")
        })
        .expect("start event");
    let event_command = start_event
        .get("command")
        .and_then(serde_json::Value::as_array)
        .expect("start event command")
        .iter()
        .map(|value| value.as_str().expect("argv string").to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        event_command, spawn[0].1,
        "start event must record the exact materialized plan that was spawned"
    );
    assert_eq!(
        start_event
            .get("session")
            .and_then(serde_json::Value::as_str),
        Some(owning_session),
        "missing team session must reuse the live owning receiver session"
    );
    assert_eq!(
        start_event
            .get("tmux_start_mode")
            .and_then(serde_json::Value::as_str),
        Some("new-window"),
        "dynamic add must not create a second tmux session"
    );

    let (rollback_team, rollback_role) =
        dynamic_add_fixture(&dynamic_root, "dynamic-rollback", fake_role);
    let events_path = rollback_team.join(".team/logs/events.jsonl");
    std::fs::create_dir_all(&events_path).expect("make the start event write fail after spawn");
    let rollback_transport = OfflineTransport::new().with_session_present(true);
    let rollback = crate::lifecycle::add_agent_with_transport(
        &rollback_team,
        &AgentId::new("mate"),
        &rollback_role,
        false,
        None,
        &rollback_transport,
    );
    assert!(rollback.is_err(), "post-spawn event failure must fail add");
    assert!(
        rollback_transport.calls().contains(&"kill_pane"),
        "post-spawn failure must kill the exact spawned pane receipt"
    );
    let rolled_back_state =
        crate::state::persist::load_runtime_state(&rollback_team).expect("load rolled back state");
    assert!(
        rolled_back_state.pointer("/agents/mate").is_none(),
        "failed dynamic add must not retain the inserted seat"
    );

    let (noop_team, noop_role) = dynamic_add_fixture(&dynamic_root, "dynamic-noop", fake_role);
    let noop_transport = OfflineTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("mate")]);
    let noop = crate::lifecycle::add_agent_with_transport(
        &noop_team,
        &AgentId::new("mate"),
        &noop_role,
        false,
        None,
        &noop_transport,
    )
    .expect_err("a newly added seat cannot succeed through start_agent.noop");
    assert!(noop.to_string().contains("start_agent.noop"));
    assert!(noop_transport.spawn_records().is_empty());
    let noop_state =
        crate::state::persist::load_runtime_state(&noop_team).expect("load noop rollback state");
    assert!(noop_state.pointer("/agents/mate").is_none());

    let (dead_team, dead_role) = dynamic_add_fixture(&dynamic_root, "dynamic-dead", fake_role);
    let dead_transport = OfflineTransport::new()
        .with_session_present(true)
        .with_spawned_panes_addressable(false);
    let dead = crate::lifecycle::add_agent_with_transport(
        &dead_team,
        &AgentId::new("mate"),
        &dead_role,
        false,
        None,
        &dead_transport,
    )
    .expect_err("dead spawned pane must not produce add-agent ok:true");
    assert!(dead
        .to_string()
        .contains("not addressable on transport socket"));
    assert!(dead_transport.calls().contains(&"kill_pane"));
    let dead_state =
        crate::state::persist::load_runtime_state(&dead_team).expect("load dead rollback state");
    assert!(dead_state.pointer("/agents/mate").is_none());

    let shared_source = include_str!("../launch/pi_mcp.rs");
    let leader_source = include_str!("../../leader/start.rs");
    let restart_agent_source = include_str!("../restart/agent.rs");
    let add_source = include_str!("../launch/add_agent.rs");
    assert!(
        leader_source.contains("materialize_pi_plan("),
        "Pi leader entry must call the sole Core materializer"
    );
    assert!(
        !leader_source.contains("Provider::Pi => provider_command_argv"),
        "Pi leader must not fall back to raw passthrough argv"
    );
    assert_eq!(
        shared_source.matches("fn materialize_pi_plan(").count(),
        1,
        "the Pi leader must depend on one Core materializer definition"
    );
    assert!(add_source.contains("start_agent_at_paths("));
    assert!(restart_agent_source.contains("&spawn.plan,"));
    std::fs::remove_dir_all(dynamic_root).expect("remove dynamic fixtures");
}
