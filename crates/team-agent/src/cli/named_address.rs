//! CLI-local stable name resolver for `team-agent send --to-name`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::transport::{PaneInfo, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedTargetKind {
    Worker,
    Leader,
    SessionWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedAddressErrorKind {
    NameInvalid,
    WorkspaceNotFound,
    StateNotFound,
    // E6 (0.5.9 offline-mailbox-toname-design §3.1): three-way taxonomy for
    // `--to-name <team>/leader` refusal — required by the E6 RED so the top-level
    // `leader_receiver` fallback is no longer reached for a wrong runtime key.
    // `WorkspaceNoState` mirrors `StateNotFound` semantics but keeps the wire
    // reason distinct for third-party sender copy.
    WorkspaceNoState,
    TeamKeyNotFound,
    LeaderNotAttached,
    NameNotResolvable,
    NameNotLive,
    NameAmbiguous,
}

impl NamedAddressErrorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NameInvalid => "name_invalid",
            Self::WorkspaceNotFound => "workspace_not_found",
            Self::StateNotFound => "state_not_found",
            Self::WorkspaceNoState => "workspace_no_state",
            Self::TeamKeyNotFound => "team_key_not_found",
            Self::LeaderNotAttached => "leader_not_attached",
            Self::NameNotResolvable => "name_not_resolvable",
            Self::NameNotLive => "name_not_live",
            Self::NameAmbiguous => "name_ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamedAddressError {
    pub kind: NamedAddressErrorKind,
    pub message: String,
    pub action: String,
    pub log: String,
    pub candidates: Vec<Value>,
    // E6: when the caller supplied a `<team>/leader` name that isn't a runtime
    // team_key, echo the requested value + the canonical alternatives so
    // spec/display-name confusion is visibly diagnosed. Empty for other
    // refusal kinds.
    pub requested_team: Option<String>,
    pub available_team_keys: Vec<String>,
    pub suggested_name: Option<String>,
    /// 0.5.45 naming-addressing (design §3.4, RED-2/RED-3): echo the
    /// user's original typo so JSON consumers + N38 human can pair
    /// `requested_name`/`suggested_name` without re-parsing. Absent
    /// for exact refusals like empty-workspace.
    pub requested_name: Option<String>,
}

