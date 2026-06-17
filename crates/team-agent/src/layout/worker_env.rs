//! 0.3.28 layout step 3 — worker spawn env (inherit-then-strip) + worker
//! spawn cwd (YAML-only).
//!
//! Python truth source actual behavior (`providers.py:130-145` + contract
//! test `worker_spawn_env_red`): worker env INHERITS the parent
//! `team-agent` process environment (proxy / CA / PATH / random user
//! vars like `NODE_EXTRA_CA_CERTS`) just like the user typing `codex`
//! in the same terminal, then OVERLAYS the Team Agent identity
//! (`TEAM_AGENT_WORKSPACE`, `TEAM_AGENT_ID`, `TEAM_AGENT_AGENT_ID`,
//! `TEAM_AGENT_OWNER_TEAM_ID`).
//!
//! The leader's identity-specific env vars (`TEAM_AGENT_LEADER_*`,
//! `TEAM_AGENT_MACHINE_FINGERPRINT`, `TEAM_AGENT_TEAM_ID`,
//! `COPILOT_DISABLE_TERMINAL_TITLE`, `TMUX`, `TMUX_PANE`) are STRIPPED
//! to prevent E60 identity contamination.
//!
//! Pre-0.3.28-final used a WHITELIST (only specific keys pass through)
//! which broke `worker_spawn_env_red` by dropping legitimate user env
//! (`NODE_EXTRA_CA_CERTS`, etc.). Switched to inherit-then-strip per
//! leader verdict.
//!
//! Python uses `workspace` as the spawn cwd unconditionally; no
//! per-agent override field exists in the spec. Rust pre-0.3.28 honoured
//! `agent.spawn_cwd` from STATE which drifts (E56). Step 3 reads only
//! YAML `spec.agent.spawn_cwd` and falls through to `workspace`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::yaml::Value as YamlValue;

/// Env-key PREFIXES that are stripped from the inherited parent env
/// before worker spawn. These carry leader-process identity and must
/// not leak into worker provider TUIs (E60 root cause).
const STRIP_PREFIXES: &[&str] = &[
    "TEAM_AGENT_LEADER_",
    "TEAM_AGENT_LEADER_BYPASS",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
];

/// Exact env-keys stripped from the inherited parent env.
const STRIP_EXACT: &[&str] = &[
    // The leader's team_id; the worker's TEAM_AGENT_OWNER_TEAM_ID overlay
    // re-supplies the relevant scope.
    "TEAM_AGENT_TEAM_ID",
    // Suppresses copilot's title-rewrite — must be re-injected by
    // `apply_copilot_instructions_overlay` based on per-agent provider,
    // not inherited from the leader's provider.
    "COPILOT_DISABLE_TERMINAL_TITLE",
    // TMUX env points at the leader's pane — would attach the worker to it.
    "TMUX",
    "TMUX_PANE",
];

/// Build the worker spawn env per Python inherit-then-strip semantics.
///
/// Inputs:
///   * `parent_env`: the team-agent process env (typically
///     `std::env::vars()`). The whole env is inherited then filtered.
///   * `workspace`: the team workspace root.
///   * `agent_id`: this worker's agent_id.
///   * `team_id`: optional owner team id.
///
/// Output: a `BTreeMap` containing:
///   * All POSIX-valid shell identifier keys from `parent_env` EXCEPT
///     those matching `STRIP_PREFIXES` / `STRIP_EXACT`.
///   * Overlay: `TEAM_AGENT_WORKSPACE`, `TEAM_AGENT_ID`,
///     `TEAM_AGENT_AGENT_ID`, `TEAM_AGENT_OWNER_TEAM_ID` (when present).
///
/// POSIX-validity filter: shell variable names must match
/// `[A-Za-z_][A-Za-z0-9_]*`. Cargo's integration-test runner exports keys
/// like `CARGO_BIN_EXE_team-agent` that contain a dash — those would
/// break `sh -lc KEY=val` assignment and are dropped.
pub fn worker_spawn_env<I>(
    parent_env: I,
    workspace: &Path,
    agent_id: &str,
    team_id: Option<&str>,
) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in parent_env {
        if is_stripped(&k) {
            continue;
        }
        if !is_posix_shell_identifier(&k) {
            continue;
        }
        env.insert(k, v);
    }
    env.insert(
        "TEAM_AGENT_WORKSPACE".to_string(),
        workspace.to_string_lossy().to_string(),
    );
    env.insert("TEAM_AGENT_ID".to_string(), agent_id.to_string());
    env.insert("TEAM_AGENT_AGENT_ID".to_string(), agent_id.to_string());
    if let Some(tid) = team_id.filter(|s| !s.is_empty()) {
        env.insert("TEAM_AGENT_OWNER_TEAM_ID".to_string(), tid.to_string());
    }
    env
}

fn is_stripped(key: &str) -> bool {
    if STRIP_EXACT.iter().any(|exact| *exact == key) {
        return true;
    }
    if STRIP_PREFIXES.iter().any(|p| key.starts_with(p)) {
        return true;
    }
    false
}

