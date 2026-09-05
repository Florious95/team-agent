use crate::cli::spec::{CommandKind, COMMAND_SPECS};
use crate::compiler::compile_role_agent;
use crate::lifecycle::launch::pi_mcp::parse_pi_leader_args;
use crate::mcp_server::TeamOrchestratorTools;
use crate::model::enums::{AuthMode, Provider, ProviderEffort};
use crate::model::ids::{AgentId, TeamKey};
use crate::model::yaml::Value;
use crate::provider::{get_adapter, ProviderCommandContext};
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

#[cfg(unix)]
const FILE_H_NATIVE_COMPILE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(unix)]
const FILE_H_NATIVE_READINESS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(unix)]
#[derive(Debug)]
struct FileHCommandTimeout {
    stage: &'static str,
    pid: u32,
    deadline: std::time::Duration,
    elapsed: std::time::Duration,
    reaped_status: std::process::ExitStatus,
}

#[cfg(unix)]
fn run_file_h_command_with_deadline(
    command: &mut std::process::Command,
    stage: &'static str,
    deadline: std::time::Duration,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<std::process::Output, FileHCommandTimeout> {
    let stdout_file = std::fs::File::create(stdout_path)
        .unwrap_or_else(|error| panic!("{stage}: create stdout receipt failed: {error}"));
    let stderr_file = std::fs::File::create(stderr_path)
        .unwrap_or_else(|error| panic!("{stage}: create stderr receipt failed: {error}"));
    let mut child = command
        .stdout(std::process::Stdio::from(stdout_file))
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()
        .unwrap_or_else(|error| panic!("{stage}: spawn failed: {error}"));
    let pid = child.id();
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|error| panic!("{stage}: poll pid {pid} failed: {error}"))
        {
            return Ok(std::process::Output {
                status,
                stdout: std::fs::read(stdout_path).unwrap_or_default(),
                stderr: std::fs::read(stderr_path).unwrap_or_default(),
            });
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            child
                .kill()
                .unwrap_or_else(|error| panic!("{stage}: kill exact pid {pid} failed: {error}"));
            let reaped_status = child
                .wait()
                .unwrap_or_else(|error| panic!("{stage}: reap exact pid {pid} failed: {error}"));
            return Err(FileHCommandTimeout {
                stage,
                pid,
                deadline,
                elapsed,
                reaped_status,
            });
        }
        std::thread::park_timeout(std::time::Duration::from_millis(10));
    }
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

#[cfg(unix)]
struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[cfg(unix)]
impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

