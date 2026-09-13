//! Scope and lifecycle boundaries for the abnormal-exit observation cache.
use std::collections::BTreeSet;

use serde_json::{json, Value};

fn members(state: &Value) -> Option<BTreeSet<String>> {
    Some(state.get("agents")?.as_object()?.keys().cloned().collect())
}

// A missing roster is unknown. Only a complete, lock-merged roster authorizes
// discarding absent members; liveness, pause status and record age do not.
fn scopes(state: &Value) -> Vec<(Option<String>, Option<BTreeSet<String>>)> {
    let mut root = members(state);
    if state.get("teams").is_some_and(|teams| !teams.is_object()) {
        root = None;
    }
    let mut scopes = Vec::new();
    if let Some(teams) = state.get("teams").and_then(Value::as_object) {
        for (team, entry) in teams {
            let roster = members(entry);
            root = match (root, &roster) {
                (Some(mut union), Some(keys)) => {
                    union.extend(keys.iter().cloned());
                    Some(union)
                }
                _ => None,
            };
            scopes.push((Some(team.clone()), roster));
        }
    }
    scopes.push((None, root));
    scopes
}

pub(crate) fn needs_compaction(state: &Value) -> bool {
    scopes(state).into_iter().any(|(team, roster)| {
        let entry = team.as_ref().map_or(state, |key| &state["teams"][key]);
        roster.is_some_and(|keys| {
            entry
                .pointer("/coordinator/abnormal_exit_watch")
                .and_then(Value::as_object)
                .is_some_and(|watch| watch.keys().any(|key| !keys.contains(key)))
        })
    })
}

/// Call only after the persistence lock's latest-roster merge.
pub(crate) fn compact_after_merge(state: &mut Value, latest: Option<&Value>, deleted: &[&str]) {
    let latest_scopes = latest.map(scopes).unwrap_or_default();
    for (team, mut roster) in scopes(state) {
        // The normal merge may omit a roster row with no allowlisted stub
        // fields, or a whole sibling absent from a stale input. Membership in
        // lock-held latest still forbids pruning its observation. Only explicit
        // deletion authority can subtract it from this retention union.
        if let Some((_, latest_roster)) = latest_scopes.iter().find(|(key, _)| key == &team) {
            roster = match (roster, latest_roster) {
                (Some(mut keys), Some(latest_keys)) => {
                    keys.extend(
                        latest_keys
                            .iter()
                            .filter(|key| !deleted.contains(&key.as_str()))
                            .cloned(),
                    );
                    Some(keys)
                }
                _ => None,
            };
        }
        let Some(keys) = roster else { continue };
        let entry = match team {
            Some(team) => &mut state["teams"][&team],
            None => &mut *state,
        };
        if let Some(watch) = entry
            .pointer_mut("/coordinator/abnormal_exit_watch")
            .and_then(Value::as_object_mut)
        {
            watch.retain(|key, _| keys.contains(key));
        }
    }
}

/// New observations carry their team and cohort, never just a bare agent id.
pub(crate) fn observation_identity(state: &Value, agent_id: &str) -> Option<Value> {
    let agent = state.get("agents")?.get(agent_id)?;
    let provider = crate::provider::wire::parse_provider(agent.get("provider")?.as_str()?)?;
    let path = ["rollout_path", "transcript_path", "session_log_path"]
        .into_iter()
        .find_map(|key| agent.get(key).and_then(Value::as_str))
        .filter(|path| !path.is_empty())?;
    let spawn_epoch = agent.get("spawn_epoch").and_then(Value::as_u64);
    let spawned_at = agent
        .get("spawned_at")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    // Preserve the watcher's existing legacy cohort: missing spawn fields
    // remain explicit nulls, never a fabricated generation. A later known
    // cohort changes this identity and establishes a new baseline.
    let team = state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let team = super::projection::team_state_key(state);
            (team != super::projection::CURRENT_TEAM_ALIAS).then_some(team)
        })?;
    Some(json!({
        "team_key": team,
        "agent_id": agent_id,
        "provider": crate::provider::wire::provider_wire(provider),
        "path": path,
        "spawn_epoch": spawn_epoch,
        "spawned_at": spawned_at,
    }))
}

pub(crate) fn matches_observation(state: &Value, agent_id: &str, row: &Value) -> bool {
    observation_identity(state, agent_id)
        .is_some_and(|identity| row.get("watch_identity") == Some(&identity))
}

/// Legacy/foreign-cohort facts cannot be reused as current observation evidence.
/// This is invoked only when actually observing the member, not for paused or
/// otherwise ineligible members. The next scan establishes the ordinary baseline.
pub(crate) fn begin_observation(state: &mut Value, agent_id: &str) {
    let identity = observation_identity(state, agent_id);
    if let Some(watch) = state
        .pointer_mut("/coordinator/abnormal_exit_watch")
        .and_then(Value::as_object_mut)
    {
        if watch.get(agent_id).is_some_and(|row| {
            identity
                .as_ref()
                .is_none_or(|identity| row.get("watch_identity") != Some(identity))
        }) {
            watch.remove(agent_id);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
