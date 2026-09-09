//! Single binding fact for one quick-start/restart/diagnose operation.
//! Public status and repair actions must be projected from this type.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::registry::{registry_dir, workspace_hash, LeaderRegistryEntry};
use crate::provider::Provider;
use crate::state::persist::load_runtime_state;
use crate::state::projection::team_state_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    Bound,
    ExistingBound,
    PartiallyStarted,
    Unknown,
    Unbound,
    Conflict,
    InvalidIdentity,
    ProviderUnresolved,
    TopologyMismatch,
    GrantRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryView {
    Attached,
    Unbound,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverView {
    Attached,
    Pending,
    Unbound,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAction {
    pub id: &'static str,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingFact {
    pub kind: BindingKind,
    pub stage: Option<String>,
    pub reason: String,
    pub workers_spawned: bool,
    pub registry: RegistryView,
    pub state_receiver: ReceiverView,
    pub owner_pane: Option<String>,
    pub caller_pane: Option<String>,
    pub action: Option<BindingAction>,
}

impl Default for BindingFact {
    fn default() -> Self {
        Self::unknown("binding_not_observed")
    }
}

impl BindingFact {
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            kind: BindingKind::Unknown,
            stage: Some("registry_or_probe".to_string()),
            reason: reason.into(),
            workers_spawned: false,
            registry: RegistryView::Unknown,
            state_receiver: ReceiverView::Absent,
            owner_pane: None,
            caller_pane: None,
            action: Some(BindingAction {
                id: "diagnose",
                command: Some("team-agent diagnose --json".to_string()),
            }),
        }
    }

    pub fn is_deliverable(&self) -> bool {
        matches!(self.kind, BindingKind::Bound | BindingKind::ExistingBound)
            && self.registry == RegistryView::Attached
    }

    pub fn public_status(&self) -> &'static str {
        match self.kind {
            BindingKind::Bound | BindingKind::ExistingBound => "bound",
            BindingKind::PartiallyStarted => "leader_binding_incomplete",
            BindingKind::Unknown => "leader_binding_unknown",
            BindingKind::Unbound => "leader_receiver_unbound",
            BindingKind::Conflict => "leader_owner_conflict",
            BindingKind::InvalidIdentity | BindingKind::ProviderUnresolved => "leader_bind_refused",
            BindingKind::TopologyMismatch => "refused_dirty_topology",
            BindingKind::GrantRejected => "leader_grant_rejected",
        }
    }

    pub fn next_action_value(&self) -> Option<String> {
        self.action.as_ref().and_then(|action| {
            action
                .command
                .clone()
                .or_else(|| Some(action.id.to_string()))
        })
    }

    pub fn next_actions(&self) -> Vec<String> {
        self.next_action_value().into_iter().collect()
    }

    pub fn suggests_claim_or_takeover(&self) -> bool {
        self.action
            .as_ref()
            .is_some_and(|action| matches!(action.id, "claim-leader" | "takeover"))
            || self.next_actions().iter().any(|item| {
                item.contains("claim-leader") || item.contains("takeover")
            })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "stage": self.stage,
            "reason": self.reason,
            "workers_spawned": self.workers_spawned,
            "registry": self.registry,
            "state_receiver": self.state_receiver,
            "owner_pane": self.owner_pane,
            "caller_pane": self.caller_pane,
            "deliverable": self.is_deliverable(),
            "status": self.public_status(),
            "next_action": self.next_action_value(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerPreflight {
    NoPane,
    Ready { provider: Provider },
    Invalid { reason: String },
    ProviderUnresolved { reason: String },
}

pub fn preflight_caller() -> CallerPreflight {
    preflight_caller_from_env(|key| std::env::var(key).ok())
}

pub fn preflight_caller_from_env(env: impl Fn(&str) -> Option<String>) -> CallerPreflight {
    let tmux_pane = nonempty(env("TMUX_PANE"));
    let env_pane = nonempty(env("TEAM_AGENT_LEADER_PANE_ID"));
    if let (Some(tmux), Some(declared)) = (tmux_pane.as_deref(), env_pane.as_deref()) {
        if tmux != declared {
            return CallerPreflight::Invalid {
                reason: "caller_pane_conflict".to_string(),
            };
        }
    }
    if let Some(raw) = nonempty(env("TEAM_AGENT_LEADER_PROVIDER")) {
        if crate::provider::wire::parse_provider(&raw).is_none() {
            return CallerPreflight::Invalid {
                reason: "invalid_caller_provider".to_string(),
            };
        }
    }
    let pane = tmux_pane.or(env_pane);
    if pane.is_none() {
        return CallerPreflight::NoPane;
    }
    if let Some(raw) = nonempty(env("TEAM_AGENT_LEADER_PROVIDER")) {
        if let Some(provider) = crate::provider::wire::parse_provider(&raw) {
            return CallerPreflight::Ready { provider };
        }
    }
    CallerPreflight::ProviderUnresolved {
        reason: "provider_unresolved".to_string(),
    }
}

pub fn preflight_fact(preflight: &CallerPreflight) -> Option<BindingFact> {
    match preflight {
        CallerPreflight::Invalid { reason } => Some(BindingFact {
            kind: BindingKind::InvalidIdentity,
            stage: Some("strict_provider".to_string()),
            reason: reason.clone(),
            workers_spawned: false,
            registry: RegistryView::Unbound,
            state_receiver: ReceiverView::Absent,
            owner_pane: None,
            caller_pane: nonempty(std::env::var("TMUX_PANE").ok()),
            action: Some(BindingAction {
                id: "fix_caller_identity",
                command: Some(
                    "fix conflicting TEAM_AGENT_LEADER_* / CALLER identity; do not claim-leader"
                        .to_string(),
                ),
            }),
        }),
        CallerPreflight::ProviderUnresolved { reason } => Some(BindingFact {
            kind: BindingKind::ProviderUnresolved,
            stage: Some("strict_provider".to_string()),
            reason: reason.clone(),
            workers_spawned: false,
            registry: RegistryView::Unbound,
            state_receiver: ReceiverView::Absent,
            owner_pane: None,
            caller_pane: nonempty(std::env::var("TMUX_PANE").ok()),
            action: Some(BindingAction {
                id: "supply_provider",
                command: Some(
                    "set TEAM_AGENT_LEADER_PROVIDER or run from a provider pane; do not claim-leader"
                        .to_string(),
                ),
            }),
        }),
        CallerPreflight::NoPane | CallerPreflight::Ready { .. } => None,
    }
}

pub fn observe_leader_binding(workspace: &Path, team_key: &str) -> BindingFact {
    let caller_pane = nonempty(std::env::var("TMUX_PANE").ok());
    let registry = registry_view(workspace, team_key);
    let state = match load_runtime_state(workspace) {
        Ok(state) => state,
        Err(_) => {
            return BindingFact {
                kind: BindingKind::Unknown,
                stage: Some("state".to_string()),
                reason: "runtime_state_unreadable".to_string(),
                workers_spawned: false,
                registry,
                state_receiver: ReceiverView::Absent,
                owner_pane: None,
                caller_pane,
                action: Some(BindingAction {
                    id: "diagnose",
                    command: Some("team-agent diagnose --json".to_string()),
                }),
            };
        }
    };
    let team_state = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    let owner_pane = string_field(team_state, &["team_owner", "pane_id"])
        .or_else(|| string_field(&state, &["team_owner", "pane_id"]));
    let receiver = team_state
        .get("leader_receiver")
        .or_else(|| state.get("leader_receiver"));
    let state_receiver = match receiver.and_then(|value| value.get("status")).and_then(Value::as_str)
    {
        Some("attached") => ReceiverView::Attached,
        Some("pending") => ReceiverView::Pending,
        Some("unbound") => ReceiverView::Unbound,
        _ => ReceiverView::Absent,
    };
    let workers_spawned = team_state
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| !agents.is_empty());

    if registry == RegistryView::Unknown {
        return BindingFact {
            kind: BindingKind::Unknown,
            stage: Some("registry".to_string()),
            reason: "leader_registry_undecidable".to_string(),
            workers_spawned,
            registry,
            state_receiver,
            owner_pane,
            caller_pane,
            action: Some(BindingAction {
                id: "diagnose",
                command: Some("team-agent diagnose --json".to_string()),
            }),
        };
    }

    if registry == RegistryView::Attached {
        return BindingFact {
            kind: if owner_pane
                .as_ref()
                .zip(caller_pane.as_ref())
                .is_some_and(|(owner, caller)| owner == caller)
            {
                BindingKind::ExistingBound
            } else {
                BindingKind::Bound
            },
            stage: None,
            reason: "leader_receiver_attached".to_string(),
            workers_spawned,
            registry,
            state_receiver,
            owner_pane,
            caller_pane,
            action: None,
        };
    }

    if let (Some(owner), Some(caller)) = (owner_pane.as_ref(), caller_pane.as_ref()) {
        if owner == caller {
            return BindingFact {
                kind: BindingKind::ExistingBound,
                stage: Some("discovery_index".to_string()),
                reason: "owner_matches_caller_registry_missing".to_string(),
                workers_spawned,
                registry,
                state_receiver,
                owner_pane,
                caller_pane,
                action: None,
            };
        }
        return BindingFact {
            kind: BindingKind::Conflict,
            stage: Some("owner".to_string()),
            reason: "foreign_live_owner".to_string(),
            workers_spawned,
            registry,
            state_receiver,
            owner_pane,
            caller_pane,
            action: Some(BindingAction {
                id: "takeover",
                command: Some(format!(
                    "team-agent takeover --team {} --confirm --json",
                    if team_key.is_empty() {
                        team_state_key(&state)
                    } else {
                        team_key.to_string()
                    }
                )),
            }),
        };
    }

    if owner_pane.is_some() && caller_pane.is_none() {
        return BindingFact {
            kind: BindingKind::PartiallyStarted,
            stage: Some("caller_pane".to_string()),
            reason: "owner_recorded_but_caller_not_in_tmux".to_string(),
            workers_spawned,
            registry,
            state_receiver,
            owner_pane,
            caller_pane,
            action: Some(BindingAction {
                id: "run_from_owner_pane",
                command: Some(
                    "run team-agent from the recorded owner tmux pane; do not claim-leader"
                        .to_string(),
                ),
            }),
        };
    }

    if workers_spawned {
        return BindingFact {
            kind: BindingKind::PartiallyStarted,
            stage: Some("post_spawn".to_string()),
            reason: "workers_started_leader_not_committed".to_string(),
            workers_spawned,
            registry,
            state_receiver,
            owner_pane,
            caller_pane,
            action: Some(BindingAction {
                id: "run_from_leader_pane",
                command: Some(
                    "run from the intended leader tmux pane with a resolvable provider; do not claim-leader"
                        .to_string(),
                ),
            }),
        };
    }

    let action = if caller_pane.is_some() {
        Some(BindingAction {
            id: "claim-leader",
            command: Some(format!(
                "team-agent claim-leader --team {} --confirm --json",
                if team_key.is_empty() {
                    team_state_key(&state)
                } else {
                    team_key.to_string()
                }
            )),
        })
    } else {
        Some(BindingAction {
            id: "run_from_tmux_pane",
            command: Some(
                "run team-agent from the leader tmux pane; claim-leader is not the next step"
                    .to_string(),
            ),
        })
    };
    BindingFact {
        kind: BindingKind::Unbound,
        stage: Some("ownership".to_string()),
        reason: "no_attached_owner".to_string(),
        workers_spawned,
        registry,
        state_receiver,
        owner_pane,
        caller_pane,
        action,
    }
}

pub fn dirty_topology_actions(reason: &str, issue_ids: &[String], team: &str) -> Vec<String> {
    if reason == "worker_session_is_leader_session"
        || issue_ids
            .iter()
            .any(|id| id == "worker_session_is_leader_session")
    {
        return vec![
            "repair state.session_name to the worker session; it currently names the leader launcher session".to_string(),
            format!("team-agent diagnose --team {team} --json"),
        ];
    }
    let mut actions = vec![format!("team-agent diagnose --team {team} --json")];
    if issue_ids.iter().any(|id| id.contains("socket") || id.contains("endpoint")) {
        actions.push(
            "repair tmux endpoint/socket split from the intended leader socket".to_string(),
        );
    }
    if issue_ids.iter().any(|id| id.contains("owner_conflict") || id.contains("foreign_owner"))
    {
        actions.push(format!(
            "team-agent takeover --team {team} --confirm --json"
        ));
    }
    actions
}

pub fn apply_binding_fields(value: &mut Value, fact: &BindingFact) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.insert("binding".to_string(), fact.to_json());
    object
        .entry("reason")
        .or_insert_with(|| json!(fact.reason));
    if let Some(next) = fact.next_action_value() {
        object.insert("next_action".to_string(), json!(next));
        if !object.contains_key("next_actions") {
            object.insert("next_actions".to_string(), json!(fact.next_actions()));
        }
    } else {
        object.remove("next_action");
        if object
            .get("next_actions")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_str().is_some_and(|text| {
                        text.contains("claim-leader") || text.contains("takeover")
                    })
                })
            })
        {
            object.insert("next_actions".to_string(), json!([]));
        }
    }
}

