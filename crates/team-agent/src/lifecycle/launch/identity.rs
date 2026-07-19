use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

pub(super) fn spec_team_id(spec: &Value) -> Option<String> {
    spec.get("team")
        .and_then(|v| v.get("id").or_else(|| v.get("name")))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.get("name").and_then(Value::as_str).map(str::to_string))
}

pub(super) fn explicit_active_team_key(state: &serde_json::Value) -> Option<String> {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|team| !team.is_empty())
        .map(str::to_string)
}

pub(super) fn runtime_team_key_for_spec(
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
) -> String {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let state = serde_json::json!({
        "team_dir": team_dir.to_string_lossy(),
        "spec_path": spec_path.to_string_lossy(),
        "session_name": session_name.as_str(),
        "team": spec.get("team").map(yaml_value_to_json).unwrap_or(serde_json::Value::Null),
    });
    crate::state::projection::team_state_key(&state)
}

pub(super) fn transport_has_session(transport: &dyn Transport, session_name: &SessionName) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.has_session(session_name)
    })) {
        Ok(Ok(live)) => live,
        Ok(Err(_)) | Err(_) => false,
    }
}

pub(super) fn spec_display_backend(spec: &Value) -> DisplayBackend {
    let requested = spec
        .get("runtime")
        .and_then(|runtime| runtime.get("display_backend"))
        .and_then(Value::as_str)
        .and_then(|backend| {
            serde_json::from_value::<DisplayBackend>(serde_json::json!(backend)).ok()
        });
    crate::lifecycle::display::resolve_display_backend(requested, None).backend
}

use crate::provider::wire::parse_provider;

pub(super) fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}

/// 0.4.x provider effort MVP step 4: low-level from a raw string. Returns
/// `Some(effort)` when the level parses AND the provider supports it.
pub(crate) fn provider_effort_from_raw(
    raw: Option<&str>,
    provider: Provider,
) -> Option<ProviderEffort> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let effort = ProviderEffort::parse(raw)?;
    if effort.is_supported_by(provider) {
        Some(effort)
    } else {
        None
    }
}

/// 0.4.x provider effort MVP step 7: warning event payload when the spec
/// requested an effort level the provider does not support.
pub(crate) fn provider_effort_event_payload(
    raw: Option<&str>,
    provider: Provider,
    agent_id: &str,
) -> Option<serde_json::Value> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let effort = ProviderEffort::parse(raw)?;
    if effort.is_supported_by(provider) {
        return None;
    }
    Some(serde_json::json!({
        "agent_id": agent_id,
        "provider": format!("{provider:?}").to_lowercase(),
        "effort": effort.as_str(),
        "action": "ignored",
        "reason": "provider does not support effort",
    }))
}

/// 0.4.x provider effort MVP step 9: defensive guarantee that `CLAUDE_EFFORT`
/// is unset in the Claude/ClaudeCode worker spawn env. As of the
/// `profile_launch::provider_env_unsets` update, the base list already
/// includes `CLAUDE_EFFORT` for Claude — so this function is idempotent
/// (returns input unchanged). Kept as a belt-and-braces guard so a future
/// refactor that bypasses provider_env_unsets cannot silently drop the
/// scrub. The structural win is in `tmux_backend::shell_command` which now
/// filters env exports by env_unset (preventing inherited values from
/// re-introducing keys we just unset).
pub(crate) fn extend_worker_env_unset_for_effort(
    base: Vec<String>,
    provider: Provider,
) -> Vec<String> {
    if !matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return base;
    }
    let mut out = base;
    if !out.iter().any(|k| k == "CLAUDE_EFFORT") {
        out.push("CLAUDE_EFFORT".to_string());
    }
    out
}

/// Convenience: resolve effort for a yaml::Value agent (spec / compiled).
pub(crate) fn provider_effort_for_spawn(
    agent: &crate::model::yaml::Value,
    provider: Provider,
) -> Option<ProviderEffort> {
    provider_effort_from_raw(agent.get("effort").and_then(|v| v.as_str()), provider)
}

pub(crate) fn provider_effort_event_if_dropped(
    agent: &crate::model::yaml::Value,
    provider: Provider,
    agent_id: &str,
) -> Option<serde_json::Value> {
    provider_effort_event_payload(
        agent.get("effort").and_then(|v| v.as_str()),
        provider,
        agent_id,
    )
}

/// Same as [`provider_effort_for_spawn`] but for serde_json state values
/// (used by restart paths reading from `state.agents[id]`).
pub(crate) fn provider_effort_for_spawn_json(
    agent: &serde_json::Value,
    provider: Provider,
) -> Option<ProviderEffort> {
    provider_effort_from_raw(
        agent.get("effort").and_then(serde_json::Value::as_str),
        provider,
    )
}

pub(crate) fn provider_effort_event_if_dropped_json(
    agent: &serde_json::Value,
    provider: Provider,
    agent_id: &str,
) -> Option<serde_json::Value> {
    provider_effort_event_payload(
        agent.get("effort").and_then(serde_json::Value::as_str),
        provider,
        agent_id,
    )
}

