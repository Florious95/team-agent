//! Claude provider command-plan contracts for Batch A Lane 1.
//!
//! These tests intentionally consume the S0 public contract (`ProviderCommandContext`,
//! `ProviderProfileLaunch`, `CommandPlan`) rather than local helper internals. Before S0 lands they
//! compile red on the missing shared interface; after S0 they become behavior REDs for the provider
//! lane.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use serial_test::serial;
use team_agent::model::enums::{AuthMode, Provider};
use team_agent::provider::{
    get_adapter, AuthHintStatus, CaptureSessionContext, McpConfig, ProviderCommandContext,
    ProviderCommandOverrides, ProviderProfileLaunch, SessionId,
};

#[test]
fn claude_fresh_command_plan_returns_expected_uuid_and_suppresses_managed_mcp_config_flag() {
    let root = tmp_dir("fresh-managed-plan");
    let projects_root = root.join("claude-projects");
    let profile = managed_profile_launch(&root, &projects_root);
    let config = mcp_config();
    let tools = ["mcp_team"];
    let adapter = get_adapter(Provider::Claude);

    let plan = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::CompatibleApi,
            mcp_config: Some(&config),
            system_prompt: Some("You are a Team Agent worker."),
            model: Some("profile-effective-model"),
            tools: &tools,
            profile_launch: Some(&profile),
            agent_id_hint: None,
        })
        .expect("Claude compatible_api managed fresh command plan should build");

    let session_id = plan
        .expected_session_id
        .as_ref()
        .map(SessionId::as_str)
        .unwrap_or("");
    assert!(
        is_rfc4122_uuid(session_id),
        "Claude fresh CommandPlan.expected_session_id must be a non-empty RFC4122 UUID; plan={plan:?}"
    );
    assert_eq!(
        plan.provider_projects_root.as_deref(),
        Some(projects_root.as_path()),
        "Claude fresh CommandPlan must carry provider_projects_root so capture can search the managed projects tree first; plan={plan:?}"
    );
    assert!(
        plan.managed_mcp_config,
        "managed_mcp_config=true in ProviderProfileLaunch must propagate to CommandPlan; plan={plan:?}"
    );
    assert!(
        !argv_has_flag(&plan.argv, "--mcp-config"),
        "managed Claude compatible_api config writes .claude.json/settings.json; argv must not also pass --mcp-config; argv={:?}",
        plan.argv
    );
    assert!(
        argv_contains_adjacent(&plan.argv, &["--permission-mode", "default"])
            && !argv_has_flag(&plan.argv, "--dangerously-skip-permissions"),
        "default Claude plan must use --permission-mode default and must not mix in dangerous approval; argv={:?}",
        plan.argv
    );
}

#[test]
fn claude_resume_fork_dangerous_and_default_command_plans_are_mutually_exclusive() {
    let adapter = get_adapter(Provider::Claude);
    let config = mcp_config();
    let source_session = SessionId::new("11111111-2222-4333-8444-555555555555");
    let default_tools = ["mcp_team"];
    let dangerous_tools = ["dangerous_auto_approve", "mcp_team"];

    let resume = adapter
        .build_resume_command_plan(
            Some(&source_session),
            ProviderCommandContext {
                auth_mode: AuthMode::Subscription,
                mcp_config: Some(&config),
                system_prompt: None,
                model: Some("claude-sonnet-4-6"),
                tools: &default_tools,
                profile_launch: None,
                agent_id_hint: None,
            },
        )
        .expect("Claude resume plan should build");
    assert!(
        resume.expected_session_id.is_none(),
        "resume must not allocate a new expected_session_id; it resumes the persisted one; plan={resume:?}"
    );
    assert!(
        argv_contains_adjacent(&resume.argv, &["--resume", source_session.as_str()])
            && !argv_has_flag(&resume.argv, "--fork-session")
            && !argv_has_flag(&resume.argv, "--session-id"),
        "resume argv must be resume-only, not fork/fresh mixed; argv={:?}",
        resume.argv
    );

    let fork = adapter
        .fork_plan(
            Some(&source_session),
            ProviderCommandContext {
                auth_mode: AuthMode::Subscription,
                mcp_config: Some(&config),
                system_prompt: None,
                model: Some("claude-sonnet-4-6"),
                tools: &default_tools,
                profile_launch: None,
                agent_id_hint: None,
            },
        )
        .expect("Claude fork plan should build");
    let fork_session_id = fork
        .expected_session_id
        .as_ref()
        .map(SessionId::as_str)
        .unwrap_or("");
    assert!(
        is_rfc4122_uuid(fork_session_id) && fork_session_id != source_session.as_str(),
        "fork must allocate a new RFC4122 expected_session_id distinct from source; plan={fork:?}"
    );
    assert!(
        argv_contains_adjacent(&fork.argv, &["--resume", source_session.as_str(), "--fork-session"])
            && argv_has_flag(&fork.argv, "--session-id"),
        "fork argv must carry both source resume and new --session-id; argv={:?}",
        fork.argv
    );

    let dangerous = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: Some(&config),
            system_prompt: None,
            model: Some("claude-sonnet-4-6"),
            tools: &dangerous_tools,
            profile_launch: None,
                agent_id_hint: None,
        })
        .expect("Claude dangerous plan should build");
    assert!(
        argv_has_flag(&dangerous.argv, "--dangerously-skip-permissions")
            && !argv_contains_adjacent(&dangerous.argv, &["--permission-mode", "default"]),
        "dangerous approval and default permission-mode are mutually exclusive; argv={:?}",
        dangerous.argv
    );
}