pub fn registry_view(workspace: &Path, team_key: &str) -> RegistryView {
    let Some(dir) = registry_dir() else {
        return RegistryView::Unknown;
    };
    if team_key.is_empty() {
        return RegistryView::Unbound;
    }
    let hash = workspace_hash(workspace);
    let path = dir.join(format!("{hash}__{team_key}.json"));
    match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RegistryView::Unbound,
        Err(_) => RegistryView::Unknown,
        Ok(text) => match serde_json::from_str::<LeaderRegistryEntry>(&text) {
            Err(_) => RegistryView::Unknown,
            Ok(entry) => {
                if entry.status != "attached" {
                    return RegistryView::Unbound;
                }
                if let Some(authorized) = entry
                    .channel
                    .get("authorized_team_workspace")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    if !crate::state::owner_gate::workspace_paths_match(authorized, workspace) {
                        return RegistryView::Unbound;
                    }
                }
                RegistryView::Attached
            }
        },
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|raw| !raw.is_empty())
}

fn string_field(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

pub fn grant_authorizes_workspace(workspace: &Path, state: &Value, observed_nonce: Option<&str>) -> bool {
    let receiver = state
        .get("leader_receiver")
        .or_else(|| {
            state
                .get("teams")
                .and_then(Value::as_object)
                .and_then(|teams| teams.values().find_map(|team| team.get("leader_receiver")))
        });
    let Some(receiver) = receiver else {
        return false;
    };
    let authority = receiver
        .get("scope_authority")
        .and_then(Value::as_str)
        .unwrap_or("");
    if authority != "explicit_claim" && authority != "fresh_caller" {
        return false;
    }
    let Some(authorized) = receiver
        .get("authorized_team_workspace")
        .and_then(Value::as_str)
        .filter(|raw| !raw.is_empty())
    else {
        return false;
    };
    if crate::state::owner_gate::workspace_paths_match(Path::new(authorized), workspace) {
        let recorded = receiver
            .get("binding_nonce")
            .and_then(Value::as_str)
            .filter(|raw| !raw.is_empty());
        return match (recorded, observed_nonce) {
            (Some(recorded), Some(live)) => recorded == live,
            (Some(_), None) => false,
            (None, _) => false,
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_unresolved_does_not_recommend_claim() {
        let fact = preflight_fact(&CallerPreflight::ProviderUnresolved {
            reason: "provider_unresolved".to_string(),
        })
        .expect("fact");
        assert_eq!(fact.kind, BindingKind::ProviderUnresolved);
        assert_eq!(fact.public_status(), "leader_bind_refused");
        assert!(!fact.suggests_claim_or_takeover());
        assert!(fact
            .next_action_value()
            .is_some_and(|action| action.contains("TEAM_AGENT_LEADER_PROVIDER")));
    }

    #[test]
    fn invalid_tuple_does_not_recommend_claim() {
        let fact = preflight_fact(&CallerPreflight::Invalid {
            reason: "caller_pane_conflict".to_string(),
        })
        .expect("fact");
        assert_eq!(fact.kind, BindingKind::InvalidIdentity);
        assert!(!fact.suggests_claim_or_takeover());
    }

    #[test]
    fn conflicting_pane_env_is_invalid() {
        let preflight = preflight_caller_from_env(|key| match key {
            "TMUX_PANE" => Some("%1".to_string()),
            "TEAM_AGENT_LEADER_PANE_ID" => Some("%2".to_string()),
            _ => None,
        });
        assert!(matches!(preflight, CallerPreflight::Invalid { .. }));
    }

    #[test]
    fn unparsable_provider_env_is_invalid_not_fallback() {
        let preflight = preflight_caller_from_env(|key| match key {
            "TMUX_PANE" => Some("%1".to_string()),
            "TEAM_AGENT_LEADER_PROVIDER" => Some("not-a-provider".to_string()),
            _ => None,
        });
        assert!(matches!(preflight, CallerPreflight::Invalid { .. }));
    }

    #[test]
    fn missing_provider_with_pane_is_unresolved() {
        let preflight = preflight_caller_from_env(|key| match key {
            "TMUX_PANE" => Some("%1".to_string()),
            _ => None,
        });
        assert!(matches!(
            preflight,
            CallerPreflight::ProviderUnresolved { .. }
        ));
    }

    #[test]
    fn worker_session_mismatch_does_not_claim() {
        let actions = dirty_topology_actions(
            "worker_session_is_leader_session",
            &["worker_session_is_leader_session".to_string()],
            "alpha",
        );
        assert!(actions.iter().all(|item| !item.contains("claim-leader")));
        assert!(actions.iter().all(|item| !item.contains("takeover")));
        assert!(actions.iter().any(|item| item.contains("session_name")));
    }

    #[test]
    fn registry_unknown_does_not_claim() {
        let fact = BindingFact::unknown("leader_registry_undecidable");
        assert_eq!(fact.kind, BindingKind::Unknown);
        assert!(!fact.suggests_claim_or_takeover());
        assert!(fact
            .next_action_value()
            .is_some_and(|action| action.contains("diagnose")));
    }

    #[test]
    fn bound_projection_has_no_claim_action() {
        let fact = BindingFact {
            kind: BindingKind::ExistingBound,
            stage: None,
            reason: "owner_matches_caller_registry_missing".to_string(),
            workers_spawned: true,
            registry: RegistryView::Unbound,
            state_receiver: ReceiverView::Pending,
            owner_pane: Some("%1".to_string()),
            caller_pane: Some("%1".to_string()),
            action: None,
        };
        assert!(!fact.suggests_claim_or_takeover());
        assert_eq!(fact.next_action_value(), None);
    }
}
