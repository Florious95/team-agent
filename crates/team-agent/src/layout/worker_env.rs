//!
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
use crate::provider::wire::{parse_provider, provider_wire};
use crate::provider::Provider;

/// Current worker caller context is deliberately separate from `TEAM_AGENT_LEADER_*`.
/// The launcher writes only the typed provider; the tmux invocation boundary adds the
/// actual pane and socket for the provider command itself.
pub(crate) const CALLER_CONTEXT_PREFIX: &str = "TEAM_AGENT_CALLER_";
pub(crate) const CALLER_PROVIDER_ENV: &str = "TEAM_AGENT_CALLER_PROVIDER";
pub(crate) const CALLER_PANE_ENV: &str = "TEAM_AGENT_CALLER_PANE_ID";
pub(crate) const CALLER_ENDPOINT_ENV: &str = "TEAM_AGENT_CALLER_TMUX_ENDPOINT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallerProviderResolution {
    Absent,
    Valid(Provider),
    Invalid,
}

/// Env-key PREFIXES that are stripped from the inherited parent env
/// before worker spawn. These carry leader-process identity and must
/// not leak into worker provider TUIs (E60 root cause).
const STRIP_PREFIXES: &[&str] = &[
    "TEAM_AGENT_LEADER_",
    "TEAM_AGENT_LEADER_BYPASS",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
    CALLER_CONTEXT_PREFIX,
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

const WORKER_IDENTITY_EXACT: &[&str] = &["CLAUDECODE", "CLAUDE_EFFORT", "CODEX_THREAD_ID"];

const WORKER_IDENTITY_PREFIXES: &[&str] = &["CLAUDE_CODE_"];

///
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
///     `TEAM_AGENT_AGENT_ID`, `TEAM_AGENT_OWNER_TEAM_ID` (when present),
///     `TEAM_AGENT_AUTH_MODE` (when present).
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
    auth_mode: Option<&str>,
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
    if let Some(auth) = auth_mode.filter(|s| !s.is_empty()) {
        env.insert("TEAM_AGENT_AUTH_MODE".to_string(), auth.to_string());
    }
    env
}

/// Overlay the final typed provider after profile/env overlays. Any inherited or
/// profile-supplied caller namespace is discarded before this fresh value is written.
pub(crate) fn inject_current_caller_provider(
    env: &mut BTreeMap<String, String>,
    provider: Provider,
) {
    env.retain(|key, _| !key.starts_with(CALLER_CONTEXT_PREFIX));
    env.insert(
        CALLER_PROVIDER_ENV.to_string(),
        provider_wire(provider).to_string(),
    );
}

/// Resolve the narrow caller context used by both initial-state seeding and fresh bind.
/// `current_*` come from the invoking process; `scoped_endpoint` comes from the selected
/// transport. A partial, malformed, stale, or conflicting context is never downgraded to
/// command attribution.
pub(crate) fn caller_provider_resolution(
    current_pane: Option<&str>,
    current_endpoint: Option<&str>,
    scoped_endpoint: Option<&str>,
) -> CallerProviderResolution {
    let provider = std::env::var(CALLER_PROVIDER_ENV).ok();
    let context_pane = std::env::var(CALLER_PANE_ENV).ok();
    let context_endpoint = std::env::var(CALLER_ENDPOINT_ENV).ok();
    let explicit_provider = std::env::var("TEAM_AGENT_LEADER_PROVIDER").ok();
    let explicit_pane = std::env::var("TEAM_AGENT_LEADER_PANE_ID").ok();
    caller_provider_resolution_from_values(
        provider.as_deref(),
        context_pane.as_deref(),
        context_endpoint.as_deref(),
        explicit_provider.as_deref(),
        explicit_pane.as_deref(),
        current_pane,
        current_endpoint,
        scoped_endpoint,
    )
}