#[test]
fn claude_disallowed_tools_mapping_covers_the_complete_python_tool_set() {
    let adapter = get_adapter(Provider::Claude);
    let config = mcp_config();
    let no_filesystem_tools = ["mcp_team"];

    let plan = adapter
        .build_command_plan(ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: Some(&config),
            system_prompt: None,
            model: Some("claude-sonnet-4-6"),
            tools: &no_filesystem_tools,
            profile_launch: None,
                agent_id_hint: None,
        })
        .expect("Claude plan should build with explicit tools");
    let disallowed = flag_values(&plan.argv, "--disallowedTools");

    for expected in [
        "Bash",
        "Read",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Glob",
        "Grep",
    ] {
        assert!(
            disallowed.iter().any(|value| value == expected),
            "Claude --disallowedTools must include the full Python permission mapping when a capability is not granted; missing={expected} disallowed={disallowed:?} argv={:?}",
            plan.argv
        );
    }
}

#[test]
#[serial(env)]
fn claude_capture_prefers_expected_session_id_under_provider_projects_root() {
    let root = tmp_dir("capture-expected");
    let spawn_cwd = root.join("team");
    let projects_root = root.join("managed-claude").join("projects");
    let home = root.join("home");
    std::fs::create_dir_all(&spawn_cwd).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();

    let expected = "22222222-3333-4444-9555-666666666666";
    write_claude_transcript(
        &projects_root.join("target.jsonl"),
        expected,
        &spawn_cwd,
        "target-agent-marker",
    );
    write_claude_transcript(
        &home.join(".claude").join("projects").join("distractor.jsonl"),
        "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        &spawn_cwd,
        "distractor-agent-marker",
    );

    let _guard = EnvGuard::set_path_and_home(&home);
    let context = CaptureSessionContext {
        agent_id: "clauder".to_string(),
        spawn_cwd: spawn_cwd.clone(),
        pane_id: Some("%7".to_string()),
        pane_pid: Some(4242),
        spawned_at: Some("2026-06-09T10:00:00+08:00".to_string()),
        expected_session_id: Some(SessionId::new(expected)),
        provider_projects_root: Some(projects_root.clone()),
    };

    let candidates = get_adapter(Provider::Claude)
        .capture_session_candidates(&context, 0)
        .expect("Claude capture candidate scan should be fallible but successful");
    let first = candidates
        .first()
        .and_then(|candidate| candidate.captured.session_id.as_ref())
        .map(SessionId::as_str);

    assert_eq!(
        first,
        Some(expected),
        "capture must prefer expected_session_id under provider_projects_root over HOME/path-order distractors; candidates={candidates:?}"
    );
}

