//! ---
//! purpose: 钉死 fork 能力矩阵走 fork_agent_with_transport，不查表
//! contract:
//!   provides:
//!     - name: A16-capability-matrix-behavior
//!       what: grok/claude subscription 真注入斜杠命令；compatible_api 拒；未验证 provider 拒且理由是未验证
//! boundary:
//!   - 不把 in_window_fork_command 查表当通过
//!   - 基线红必须是断言失败，不是 unresolved import
//!   - claude 屏幕标记来自 2026-08-17 真机 v2.1.181，不猜 grok 的 forked from
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use team_agent::lifecycle::launch::fork_agent_with_transport;
use team_agent::model::ids::AgentId;
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};

/// Empirically captured 2026-08-17 on Claude Code v2.1.181 after `/branch`.
const CLAUDE_BRANCH_MARK: &str = "Branched conversation";

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
fn claude_subscription_injects_branch_and_accepts_branched_conversation() {
    let ws = seat_ws("claude-sub", "claude", "subscription");
    let (result, injected) = fork_on(
        &ws,
        "Branched conversation. You are now in the new branch (session abc).\n",
    );
    assert!(
        result.is_ok(),
        "claude subscription must take the same in-window path: {result:?}"
    );
    assert!(
        injected.iter().any(|t| t == "/branch"),
        "claude v2.1.181 session-split command is /branch (not grok /fork); got {injected:?}"
    );
    assert_eq!(
        result.as_ref().unwrap().backing_state,
        team_agent::lifecycle::ForkBackingState::Verified,
        "mark {CLAUDE_BRANCH_MARK:?} must verify"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn claude_empty_conversation_is_explicit_refuse_not_ok() {
    let ws = seat_ws("claude-empty", "claude", "subscription");
    let (result, injected) = fork_on(
        &ws,
        "Failed to branch conversation: No conversation to branch\n",
    );
    assert!(
        injected.iter().any(|t| t == "/branch"),
        "empty conversation still injects /branch; got {injected:?}"
    );
    let err = result.expect_err("empty conversation must not look like success");
    let text = err.to_string();
    assert!(
        text.contains("Failed to branch conversation: No conversation to branch"),
        "must surface the screen refusal verbatim; got {text}"
    );
    let _ = std::fs::remove_dir_all(&ws);
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
    for provider in ["codex", "copilot", "cursor_agent", "gemini_cli"] {
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
