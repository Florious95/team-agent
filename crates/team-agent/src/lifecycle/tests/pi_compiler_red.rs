use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::compiler::{compile_role_agent, CompiledRole};
use crate::lifecycle::launch::pi_mcp::{parse_pi_list_models_table, select_exact_pi_model};
use crate::model::enums::ProviderEffort;
use crate::model::yaml::Value;
use crate::model::ModelError;
use crate::provider::adapters::pi::{
    build_pi_command_argv, pi_tool_mapping, PiCommandRequest, PiSessionSelector, PiToolMapping,
};

const CATALOG: &[u8] = include_bytes!("fixtures/pi_list_models_g0.stdout.txt");
static NEXT_ROLE: AtomicU32 = AtomicU32::new(0);

fn compile_pi_role(front_matter: &str) -> Result<CompiledRole, ModelError> {
    compile_pi_role_with_team(front_matter, &Value::Map(Vec::new()))
}

fn compile_pi_role_with_team(
    front_matter: &str,
    team_meta: &Value,
) -> Result<CompiledRole, ModelError> {
    let seq = NEXT_ROLE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "team-agent-pi-compiler-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create role fixture root");
    let role = root.join("pi-worker.md");
    std::fs::write(
        &role,
        format!("---\n{front_matter}\n---\nPi worker contract.\n"),
    )
    .expect("write role fixture");
    let result = compile_role_agent(&role, team_meta, "/workspace");
    std::fs::remove_dir_all(root).expect("remove role fixture root");
    result
}

fn valid_role_with(extra: &str) -> String {
    format!(
        "name: pi-worker\nrole: developer\nprovider: pi\nmodel: team-agent/qwen3.8-27b\nauth_mode: subscription\neffort: medium\ntools:\n  - mcp_team\n  - fs_read\n  - fs_list\n  - fs_write\n  - execute_bash\ndangerously_skip_permissions: true\n{extra}"
    )
}

fn compile_error(result: Result<CompiledRole, ModelError>) -> ModelError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("incomplete Pi role must refuse"),
    }
}

fn command(tools: &[&str], effort: ProviderEffort, model: &str) -> Result<Vec<String>, String> {
    build_pi_command_argv(PiCommandRequest {
        executable: Path::new("/verified/pi"),
        extension: Path::new("/workspace/.team/runtime/pi/t1/pi-worker/team-mcp.ts"),
        model: Some(model),
        effort: Some(effort),
        system_prompt: "Pi worker contract.",
        tool_categories: tools,
        session_dir: Path::new("/workspace/.team/runtime/pi/t1/pi-worker/sessions"),
        session: PiSessionSelector::Fresh {
            session_id: "c5ddf218-24a5-4e74-960c-ab6606ea7e8c",
        },
        agent_id: "pi-worker",
    })
    .map_err(|error| error.to_string())
}

#[test]
fn pi_role_requires_mcp_team_and_preserves_provider_defaults() {
    let positive = compile_pi_role(&valid_role_with(""));
    assert!(positive.is_ok(), "fully explicit Pi role must compile");

    let missing_mcp = valid_role_with("").replace("  - mcp_team\n", "");
    let unqualified =
        valid_role_with("").replace("model: team-agent/qwen3.8-27b", "model: qwen3.8-27b");

    for (label, role) in [
        ("mcp_team", missing_mcp),
        ("qualified exact model", unqualified),
    ] {
        let error = compile_error(compile_pi_role(&role));
        assert!(
            error.to_string().to_ascii_lowercase().contains(label),
            "error must name {label}; got {error}"
        );
    }

    let defaults = valid_role_with("")
        .replace("model: team-agent/qwen3.8-27b\n", "")
        .replace("effort: medium\n", "");
    let team_meta = Value::Map(vec![
        (
            "provider_models".to_string(),
            Value::Map(vec![(
                "pi".to_string(),
                Value::Str("team-default/forced-model".to_string()),
            )]),
        ),
        (
            "default_model".to_string(),
            Value::Str("global-default/forced-model".to_string()),
        ),
        (
            "provider_effort".to_string(),
            Value::Str("high".to_string()),
        ),
    ]);
    let compiled = compile_pi_role_with_team(&defaults, &team_meta)
        .expect("Pi model and effort may use provider defaults");
    assert_eq!(
        compiled.agent.get("model"),
        Some(&Value::Null),
        "omitted Pi model must not inherit a synthesized framework default"
    );
    assert!(
        compiled.agent.get("effort").is_none(),
        "omitted Pi effort must not inherit a synthesized framework default"
    );

    let models = parse_pi_list_models_table(CATALOG).expect("catalog fixture");
    assert!(select_exact_pi_model(&models, "team-agent/qwen3.8-27b").is_ok());
    assert!(select_exact_pi_model(&models, "foo/bar").is_err());
}