/// Stage 1 (identity-boundary unified plan, architect direction 2026-06-23):
/// when Claude's `expected_session_id` is set but the matching transcript
/// is NOT on disk, the scanner MUST NOT fall back to a same-cwd sibling
/// transcript (the AI-sync repro: `reviewer._pending=36dd` got assigned
/// `frontend`'s `7d5f.jsonl`). It must return either empty OR only candidates
/// with positive worker identity. Pre-fix the scanner sorted same-cwd
/// candidates with expected-first and the allocator picked the top one.
#[test]
#[serial(env)]
fn claude_capture_expected_id_missing_does_not_fall_back_to_same_cwd_sibling() {
    let root = tmp_dir("capture-expected-missing");
    let spawn_cwd = root.join("ai-sync");
    let projects_root = root.join("managed-claude").join("projects");
    let home = root.join("home");
    std::fs::create_dir_all(&spawn_cwd).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();

    let expected = "36dd1d1a-2766-4636-856c-03f14a4bb803"; // reviewer's expected
    let sibling = "7d5f430a-00cd-42fa-986e-43296b90b8f4"; // frontend's transcript
    // Only the sibling's transcript exists on disk — expected is missing.
    write_claude_transcript(
        &projects_root.join("sibling.jsonl"),
        sibling,
        &spawn_cwd,
        "frontend-marker", // does NOT mention reviewer
    );

    let _guard = EnvGuard::set_path_and_home(&home);
    let context = CaptureSessionContext {
        agent_id: "reviewer".to_string(),
        spawn_cwd: spawn_cwd.clone(),
        pane_id: Some("%9".to_string()),
        pane_pid: Some(9999),
        spawned_at: Some("2026-06-23T10:00:00+08:00".to_string()),
        expected_session_id: Some(SessionId::new(expected)),
        provider_projects_root: Some(projects_root.clone()),
    };

    let candidates = get_adapter(Provider::Claude)
        .capture_session_candidates(&context, 0)
        .expect("Claude scan should succeed");

    // Stage 1 contract: no candidate's session_id matches the expected → the
    // scanner must return only candidates with positive worker identity (or
    // empty). The sibling has no `reviewer` marker → must NOT appear.
    let returned_sibling = candidates.iter().any(|candidate| {
        candidate
            .captured
            .session_id
            .as_ref()
            .is_some_and(|sid| sid.as_str() == sibling)
    });
    assert!(
        !returned_sibling,
        "Stage 1: expected_session_id miss must not fall back to a same-cwd \
         sibling without positive worker identity. AI-sync repro: \
         reviewer._pending={expected} captured sibling={sibling}; candidates={candidates:?}"
    );
}

/// Stage 1: when expected id IS on disk, the existing exact-match return
/// path still wins (this is the architect §5 "Claude exact expected-id
/// still works" gate; the strict-miss fix must not break the strict-hit
/// path). Regression guard against the strict mode regressing positive
/// captures.
#[test]
#[serial(env)]
fn claude_capture_expected_id_exact_match_still_wins() {
    let root = tmp_dir("capture-expected-exact");
    let spawn_cwd = root.join("team");
    let projects_root = root.join("managed-claude").join("projects");
    let home = root.join("home");
    std::fs::create_dir_all(&spawn_cwd).unwrap();
    std::fs::create_dir_all(&projects_root).unwrap();
    std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();

    let expected = "36dd1d1a-2766-4636-856c-03f14a4bb803";
    let sibling = "7d5f430a-00cd-42fa-986e-43296b90b8f4";
    write_claude_transcript(
        &projects_root.join("expected.jsonl"),
        expected,
        &spawn_cwd,
        "reviewer-marker",
    );
    write_claude_transcript(
        &projects_root.join("sibling.jsonl"),
        sibling,
        &spawn_cwd,
        "frontend-marker",
    );

    let _guard = EnvGuard::set_path_and_home(&home);
    let context = CaptureSessionContext {
        agent_id: "reviewer".to_string(),
        spawn_cwd: spawn_cwd.clone(),
        pane_id: Some("%9".to_string()),
        pane_pid: Some(9999),
        spawned_at: Some("2026-06-23T10:00:00+08:00".to_string()),
        expected_session_id: Some(SessionId::new(expected)),
        provider_projects_root: Some(projects_root.clone()),
    };

    let candidates = get_adapter(Provider::Claude)
        .capture_session_candidates(&context, 0)
        .expect("Claude scan should succeed");

    let chosen = candidates
        .first()
        .and_then(|candidate| candidate.captured.session_id.as_ref())
        .map(SessionId::as_str);
    assert_eq!(
        chosen,
        Some(expected),
        "Stage 1: exact-match path must still return the expected transcript \
         even with same-cwd siblings present; candidates={candidates:?}"
    );
}

