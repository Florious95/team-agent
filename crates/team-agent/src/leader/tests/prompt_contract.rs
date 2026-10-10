//! Process-isolated launch planning, not native provider inference.
#![cfg(unix)]
#[path = "../../../tests/support/hermetic.rs"]
mod hermetic;
use crate::leader::{LeaderError, LeaderStartMode};
use crate::lifecycle::launch::pi_mcp::{materialize_pi_plan, PiMaterializeRequest, PiSessionScope};
use crate::provider::{McpConfig, Provider};
use hermetic::HermeticTestEnv;
use serde_json::json;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const SENTINEL: &str = "LEADER_ONLY_309_SENTINEL_🙂";
const CHILD: &str = "TEAM_AGENT_TEST_LEADER_PROMPT_CHILD";
fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        body();
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("leader::tests::prompt_contract::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated test failed: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn native_bin(h: &HermeticTestEnv) -> PathBuf {
    let bin = h.workspace("bin");
    for name in ["claude", "codex", "grok", "agent", "copilot", "tmux"] {
        let path = bin.join(name);
        let source = if name == "tmux" {
            "#!/bin/sh\nif [ \"$1\" = -V ]; then echo 'tmux 3.3'; exit 0; fi\nexit 1\n"
        } else {
            "#!/bin/sh\nexit 77\n"
        };
        fs::write(&path, source).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let real = h.root().join("pi-real");
    fs::write(&real, "#!/bin/sh\ncase \"$1\" in\n--version) echo '1.0.0';;\n--list-models) printf 'provider model\\nteam-agent test-model\\n';;\n*) exit 77;;\nesac\n").unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(real, bin.join("pi")).unwrap();
    bin
}
fn plan(
    provider: Provider,
    workspace: &Path,
    args: &[String],
    attach: bool,
) -> Result<crate::leader::LeaderStartPlan, LeaderError> {
    crate::leader::start::leader_start_plan_after_ambient_authority_check(
        provider, args, workspace, attach, attach, None, true,
    )
}
fn prompt_text(argv: &[String], flag: &str) -> String {
    let index = argv.iter().position(|arg| arg == flag).unwrap();
    argv[index + 1].clone()
}
fn stable_argv(plan: &crate::leader::LeaderStartPlan) -> (Vec<String>, Vec<String>) {
    // Fresh Pi session UUIDs are intentionally regenerated; all other bytes must match.
    let normalize = |argv: &[String]| {
        argv.iter()
            .map(|arg| {
                if let Some(index) = plan
                    .provider_argv
                    .iter()
                    .position(|token| token == "--session-id")
                {
                    arg.replace(&plan.provider_argv[index + 1], "<fresh-session-id>")
                } else {
                    arg.clone()
                }
            })
            .collect()
    };
    (normalize(&plan.argv), normalize(&plan.provider_argv))
}
fn assert_no_payload_tree(path: &Path) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_no_payload_tree(&path);
        } else if let Ok(bytes) = fs::read(&path) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(SENTINEL),
                "payload persisted outside global file: {}",
                path.display()
            );
        }
    }
}
#[test]
fn leader_prompt_plans_preserve_absent_baseline_append_only_leaders_and_never_materialize_into_workers(
) {
    isolated("leader_prompt_plans_preserve_absent_baseline_append_only_leaders_and_never_materialize_into_workers", || {
        let h = HermeticTestEnv::enter("leader-prompt-plans");
        let bin = native_bin(&h);
        let _path = h.with_env("PATH", bin.to_str().unwrap());
        let _routing = h.with_env("TEAM_AGENT_CLI_ARGV_ROUTING", "off");
        let config = crate::leader::prompt::config_path().unwrap();
        let providers = [Provider::Pi, Provider::Claude, Provider::ClaudeCode, Provider::Codex,
            Provider::Grok, Provider::CursorAgent, Provider::Copilot];
        for provider in providers {
            let workspace = h.workspace(&format!("provider-{}", crate::provider::wire::provider_wire(provider)));
            let baseline = plan(provider, &workspace, &[], false).unwrap();
            assert!(baseline.leader_prompt.is_none());
            fs::create_dir_all(config.parent().unwrap()).unwrap(); fs::write(&config, []).unwrap();
            let empty = plan(provider, &workspace, &[], false).unwrap();
            assert_eq!(stable_argv(&empty), stable_argv(&baseline), "empty configuration changed argv for {provider:?}");
            assert_eq!(empty.leader_env, baseline.leader_env);
            fs::write(&config, SENTINEL).unwrap();
            if matches!(provider, Provider::CursorAgent | Provider::Copilot) {
                let LeaderError::Prompt(error) = plan(provider, &workspace, &[], false).unwrap_err() else { panic!("typed unsupported") };
                assert_eq!(error.reason, "leader_prompt_unsupported");
            } else {
                let configured = plan(provider, &workspace, &[], false).unwrap();
                assert_eq!(configured.mode, baseline.mode);
                assert_eq!(configured.leader_env, baseline.leader_env);
                assert!(configured.leader_prompt.is_some());
                assert!(configured.provider_argv.iter().any(|arg| arg.contains(SENTINEL)));
                if provider == Provider::Pi {
                    assert_eq!(prompt_text(&configured.provider_argv, "--append-system-prompt"),
                        format!("{}\n\n{SENTINEL}", prompt_text(&baseline.provider_argv, "--append-system-prompt")));
                }
                // Only the process argv carries the global payload, never the wrapper/state/env.
                assert_no_payload_tree(&workspace);
            }
            fs::remove_file(&config).unwrap();
        }
        fs::write(&config, SENTINEL).unwrap();
        let workspace = h.workspace("worker");
        let candidate = h.root().join("candidate"); fs::write(&candidate, "candidate").unwrap();
        let mcp = McpConfig { raw: json!({"team_orchestrator": {
            "command": candidate, "args": ["mcp-server"], "cwd": workspace,
            "env": {"TEAM_AGENT_ID": "worker", "TEAM_AGENT_OWNER_TEAM_ID": "current"}
        }}) };
        for body in [SENTINEL.as_bytes(), &[0xff][..]] {
            fs::write(&config, body).unwrap();
            let worker = materialize_pi_plan(PiMaterializeRequest {
                workspace: &workspace, team_id: "current", agent_id: "worker", model: None,
                effort: None, system_prompt: "Compiled Worker contract; unchanged", team_mcp_tools: &["report_result"],
                mcp_config: &mcp, session_scope: PiSessionScope::Isolated,
            }).unwrap();
            assert!(!worker.argv.iter().any(|arg| arg.contains(SENTINEL)));
            assert!(!mcp.raw.to_string().contains(SENTINEL));
            assert_no_payload_tree(&workspace);
        }
    });
}
#[test]
fn leader_prompt_attach_skips_corrupt_configuration_and_all_injection_for_every_leader_entry() {
    isolated(
        "leader_prompt_attach_skips_corrupt_configuration_and_all_injection_for_every_leader_entry",
        || {
            let h = HermeticTestEnv::enter("leader-prompt-attach");
            let bin = native_bin(&h);
            let _path = h.with_env("PATH", bin.to_str().unwrap());
            let _routing = h.with_env("TEAM_AGENT_CLI_ARGV_ROUTING", "off");
            let config = crate::leader::prompt::config_path().unwrap();
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            for provider in [
                Provider::Pi,
                Provider::Claude,
                Provider::ClaudeCode,
                Provider::Codex,
                Provider::Grok,
                Provider::CursorAgent,
                Provider::Copilot,
            ] {
                let workspace = h.workspace(&format!("attach-{provider:?}"));
                fs::write(&config, []).unwrap();
                let baseline = plan(provider, &workspace, &[], true).unwrap();
                for body in [SENTINEL.as_bytes(), &[0xff][..]] {
                    fs::write(&config, body).unwrap();
                    let attached = plan(provider, &workspace, &[], true).unwrap();
                    assert_eq!(attached.mode, LeaderStartMode::AttachExisting);
                    assert!(attached.leader_prompt.is_none());
                    assert_eq!(attached.argv, baseline.argv);
                    assert_eq!(attached.provider_argv, baseline.provider_argv);
                    assert_eq!(attached.leader_env, baseline.leader_env);
                    assert!(!workspace.join(".team").exists());
                }
            }
        },
    );
}
#[test]
fn leader_prompt_routes_are_merged_or_rejected_before_pi_artifacts() {
    isolated(
        "leader_prompt_routes_are_merged_or_rejected_before_pi_artifacts",
        || {
            let h = HermeticTestEnv::enter("leader-prompt-routes");
            let bin = native_bin(&h);
            let _path = h.with_env("PATH", bin.to_str().unwrap());
            let _routing = h.with_env("TEAM_AGENT_CLI_ARGV_ROUTING", "on");
            let config = crate::leader::prompt::config_path().unwrap();
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            fs::write(&config, SENTINEL).unwrap();
            let route = crate::provider::argv_route::config_path().unwrap();
            fs::write(
                &route,
                json!({"schema_version":1,"enabled":true,"providers":{
                    "claude":["--append-system-prompt=from route"],
                    "codex":["-c", "developer_instructions=\"from route\""],
                    "grok":["--rules", "from route"]
                }})
                .to_string(),
            )
            .unwrap();
            for provider in [Provider::Claude, Provider::Codex, Provider::Grok] {
                let workspace = h.workspace(&format!("route-{provider:?}"));
                let argv = plan(provider, &workspace, &[], false)
                    .unwrap()
                    .provider_argv;
                assert!(argv
                    .iter()
                    .any(|arg| arg.contains("from route") && arg.contains(SENTINEL)));
                let args = match provider {
                    Provider::Codex => vec![
                        "--config".into(),
                        "developer_instructions=\"explicit\"".into(),
                    ],
                    Provider::Grok => vec!["--rules".into(), "explicit".into()],
                    _ => vec!["--append-system-prompt".into(), "explicit".into()],
                };
                let LeaderError::Prompt(error) =
                    plan(provider, &workspace, &args, false).unwrap_err()
                else {
                    panic!("typed duplicate refusal")
                };
                assert_eq!(error.reason, "leader_prompt_conflict");
                assert!(!workspace.join(".team").exists());
            }
            for tokens in [
                json!(["--append-system-prompt", "route override"]),
                json!(["--"]),
            ] {
                fs::write(
                    &route,
                    json!({"schema_version":1,"enabled":true,"providers":{"pi":tokens}})
                        .to_string(),
                )
                .unwrap();
                let workspace = h.workspace("pi-route-refusal");
                let LeaderError::Prompt(error) =
                    plan(Provider::Pi, &workspace, &[], false).unwrap_err()
                else {
                    panic!("typed Pi route refusal")
                };
                assert_eq!(error.reason, "leader_prompt_conflict");
                assert!(!workspace.join(".team").exists());
            }
        },
    );
}
