//! ---
//! purpose: 钉死 fork 能力矩阵走 fork_agent_with_transport，不查表
//! contract:
//!   provides:
//!     - name: A16-capability-matrix-behavior
//!       what: grok subscription 真注入斜杠命令；Claude 同名就地分身拒并要求新席位；compatible_api 拒；未验证 provider 拒且理由是未验证
//! boundary:
//!   - 不把 in_window_fork_command 查表当通过
//!   - 基线红必须是断言失败，不是 unresolved import
//!   - Claude 不再注入 /branch：它会把源席自身切到新分支且不产生新席
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use team_agent::lifecycle::launch::fork_agent_with_transport;
use team_agent::model::ids::AgentId;
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn seat_ws(tag: &str, provider: &str, auth: &str) -> std::path::PathBuf {
    let ws = std::env::temp_dir().join(format!(
        "ta-fork-cap-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        format!(
            "---\nname: fx\nobjective: capability matrix.\nprovider: {provider}\nauth_mode: {auth}\n---\n"
        ),
    )
    .unwrap();
    let profile = if auth == "subscription" {
        String::new()
    } else {
        "profile: local\n".to_string()
    };
    std::fs::write(
        ws.join("agents").join("seer.md"),
        format!(
            "---\nname: seer\nrole: Seer\nprovider: {provider}\nmodel: x\nauth_mode: {auth}\n{profile}dangerously_skip_permissions: true\ntools:\n  - provider_builtin\n---\nseer.\n"
        ),
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&ws).expect("compile");
    std::fs::write(ws.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-fx",
            "team_key": "fx",
            "agents": {
                "seer": {
                    "status": "running",
                    "provider": provider,
                    "auth_mode": auth,
                    "window": "seer",
                    "pane_id": "%7"
                }
            }
        }),
    )
    .unwrap();
    ws
}

fn fork_on(
    ws: &std::path::Path,
    screen: &str,
) -> (
    Result<team_agent::lifecycle::ForkAgentReport, team_agent::lifecycle::LifecycleError>,
    Vec<String>,
) {
    let buffers = Arc::new(Mutex::new(Vec::new()));
    struct Rec {
        screen: String,
        buffers: Arc<Mutex<Vec<String>>>,
    }
    impl CommandRunner for Rec {
        fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
            if argv.iter().any(|a| a == "set-buffer") {
                if let Some(text) = argv.last() {
                    self.buffers.lock().unwrap().push(text.clone());
                }
            }
            if argv.iter().any(|a| a == "capture-pane") {
                return Ok(ok(&self.screen));
            }
            Ok(ok(""))
        }
    }
    let tx = TmuxBackend::with_runner(Box::new(Rec {
        screen: screen.to_string(),
        buffers: buffers.clone(),
    }));
    let result = fork_agent_with_transport(
        ws,
        &AgentId::new("seer"),
        &AgentId::new("seer"),
        None,
        false,
        None,
        &tx,
    );
    let injected = buffers.lock().unwrap().clone();
    (result, injected)
}

#[test]
fn grok_subscription_injects_slash_fork_and_accepts_forked_from() {
    let ws = seat_ws("grok-sub", "grok", "subscription");
    let (result, injected) = fork_on(&ws, "Session new (forked from old-id)\n");
    assert!(result.is_ok(), "grok subscription must take in-window path: {result:?}");
    assert!(
        injected.iter().any(|t| t == "/fork"),
        "must inject official /fork via set-buffer, got {injected:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn legacy_claude_subscription_refuses_in_place_branch_and_asks_for_a_new_seat() {
    // `/branch` moved the source process onto the branch while its seat kept the
    // old session tuple, and created no seat. Claude forks only into a new seat.
    for (tag, screen) in [
        ("claude-sub", "Branched conversation. You are now in the new branch (session abc).\n"),
        ("claude-empty", "Failed to branch conversation: No conversation to branch\n"),
    ] {
        let ws = seat_ws(tag, "claude", "subscription");
        let (result, injected) = fork_on(&ws, screen);
        let text = result.expect_err("in-place Claude fork must refuse").to_string();
        assert!(text.contains("--as <new agent id>"), "refusal must ask for a new seat; got {text}");
        assert!(!text.contains("claude_tui"), "the user never edits the provider key; got {text}");
        assert!(
            !injected.iter().any(|t| t == "/branch" || t == "/fork"),
            "legacy Claude must not inject a slash command; got {injected:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[test]
fn claude_compatible_api_is_refused_as_unsupported_auth() {
    let ws = seat_ws("claude-compat", "claude", "compatible_api");
    let (result, injected) = fork_on(&ws, "Branched conversation.\n");
    let err = result.expect_err("compatible_api must refuse");
    let text = err.to_string();
    assert!(
        text.contains("does not support"),
        "auth_mode gate keeps the support wording; got {text}"
    );
    assert!(
        !injected.iter().any(|t| t == "/branch" || t == "/fork"),
        "compatible_api must not inject a slash command; got {injected:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn unverified_providers_are_refused_as_unverified_not_unsupported() {
    for provider in ["codex", "copilot", "gemini_cli"] {
        let ws = seat_ws(provider, provider, "subscription");
        let (result, injected) = fork_on(&ws, "forked from\n");
        let err = match result {
            Err(error) => error,
            Ok(ok) => panic!("{provider} must refuse (unverified), got {ok:?}"),
        };
        assert!(
            !injected.iter().any(|t| t == "/fork" || t == "/branch"),
            "{provider} must not inject a slash command; got {injected:?}"
        );
        let text = err.to_string();
        assert!(
            text.contains("未验证") || text.to_lowercase().contains("unverified"),
            "{provider} must be refused as unverified, not 'does not support'; got {text}"
        );
        assert!(
            !text.contains("does not support native session fork"),
            "{provider} must not reuse the support wording (leads to 'switch provider'); got {text}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