fn caller_provider_resolution_from_values(
    provider_raw: Option<&str>,
    context_pane: Option<&str>,
    context_endpoint: Option<&str>,
    explicit_provider: Option<&str>,
    explicit_pane: Option<&str>,
    current_pane: Option<&str>,
    current_endpoint: Option<&str>,
    scoped_endpoint: Option<&str>,
) -> CallerProviderResolution {
    if provider_raw.is_none() && context_pane.is_none() && context_endpoint.is_none() {
        return CallerProviderResolution::Absent;
    }
    let Some(provider_raw) = provider_raw.filter(|value| !value.is_empty()) else {
        return CallerProviderResolution::Invalid;
    };
    let Some(provider) = parse_provider(provider_raw) else {
        return CallerProviderResolution::Invalid;
    };
    let Some(context_pane) = context_pane.filter(|value| !value.is_empty()) else {
        return CallerProviderResolution::Invalid;
    };
    let Some(context_endpoint) = context_endpoint.filter(|value| !value.is_empty()) else {
        return CallerProviderResolution::Invalid;
    };
    if !valid_tmux_pane(context_pane)
        || !valid_tmux_endpoint(context_endpoint)
        || current_pane != Some(context_pane)
        || current_endpoint != Some(context_endpoint)
        || scoped_endpoint != Some(context_endpoint)
    {
        return CallerProviderResolution::Invalid;
    }
    if let Some(explicit) = explicit_provider.filter(|value| !value.is_empty()) {
        if parse_provider(explicit) != Some(provider) {
            return CallerProviderResolution::Invalid;
        }
    }
    if let Some(explicit_pane) = explicit_pane.filter(|value| !value.is_empty()) {
        if explicit_pane != context_pane {
            return CallerProviderResolution::Invalid;
        }
    }
    CallerProviderResolution::Valid(provider)
}