#[test]
#[serial(env)]
fn claude_auth_status_uses_zero_token_cli_status_json() {
    let root = tmp_dir("auth-status");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("claude"),
        r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  printf '{"authenticated":true,"source":"mock"}\n'
  exit 0
fi
exit 64
"#,
    );
    let _guard = EnvGuard::prepend_path(&bin);

    let adapter = get_adapter(Provider::Claude);
    assert_eq!(
        adapter.auth_hint(AuthMode::Subscription),
        AuthHintStatus::Present,
        "claude auth status rc=0 authenticated=true must classify as present"
    );

    write_executable(
        &bin.join("claude"),
        r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  printf '{"authenticated":false,"source":"mock"}\n'
  exit 1
fi
exit 64
"#,
    );
    assert!(
        matches!(
            adapter.auth_hint(AuthMode::Subscription),
            AuthHintStatus::Missing | AuthHintStatus::MissingOrUnknown
        ),
        "claude auth status rc!=0 authenticated=false must not be reported present"
    );
}

fn managed_profile_launch(root: &Path, projects_root: &Path) -> ProviderProfileLaunch {
    ProviderProfileLaunch {
        env_overlay: BTreeMap::from([
            ("CLAUDE_CONFIG_DIR".to_string(), root.join("claude-config").display().to_string()),
            ("ANTHROPIC_BASE_URL".to_string(), "https://llm.local/v1".to_string()),
            ("ANTHROPIC_AUTH_TOKEN".to_string(), "local-token".to_string()),
            ("ANTHROPIC_MODEL".to_string(), "profile-effective-model".to_string()),
        ]),
        env_unset: BTreeSet::from(["ANTHROPIC_API_KEY".to_string()]),
        command_overrides: ProviderCommandOverrides {
            model: Some("profile-effective-model".to_string()),
            ..ProviderCommandOverrides::default()
        },
        claude_config_dir: Some(root.join("claude-config")),
        claude_projects_root: Some(projects_root.to_path_buf()),
        managed_mcp_config: true,
    }
}

fn mcp_config() -> McpConfig {
    McpConfig {
        raw: json!({
            "team_orchestrator": {
                "type": "stdio",
                "command": "team-agent",
                "args": ["mcp-server", "--workspace", "{workspace}"],
                "env": {
                    "TEAM_AGENT_OWNER_TEAM_ID": "{team_id}",
                    "TEAM_AGENT_AGENT_ID": "{agent_id}"
                }
            }
        }),
    }
}

fn write_claude_transcript(path: &Path, session_id: &str, cwd: &Path, marker: &str) {
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    std::fs::write(
        path,
        format!(
            "{{\"type\":\"system\",\"sessionId\":\"{session_id}\",\"cwd\":\"{}\",\"marker\":\"{marker}\"}}\n",
            cwd.display()
        ),
    )
    .unwrap();
}

fn argv_has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn flag_values(argv: &[String], flag: &str) -> Vec<String> {
    argv.windows(2)
        .filter(|window| window[0] == flag)
        .map(|window| window[1].clone())
        .collect()
}

fn is_rfc4122_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for index in [8, 13, 18, 23] {
        if bytes[index] != b'-' {
            return false;
        }
    }
    for (index, byte) in bytes.iter().enumerate() {
        if [8, 13, 18, 23].contains(&index) {
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    matches!(bytes[14], b'1' | b'2' | b'3' | b'4' | b'5' | b'6' | b'7' | b'8')
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
}

fn write_executable(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-claude-provider-command-plan-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct EnvGuard {
    old_path: Option<String>,
    old_home: Option<String>,
}

impl EnvGuard {
    fn prepend_path(bin: &Path) -> Self {
        let old_path = std::env::var("PATH").ok();
        let new_path = match &old_path {
            Some(path) => format!("{}:{path}", bin.display()),
            None => bin.display().to_string(),
        };
        unsafe {
            std::env::set_var("PATH", new_path);
        }
        Self {
            old_path,
            old_home: std::env::var("HOME").ok(),
        }
    }

    fn set_path_and_home(home: &Path) -> Self {
        let old_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self {
            old_path: std::env::var("PATH").ok(),
            old_home,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(path) = self.old_path.take() {
                std::env::set_var("PATH", path);
            }
            if let Some(home) = self.old_home.take() {
                std::env::set_var("HOME", home);
            }
        }
    }
}
