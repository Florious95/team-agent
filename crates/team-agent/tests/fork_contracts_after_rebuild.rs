//! ---
//! purpose: 钉死 fork 重建后不再静默吞 --as、不再留死分支
//! contract:
//!   provides:
//!     - name: A13-no-silent-shells
//!       what: --as 要么报错要么新名可寻址；Pending 死臂已删
//! boundary:
//!   - 不改 claude/codex/copilot 能力断言
//!   - 基线红必须是断言失败，不是 unresolved import
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

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

struct ForkScreenRunner {
    screen: String,
}

impl CommandRunner for ForkScreenRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        if argv.iter().any(|a| a == "capture-pane") {
            return Ok(ok(&self.screen));
        }
        Ok(ok(""))
    }
}

fn copilot_ws(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!(
        "ta-fork-contract-{}-{}",
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: fx\nobjective: fork contract.\nprovider: grok\nauth_mode: subscription\n---\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("agents").join("seer.md"),
        "---\nname: seer\nrole: Seer\nprovider: copilot\nmodel: x\nauth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - provider_builtin\n---\nseer.\n",
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
                    "provider": "copilot",
                    "auth_mode": "subscription",
                    "window": "seer",
                    "pane_id": "%7"
                }
            }
        }),
    )
    .unwrap();
    ws
}

fn backend() -> TmuxBackend {
    TmuxBackend::with_runner(Box::new(ForkScreenRunner {
        screen: "Session new (forked from old-id)\n".into(),
    }))
}

/// A legacy seat has no in-place fork any more: `--as <other>` must be refused
/// before any side effect, never accepted and reported under the source name.
#[test]
fn as_flag_is_refused_not_silently_rewritten_to_source() {
    let ws = copilot_ws("as");
    let tx = backend();
    let result = fork_agent_with_transport(
        &ws,
        &AgentId::new("seer"),
        &AgentId::new("seer2"),
        None,
        false,
        None,
        &tx,
    );
    let text = result
        .expect_err("--as on a legacy seat must be refused, not swallowed")
        .to_string();
    assert!(
        text.contains("native session fork is unverified")
            && text.contains("no legacy fork or fresh replacement was performed"),
        "refusal must say no fork or replacement happened; got {text}"
    );
    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        state.get("agents").and_then(|a| a.get("seer2")).is_none(),
        "a refused fork must not register the --as name; state={state}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// PendingContextFork else-arm is dead (the in-window fork it served was excised).
/// The token must stay gone from fork_agent.rs.
#[test]
fn pending_context_fork_dead_arm_is_gone() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/lifecycle/launch/fork_agent.rs");
    let src = std::fs::read_to_string(&path).expect("read fork_agent.rs (positive control)");
    // Positive control: the in-window "forked from" screen mark left with the
    // Grok in-window fork, so anchor on the surviving legacy entry instead.
    assert!(
        src.contains("fn fork_agent_with_transport"),
        "positive control: fork_agent.rs must still export fork_agent_with_transport"
    );
    assert!(
        !src.contains("PendingContextFork"),
        "dead else-arm ForkBackingState::PendingContextFork must be deleted from fork_agent.rs"
    );
}