impl NamedAddressError {
    pub(crate) fn new(kind: NamedAddressErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            action: "inspect team-agent status and use an explicit workspace/team name".to_string(),
            log: "named-address resolver refused before injection".to_string(),
            candidates: Vec::new(),
            requested_team: None,
            available_team_keys: Vec::new(),
            suggested_name: None,
            requested_name: None,
        }
    }

    pub(crate) fn n38_message(&self) -> String {
        // 0.5.45 naming-addressing (design §3.4, RED-3): the N38 first
        // line embeds the human-readable `message` (which contains
        // the original typo like `qa-namng`/`btea`) after the reason.
        // Pre-0.5.45 the line was `Error: <reason>` only; the typo
        // hid in the JSON-only `error` field, so the human line
        // couldn't be scanned to recover the original request.
        format!(
            "Error: {}: {}\nAction: {}\nLog: {}",
            self.kind.as_str(),
            self.message,
            self.action,
            self.log
        )
    }

    /// 0.5.45 (design §3.4, RED-2 named-team): rank team-scope typo
    /// against target workspace `teams.*` keys; candidate `name`
    /// preserves the qualified `team/entity` shape so the copyable
    /// token in Action can be pasted back into `--to-name` unchanged.
    fn with_scoped_suggestions_for_team(
        self,
        state: &Value,
        team_typo: &str,
        entity: &str,
        parsed: &ParsedNamedAddress,
    ) -> Self {
        use crate::model::name_similarity::{rank, Candidate};
        let candidates: Vec<Candidate<(String, String)>> = state
            .get("teams")
            .and_then(Value::as_object)
            .map(|teams| {
                teams
                    .keys()
                    .map(|team_key| Candidate {
                        match_key: team_key.clone(),
                        stable_key: team_key.clone(),
                        payload: (team_key.clone(), entity.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ranked = rank(team_typo, &candidates);
        let requested_display = parsed.display_name();
        let candidate_values: Vec<Value> = ranked
            .iter()
            .map(|(team_key, entity)| {
                let name = format!("{team_key}/{entity}");
                json!({
                    "name": name,
                    "team_key": team_key,
                    "agent_id": entity,
                    "advisory": true,
                })
            })
            .collect();
        let best = ranked
            .first()
            .map(|(team_key, entity)| format!("{team_key}/{entity}"));
        self.with_scoped_suggestions(requested_display, candidate_values, best)
    }

    /// 0.5.45 (design §3.4, RED-2 named-agent): rank agent-scope typo
    /// against the legal `teams[team].agents` keys.
    fn with_scoped_suggestions_for_agent(
        self,
        team_entry: &Value,
        team: &str,
        agent_typo: &str,
        parsed: &ParsedNamedAddress,
    ) -> Self {
        use crate::model::name_similarity::{rank, Candidate};
        let candidates: Vec<Candidate<String>> = team_entry
            .get("agents")
            .and_then(Value::as_object)
            .map(|agents| {
                agents
                    .keys()
                    .map(|agent_id| Candidate {
                        match_key: agent_id.clone(),
                        stable_key: agent_id.clone(),
                        payload: agent_id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ranked = rank(agent_typo, &candidates);
        let requested_display = parsed.display_name();
        let candidate_values: Vec<Value> = ranked
            .iter()
            .map(|agent_id| {
                json!({
                    "name": format!("{team}/{agent_id}"),
                    "team_key": team,
                    "agent_id": agent_id,
                    "advisory": true,
                })
            })
            .collect();
        let best = ranked
            .first()
            .map(|agent_id| format!("{team}/{agent_id}"));
        self.with_scoped_suggestions(requested_display, candidate_values, best)
    }

    /// 0.5.45 naming-addressing (design §3.3/§3.4): attach an
    /// advisory suggestion payload. `requested` is the original typo
    /// (surfaced verbatim in JSON + N38 Action); `ranked_candidates`
    /// carry the scope-safe candidate payloads produced by
    /// `model::name_similarity::rank`. The best candidate populates
    /// `suggested_name` for convenience; the full list appears as
    /// `candidates` in the JSON envelope.
    pub(crate) fn with_scoped_suggestions(
        mut self,
        requested: impl Into<String>,
        ranked_candidates: Vec<Value>,
        best_copyable: Option<String>,
    ) -> Self {
        let requested = requested.into();
        self.requested_name = Some(requested.clone());
        self.candidates = ranked_candidates;
        self.suggested_name = best_copyable.clone();
        // Rewrite Action so the human line points at the actual
        // returned candidate (RED-4). Falls back to a status-JSON
        // hint when no candidate cleared the threshold.
        if let Some(best) = best_copyable {
            self.action = format!(
                "Did you mean `{best}`? Rerun with `--to-name {best}`."
            );
        } else {
            self.action = format!(
                "No close matches for `{requested}`. \
                 Run `team-agent status --json` for the current team_key + agent list \
                 and combine them as `<team>/<agent>` for `--to-name`."
            );
        }
        self
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".to_string(), Value::Bool(false));
        obj.insert("status".to_string(), Value::String("refused".to_string()));
        obj.insert(
            "reason".to_string(),
            Value::String(self.kind.as_str().to_string()),
        );
        obj.insert("error".to_string(), Value::String(self.message.clone()));
        obj.insert("action".to_string(), Value::String(self.action.clone()));
        obj.insert("log".to_string(), Value::String(self.log.clone()));
        obj.insert(
            "candidates".to_string(),
            Value::Array(self.candidates.clone()),
        );
        if let Some(requested) = self.requested_team.as_deref() {
            obj.insert(
                "requested_team".to_string(),
                Value::String(requested.to_string()),
            );
        }
        if !self.available_team_keys.is_empty() {
            obj.insert(
                "available_team_keys".to_string(),
                Value::Array(
                    self.available_team_keys
                        .iter()
                        .map(|k| Value::String(k.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(suggested) = self.suggested_name.as_deref() {
            obj.insert(
                "suggested_name".to_string(),
                Value::String(suggested.to_string()),
            );
        }
        if let Some(requested) = self.requested_name.as_deref() {
            obj.insert(
                "requested_name".to_string(),
                Value::String(requested.to_string()),
            );
        }
        Value::Object(obj)
    }
}

impl std::fmt::Display for NamedAddressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.n38_message())
    }
}

impl std::error::Error for NamedAddressError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedNamedAddress {
    pub raw_name: String,
    pub target_kind: NamedTargetKind,
    pub sender_workspace: PathBuf,
    pub target_workspace: PathBuf,
    pub team_key: Option<String>,
    pub agent_id: Option<String>,
    pub pane_id: String,
    pub session_name: Option<String>,
    pub window_name: Option<String>,
    pub tmux_endpoint: Option<String>,
    pub transport_kind: Option<String>,
    pub app_server: Option<Value>,
    pub state_pane_id: Option<String>,
    pub state_pane_stale: bool,
    pub agent_status: Option<String>,
    pub warning: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct NamedWorkspaceTransport<'a> {
    pub workspace: &'a Path,
    pub transport: &'a dyn Transport,
}

pub(crate) fn resolve_name_with_transport(
    sender_workspace: &Path,
    raw_name: &str,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let parsed = parse_named_address(raw_name)?;
    let target_workspace = resolve_workspace(sender_workspace, parsed.workspace.as_deref())?;
    let state = load_state_checked(&target_workspace)?;
    resolve_in_workspace(
        sender_workspace,
        &target_workspace,
        &state,
        &parsed,
        transport,
    )
}

pub(crate) fn resolve_name_with_workspace_transports(
    sender_workspace: &Path,
    raw_name: &str,
    workspaces: &[NamedWorkspaceTransport<'_>],
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let parsed = parse_named_address(raw_name)?;
    if parsed.workspace.is_some() {
        let target_workspace = resolve_workspace(sender_workspace, parsed.workspace.as_deref())?;
        let Some(entry) = workspaces.iter().find(|entry| {
            resolve_workspace(sender_workspace, Some(entry.workspace))
                .ok()
                .as_ref()
                == Some(&target_workspace)
        }) else {
            return Err(workspace_not_found(&target_workspace));
        };
        let state = load_state_checked(&target_workspace)?;
        return resolve_in_workspace(
            sender_workspace,
            &target_workspace,
            &state,
            &parsed,
            entry.transport,
        );
    }

    let mut successes = Vec::new();
    let mut first_error = None;
    for entry in workspaces {
        let target_workspace = resolve_workspace(sender_workspace, Some(entry.workspace))?;
        match load_state_checked(&target_workspace).and_then(|state| {
            resolve_in_workspace(
                sender_workspace,
                &target_workspace,
                &state,
                &parsed,
                entry.transport,
            )
        }) {
            Ok(resolved) => successes.push(resolved),
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }

    match successes.len() {
        1 => successes.into_iter().next().ok_or_else(|| {
            NamedAddressError::new(
                NamedAddressErrorKind::NameNotResolvable,
                "named address was not resolvable",
            )
        }),
        n if n > 1 => {
            let mut err = NamedAddressError::new(
                NamedAddressErrorKind::NameAmbiguous,
                "name matches multiple workspaces",
            );
            let name = parsed.display_name();
            err.action =
                format!("Use an explicit workspace-qualified name such as <workspace>::{name}");
            err.log = "same team/name is live in multiple workspaces".to_string();
            err.candidates = successes
                .into_iter()
                .map(|resolved| candidate_json(&resolved))
                .collect();
            Err(err)
        }
        _ => Err(first_error.unwrap_or_else(|| {
            NamedAddressError::new(
                NamedAddressErrorKind::NameNotResolvable,
                "named address was not resolvable",
            )
        })),
    }
}

pub(crate) fn resolve_name_for_cli(
    sender_workspace: &Path,
    raw_name: &str,
    bare_team_scope: Option<&str>,
) -> Result<(ResolvedNamedAddress, Box<dyn Transport>), NamedAddressError> {
    let parsed = parse_named_address(raw_name)?;
    // 0.5.45 naming-addressing (design §3.1, RED-1): bare `--to-name`
    // + explicit `--team` normalizes to `ParsedTarget::TeamEntity`
    // BEFORE transport selection. Only fires when:
    //   1. address parsed as bare (no `team/`, no `workspace::`),
    //   2. caller passed a non-empty `bare_team_scope`.
    // Qualified addresses keep the scope in the address (design §1
    // priority: workspace::team/entity > team/entity > bare + --team
    // > bare workspace-unique). resolve_name_with_transport and the
    // registry-to-E6 internal helper deliberately pass `None` so
    // caller-supplied `--team` never overrides a full address.
    let parsed = normalize_bare_with_team_scope(parsed, bare_team_scope);
    let target_workspace = resolve_workspace(sender_workspace, parsed.workspace.as_deref())?;
    let state = load_state_checked(&target_workspace)?;
    let transport = transport_for_cli_target(&target_workspace, &state, &parsed);
    let resolved = resolve_in_workspace(
        sender_workspace,
        &target_workspace,
        &state,
        &parsed,
        transport.as_ref(),
    )?;
    Ok((resolved, transport))
}

/// 0.5.45 naming-addressing (design §3.1): apply bare-team scope归一.
/// Only bare targets with no workspace prefix consume `--team`;
/// qualified addresses keep the scope embedded in the address.
fn normalize_bare_with_team_scope(
    parsed: ParsedNamedAddress,
    bare_team_scope: Option<&str>,
) -> ParsedNamedAddress {
    let Some(scope) = bare_team_scope.map(str::trim).filter(|s| !s.is_empty()) else {
        return parsed;
    };
    if parsed.workspace.is_some() {
        return parsed;
    }
    match parsed.target {
        ParsedTarget::BareAgent(agent) => ParsedNamedAddress {
            workspace: parsed.workspace,
            target: ParsedTarget::TeamEntity {
                team: scope.to_string(),
                entity: agent,
            },
        },
        target => ParsedNamedAddress {
            workspace: parsed.workspace,
            target,
        },
    }
}

/// E6 wiring helper (`cli::send::maybe_enqueue_offline_leader_mailbox`):
/// re-parse a `<workspace>::<team>/leader` name and return the resolved
/// target workspace + canonical team_key so send.rs can enqueue the
/// mailbox without duplicating the parser. Returns `Ok(None)` for names
/// that aren't `<team>/leader` shapes (worker sends, session:window,
/// etc. keep their existing refusal semantics).
pub(crate) fn parse_leader_target_workspace_and_team(
    sender_workspace: &Path,
    raw_name: &str,
) -> Result<Option<(PathBuf, String)>, NamedAddressError> {
    let parsed = parse_named_address(raw_name)?;
    let target_workspace = resolve_workspace(sender_workspace, parsed.workspace.as_deref())?;
    match parsed.target {
        ParsedTarget::TeamEntity { team, entity } if entity == "leader" => {
            let state = load_state_checked(&target_workspace)?;
            let canonical = canonical_named_team_key(&target_workspace, &state, &team)
                .unwrap_or(team);
            Ok(Some((target_workspace, canonical)))
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedNamedAddress {
    workspace: Option<PathBuf>,
    target: ParsedTarget,
}

impl ParsedNamedAddress {
    fn display_name(&self) -> String {
        match &self.target {
            ParsedTarget::BareAgent(agent) => agent.clone(),
            ParsedTarget::TeamEntity { team, entity } => format!("{team}/{entity}"),
            ParsedTarget::SessionWindow { session, window } => format!("{session}:{window}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedTarget {
    BareAgent(String),
    TeamEntity { team: String, entity: String },
    SessionWindow { session: String, window: String },
}

fn parse_named_address(raw_name: &str) -> Result<ParsedNamedAddress, NamedAddressError> {
    let parsed = crate::messaging::parse_logical_address(raw_name).map_err(name_invalid)?;
    let target = match parsed.target {
        crate::messaging::LogicalAddressTarget::Worker(agent) => ParsedTarget::BareAgent(agent),
        crate::messaging::LogicalAddressTarget::TeamEntity { team, entity } => {
            ParsedTarget::TeamEntity { team, entity }
        }
        crate::messaging::LogicalAddressTarget::SessionWindow { session, window } => {
            ParsedTarget::SessionWindow { session, window }
        }
    };
    Ok(ParsedNamedAddress {
        workspace: parsed.workspace,
        target,
    })
}

fn resolve_workspace(
    sender_workspace: &Path,
    explicit: Option<&Path>,
) -> Result<PathBuf, NamedAddressError> {
    let input = explicit.unwrap_or(sender_workspace);
    if explicit.is_some() && !input.exists() {
        return Err(workspace_not_found(input));
    }
    crate::model::paths::canonical_run_workspace(input).map_err(|error| {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::WorkspaceNotFound,
            format!("workspace cannot be resolved: {error}"),
        );
        err.action = "Use an existing Team Agent workspace path before `::`".to_string();
        err.log = format!("workspace={}", input.display());
        err
    })
}

fn load_state_checked(workspace: &Path) -> Result<Value, NamedAddressError> {
    let path = crate::state::persist::runtime_state_path(workspace);
    if !path.exists() {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::StateNotFound,
            "target workspace has no Team Agent runtime state",
        );
        err.action = "Run team-agent quick-start/restart in the target workspace, or choose a workspace that has .team/runtime/state.json".to_string();
        err.log = format!("state_path={}", path.display());
        return Err(err);
    }
    crate::state::persist::load_runtime_state(workspace).map_err(|error| {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::StateNotFound,
            format!("target runtime state cannot be loaded: {error}"),
        );
        err.action = "Inspect or repair the target workspace runtime state".to_string();
        err.log = format!("state_path={}", path.display());
        err
    })
}

fn resolve_in_workspace(
    sender_workspace: &Path,
    target_workspace: &Path,
    state: &Value,
    parsed: &ParsedNamedAddress,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    match &parsed.target {
        ParsedTarget::BareAgent(agent) => {
            resolve_bare_agent(sender_workspace, target_workspace, state, agent, transport)
        }
        ParsedTarget::TeamEntity { team, entity } => {
            let canonical = canonical_named_team_key(target_workspace, state, team)
                .unwrap_or_else(|| team.clone());
            if entity == "leader" {
                resolve_leader(
                    sender_workspace,
                    target_workspace,
                    state,
                    &canonical,
                    parsed,
                    transport,
                )
            } else {
                resolve_worker(
                    sender_workspace,
                    target_workspace,
                    state,
                    &canonical,
                    entity,
                    parsed,
                    transport,
                )
            }
        }
        ParsedTarget::SessionWindow { session, window } => resolve_session_window(
            sender_workspace,
            target_workspace,
            session,
            window,
            parsed,
            transport,
        ),
    }
}

fn resolve_worker(
    sender_workspace: &Path,
    target_workspace: &Path,
    state: &Value,
    team: &str,
    agent: &str,
    parsed: &ParsedNamedAddress,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let team_entry = match team_entry(state, team) {
        Some(entry) => entry,
        None => {
            // 0.5.45 naming-addressing (design §3.4, RED-2 named):
            // typo in team scope. Candidates come from the target
            // workspace's `teams.*.*` — scope-safe because we're
            // already inside the requested workspace projection.
            return Err(name_not_resolvable_team(team)
                .with_scoped_suggestions_for_team(state, team, agent, parsed));
        }
    };
    let agent_entry = match team_entry
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|agents| agents.get(agent))
    {
        Some(entry) => entry,
        None => {
            // 0.5.45 naming-addressing (design §3.4, RED-2 named):
            // typo in agent within a legal team. Candidates come from
            // `teams[team].agents` — scope stays in the requested
            // team, so sibling teams cannot leak.
            return Err(name_not_resolvable_agent(team, agent)
                .with_scoped_suggestions_for_agent(team_entry, team, agent, parsed));
        }
    };
    let Some(session) = string_field(team_entry, "session_name") else {
        return Ok(resolved_worker_without_live(
            sender_workspace,
            target_workspace,
            team,
            agent,
            parsed,
            agent_entry,
            None,
            agent,
            transport.tmux_endpoint(),
        ));
    };
    let window = string_field(agent_entry, "window")
        .or_else(|| string_field(agent_entry, "window_name"))
        .unwrap_or(agent);
    let targets = match list_targets(transport) {
        Ok(targets) => targets,
        Err(_) => {
            return Ok(resolved_worker_without_live(
                sender_workspace,
                target_workspace,
                team,
                agent,
                parsed,
                agent_entry,
                Some(session),
                window,
                transport.tmux_endpoint(),
            ));
        }
    };
    let matches = matching_session_window(&targets, session, window);
    match matches.len() {
        1 => {
            let pane = matches[0];
            let state_pane_id = string_field(agent_entry, "pane_id").map(str::to_string);
            let state_pane_stale = state_pane_id
                .as_deref()
                .is_some_and(|state_pane| state_pane != pane.pane_id.as_str());
            let agent_status = string_field(agent_entry, "status").map(str::to_string);
            let warning = agent_status
                .as_deref()
                .filter(|status| *status == "paused" || *status == "stopped")
                .map(|status| {
                    format!("agent_status:{status}; pane is live, so delivery is allowed")
                });
            Ok(ResolvedNamedAddress {
                raw_name: parsed.display_name(),
                target_kind: NamedTargetKind::Worker,
                sender_workspace: sender_workspace.to_path_buf(),
                target_workspace: target_workspace.to_path_buf(),
                team_key: Some(team.to_string()),
                agent_id: Some(agent.to_string()),
                pane_id: pane.pane_id.as_str().to_string(),
                session_name: Some(session.to_string()),
                window_name: Some(window.to_string()),
                tmux_endpoint: transport.tmux_endpoint(),
                transport_kind: Some("direct_tmux".to_string()),
                app_server: None,
                state_pane_id,
                state_pane_stale,
                agent_status,
                warning,
            })
        }
        0 => Ok(resolved_worker_without_live(
            sender_workspace,
            target_workspace,
            team,
            agent,
            parsed,
            agent_entry,
            Some(session),
            window,
            transport.tmux_endpoint(),
        )),
        _ => Err(name_ambiguous(
            "multiple live panes match the same session/window",
            matches
                .into_iter()
                .map(|pane| {
                    json!({
                        "name": format!("{team}/{agent}"),
                        "workspace": target_workspace.display().to_string(),
                        "team_key": team,
                        "agent_id": agent,
                        "session": session,
                        "window": window,
                        "pane_id": pane.pane_id.as_str(),
                    })
                })
                .collect(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolved_worker_without_live(
    sender_workspace: &Path,
    target_workspace: &Path,
    team: &str,
    agent: &str,
    parsed: &ParsedNamedAddress,
    agent_entry: &Value,
    session: Option<&str>,
    window: &str,
    tmux_endpoint: Option<String>,
) -> ResolvedNamedAddress {
    let state_pane_id = string_field(agent_entry, "pane_id").map(str::to_string);
    ResolvedNamedAddress {
        raw_name: parsed.display_name(),
        target_kind: NamedTargetKind::Worker,
        sender_workspace: sender_workspace.to_path_buf(),
        target_workspace: target_workspace.to_path_buf(),
        team_key: Some(team.to_string()),
        agent_id: Some(agent.to_string()),
        pane_id: state_pane_id.clone().unwrap_or_default(),
        session_name: session.map(str::to_string),
        window_name: Some(window.to_string()),
        tmux_endpoint,
        transport_kind: Some("direct_tmux".to_string()),
        app_server: None,
        state_pane_id,
        state_pane_stale: true,
        agent_status: string_field(agent_entry, "status").map(str::to_string),
        warning: Some(
            "agent has no live pane; message will be persisted for standard delivery recovery"
                .to_string(),
        ),
    }
}

fn resolve_bare_agent(
    sender_workspace: &Path,
    target_workspace: &Path,
    state: &Value,
    agent: &str,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let Some(teams) = state.get("teams").and_then(Value::as_object) else {
        return Err(name_not_resolvable("state has no teams map"));
    };
    let mut live = Vec::new();
    let mut state_matches = 0usize;
    for (team, team_entry) in teams {
        let Some(agents) = team_entry.get("agents").and_then(Value::as_object) else {
            continue;
        };
        if agents.get(agent).is_none() {
            continue;
        }
        state_matches += 1;
        let parsed = ParsedNamedAddress {
            workspace: None,
            target: ParsedTarget::TeamEntity {
                team: team.clone(),
                entity: agent.to_string(),
            },
        };
        if let Ok(resolved) = resolve_worker(
            sender_workspace,
            target_workspace,
            state,
            team,
            agent,
            &parsed,
            transport,
        ) {
            live.push(resolved);
        }
    }
    match live.len() {
        1 => live
            .into_iter()
            .next()
            .ok_or_else(|| name_not_resolvable("named address resolved no live candidates")),
        n if n > 1 => Err(name_ambiguous(
            "bare agent name is live in multiple teams",
            live.into_iter()
                .map(|resolved| candidate_json(&resolved))
                .collect(),
        )),
        _ if state_matches > 0 => {
            let mut err = NamedAddressError::new(
                NamedAddressErrorKind::NameNotLive,
                "agent exists in state but no live pane matched",
            );
            err.action =
                format!("Use an explicit team name or restart the target agent: <team>/{agent}");
            err.log = format!("workspace={} agent_id={agent}", target_workspace.display());
            Err(err)
        }
        _ => Err(name_not_resolvable(&format!(
            "agent `{agent}` is not present in any team"
        ))),
    }
}

/// Phase-DX E3 (plan §4 / CR supplements A/B/C, P0 red lines #1 & #6):
/// build advisory diagnostic candidates for a failed `--to-name leader`
/// resolution. The candidate list is deliberately shaped to be **read by
/// humans**, not consumed as route authority: every entry carries
/// `advisory: true` and a `source` tag identifying where it was observed.
///
/// Candidate source rules:
/// - The transport passed in is already scoped by [`transport_for_cli_target`]
///   to the team's recorded `leader_receiver.tmux_socket` (or the workspace
///   default when no socket was recorded), so `transport.list_targets()`
///   enumerates only that endpoint. We do NOT scan other tmux sockets or
///   fall back to reverse-authority tmux-session discovery.
/// - `pane_id` fields are copied verbatim from `list_targets`. `session` is
///   the tmux session, and `socket` records the tmux endpoint the candidate
///   was seen on (whichever the scoped transport reports).
///
/// Callers must never treat any candidate as authoritative; MUST-NOT-12 stays
/// intact — resolver refusal is still the outcome even when candidates fire.
fn leader_advisory_candidates(
    state: &Value,
    team: &str,
    transport: &dyn Transport,
    source: &str,
) -> Vec<Value> {
    let team_scoped_socket = team_entry(state, team)
        .and_then(|entry| entry.get("leader_receiver"))
        .and_then(|receiver| string_field(receiver, "tmux_socket"));
    let socket = match team_scoped_socket {
        Some(s) => Some(s.to_string()),
        None => state
            // ALLOWED-LEGACY-SINGLE-TEAM: diagnostic-only socket label for the advisory candidates; never fed back into routing.
            .get("leader_receiver")
            .and_then(|receiver| string_field(receiver, "tmux_socket"))
            .map(str::to_string)
            .or_else(|| transport.tmux_endpoint()),
    };
    let targets = match transport.list_targets() {
        Ok(targets) => targets,
        Err(_) => return Vec::new(),
    };
    targets
        .into_iter()
        .map(|pane| {
            json!({
                "pane_id": pane.pane_id.as_str(),
                "session": pane.session.as_str(),
                "window": pane.window_name.as_ref().map(|w| w.as_str()),
                "socket": socket,
                "source": source,
                "advisory": json!(true),
            })
        })
        .collect()
}

fn resolve_leader(
    sender_workspace: &Path,
    target_workspace: &Path,
    state: &Value,
    team: &str,
    parsed: &ParsedNamedAddress,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    // E6 taxonomy split (0.5.9 offline-mailbox-toname-design §3.1): before
    // reading any leader_receiver, decide whether `<team>` is actually the
    // runtime team key. Wrong keys must not fall through to top-level
    // leader_receiver (that was the pre-fix bug where `twitter-autopub/leader`
    // silently reused the `current` team's receiver and reported a stale
    // pane_id). Legacy single-team compat is preserved only when the state
    // has no `teams` map yet and the requested key equals `active_team_key`.
    let receiver =
        resolve_team_scoped_leader_receiver(state, team, target_workspace)?.ok_or_else(|| {
            let mut err = leader_not_attached_third_party(
                target_workspace,
                team,
                None,
                None,
                None,
                transport.tmux_endpoint(),
            );
            err.candidates =
                leader_advisory_candidates(state, team, transport, "state_recorded_socket");
            err
        })?;
    if let Some((mode, transport_kind)) = receiver_transport_conflict(receiver) {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::NameNotLive,
            "leader_receiver mode/transport_kind conflict",
        );
        err.action =
            "Rebind the leader: team-agent claim-leader, takeover, or attach-app-server-leader"
                .to_string();
        err.log = format!(
            "name={team}/leader mode={mode} transport_kind={transport_kind} expected=single_transport_kind"
        );
        return Err(err);
    }
    if crate::codex_app_server::receiver_is_app_server(receiver) {
        return resolve_app_server_leader(
            sender_workspace,
            target_workspace,
            team,
            parsed,
            receiver,
        );
    }
    let pane_id = string_field(receiver, "pane_id")
        .filter(|pane| !pane.is_empty())
        .ok_or_else(|| {
            let mut err = leader_not_attached_third_party(
                target_workspace,
                team,
                None,
                None,
                None,
                transport.tmux_endpoint(),
            );
            err.candidates =
                leader_advisory_candidates(state, team, transport, "state_recorded_socket");
            err
        })?;
    let session = string_field(receiver, "session_name");
    let window = string_field(receiver, "window_name");
    let socket = string_field(receiver, "tmux_socket")
        .map(str::to_string)
        .or_else(|| transport.tmux_endpoint());
    let targets = list_targets(transport)?;
    let matches = targets
        .iter()
        .filter(|target| {
            target.pane_id.as_str() == pane_id
                && session.is_none_or(|expected| target.session.as_str() == expected)
                && window.is_none_or(|expected| {
                    target
                        .window_name
                        .as_ref()
                        .is_some_and(|name| name.as_str() == expected)
                })
        })
        .collect::<Vec<_>>();
    let stale_pane_seen = targets
        .iter()
        .any(|target| target.pane_id.as_str() == pane_id);
    match matches.len() {
        1 => Ok(ResolvedNamedAddress {
            raw_name: parsed.display_name(),
            target_kind: NamedTargetKind::Leader,
            sender_workspace: sender_workspace.to_path_buf(),
            target_workspace: target_workspace.to_path_buf(),
            team_key: Some(team.to_string()),
            agent_id: None,
            pane_id: pane_id.to_string(),
            session_name: session.map(str::to_string),
            window_name: window.map(str::to_string),
            tmux_endpoint: socket,
            transport_kind: Some("direct_tmux".to_string()),
            app_server: None,
            state_pane_id: Some(pane_id.to_string()),
            state_pane_stale: false,
            agent_status: None,
            warning: None,
        }),
        0 => {
            let mut err = leader_not_attached_third_party(
                target_workspace,
                team,
                Some(pane_id),
                session,
                window,
                socket,
            );
            if stale_pane_seen {
                err.log = format!("{} stale_tuple_mismatch=true", err.log);
            }
            err.candidates =
                leader_advisory_candidates(state, team, transport, "state_recorded_socket");
            Err(err)
        }
        _ => Err(name_ambiguous(
            "multiple live panes matched leader_receiver pane id",
            matches
                .into_iter()
                .map(|pane| {
                    json!({
                        "name": format!("{team}/leader"),
                        "workspace": target_workspace.display().to_string(),
                        "team_key": team,
                        "pane_id": pane.pane_id.as_str(),
                        "session": pane.session.as_str(),
                        "window": pane.window_name.as_ref().map(|w| w.as_str()),
                    })
                })
                .collect(),
        )),
    }
}

fn receiver_transport_conflict(receiver: &Value) -> Option<(String, String)> {
    let mode = receiver.get("mode").and_then(Value::as_str)?;
    let transport_kind = receiver.get("transport_kind").and_then(Value::as_str)?;
    if !mode.is_empty() && !transport_kind.is_empty() && mode != transport_kind {
        return Some((mode.to_string(), transport_kind.to_string()));
    }
    None
}

fn resolve_app_server_leader(
    sender_workspace: &Path,
    target_workspace: &Path,
    team: &str,
    parsed: &ParsedNamedAddress,
    receiver: &Value,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let expected = crate::codex_app_server::binding_from_receiver(receiver).map_err(|error| {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::NameNotLive,
            format!("app-server leader receiver is incomplete: {error}"),
        );
        err.action = format!("Rebind the leader: team-agent attach-app-server-leader --team {team} --socket <socket> --thread-id <thread_id>");
        err.log = format!("name={team}/leader expected=leader_receiver.app_server");
        err
    })?;
    let actual = crate::codex_app_server::attach_probe(&expected.socket, &expected.thread_id)
        .map_err(|error| {
            let mut err = NamedAddressError::new(
                NamedAddressErrorKind::NameNotLive,
                format!("app-server leader thread is not live: {error}"),
            );
            err.action = format!("Rebind the leader: team-agent attach-app-server-leader --team {team} --socket {} --thread-id {}", expected.socket, expected.thread_id);
            err.log = format!(
                "name={team}/leader socket={} thread_id={} reason={}",
                expected.socket,
                expected.thread_id,
                error
            );
            err
        })?;
    if actual.thread_id != expected.thread_id
        || actual.session_id != expected.session_id
        || actual.cwd != expected.cwd
        || actual.cli_version != expected.cli_version
    {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::NameNotLive,
            "app-server leader thread identity tuple is stale",
        );
        err.action = format!("Rebind the leader: team-agent attach-app-server-leader --team {team} --socket {} --thread-id {}", expected.socket, expected.thread_id);
        err.log = format!(
            "name={team}/leader expected_thread={} actual_thread={} expected_session={} actual_session={} expected_cwd={} actual_cwd={}",
            expected.thread_id,
            actual.thread_id,
            expected.session_id,
            actual.session_id,
            expected.cwd,
            actual.cwd
        );
        return Err(err);
    }
    Ok(ResolvedNamedAddress {
        raw_name: parsed.display_name(),
        target_kind: NamedTargetKind::Leader,
        sender_workspace: sender_workspace.to_path_buf(),
        target_workspace: target_workspace.to_path_buf(),
        team_key: Some(team.to_string()),
        agent_id: None,
        pane_id: String::new(),
        session_name: None,
        window_name: None,
        tmux_endpoint: None,
        transport_kind: Some("codex_app_server".to_string()),
        app_server: receiver.get("app_server").cloned(),
        state_pane_id: None,
        state_pane_stale: false,
        agent_status: None,
        warning: None,
    })
}

fn resolve_session_window(
    sender_workspace: &Path,
    target_workspace: &Path,
    session: &str,
    window: &str,
    parsed: &ParsedNamedAddress,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let targets = list_targets(transport)?;
    let matches = matching_session_window(&targets, session, window);
    match matches.len() {
        1 => Ok(ResolvedNamedAddress {
            raw_name: parsed.display_name(),
            target_kind: NamedTargetKind::SessionWindow,
            sender_workspace: sender_workspace.to_path_buf(),
            target_workspace: target_workspace.to_path_buf(),
            team_key: None,
            agent_id: None,
            pane_id: matches[0].pane_id.as_str().to_string(),
            session_name: Some(session.to_string()),
            window_name: Some(window.to_string()),
            tmux_endpoint: transport.tmux_endpoint(),
            transport_kind: Some("direct_tmux".to_string()),
            app_server: None,
            state_pane_id: None,
            state_pane_stale: false,
            agent_status: None,
            warning: None,
        }),
        0 => {
            let mut err = NamedAddressError::new(
                NamedAddressErrorKind::NameNotLive,
                "diagnostic session/window name is not live",
            );
            err.action = "Check tmux session/window names or use a canonical <workspace>::<team>/<agent> name".to_string();
            err.log = format!(
                "workspace={} session={session} window={window} tmux_endpoint={}",
                target_workspace.display(),
                transport
                    .tmux_endpoint()
                    .unwrap_or_else(|| "<unknown>".to_string())
            );
            Err(err)
        }
        _ => Err(name_ambiguous(
            "multiple live panes match diagnostic session/window",
            matches
                .into_iter()
                .map(|pane| {
                    json!({
                        "name": format!("{session}:{window}"),
                        "workspace": target_workspace.display().to_string(),
                        "session": session,
                        "window": window,
                        "pane_id": pane.pane_id.as_str(),
                    })
                })
                .collect(),
        )),
    }
}

fn transport_for_cli_target(
    target_workspace: &Path,
    state: &Value,
    parsed: &ParsedNamedAddress,
) -> Box<dyn Transport> {
    if let ParsedTarget::TeamEntity { team, entity } = &parsed.target {
        let canonical = canonical_named_team_key(target_workspace, state, team)
            .unwrap_or_else(|| team.clone());
        if entity == "leader" {
            let team_scoped_socket = team_entry(state, &canonical)
                .and_then(|entry| entry.get("leader_receiver"))
                .and_then(|receiver| string_field(receiver, "tmux_socket"));
            let socket = team_scoped_socket.map(str::to_string).or_else(|| {
                state
                    // ALLOWED-LEGACY-SINGLE-TEAM: pre-teams-map compat probe endpoint; canonical taxonomy in `resolve_team_scoped_leader_receiver` still gates identity.
                    .get("leader_receiver")
                    .and_then(|receiver| string_field(receiver, "tmux_socket"))
                    .map(str::to_string)
            });
            if let Some(socket) = socket {
                return Box::new(crate::tmux_backend::TmuxBackend::for_tmux_endpoint(&socket));
            }
        }
        if let Some(entry) = team_entry(state, &canonical) {
            let selected = crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
                target_workspace,
                Some(entry),
            );
            return Box::new(selected.backend);
        }
    }

    if let ParsedTarget::BareAgent(agent) = &parsed.target {
        if let Some((_, entry)) = unique_state_team_for_agent(state, agent) {
            let selected = crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
                target_workspace,
                Some(entry),
            );
            return Box::new(selected.backend);
        }
    }

    let selected = crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
        target_workspace,
        Some(state),
    );
    Box::new(selected.backend)
}

fn unique_state_team_for_agent<'a>(state: &'a Value, agent: &str) -> Option<(&'a str, &'a Value)> {
    let teams = state.get("teams")?.as_object()?;
    let mut matches = teams
        .iter()
        .filter(|(_, team)| {
            team.get("agents")
                .and_then(Value::as_object)
                .is_some_and(|agents| agents.contains_key(agent))
        })
        .map(|(team, entry)| (team.as_str(), entry));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn matching_session_window<'a>(
    targets: &'a [PaneInfo],
    session: &str,
    window: &str,
) -> Vec<&'a PaneInfo> {
    targets
        .iter()
        .filter(|target| {
            target.session.as_str() == session
                && target
                    .window_name
                    .as_ref()
                    .is_some_and(|name| name.as_str() == window)
        })
        .collect()
}

fn list_targets(transport: &dyn Transport) -> Result<Vec<PaneInfo>, NamedAddressError> {
    transport.list_targets().map_err(|error| {
        let mut err = NamedAddressError::new(
            NamedAddressErrorKind::NameNotLive,
            format!("target tmux endpoint could not be listed: {error}"),
        );
        err.action =
            "Verify the target tmux server is running, or reattach/restart the target team"
                .to_string();
        err.log = format!(
            "tmux_endpoint={} list_targets_error={error}",
            transport
                .tmux_endpoint()
                .unwrap_or_else(|| "<unknown>".to_string())
        );
        err
    })
}

fn team_entry<'a>(state: &'a Value, team: &str) -> Option<&'a Value> {
    state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team))
}

fn canonical_named_team_key(
    target_workspace: &Path,
    state: &Value,
    requested: &str,
) -> Option<String> {
    if team_entry(state, requested).is_some() || is_legacy_single_team_active(state, requested) {
        return Some(requested.to_string());
    }
    let selected = crate::state::projection::select_runtime_state(
        target_workspace,
        Some(requested),
    )
    .ok()?;
    let canonical = selected
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())?;
    state
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(|teams| teams.contains_key(canonical))
        .then(|| canonical.to_string())
}

/// E6 (0.5.9 offline-mailbox-toname-design §3.1): decide which
/// `leader_receiver` object, if any, applies to `<team>/leader` before any
/// downstream code reads it. Wrong runtime keys are rejected here with
/// `team_key_not_found` — the pre-fix behavior was to silently reuse the
/// top-level `leader_receiver` (the `current` team's receiver in the gate
/// evidence), producing misleading `expected_pane=<missing>` messages.
///
/// Legacy single-team compat is preserved only when the state has no
/// `teams` map at all AND the requested team equals `active_team_key`.
/// That branch is a documented exception to the E6 fallback grep guard;
/// the exception is tagged inline with `ALLOWED-LEGACY-SINGLE-TEAM` so
/// the guard admits it explicitly (see the R2 rework in
/// `.team/artifacts/059-impl-cr-verdict.md §4`). The exception is
/// scheduled to be removed when B1 canonical state layout lands
/// (`.team/artifacts/next-version-staged-plan.md §5 Phase-Foundation-1`),
/// at which point every runtime state carries a `teams` map and the
/// legacy branch becomes unreachable dead code.
fn resolve_team_scoped_leader_receiver<'a>(
    state: &'a Value,
    team: &str,
    target_workspace: &Path,
) -> Result<Option<&'a Value>, NamedAddressError> {
    if let Some(entry) = team_entry(state, team) {
        return Ok(entry.get("leader_receiver"));
    }
    if is_legacy_single_team_active(state, team) {
        // ALLOWED-LEGACY-SINGLE-TEAM: pre-teams-map compat; state has no teams map AND request matches active_team_key.
        return Ok(state.get("leader_receiver"));
    }
    Err(team_key_not_found(target_workspace, state, team))
}

fn is_legacy_single_team_active(state: &Value, team: &str) -> bool {
    state.get("teams").is_none()
        && state
            .get("active_team_key")
            .and_then(Value::as_str)
            .is_some_and(|k| k == team)
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn candidate_json(resolved: &ResolvedNamedAddress) -> Value {
    json!({
        "name": resolved.raw_name,
        "workspace": resolved.target_workspace.display().to_string(),
        "team_key": resolved.team_key,
        "agent_id": resolved.agent_id,
        "session": resolved.session_name,
        "window": resolved.window_name,
        "pane_id": resolved.pane_id,
        "tmux_endpoint": resolved.tmux_endpoint,
    })
}

fn name_invalid(message: &str) -> NamedAddressError {
    let mut err = NamedAddressError::new(NamedAddressErrorKind::NameInvalid, message);
    err.action =
        "Use <team>/<agent>, <workspace>::<team>/<agent>, <team>/leader, or <session>:<window>"
            .to_string();
    err.log = "name parser rejected the input before reading state".to_string();
    err
}

fn workspace_not_found(path: &Path) -> NamedAddressError {
    let mut err = NamedAddressError::new(
        NamedAddressErrorKind::WorkspaceNotFound,
        "workspace path does not exist",
    );
    err.action = "Use an existing workspace path before `::`".to_string();
    err.log = format!("workspace={}", path.display());
    err
}

fn name_not_resolvable(message: &str) -> NamedAddressError {
    let mut err = NamedAddressError::new(
        NamedAddressErrorKind::NameNotResolvable,
        message.to_string(),
    );
    err.action = "Use a canonical <workspace>::<team>/<agent> or <team>/<agent> name from team-agent status --json".to_string();
    err.log = "state did not contain the requested team/name tuple".to_string();
    err
}

fn name_not_resolvable_team(team: &str) -> NamedAddressError {
    name_not_resolvable(&format!(
        "team `{team}` is not present in target workspace state"
    ))
}

fn name_not_resolvable_agent(team: &str, agent: &str) -> NamedAddressError {
    name_not_resolvable(&format!("agent `{agent}` is not present in team `{team}`"))
}

fn name_not_live_worker(
    team: &str,
    agent: &str,
    session: &str,
    window: &str,
    endpoint: Option<String>,
) -> NamedAddressError {
    let mut err = NamedAddressError::new(
        NamedAddressErrorKind::NameNotLive,
        "state has the team/agent but no live pane matches its session/window",
    );
    err.action = format!("Restart the target agent or send to a live pane after checking `team-agent status --team {team} --json`");
    err.log = format!(
        "name={team}/{agent} expected_session={session} expected_window={window} tmux_endpoint={}",
        endpoint.unwrap_or_else(|| "<unknown>".to_string())
    );
    err
}

/// E6 (0.5.9 offline-mailbox §3.1): requested `<team>` is not a runtime team_key.
/// The refusal echoes the requested string plus the canonical alternatives so
/// the caller can retry with `::<canonical_key>/leader`. The action copy is
/// deliberately neutral — no `claim-leader` / `takeover` — because a third-party
/// sender must never be told to seize the target team's leader.
fn team_key_not_found(target_workspace: &Path, state: &Value, team: &str) -> NamedAddressError {
    let available = available_team_keys(state);
    let workspace = target_workspace.display();
    let mut err = NamedAddressError::new(
        NamedAddressErrorKind::TeamKeyNotFound,
        format!(
            "team key `{team}` is not a runtime key in target workspace `{workspace}` (spec/display name may differ from runtime key)"
        ),
    );
    let suggested_key = available.first().cloned();
    err.action = match suggested_key.as_deref() {
        Some(k) => format!(
            "Retry with the canonical key, e.g. `{workspace}::{k}/leader`; check `team-agent status --workspace {workspace} --json` for team_key"
        ),
        None => format!(
            "No runtime team keys are present; check `team-agent status --workspace {workspace} --json`"
        ),
    };
    err.log = format!(
        "requested_team={team} available_team_keys={:?} workspace={workspace}",
        available
    );
    err.requested_team = Some(team.to_string());
    err.suggested_name = suggested_key
        .as_deref()
        .map(|k| format!("{workspace}::{k}/leader"));
    err.available_team_keys = available;
    err
}

/// E6 (0.5.9 offline-mailbox §3.1): key hit but the leader receiver is either
/// absent or not live. `leader_not_attached_third_party` is the neutral variant
/// used by the resolver: it never suggests `claim-leader` / `takeover` so the
/// caller (which may be a third-party sender) never sees ownership-seizing copy.
/// The owner-facing action (attach-leader) belongs to `status_port`
/// pending_leader_notifications and diagnose.
fn leader_not_attached_third_party(
    target_workspace: &Path,
    team: &str,
    pane_id: Option<&str>,
    session: Option<&str>,
    window: Option<&str>,
    socket: Option<String>,
) -> NamedAddressError {
    let workspace = target_workspace.display();
    let mut err = NamedAddressError::new(
        NamedAddressErrorKind::LeaderNotAttached,
        "target team leader is not attached on its recorded tmux endpoint",
    );
    err.action = format!(
        "Ask the target team owner to run `team-agent attach-leader --workspace {workspace} --team {team}`; the sender-side offline mailbox will requeue the message once attach succeeds"
    );
    err.log = format!(
        "name={team}/leader expected_pane={} expected_session={} expected_window={} tmux_socket={}",
        pane_id.unwrap_or("<missing>"),
        session.unwrap_or("<unknown>"),
        window.unwrap_or("<unknown>"),
        socket.unwrap_or_else(|| "<unknown>".to_string())
    );
    err.requested_team = Some(team.to_string());
    err
}

pub(crate) fn available_team_keys(state: &Value) -> Vec<String> {
    let mut keys: Vec<String> = state
        .get("teams")
        .and_then(Value::as_object)
        .map(|teams| teams.keys().cloned().collect())
        .unwrap_or_default();
    if keys.is_empty() {
        if let Some(active) = state.get("active_team_key").and_then(Value::as_str) {
            if !active.is_empty() {
                keys.push(active.to_string());
            }
        }
    }
    keys.sort();
    keys
}

fn name_not_live_leader(
    team: &str,
    pane_id: Option<&str>,
    session: Option<&str>,
    window: Option<&str>,
    socket: Option<String>,
) -> NamedAddressError {
    let mut err = NamedAddressError::new(
        NamedAddressErrorKind::NameNotLive,
        "leader_receiver is missing or not live on its recorded tmux socket",
    );
    err.action = "Run claim-leader, attach-leader, or takeover for the target team; resolver will not guess another leader pane".to_string();
    err.log = format!(
        "name={team}/leader expected_pane={} expected_session={} expected_window={} tmux_socket={}",
        pane_id.unwrap_or("<missing>"),
        session.unwrap_or("<unknown>"),
        window.unwrap_or("<unknown>"),
        socket.unwrap_or_else(|| "<unknown>".to_string())
    );
    err
}

fn name_ambiguous(message: &str, candidates: Vec<Value>) -> NamedAddressError {
    let mut err = NamedAddressError::new(NamedAddressErrorKind::NameAmbiguous, message);
    err.action = "Use an explicit <workspace>::<team>/<agent> or <team>/<agent> name".to_string();
    err.log = format!("{} candidates matched", candidates.len());
    err.candidates = candidates;
    err
}
