use std::path::Path;

use crate::lifecycle::launch::pi_mcp::{pi_leader_session_scope, PiSessionScope};
use crate::model::enums::ProviderEffort;
use crate::provider::adapters::pi::{build_pi_command_argv, PiCommandRequest, PiSessionSelector};

fn request(session: PiSessionSelector<'static>) -> PiCommandRequest<'static> {
    PiCommandRequest {
        executable: Path::new("/verified/pi"),
        extension: Path::new("/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts"),
        model: Some("team-agent/qwen3.8-27b"),
        effort: Some(ProviderEffort::High),
        system_prompt: "frozen worker prompt",
        tool_categories: &["mcp_team", "fs_read", "fs_write", "execute_bash"],
        session_dir: Some(Path::new(
            "/workspace/.team/runtime/pi/team-a/worker-a/sessions",
        )),
        session,
        agent_id: "worker-a",
    }
}

#[test]
fn pi_command_omits_unset_model_and_thinking_flags() {
    let mut request = request(PiSessionSelector::Fresh {
        session_id: "a2320d4e-7b3a-44ae-b3b8-d5de57033b01",
    });
    request.model = None;
    request.effort = None;
    let argv = build_pi_command_argv(request).expect("Pi provider defaults");

    assert!(!argv.iter().any(|arg| arg == "--model"), "argv={argv:?}");
    assert!(!argv.iter().any(|arg| arg == "--thinking"), "argv={argv:?}");
}

fn leader_request() -> PiCommandRequest<'static> {
    PiCommandRequest {
        executable: Path::new("/verified/pi"),
        extension: Path::new("/workspace/.team/runtime/pi/current/leader/team-mcp.ts"),
        model: None,
        effort: None,
        system_prompt: "frozen leader prompt",
        tool_categories: &["mcp_team", "fs_read", "fs_write", "execute_bash"],
        session_dir: None,
        session: PiSessionSelector::Fresh {
            session_id: "da8c3622-2378-4d05-a26c-e826a6ef6d63",
        },
        agent_id: "leader",
    }
}

#[test]
fn pi_interactive_leader_argv_preserves_native_session_discovery() {
    let argv = build_pi_command_argv(leader_request()).expect("native Pi leader command");

    assert_eq!(
        argv,
        [
            "/verified/pi",
            "-e",
            "/workspace/.team/runtime/pi/current/leader/team-mcp.ts",
            "--append-system-prompt",
            "frozen leader prompt",
            "--session-id",
            "da8c3622-2378-4d05-a26c-e826a6ef6d63",
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
    let fresh = build_pi_command_argv(request(PiSessionSelector::Fresh {
        session_id: "da8c3622-2378-4d05-a26c-e826a6ef6d63",
    }))
    .expect("fresh Pi command");
    assert_eq!(
        fresh,
        [
            "/verified/pi",
            "-e",
            "/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts",
            "--model",
            "team-agent/qwen3.8-27b",
            "--thinking",
            "high",
            "--append-system-prompt",
            "frozen worker prompt",
            "--session-dir",
            "/workspace/.team/runtime/pi/team-a/worker-a/sessions",
            "--session-id",
            "da8c3622-2378-4d05-a26c-e826a6ef6d63",
            "--name",
            "worker-a",
        ]
        .map(str::to_string)
    );

    let resume = build_pi_command_argv(request(PiSessionSelector::Resume {
        path: Path::new(
            "/workspace/.team/runtime/pi/team-a/worker-a/sessions/2026/08/session.jsonl",
        ),
    }))
    .expect("resume Pi command");
    assert!(resume.windows(2).any(|pair| {
        pair == [
            "--session",
            "/workspace/.team/runtime/pi/team-a/worker-a/sessions/2026/08/session.jsonl",
        ]
    }));
    assert!(!resume.iter().any(|arg| arg == "--session-id"));

    for argv in [&fresh, &resume] {
        for forbidden in [
            "rpc",
            "--mode",
            "--print",
            "-p",
            "-r",
            "--resume",
            "-c",
            "--continue",
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
                !argv.iter().any(|arg| arg == forbidden),
                "materializer-owned Pi argv must not inherit {forbidden}: {argv:?}"
            );
        }
    }
}

#[test]
fn pi_command_keeps_direct_plugins_skills_context_auth_and_tools() {
    let argv = build_pi_command_argv(request(PiSessionSelector::Fresh {
        session_id: "76f682b1-ecc3-4586-850f-ab2e2bb04cb3",
    }))
    .expect("fresh Pi command");

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
            "TeamMate Pi must preserve the direct Pi default instead of adding {forbidden}; argv={argv:?}"
        );
    }
    assert!(argv.windows(2).any(|pair| {
        pair == [
            "-e",
            "/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts",
        ]
    }));
    assert_eq!(argv.iter().filter(|arg| arg.as_str() == "-e").count(), 1);
    assert!(argv
        .windows(2)
        .any(|pair| pair == ["--append-system-prompt", "frozen worker prompt"]));
}