fn valid_tmux_pane(value: &str) -> bool {
    value.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn valid_tmux_endpoint(value: &str) -> bool {
    value.starts_with('/') && !value.contains(',')
}

pub(crate) fn isolate_worker_spawn_env(
    _target_provider: Provider,
    env: &mut BTreeMap<String, String>,
    base_env_unset: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut env_unset = base_env_unset
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    // Clear any stale values held by the tmux server before the scoped provider
    // invocation assigns the fresh context. The inert shell tail must not retain it.
    for key in [CALLER_PROVIDER_ENV, CALLER_PANE_ENV, CALLER_ENDPOINT_ENV] {
        env.remove(key);
        env_unset.insert(key.to_string());
    }
    for key in WORKER_IDENTITY_EXACT {
        env.remove(*key);
        env_unset.insert((*key).to_string());
    }
    let dynamic_keys = env
        .keys()
        .filter(|key| is_worker_identity_key(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in dynamic_keys {
        env.remove(&key);
        env_unset.insert(key);
    }
    env_unset.into_iter().collect()
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

fn is_worker_identity_key(key: &str) -> bool {
    WORKER_IDENTITY_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

fn is_posix_shell_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

///
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
            (
                "TEAM_AGENT_LEADER_PROVIDER".to_string(),
                "claude".to_string(),
            ),
            (
                "TEAM_AGENT_LEADER_SESSION_UUID".to_string(),
                "leader-uuid".to_string(),
            ),
            (
                "TEAM_AGENT_MACHINE_FINGERPRINT".to_string(),
                "fp".to_string(),
            ),
            ("TEAM_AGENT_TEAM_ID".to_string(), "alpha".to_string()),
            ("TEAM_AGENT_LEADER_BYPASS".to_string(), "1".to_string()),
            (
                "COPILOT_DISABLE_TERMINAL_TITLE".to_string(),
                "1".to_string(),
            ),
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
            None,
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
            Some("subscription"),
        );
        assert_eq!(
            env.get("TEAM_AGENT_ID").map(String::as_str),
            Some("developer")
        );
        assert_eq!(
            env.get("TEAM_AGENT_AGENT_ID").map(String::as_str),
            Some("developer")
        );
        assert_eq!(
            env.get("TEAM_AGENT_WORKSPACE").map(String::as_str),
            Some("/ws")
        );
        assert_eq!(
            env.get("TEAM_AGENT_OWNER_TEAM_ID").map(String::as_str),
            Some("alpha")
        );
        assert_eq!(
            env.get("TEAM_AGENT_AUTH_MODE").map(String::as_str),
            Some("subscription")
        );
    }

    #[test]
    fn worker_spawn_env_inherits_parent_env_minus_strip_list() {
        let env = worker_spawn_env(
            make_parent_env(&[]),
            Path::new("/ws"),
            "developer",
            None,
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
        let env = worker_spawn_env(parent, Path::new("/ws"), "developer", None, None);
        assert!(!env.contains_key("CARGO_BIN_EXE_team-agent"));
    }

    #[test]
    fn worker_spawn_env_strips_inherited_caller_context() {
        let parent = make_parent_env(&[
            (CALLER_PROVIDER_ENV, "claude"),
            (CALLER_PANE_ENV, "%1"),
            (CALLER_ENDPOINT_ENV, "/tmp/old-tmux.sock"),
        ]);
        let env = worker_spawn_env(parent, Path::new("/ws"), "developer", None, None);
        assert!(!env.keys().any(|key| key.starts_with(CALLER_CONTEXT_PREFIX)));
    }

    #[test]
    fn inject_current_caller_provider_overwrites_namespace_from_final_provider() {
        let mut env = BTreeMap::from([
            (CALLER_PROVIDER_ENV.to_string(), "claude".to_string()),
            (CALLER_PANE_ENV.to_string(), "%1".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ]);
        inject_current_caller_provider(&mut env, Provider::Pi);
        assert_eq!(env.get(CALLER_PROVIDER_ENV).map(String::as_str), Some("pi"));
        assert!(!env.contains_key(CALLER_PANE_ENV));
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    #[test]
    fn isolate_worker_spawn_env_clears_context_from_outer_shell() {
        let mut env = BTreeMap::from([(CALLER_PROVIDER_ENV.to_string(), "pi".to_string())]);
        let unset = isolate_worker_spawn_env(Provider::Pi, &mut env, Vec::<String>::new());
        assert!(!env.contains_key(CALLER_PROVIDER_ENV));
        assert!(unset.iter().any(|key| key == CALLER_PROVIDER_ENV));
        assert!(unset.iter().any(|key| key == CALLER_PANE_ENV));
        assert!(unset.iter().any(|key| key == CALLER_ENDPOINT_ENV));
    }

    #[test]
    fn caller_context_requires_exact_current_pane_and_endpoint() {
        let resolution = caller_provider_resolution_from_values(
            Some("pi"),
            Some("%1"),
            Some("/tmp/tmux.sock"),
            None,
            None,
            Some("%1"),
            Some("/tmp/tmux.sock"),
            Some("/tmp/tmux.sock"),
        );
        assert_eq!(resolution, CallerProviderResolution::Valid(Provider::Pi));
        assert_eq!(
            caller_provider_resolution_from_values(
                Some("pi"),
                Some("%1"),
                Some("/tmp/tmux.sock"),
                None,
                None,
                Some("%2"),
                Some("/tmp/tmux.sock"),
                Some("/tmp/tmux.sock"),
            ),
            CallerProviderResolution::Invalid
        );
        assert_eq!(
            caller_provider_resolution_from_values(
                Some("pi"),
                Some("%1"),
                Some("/tmp/tmux.sock"),
                Some("codex"),
                None,
                Some("%1"),
                Some("/tmp/tmux.sock"),
                Some("/tmp/tmux.sock"),
            ),
            CallerProviderResolution::Invalid
        );
    }

    #[test]
    fn worker_spawn_cwd_returns_workspace_when_no_spec_override() {
        // YAML agent with no spawn_cwd field.
        let agent = YamlValue::Map(vec![(
            "id".to_string(),
            YamlValue::Str("developer".to_string()),
        )]);
        assert_eq!(
            worker_spawn_cwd(&agent, Path::new("/ws")),
            PathBuf::from("/ws")
        );
    }

    #[test]
    fn worker_spawn_cwd_returns_yaml_override_when_present() {
        let agent = YamlValue::Map(vec![
            ("id".to_string(), YamlValue::Str("developer".to_string())),
            (
                "spawn_cwd".to_string(),
                YamlValue::Str("/explicit/cwd".to_string()),
            ),
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
        assert_eq!(
            worker_spawn_cwd(&agent, Path::new("/ws")),
            PathBuf::from("/ws")
        );
    }
}