#[cfg(unix)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
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
            .contains("team-agent codex|claude|copilot|grok|cursor|pi ..."),
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

    let root = hermetic.workspace("core-dual");
    let role = root.join("worker.md");
    std::fs::write(
        &role,
        "---\nname: worker-a\nrole: developer\nprovider: pi\nmodel: team-agent/qwen3.8-27b\nauth_mode: subscription\neffort: max\ntools:\n  - mcp_team\ndangerously_skip_permissions: true\n---\nworker contract\n",
    )
    .expect("write teammate role");
    let teammate = compile_role_agent(&role, &Value::Map(Vec::new()), "/workspace")
        .expect("provider: pi teammate must compile separately");
    assert_eq!(
        teammate.agent.get("provider").and_then(Value::as_str),
        Some("pi")
    );

    std::fs::write(
        &role,
        "---\nname: worker-a\nrole: developer\nprovider: pi\nauth_mode: subscription\ntools:\n  - mcp_team\ndangerously_skip_permissions: true\n---\nworker contract\n",
    )
    .expect("write provider-default teammate role");
    let defaults = compile_role_agent(&role, &Value::Map(Vec::new()), "/workspace")
        .expect("provider: pi teammate may use Pi model and effort defaults");
    assert_eq!(defaults.agent.get("model"), Some(&Value::Null));
    assert!(
        defaults.agent.get("effort").is_none(),
        "omitted Pi model and effort must remain absent"
    );
    std::fs::remove_dir_all(root).expect("remove role fixture");

    let adapter = get_adapter(Provider::Pi);
    let raw_build = adapter.build_command_plan(ProviderCommandContext {
        auth_mode: AuthMode::Subscription,
        mcp_config: None,
        system_prompt: None,
        model: None,
        tools: &[],
        profile_launch: None,
        agent_id_hint: None,
        effort: None,
    });
    assert!(raw_build
        .expect_err("Pi adapter must refuse callers that skip the shared materializer")
        .to_string()
        .contains("shared lifecycle materializer"));

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

    let pi_role = "---\nname: mate\nrole: Pi Dynamic Worker\nprovider: pi\nmodel: team-agent/qwen3.8-27b\nauth_mode: subscription\neffort: max\ntools:\n  - mcp_team\ndangerously_skip_permissions: true\n---\n\ndynamic pi\n";
    let (noop_team, noop_role) = dynamic_add_fixture(&dynamic_root, "dynamic-noop", pi_role);
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
    .expect_err("a newly added Pi seat cannot succeed through start_agent.noop");
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

    #[cfg(unix)]
    {
        let live = dynamic_root.join("live-public-add");
        std::fs::create_dir_all(live.join("agents")).expect("create live agents dir");
        std::fs::create_dir_all(live.join("roles")).expect("create live roles dir");
        std::fs::write(
            live.join("TEAM.md"),
            "---\nname: current\nobjective: Public no-spec add fixture.\nprovider: fake\n---\n\nfixture\n",
        )
        .expect("write live TEAM.md");
        std::fs::write(
            live.join("agents/implementer.md"),
            "---\nname: implementer\nrole: Existing Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nexisting\n",
        )
        .expect("write live existing role");
        std::fs::write(live.join("roles/mate.md"), fake_role).expect("write live add role");
        let live_socket = "/private/tmp/tmux-501/ta-live-owning";
        let live_session = "team-agent-leader-pi-live-owning";
        crate::state::persist::save_runtime_state(
            &live,
            &json!({
                "active_team_key": "current",
                "tmux_endpoint": worker_socket,
                "tmux_socket": worker_socket,
                "agents": {
                    "implementer": {
                        "status": "running",
                        "provider": "fake",
                        "model": "fake",
                        "auth_mode": "subscription",
                        "window": "implementer"
                    }
                },
                "teams": {
                    "current": {
                        "status": "alive",
                        "team_dir": live,
                        "session_name": null,
                        "agents": {
                            "implementer": {
                                "status": "running",
                                "provider": "fake",
                                "model": "fake",
                                "auth_mode": "subscription",
                                "window": "implementer"
                            }
                        },
                        "leader_receiver": {
                            "status": "attached",
                            "session_name": live_session,
                            "tmux_socket": live_socket
                        }
                    },
                    "sibling": {
                        "status": "alive",
                        "team_dir": live.join("sibling"),
                        "session_name": null,
                        "agents": {},
                        "leader_receiver": {
                            "status": "attached",
                            "session_name": "team-agent-leader-pi-sibling",
                            "tmux_socket": "/private/tmp/tmux-501/ta-live-sibling"
                        }
                    }
                }
            }),
        )
        .expect("seed live nested receiver");
        super::launch_spawn::seed_healthy_coordinator(&live);
        let runtime_spec = crate::model::paths::runtime_spec_path(&live, "current");
        assert!(
            !runtime_spec.exists(),
            "fixture must enter the TEAM.md fallback before runtime spec exists"
        );

        let shim_root = live.join("tmux-shim");
        std::fs::create_dir_all(&shim_root).expect("create tmux shim dir");
        let shim_log = shim_root.join("argv.log");
        let shim_source = shim_root.join("tmux.c");
        let shim = shim_root.join("tmux");
        std::fs::write(
            &shim_source,
            r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int has_arg(int argc, char **argv, const char *needle) {
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], needle) == 0) return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (getenv("TEAM_AGENT_PI_TEST_HANG") != NULL) for (;;) pause();
    const char *log_path = getenv("TEAM_AGENT_PI_TMUX_LOG");
    const char *socket = getenv("TEAM_AGENT_PI_OWNING_SOCKET");
    const char *session = getenv("TEAM_AGENT_PI_OWNING_SESSION");
    const char *workspace = getenv("TEAM_AGENT_PI_RUN_WORKSPACE");
    FILE *log = log_path == NULL ? NULL : fopen(log_path, "a");
    if (log == NULL || socket == NULL || session == NULL || workspace == NULL) return 92;
    for (int i = 1; i < argc; i++) fprintf(log, "%s%s", i == 1 ? "" : " ", argv[i]);
    fputc('\n', log);
    fclose(log);

    int owns_socket = 0;
    for (int i = 1; i + 1 < argc; i++) {
        if (strcmp(argv[i], "-S") == 0 && strcmp(argv[i + 1], socket) == 0) owns_socket = 1;
    }
    if (!owns_socket) return 91;
    if (has_arg(argc, argv, "new-window") || has_arg(argc, argv, "display-message")) {
        puts("%9101");
    } else if (has_arg(argc, argv, "list-panes")) {
        printf("%%9101__TA_FIELD__%s__TA_FIELD__0__TA_FIELD__mate__TA_FIELD__0__TA_FIELD__/dev/ttys099__TA_FIELD__sh__TA_FIELD__1__TA_FIELD__%s__TA_FIELD__1__TA_FIELD__0__TA_FIELD__9101__TA_FIELD__\n", session, workspace);
    }
    return 0;
}
"#,
        )
        .expect("write native tmux shim source");
        let compile = run_file_h_command_with_deadline(
            std::process::Command::new("cc")
                .args(["-O0", "-o"])
                .arg(&shim)
                .arg(&shim_source),
            "File-H native fake compile",
            FILE_H_NATIVE_COMPILE_DEADLINE,
            &shim_root.join("compile.stdout"),
            &shim_root.join("compile.stderr"),
        )
        .unwrap_or_else(|timeout| panic!("{timeout:?}"));
        assert!(
            compile.status.success(),
            "native tmux shim compile failed: {}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let tooth_started = std::time::Instant::now();
        let tooth = run_file_h_command_with_deadline(
            std::process::Command::new(&shim).env("TEAM_AGENT_PI_TEST_HANG", "1"),
            "File-H native fake readiness timeout tooth",
            FILE_H_NATIVE_READINESS_DEADLINE,
            &shim_root.join("readiness-tooth.stdout"),
            &shim_root.join("readiness-tooth.stderr"),
        )
        .expect_err("non-exiting native fake must hit the readiness deadline");
        assert_eq!(tooth.stage, "File-H native fake readiness timeout tooth");
        assert_eq!(tooth.deadline, FILE_H_NATIVE_READINESS_DEADLINE);
        assert!(!tooth.reaped_status.success(), "{tooth:?}");
        assert!(
            !crate::platform::process::pid_is_alive(tooth.pid),
            "readiness timeout must leave its exact child dead and reaped: {tooth:?}"
        );
        assert!(
            tooth_started.elapsed()
                <= FILE_H_NATIVE_READINESS_DEADLINE + std::time::Duration::from_secs(1),
            "readiness timeout tooth exceeded its deadline plus reap allowance: {tooth:?}"
        );

        let preflight = run_file_h_command_with_deadline(
            std::process::Command::new(&shim)
                .args(["-S", worker_socket, "has-session"])
                .env("TEAM_AGENT_PI_TMUX_LOG", shim_root.join("preflight.log"))
                .env("TEAM_AGENT_PI_OWNING_SOCKET", worker_socket)
                .env("TEAM_AGENT_PI_OWNING_SESSION", live_session)
                .env("TEAM_AGENT_PI_RUN_WORKSPACE", &live),
            "File-H native fake has-session preflight",
            FILE_H_NATIVE_READINESS_DEADLINE,
            &shim_root.join("preflight.stdout"),
            &shim_root.join("preflight.stderr"),
        )
        .unwrap_or_else(|timeout| panic!("{timeout:?}"));
        assert!(
            preflight.status.success(),
            "native tmux shim preflight failed: {}",
            String::from_utf8_lossy(&preflight.stderr)
        );
        let previous_path = std::env::var_os("PATH").unwrap_or_default();
        let joined_path = std::env::join_paths(
            std::iter::once(shim_root.clone()).chain(std::env::split_paths(&previous_path)),
        )
        .expect("join native shim PATH");
        let _path = EnvVarGuard::set("PATH", joined_path);
        let _log = EnvVarGuard::set("TEAM_AGENT_PI_TMUX_LOG", &shim_log);
        let _socket = EnvVarGuard::set("TEAM_AGENT_PI_OWNING_SOCKET", worker_socket);
        let _session = EnvVarGuard::set("TEAM_AGENT_PI_OWNING_SESSION", live_session);
        let _workspace = EnvVarGuard::set("TEAM_AGENT_PI_RUN_WORKSPACE", &live);

        let tools = TeamOrchestratorTools::with_identity(
            &live,
            Some(AgentId::new("leader")),
            Some(TeamKey::new("current")),
        );
        let result = tools
            .add_agent("mate", "roles/mate.md")
            .expect("public MCP add must use the persisted worker endpoint");
        assert_eq!(
            result.fields.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        let tmux_argv = std::fs::read_to_string(&shim_log).expect("read tmux shim log");
        assert!(
            tmux_argv.contains(&format!("-S {worker_socket}")),
            "public add must address the persisted worker socket: {tmux_argv}"
        );
        assert!(
            tmux_argv.contains(&format!(
                "new-window -d -P -F #{{pane_id}} -t {live_session} -n mate"
            )),
            "public add must reuse the owner session with new-window: {tmux_argv}"
        );
        assert!(
            tmux_argv.contains(&live.to_string_lossy().to_string()),
            "spawn command must retain the MCP scratch as cwd: {tmux_argv}"
        );
        assert!(
            !tmux_argv.contains("new-session"),
            "public add must not start a same-named session on another server: {tmux_argv}"
        );
        let parent_socket = crate::tmux_backend::socket_name_for_workspace(
            live.parent().expect("live scratch parent"),
        );
        assert!(
            !tmux_argv.contains(&parent_socket),
            "public add must not derive transport from the scratch parent: {tmux_argv}"
        );

        let added_state = crate::state::persist::load_runtime_state(&live)
            .expect("load public add state for send/status");
        assert_eq!(
            added_state
                .pointer("/teams/current/session_name")
                .and_then(serde_json::Value::as_str),
            Some(live_session),
            "dynamic add must persist the selected receiver session for shared send/status routing"
        );
        assert!(
            added_state
                .pointer("/teams/sibling/session_name")
                .is_some_and(serde_json::Value::is_null),
            "selected-team routing must not borrow or rewrite a sibling session: {added_state}"
        );
        assert_eq!(
            added_state
                .pointer("/teams/sibling/leader_receiver/session_name")
                .and_then(serde_json::Value::as_str),
            Some("team-agent-leader-pi-sibling")
        );

        let mut old_state = added_state.clone();
        old_state["teams"]["current"]["session_name"] = serde_json::Value::Null;
        crate::state::persist::save_runtime_state(&live, &old_state)
            .expect("seed controlled old null-session view");
        let blocked = tools
            .send_message(
                &crate::messaging::MessageTarget::Single("mate".to_string()),
                "controlled old session routing probe",
                None,
                None,
                None,
            )
            .expect("old public send must return a structured refusal")
            .to_value();
        assert_eq!(blocked.get("ok"), Some(&json!(false)), "{blocked}");
        assert_eq!(
            blocked.get("reason"),
            Some(&json!("tmux_target_missing")),
            "the old null-session view must fail at real send target resolution: {blocked}"
        );
        let old_status = tools
            .get_team_status()
            .expect("old public status must return an honest dead view");
        assert_eq!(
            old_status
                .fields
                .get("agents")
                .and_then(|agents| agents.get("mate"))
                .and_then(|mate| mate.get("stale_reason"))
                .and_then(serde_json::Value::as_str),
            Some("pane_dead"),
            "the old null-session view must fail at real status liveness resolution: {:?}",
            old_status.fields
        );
        crate::state::persist::save_runtime_state(&live, &added_state)
            .expect("restore corrected selected-team session");

        let sent = tools
            .send_message(
                &crate::messaging::MessageTarget::Single("mate".to_string()),
                "session routing probe",
                None,
                None,
                None,
            )
            .expect("public send must resolve the added mate")
            .to_value();
        assert_eq!(sent.get("status"), Some(&json!("accepted")), "{sent}");
        assert!(
            sent.get("message_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|message_id| message_id.starts_with("msg_")),
            "blocked target resolution must not be reported as a successful send: {sent}"
        );

        let status = tools
            .get_team_status()
            .expect("public status must resolve the selected owner session");
        assert_eq!(
            status
                .fields
                .get("session_name")
                .and_then(serde_json::Value::as_str),
            Some(live_session),
            "status must report the selected receiver session"
        );
        assert_ne!(
            status
                .fields
                .get("agents")
                .and_then(|agents| agents.get("mate"))
                .and_then(|mate| mate.get("stale_reason"))
                .and_then(serde_json::Value::as_str),
            Some("pane_dead"),
            "the live owner-session mate must not be reported pane_dead: {:?}",
            status.fields
        );
    }

    let shared_source = include_str!("../launch/pi_mcp.rs");
    let leader_source = include_str!("../../leader/start.rs");
    let teammate_source = include_str!("../launch/spawn.rs");
    let restart_source = include_str!("../restart/common.rs");
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
        "leader and teammate call sites must depend on one shared materializer definition"
    );
    assert_eq!(
        shared_source
            .matches("fn materialize_pi_resume_plan(")
            .count(),
        1,
        "resume must share the same Core materializer module"
    );
    assert!(teammate_source.contains("materialize_pi_plan("));
    assert!(restart_source.contains("materialize_pi_plan("));
    assert!(add_source.contains("start_agent_at_paths("));
    assert!(restart_agent_source.contains("&spawn.plan,"));
    std::fs::remove_dir_all(dynamic_root).expect("remove dynamic fixtures");
}
