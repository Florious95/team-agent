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
    if spawn_epoch.is_none() && spawned_at.is_none() {
        return None;
    }
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
pub(crate) fn begin_observation(state: &mut Value, agent_id: &str) -> bool {
    let Some(identity) = observation_identity(state, agent_id) else {
        return false;
    };
    if let Some(watch) = state
        .pointer_mut("/coordinator/abnormal_exit_watch")
        .and_then(Value::as_object_mut)
    {
        if watch
            .get(agent_id)
            .is_some_and(|row| row.get("watch_identity") != Some(&identity))
        {
            watch.remove(agent_id);
        }
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::state::persist::{load_runtime_state, runtime_state_path, save_runtime_state};

    fn workspace() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "watch-188-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join(".team/runtime")).unwrap();
        path
    }

    #[test]
    fn complete_empty_roster_prunes_but_missing_roster_and_paused_members_survive() {
        let mut state = json!({
            "agents": {},
            "teams": {
                "empty": {"agents": {}, "coordinator": {"abnormal_exit_watch": {"old": {}}}},
                "unknown": {"coordinator": {"abnormal_exit_watch": {"old": {}}}},
                "paused": {"agents": {"p": {"status": "paused"}}, "coordinator": {
                    "abnormal_exit_watch": {"p": {"last_notified_key": "keep"}, "gone": {}}
                }}
            },
            "coordinator": {"abnormal_exit_watch": {"unknown": {}}}
        });
        compact_after_merge(&mut state, None, &[]);
        assert_eq!(
            state["teams"]["empty"]["coordinator"]["abnormal_exit_watch"],
            json!({})
        );
        assert_eq!(
            state["teams"]["unknown"]["coordinator"]["abnormal_exit_watch"],
            json!({"old": {}})
        );
        assert_eq!(
            state["teams"]["paused"]["coordinator"]["abnormal_exit_watch"],
            json!({"p": {"last_notified_key": "keep"}})
        );
        assert_eq!(
            state["coordinator"]["abnormal_exit_watch"],
            json!({"unknown": {}})
        );
        assert!(!needs_compaction(&state));
    }

    #[test]
    fn root_legacy_union_is_not_migrated_by_bare_agent_name() {
        let mut state = json!({"agents": {}, "teams": {
            "a": {"agents": {"w": {}}}, "b": {"agents": {"w": {}}}
        }, "coordinator": {"abnormal_exit_watch": {"w": {"last_notified_key": "legacy"}, "gone": {}}}});
        compact_after_merge(&mut state, None, &[]);
        assert_eq!(
            state["coordinator"]["abnormal_exit_watch"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        for team in ["a", "b"] {
            let projection = crate::state::projection::project_top_level_view(&state, team);
            assert!(projection["coordinator"]
                .get("abnormal_exit_watch")
                .is_none());
        }
    }

    #[test]
    fn fallback_preserves_other_coordinator_fields_and_owned_watch() {
        let state = json!({"coordinator": {"tick": 9, "abnormal_exit_watch": {"w": {}}}, "teams": {
            "a": {"agents": {}},
            "b": {"agents": {}, "coordinator": {"abnormal_exit_watch": {"owned": {}}, "tick": 2}}
        }});
        let a = crate::state::projection::project_top_level_view(&state, "a");
        let b = crate::state::projection::project_top_level_view(&state, "b");
        assert_eq!(a["coordinator"], json!({"tick": 9}));
        assert_eq!(b["coordinator"], state["teams"]["b"]["coordinator"]);
    }

    fn observed() -> Value {
        let mut state = json!({"active_team_key": "a", "agents": {"w": {
            "provider": "codex", "rollout_path": "/fixture/w.jsonl", "spawn_epoch": 1
        }}, "coordinator": {"abnormal_exit_watch": {"w": {"last_notified_key": "keep"}}}});
        state["coordinator"]["abnormal_exit_watch"]["w"]["watch_identity"] =
            observation_identity(&state, "w").unwrap();
        state
    }

    #[test]
    fn observation_identity_separates_same_name_team_path_and_spawn_generation() {
        let initial = observed();
        let mut unchanged = initial.clone();
        begin_observation(&mut unchanged, "w");
        begin_observation(&mut unchanged, "w");
        assert_eq!(
            unchanged, initial,
            "same observation retains notification dedupe"
        );
        for (pointer, value) in [
            ("/active_team_key", json!("b")),
            ("/agents/w/rollout_path", json!("/fixture/other.jsonl")),
            ("/agents/w/spawn_epoch", json!(2)),
            ("/agents/w/provider", json!("claude")),
            ("/agents/w/spawned_at", json!("new-incarnation")),
        ] {
            let mut changed = initial.clone();
            if pointer.ends_with("spawned_at") {
                changed["agents"]["w"]["spawned_at"] = value;
            } else {
                *changed.pointer_mut(pointer).unwrap() = value;
            }
            begin_observation(&mut changed, "w");
            assert!(changed["coordinator"]["abnormal_exit_watch"]
                .get("w")
                .is_none());
        }
    }

    #[test]
    fn legacy_unknown_identity_is_retained_until_member_is_observed() {
        let mut state = observed();
        state["coordinator"]["abnormal_exit_watch"]["w"]
            .as_object_mut()
            .unwrap()
            .remove("watch_identity");
        let legacy = state.clone();
        compact_after_merge(&mut state, None, &[]);
        assert_eq!(
            state, legacy,
            "membership compaction cannot guess legacy identity"
        );
        begin_observation(&mut state, "w");
        assert!(state["coordinator"]["abnormal_exit_watch"]
            .get("w")
            .is_none());
    }

    #[test]
    fn unchanged_normal_save_compacts_large_old_snapshots_and_cannot_resurrect_them() {
        let ws = workspace();
        let watch: serde_json::Map<String, Value> = (0..100)
            .map(|n| {
                (
                    format!("old-{n}"),
                    json!({"last_notified_key": "x".repeat(100)}),
                )
            })
            .collect();
        let mut state =
            json!({"agents": {}, "teams": {}, "coordinator": {"abnormal_exit_watch": watch}});
        for n in 0..8 {
            state["teams"][format!("team-{n}")] =
                json!({"agents": {}, "coordinator": state["coordinator"].clone()});
        }
        let original = serde_json::to_vec(&state).unwrap().len();
        // Warm the old cache without invoking the load-time migrations, which
        // could otherwise compact before the no-change save under test.
        crate::state::persist::save_runtime_state_without_migrations(&ws, &state).unwrap();
        let stale = state;
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(runtime_state_path(&ws)).unwrap())
                .unwrap(),
            stale
        );
        save_runtime_state(&ws, &stale).unwrap();
        let first = std::fs::read(runtime_state_path(&ws)).unwrap();
        assert!(
            first.len() < original / 10,
            "size must depend on actual members, not watch x teams"
        );
        save_runtime_state(&ws, &stale).unwrap();
        assert_eq!(std::fs::read(runtime_state_path(&ws)).unwrap(), first);
        std::fs::remove_dir_all(ws).unwrap();
    }

    #[test]
    fn latest_roster_merge_precedes_pruning_and_explicit_deletion_removes_watch() {
        let ws = workspace();
        let latest = json!({"active_team_key": "a", "session_name": "team-a", "agents": {
            "w": {"agent_id": "w", "provider": "codex", "status": "paused"}
        }, "coordinator": {"abnormal_exit_watch": {"w": {"last_notified_key": "keep"}}}});
        save_runtime_state(&ws, &latest).unwrap();
        let mut stale = load_runtime_state(&ws).unwrap();
        stale["agents"] = json!({});
        save_runtime_state(&ws, &stale).unwrap();
        let merged = load_runtime_state(&ws).unwrap();
        assert!(merged["agents"].get("w").is_some());
        assert_eq!(
            merged["coordinator"]["abnormal_exit_watch"]["w"]["last_notified_key"],
            "keep"
        );
        crate::state::persist::save_runtime_state_with_deleted_agents(&ws, &stale, &["w"]).unwrap();
        let removed = load_runtime_state(&ws).unwrap();
        assert!(removed["agents"].get("w").is_none());
        assert_eq!(removed["coordinator"]["abnormal_exit_watch"], json!({}));
        std::fs::remove_dir_all(ws).unwrap();
    }

    #[test]
    fn unknown_team_or_generation_retains_history_without_authorizing_reuse() {
        for pointer in ["/active_team_key", "/agents/w/spawn_epoch"] {
            let mut state = observed();
            *state.pointer_mut(pointer).unwrap() = Value::Null;
            let before = state.clone();
            assert!(observation_identity(&state, "w").is_none());
            begin_observation(&mut state, "w");
            assert_eq!(state, before);
            assert!(!matches_observation(
                &state,
                "w",
                &state["coordinator"]["abnormal_exit_watch"]["w"]
            ));
        }
    }

    #[test]
    fn malformed_team_catalog_does_not_authorize_root_pruning() {
        let mut state =
            json!({"agents": {}, "teams": [], "coordinator": {"abnormal_exit_watch": {"w": {}}}});
        let before = state.clone();
        compact_after_merge(&mut state, None, &[]);
        assert_eq!(state, before);
        assert!(!needs_compaction(&state));
    }

    #[test]
    fn lock_held_latest_membership_protects_rows_without_roster_stub_fields() {
        let latest = json!({"agents": {"unknown-row": {}}, "teams": {
            "new-sibling": {"agents": {"sibling": {}}}
        }});
        let mut incoming = json!({"agents": {}, "coordinator": {"abnormal_exit_watch": {
            "unknown-row": {}, "sibling": {}, "gone": {}
        }}});
        compact_after_merge(&mut incoming, Some(&latest), &[]);
        assert_eq!(
            incoming["coordinator"]["abnormal_exit_watch"],
            json!({"unknown-row": {}, "sibling": {}})
        );
        compact_after_merge(&mut incoming, Some(&latest), &["unknown-row"]);
        assert_eq!(
            incoming["coordinator"]["abnormal_exit_watch"],
            json!({"sibling": {}})
        );
    }
}
