use crate::cli::spec::{CommandKind, COMMAND_SPECS};
use crate::compiler::compile_role_agent;
use crate::lifecycle::launch::pi_mcp::parse_pi_leader_args;
use crate::model::enums::{AuthMode, Provider, ProviderEffort};
use crate::model::yaml::Value;
use crate::provider::{get_adapter, ProviderCommandContext};

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

    let shared_source = include_str!("../launch/pi_mcp.rs");
    let leader_source = include_str!("../../leader/start.rs");
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
}