fn is_posix_shell_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Compute worker spawn cwd per Python semantics: YAML
/// `agent.spawn_cwd` if set, else `workspace`. The persisted-state
/// `agent.spawn_cwd` override (the pre-0.3.28 Rust extension that caused
/// E56 drift) is NOT consulted.
pub fn worker_spawn_cwd(spec_agent: &YamlValue, workspace: &Path) -> PathBuf {
    spec_agent
        .get("spawn_cwd")
        .and_then(YamlValue::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parent_env(extras: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut env = vec![
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("HOME".to_string(), "/Users/test".to_string()),
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("CLAUDE_API_KEY".to_string(), "sk-test".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-test".to_string()),
            ("COPILOT_TOKEN".to_string(), "tok".to_string()),
            // Leader identity — must be stripped.
            ("TEAM_AGENT_LEADER_PROVIDER".to_string(), "claude".to_string()),
            ("TEAM_AGENT_LEADER_SESSION_UUID".to_string(), "leader-uuid".to_string()),
            ("TEAM_AGENT_MACHINE_FINGERPRINT".to_string(), "fp".to_string()),
            ("TEAM_AGENT_TEAM_ID".to_string(), "alpha".to_string()),
            ("TEAM_AGENT_LEADER_BYPASS".to_string(), "1".to_string()),
            ("COPILOT_DISABLE_TERMINAL_TITLE".to_string(), "1".to_string()),
            // Random noise — non-whitelisted prefix.
            ("RANDOM_NOISE".to_string(), "x".to_string()),
            ("TMUX".to_string(), "leader-pane".to_string()),
            ("TMUX_PANE".to_string(), "%0".to_string()),
        ];
        for (k, v) in extras {
            env.push((k.to_string(), v.to_string()));
        }
        env
    }

    #[test]
    fn worker_spawn_env_strips_leader_identity_keys() {
        let env = worker_spawn_env(
            make_parent_env(&[]),
            Path::new("/ws"),
            "developer",
            Some("alpha"),
        );
        for stripped in [
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_MACHINE_FINGERPRINT",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_LEADER_BYPASS",
            "COPILOT_DISABLE_TERMINAL_TITLE",
            "TMUX",
            "TMUX_PANE",
        ] {
            assert!(
                !env.contains_key(stripped),
                "worker env must not contain leader identity key `{stripped}`; env={env:?}"
            );
        }
    }

    #[test]
    fn worker_spawn_env_injects_agent_workspace_and_team_keys() {
        let env = worker_spawn_env(
            make_parent_env(&[]),
            Path::new("/ws"),
            "developer",
            Some("alpha"),
        );
        assert_eq!(env.get("TEAM_AGENT_ID").map(String::as_str), Some("developer"));
        assert_eq!(env.get("TEAM_AGENT_AGENT_ID").map(String::as_str), Some("developer"));
        assert_eq!(env.get("TEAM_AGENT_WORKSPACE").map(String::as_str), Some("/ws"));
        assert_eq!(env.get("TEAM_AGENT_OWNER_TEAM_ID").map(String::as_str), Some("alpha"));
    }

    #[test]
    fn worker_spawn_env_inherits_parent_env_minus_strip_list() {
        let env = worker_spawn_env(
            make_parent_env(&[]),
            Path::new("/ws"),
            "developer",
            None,
        );
        // Inherit-then-strip: every parent key passes through EXCEPT the
        // strip list. PATH-like + provider creds + random user vars all
        // present.
        assert!(env.contains_key("PATH"));
        assert!(env.contains_key("HOME"));
        assert!(env.contains_key("TERM"));
        assert!(env.contains_key("CLAUDE_API_KEY"));
        assert!(env.contains_key("OPENAI_API_KEY"));
        // Random user env (e.g. NODE_EXTRA_CA_CERTS, TA_SPAWN_ENV_CANARY)
        // MUST pass through — this was the worker_spawn_env_red contract
        // that the pre-final whitelist approach broke.
        assert!(
            env.contains_key("RANDOM_NOISE"),
            "0.3.28 final: worker env inherits parent — random user vars \
             must pass through (NODE_EXTRA_CA_CERTS / TA_SPAWN_ENV_CANARY \
             contract). Got env={env:?}"
        );
    }

    #[test]
    fn worker_spawn_env_drops_posix_invalid_keys() {
        let parent = make_parent_env(&[("CARGO_BIN_EXE_team-agent", "/bin/x")]);
        let env = worker_spawn_env(parent, Path::new("/ws"), "developer", None);
        assert!(!env.contains_key("CARGO_BIN_EXE_team-agent"));
    }

    #[test]
    fn worker_spawn_cwd_returns_workspace_when_no_spec_override() {
        // YAML agent with no spawn_cwd field.
        let agent = YamlValue::Map(vec![("id".to_string(), YamlValue::Str("developer".to_string()))]);
        assert_eq!(worker_spawn_cwd(&agent, Path::new("/ws")), PathBuf::from("/ws"));
    }

    #[test]
    fn worker_spawn_cwd_returns_yaml_override_when_present() {
        let agent = YamlValue::Map(vec![
            ("id".to_string(), YamlValue::Str("developer".to_string())),
            ("spawn_cwd".to_string(), YamlValue::Str("/explicit/cwd".to_string())),
        ]);
        assert_eq!(
            worker_spawn_cwd(&agent, Path::new("/ws")),
            PathBuf::from("/explicit/cwd")
        );
    }

    #[test]
    fn worker_spawn_cwd_returns_workspace_when_spec_override_is_empty() {
        let agent = YamlValue::Map(vec![
            ("id".to_string(), YamlValue::Str("developer".to_string())),
            ("spawn_cwd".to_string(), YamlValue::Str("".to_string())),
        ]);
        assert_eq!(worker_spawn_cwd(&agent, Path::new("/ws")), PathBuf::from("/ws"));
    }
}
