use std::path::Path;

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
        session_dir: Path::new("/workspace/.team/runtime/pi/team-a/worker-a/sessions"),
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

#[test]
fn pi_command_is_exact_regular_tui_and_has_no_ambient_flags() {
    let fresh = build_pi_command_argv(request(PiSessionSelector::Fresh {
        session_id: "da8c3622-2378-4d05-a26c-e826a6ef6d63",
    }))
    .expect("fresh Pi command");
    assert_eq!(
        fresh,
        [
            "/verified/pi",
            "--no-extensions",
            "-e",
            "/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts",
            "--no-approve",
            "--no-context-files",
            "--no-skills",
            "--no-prompt-templates",
            "--tui-mode",
            "regular",
            "--model",
            "team-agent/qwen3.8-27b",
            "--thinking",
            "high",
            "--system-prompt",
            "frozen worker prompt",
            "--tools",
            "bash,edit,mcp,read,write",
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
        ] {
            assert!(
                !argv.iter().any(|arg| arg == forbidden),
                "materializer-owned Pi argv must not inherit {forbidden}: {argv:?}"
            );
        }
    }
}

#[test]
fn pi_command_uses_no_context_no_extensions_no_approve() {
    let argv = build_pi_command_argv(request(PiSessionSelector::Fresh {
        session_id: "76f682b1-ecc3-4586-850f-ab2e2bb04cb3",
    }))
    .expect("fresh Pi command");

    for required in [
        "--no-context-files",
        "--no-extensions",
        "--no-approve",
        "--no-skills",
        "--no-prompt-templates",
    ] {
        assert_eq!(
            argv.iter().filter(|arg| arg.as_str() == required).count(),
            1,
            "required isolation flag must occur once: {required}; argv={argv:?}"
        );
    }
    assert!(argv.windows(2).any(|pair| {
        pair == [
            "-e",
            "/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts",
        ]
    }));
    assert_eq!(argv.iter().filter(|arg| arg.as_str() == "-e").count(), 1);
    assert!(!argv.iter().any(|arg| arg == "--approve"));
}
