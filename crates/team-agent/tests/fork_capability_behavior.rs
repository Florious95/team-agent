//! ---
//! purpose: 钉死 fork 能力矩阵走 fork_agent_with_transport，不查表
//! contract:
//!   provides:
//!     - name: A16-capability-matrix-behavior
//!       what: 升级前遗留的 claude/codex/grok/pi worker 行在注入前即拒；未验证 provider 拒且理由是未验证
//! boundary:
//!   - 不把能力查表当通过
//!   - 基线红必须是断言失败，不是 unresolved import
//!   - 不注入 /branch 或 /fork：Claude Code / Codex / Grok / Pi 只走 contract 管道 fork 出新席
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
    // TEAM.md names the leader, which accepts only canonical provider keys.
    let leader = match provider {
        "codex_cli" | "codex-cli" => "codex",
        other => other,
    };
    std::fs::write(
        ws.join("TEAM.md"),
        format!(
            "---\nname: fx\nobjective: capability matrix.\nprovider: {leader}\nauth_mode: {auth}\n---\n"
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
fn legacy_worker_rows_are_refused_as_retired_before_any_injection() {
    // A pre-upgrade worker row has no legacy runtime to fork from; the user is
    // told to re-add the seat by its public provider name, never the leaf id.
    for (tag, provider, auth, public) in [
        ("claude-sub", "claude", "subscription", "claude"),
        ("claude-compat", "claude", "compatible_api", "claude"),
        ("pi-sub", "pi", "subscription", "pi"),
        ("codex-sub", "codex", "subscription", "codex"),
        ("codex_cli-sub", "codex_cli", "subscription", "codex"),
        ("codex-cli-sub", "codex-cli", "subscription", "codex"),
        ("grok-sub", "grok", "subscription", "grok"),
        ("grok-compat", "grok", "compatible_api", "grok"),
    ] {
        let ws = seat_ws(tag, provider, auth);
        let (result, injected) = fork_on(&ws, "Branched conversation. (forked from old-id)\n");
        let text = result.expect_err("a retired worker row must refuse").to_string();
        assert!(
            text.contains(&format!(
                "legacy {public} seat has no runtime in this release; stop or remove it, then add it again (provider: {public})"
            )),
            "{tag}: refusal must name the retired seat and how to re-add it; got {text}"
        );
        assert!(
            !["claude_tui", "pi_tui", "codex_tui", "grok_build"]
                .iter()
                .any(|leaf| text.contains(leaf)),
            "{tag}: the user never edits the provider key; got {text}"
        );
        assert!(
            !injected.iter().any(|t| t == "/branch" || t == "/fork"),
            "{tag}: a retired row must not inject a slash command; got {injected:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[test]
fn unverified_providers_are_refused_as_unverified_not_unsupported() {
    for provider in ["copilot", "gemini_cli"] {
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
