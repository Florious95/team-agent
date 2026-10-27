use std::path::Path;

use crate::lifecycle::launch::pi_mcp::{
    build_pi_command_argv, pi_leader_session_scope, PiCommandRequest, PiSessionScope,
};
use crate::model::enums::ProviderEffort;

fn isolated_leader_request() -> PiCommandRequest<'static> {
    PiCommandRequest {
        extension: Path::new("/workspace/.team/runtime/pi/team-a/leader/team-mcp.ts"),
        model: Some("team-agent/qwen3.8-27b"),
        effort: Some(ProviderEffort::High),
        system_prompt: "frozen leader prompt",
        session_dir: Some(Path::new(
            "/workspace/.team/runtime/pi/team-a/leader/sessions",
        )),
        agent_id: "leader",
    }
}

#[test]
fn pi_command_omits_unset_model_and_thinking_flags() {
    let mut request = isolated_leader_request();
    request.model = None;
    request.effort = None;
    let argv = build_pi_command_argv(request).expect("Pi provider defaults");

    assert!(!argv.iter().any(|arg| arg == "--model"), "argv={argv:?}");
    assert!(!argv.iter().any(|arg| arg == "--thinking"), "argv={argv:?}");
}

fn leader_request() -> PiCommandRequest<'static> {
    PiCommandRequest {
        extension: Path::new("/workspace/.team/runtime/pi/current/leader/team-mcp.ts"),
        model: None,
        effort: None,
        system_prompt: "frozen leader prompt",
        session_dir: None,
        agent_id: "leader",
    }
}

#[test]
fn pi_interactive_leader_argv_preserves_native_session_discovery() {
    let argv = build_pi_command_argv(leader_request()).expect("native Pi leader command");

    assert_eq!(
        argv,
        [
            "pi",
            "-e",
            "/workspace/.team/runtime/pi/current/leader/team-mcp.ts",
            "--append-system-prompt",
            "frozen leader prompt",
            "--name",
            "leader",
        ]
        .map(str::to_string)
    );
    assert!(!argv.iter().any(|arg| arg == "--session-dir"));
    for forbidden in ["--cwd", "--workspace", "--session"] {
        assert!(!argv.iter().any(|arg| arg == forbidden), "argv={argv:?}");
    }
}

#[test]
fn pi_leader_session_scope_keeps_explicit_and_external_routes_isolated() {
    assert_eq!(
        pi_leader_session_scope(&[], false, false),
        PiSessionScope::NativeDefault
    );
    for (args, external_path, allow_nested_attach) in [
        (
            vec![
                "--model".to_string(),
                "openai-codex/gpt-5.6-sol".to_string(),
            ],
            false,
            false,
        ),
        (Vec::new(), true, false),
        (Vec::new(), false, true),
    ] {
        assert_eq!(
            pi_leader_session_scope(&args, external_path, allow_nested_attach),
            PiSessionScope::Isolated,
            "only the zero-argument managed route may use Pi's native root"
        );
    }
}

#[test]
fn pi_command_preserves_direct_pi_defaults_with_only_team_runtime_additions() {
    let fresh = build_pi_command_argv(isolated_leader_request()).expect("fresh Pi command");
    assert_eq!(
        fresh,
        [
            "pi",
            "-e",
            "/workspace/.team/runtime/pi/team-a/leader/team-mcp.ts",
            "--model",
            "team-agent/qwen3.8-27b",
            "--thinking",
            "high",
            "--append-system-prompt",
            "frozen leader prompt",
            "--session-dir",
            "/workspace/.team/runtime/pi/team-a/leader/sessions",
            "--name",
            "leader",
        ]
        .map(str::to_string)
    );

    for forbidden in [
        "rpc",
        "--mode",
        "--print",
        "-p",
        "-r",
        "--resume",
        "-c",
        "--continue",
        "--session",
        "--session-id",
        "--mcp-config",
        "--provider",
        "--fork",
        "--workspace",
        "--cwd",
        "--rules",
        "--always-approve",
        "--force",
        "--trust",
        "--sandbox",
        "--no-extensions",
        "--no-approve",
        "--no-context-files",
        "--no-skills",
        "--no-prompt-templates",
        "--tui-mode",
        "--tools",
        "--system-prompt",
    ] {
        assert!(
            !fresh.iter().any(|arg| arg == forbidden),
            "materializer-owned Pi argv must not inherit {forbidden}: {fresh:?}"
        );
    }
}

#[test]
fn pi_command_keeps_direct_plugins_skills_context_auth_and_tools() {
    let argv = build_pi_command_argv(isolated_leader_request()).expect("fresh Pi command");

    for forbidden in [
        "--no-context-files",
        "--no-extensions",
        "--no-approve",
        "--no-skills",
        "--no-prompt-templates",
        "--tools",
        "--system-prompt",
        "--tui-mode",
    ] {
        assert_eq!(
            argv.iter().filter(|arg| arg.as_str() == forbidden).count(),
            0,
            "Pi leader must preserve the direct Pi default instead of adding {forbidden}; argv={argv:?}"
        );
    }
    assert!(argv.windows(2).any(|pair| {
        pair == [
            "-e",
            "/workspace/.team/runtime/pi/team-a/leader/team-mcp.ts",
        ]
    }));
    assert_eq!(argv.iter().filter(|arg| arg.as_str() == "-e").count(), 1);
    assert!(argv
        .windows(2)
        .any(|pair| pair == ["--append-system-prompt", "frozen leader prompt"]));
}