pub(super) fn quick_start_requested_team_key<'a>(
    team_id: Option<&'a str>,
    name: Option<&'a str>,
) -> Option<&'a str> {
    team_id.or(name).filter(|team| !team.is_empty())
}

pub(super) struct QuickStartDepth {
    pub(super) parent_team_key: Option<String>,
    pub(super) team_depth: u64,
}

pub(super) fn quick_start_depth_guard(
    workspace: &Path,
    _agents_dir: &Path,
    requested_team: Option<&str>,
    _strict_real_runtime: bool,
) -> Result<QuickStartDepth, LifecycleError> {
    let env_parent = std::env::var("TEAM_AGENT_OWNER_TEAM_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let parent = env_parent;
    let Some(parent) = parent else {
        let state = crate::state::persist::load_runtime_state(workspace)
            .unwrap_or_else(|_| serde_json::json!({}));
        let ambiguous_nested_intent = requested_team.is_some_and(|team| {
            looks_ambiguous_child_team_key(team) || looks_grandchild_team_key(team)
        });
        if has_live_runtime_teams(&state) && ambiguous_nested_intent {
            if requested_team.is_some_and(looks_grandchild_team_key) {
                if let Some(parent_key) = infer_parent_team_from_active_state(&state) {
                    let parent_state =
                        crate::state::projection::project_top_level_view(&state, &parent_key);
                    let parent_depth = parent_state
                        .get("team_depth")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1);
                    return Ok(QuickStartDepth {
                        parent_team_key: Some(parent_key),
                        team_depth: parent_depth.saturating_add(1),
                    });
                }
            }
            return Err(LifecycleError::RequirementUnmet(
                "cannot infer parent team for nested quick-start; pass an explicit worker/subleader owner context"
                    .to_string(),
            ));
        }
        return Ok(QuickStartDepth {
            parent_team_key: None,
            team_depth: 1,
        });
    };
    let state = crate::state::persist::load_runtime_state(workspace)
        .unwrap_or_else(|_| serde_json::json!({}));
    let parent_key = crate::state::projection::resolve_owner_team_id(&state, &parent)
        .canonical_key()
        .map(str::to_string)
        .unwrap_or(parent);
    let parent_state = crate::state::projection::project_top_level_view(&state, &parent_key);
    let parent_depth = parent_state
        .get("team_depth")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let team_depth = parent_depth.saturating_add(1);
    Ok(QuickStartDepth {
        parent_team_key: Some(parent_key),
        team_depth,
    })
}

pub(super) fn infer_parent_team_from_active_state(state: &serde_json::Value) -> Option<String> {
    let active = explicit_active_team_key(state)?;
    let team = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(&active))?;
    team_has_running_agent(team).then_some(active)
}

pub(super) fn has_live_runtime_teams(state: &serde_json::Value) -> bool {
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.values().any(team_has_running_agent))
}

pub(super) fn team_has_running_agent(team: &serde_json::Value) -> bool {
    team.get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                agent.get("status").and_then(serde_json::Value::as_str) == Some("running")
            })
        })
}

pub(super) fn looks_ambiguous_child_team_key(team: &str) -> bool {
    let team = team.trim().to_ascii_lowercase();
    team != "child"
        && (team.starts_with("child-")
            || team.starts_with("child_")
            || team.starts_with("child.")
            || team.starts_with("child"))
}

pub(super) fn looks_grandchild_team_key(team: &str) -> bool {
    let team = team.trim().to_ascii_lowercase();
    team == "grandchild"
        || team.starts_with("grandchild-")
        || team.starts_with("grandchild_")
        || team.starts_with("grandchild.")
        || team.starts_with("grandchild")
}

pub(super) fn annotate_team_depth(
    state: &mut serde_json::Value,
    parent_team_key: Option<&str>,
    team_depth: u64,
) {
    let Some(obj) = state.as_object_mut() else {
        return;
    };
    obj.insert("team_depth".to_string(), serde_json::json!(team_depth));
    if let Some(parent) = parent_team_key.filter(|value| !value.is_empty()) {
        obj.insert("parent_team_key".to_string(), serde_json::json!(parent));
    }
}

pub(super) fn annotate_persisted_team_depth(
    workspace: &Path,
    team_key: &str,
    parent_team_key: Option<&str>,
    team_depth: u64,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let Some(team) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team_key))
    else {
        return Ok(());
    };
    annotate_team_depth(team, parent_team_key, team_depth);
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::AnnotateTeamDepth { team_key },
            &state,
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

pub(super) fn runtime_state_has_quick_start_team(state: &serde_json::Value, team: &str) -> bool {
    explicit_active_team_key(state).as_deref() == Some(team)
        || state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|teams| {
                teams.contains_key(team)
                    || teams
                        .values()
                        .any(|entry| json_team_identity_matches(entry, team))
            })
        || crate::state::projection::team_state_key(state) == team
        || json_team_identity_matches(state, team)
        || state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| session == team || session.strip_prefix("team-") == Some(team))
}

pub(super) fn json_team_identity_matches(state: &serde_json::Value, team: &str) -> bool {
    state
        .get("team")
        .and_then(|value| value.get("id").or_else(|| value.get("name")))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value == team)
        || state
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == team)
}