#[test]
fn pi_role_uses_standard_bypass_field_without_fabricating_argv() {
    let compiled = compile_pi_role(&valid_role_with("")).expect("true acknowledgement");
    assert_eq!(
        compiled.agent.get("dangerously_skip_permissions"),
        Some(&Value::Bool(true))
    );

    let argv = command(
        &["mcp_team"],
        ProviderEffort::Medium,
        "team-agent/qwen3.8-27b",
    )
    .expect("true acknowledgement maps to intrinsic unrestricted mode");
    for forbidden in [
        "--approve",
        "--always-approve",
        "--force",
        "--trust",
        "--sandbox",
        "dangerous_auto_approve",
    ] {
        assert!(
            !argv.iter().any(|arg| arg == forbidden),
            "Pi acknowledgement must not fabricate a bypass flag {forbidden}: {argv:?}"
        );
    }

    let false_ack = valid_role_with("").replace(
        "dangerously_skip_permissions: true",
        "dangerously_skip_permissions: false",
    );
    let compiled = compile_pi_role(&false_ack).expect("standard false bypass value");
    assert_eq!(
        compiled.agent.get("dangerously_skip_permissions"),
        Some(&Value::Bool(false))
    );

    let missing_ack = valid_role_with("").replace("dangerously_skip_permissions: true\n", "");
    let text = compile_error(compile_pi_role(&missing_ack)).to_string();
    assert!(
        text.contains("missing front matter field dangerously_skip_permissions"),
        "got {text}"
    );
}

#[test]
fn pi_max_effort_is_supported_without_model_suffix() {
    let max_role = valid_role_with("").replace("effort: medium", "effort: max");
    compile_pi_role(&max_role).expect("Pi supports explicit max effort");

    let argv =
        command(&["mcp_team"], ProviderEffort::Max, "team-agent/qwen3.8-27b").expect("max Pi argv");
    assert!(
        argv.windows(2).any(|pair| pair == ["--thinking", "max"]),
        "max must map to --thinking max: {argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--model", "team-agent/qwen3.8-27b"]),
        "model must remain exact and unsuffixed: {argv:?}"
    );
    assert!(!argv.iter().any(|arg| arg.ends_with(":max")));

    let models = parse_pi_list_models_table(CATALOG).expect("catalog fixture");
    assert!(select_exact_pi_model(&models, "team-agent/qwen3.8-27b:max").is_err());
}

#[test]
fn pi_tools_allowlist_keeps_mcp_and_rejects_unknown_categories() {
    assert_eq!(pi_tool_mapping("mcp_team"), PiToolMapping::Mcp);
    assert_eq!(
        pi_tool_mapping("fs_read"),
        PiToolMapping::Builtin(&["read"])
    );
    assert_eq!(
        pi_tool_mapping("fs_list"),
        PiToolMapping::Builtin(&["grep", "find", "ls"])
    );
    assert_eq!(
        pi_tool_mapping("fs_write"),
        PiToolMapping::Builtin(&["edit", "write"])
    );
    assert_eq!(
        pi_tool_mapping("execute_bash"),
        PiToolMapping::Builtin(&["bash"])
    );

    for unsupported in ["git_diff", "network", "provider_builtin", "not_a_tool"] {
        assert_eq!(
            pi_tool_mapping(unsupported),
            PiToolMapping::Unsupported,
            "unknown categories must not silently broaden to bash"
        );
        assert!(
            command(
                &["mcp_team", unsupported],
                ProviderEffort::Medium,
                "team-agent/qwen3.8-27b"
            )
            .is_err(),
            "unsupported tool must refuse the command: {unsupported}"
        );
    }

    let argv = command(
        &["mcp_team", "fs_write", "fs_read", "fs_list", "execute_bash"],
        ProviderEffort::Medium,
        "team-agent/qwen3.8-27b",
    )
    .expect("known allowlist");
    let tools = argv
        .windows(2)
        .find(|pair| pair[0] == "--tools")
        .map(|pair| pair[1].as_str())
        .expect("--tools value");
    assert_eq!(
        tools, "bash,edit,find,grep,ls,mcp,read,write",
        "tools must be sorted and deduplicated"
    );
}
