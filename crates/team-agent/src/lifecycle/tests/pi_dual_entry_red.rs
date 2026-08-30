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
    assert_eq!(leader.model, "team-agent/qwen3.8-27b");
    assert_eq!(leader.effort, ProviderEffort::Max);

    for invalid in [
        vec!["--thinking".to_string(), "medium".to_string()],
        vec!["--model".to_string(), "team-agent/qwen3.8-27b".to_string()],
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
            "leader input must refuse missing, ambiguous, or materializer-owned fields: {invalid:?}"
        );
    }

    let root = std::env::temp_dir().join(format!("team-agent-pi-core-dual-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create role fixture");
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
        "---\nname: worker-a\nrole: developer\nprovider: pi\nauth_mode: subscription\neffort: max\ntools:\n  - mcp_team\ndangerously_skip_permissions: true\n---\nworker contract\n",
    )
    .expect("write missing-model teammate role");
    assert!(
        compile_role_agent(&role, &Value::Map(Vec::new()), "/workspace").is_err(),
        "provider: pi teammate without an exact model must refuse"
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

    let dynamic_root = std::env::temp_dir().join(format!(
        "team-agent-pi-teammate-dual-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dynamic_root);
    std::fs::create_dir_all(&dynamic_root).expect("create dynamic root");
    let fake_role = "---\nname: mate\nrole: Dynamic Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\ndynamic\n";

    let (success_team, success_role) =
        dynamic_add_fixture(&dynamic_root, "dynamic-success", fake_role);
    let success_agents = crate::state::persist::load_runtime_state(&success_team)
        .expect("load selected-team fixture")
        .get("agents")
        .cloned()
        .expect("fixture agents");
    let owning_socket = "/private/tmp/tmux-501/ta-owning-team";
    let owning_session = "team-agent-leader-pi-owning-team";
    let wrong_socket = "/private/tmp/tmux-501/ta-workspace-derived";
    crate::state::persist::save_runtime_state(
        &success_team,
        &json!({
            "active_team_key": "dynamic-success",
            "tmux_endpoint": wrong_socket,
            "tmux_socket": wrong_socket,
            "teams": {
                "dynamic-success": {
                    "agents": success_agents,
                    "team_dir": success_team,
                    "session_name": null,
                    "tmux_endpoint": wrong_socket,
                    "tmux_socket": wrong_socket,
                    "leader_receiver": {
                        "status": "attached",
                        "session_name": owning_session,
                        "tmux_socket": owning_socket
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
        .expect("resolve selected owning transport");
    assert_eq!(
        selected_transport.tmux_endpoint().as_deref(),
        Some(owning_socket),
        "nested selected-team receiver must beat stale top-level and team socket fields"
    );
    let mut annotation_state =
        crate::state::persist::load_runtime_state(&success_team).expect("load annotation fixture");
    let wrong_transport = OfflineTransport::new().with_tmux_endpoint(wrong_socket);
    crate::lifecycle::launch::annotate_runtime_tmux_endpoint(
        &mut annotation_state,
        &wrong_transport,
        &success_team,
    );
    assert_eq!(
        annotation_state
            .get("tmux_endpoint")
            .and_then(serde_json::Value::as_str),
        Some(owning_socket),
        "worker-derived annotation must not clobber the owning endpoint"
    );
    assert_eq!(
        annotation_state
            .pointer("/teams/dynamic-success/leader_receiver/tmux_socket")
            .and_then(serde_json::Value::as_str),
        Some(owning_socket)
    );
    let success_transport = OfflineTransport::new()
        .with_session_present(true)
        .with_tmux_endpoint(owning_socket);
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
        use std::os::unix::fs::PermissionsExt;

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
                "tmux_endpoint": wrong_socket,
                "tmux_socket": wrong_socket,
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
        let shim = shim_root.join("tmux");
        std::fs::write(
            &shim,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$TEAM_AGENT_PI_TMUX_LOG"
case "$*" in
  *"-S $TEAM_AGENT_PI_OWNING_SOCKET"*) ;;
  *) exit 91 ;;
esac
case "$*" in
  *"new-window "*) printf '%%9101\n' ;;
  *"list-panes -a -F "*)
    printf '%%9101__TA_FIELD__%s__TA_FIELD__0__TA_FIELD__mate__TA_FIELD__0__TA_FIELD__/dev/ttys099__TA_FIELD__sh__TA_FIELD__1__TA_FIELD__%s__TA_FIELD__1__TA_FIELD__0__TA_FIELD__9101__TA_FIELD__\n' "$TEAM_AGENT_PI_OWNING_SESSION" "$TEAM_AGENT_PI_RUN_WORKSPACE"
    ;;
  *"display-message -p -t %9101 #{pane_id}"*) printf '%%9101\n' ;;
  *) ;;
esac
"#,
        )
        .expect("write tmux shim");
        let mut permissions = std::fs::metadata(&shim)
            .expect("stat tmux shim")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).expect("chmod tmux shim");
        let previous_path = std::env::var_os("PATH").unwrap_or_default();
        let joined_path = std::env::join_paths(
            std::iter::once(shim_root.clone()).chain(std::env::split_paths(&previous_path)),
        )
        .expect("join shim PATH");
        let _path = EnvVarGuard::set("PATH", joined_path);
        let _log = EnvVarGuard::set("TEAM_AGENT_PI_TMUX_LOG", &shim_log);
        let _socket = EnvVarGuard::set("TEAM_AGENT_PI_OWNING_SOCKET", live_socket);
        let _session = EnvVarGuard::set("TEAM_AGENT_PI_OWNING_SESSION", live_session);
        let _workspace = EnvVarGuard::set("TEAM_AGENT_PI_RUN_WORKSPACE", &live);

        let tools = TeamOrchestratorTools::with_identity(
            &live,
            Some(AgentId::new("leader")),
            Some(TeamKey::new("current")),
        );
        let result = tools
            .add_agent("mate", "roles/mate.md")
            .expect("public MCP add must use the nested owning receiver");
        assert_eq!(
            result.fields.get("ok").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        let tmux_argv = std::fs::read_to_string(&shim_log).expect("read tmux shim log");
        assert!(
            tmux_argv.contains(&format!("-S {live_socket}")),
            "public add must address the selected owning socket: {tmux_argv}"
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
