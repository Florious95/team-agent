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
}

impl NamedAddressError {
    pub(crate) fn new(kind: NamedAddressErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            action: "inspect team-agent status and use an explicit workspace/team name".to_string(),
            log: "named-address resolver refused before injection".to_string(),
            candidates: Vec::new(),
        }
    }

    pub(crate) fn n38_message(&self) -> String {
        format!(
            "Error: {}\nAction: {}\nLog: {}",
            self.kind.as_str(),
            self.action,
            self.log
        )
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "ok": false,
            "status": "refused",
            "reason": self.kind.as_str(),
            "error": self.message,
            "action": self.action,
            "log": self.log,
            "candidates": self.candidates,
        })
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
) -> Result<(ResolvedNamedAddress, Box<dyn Transport>), NamedAddressError> {
    let parsed = parse_named_address(raw_name)?;
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
    let raw = raw_name.trim();
    if raw.is_empty() {
        return Err(name_invalid("name is empty"));
    }
    let (workspace, name) = if let Some((workspace, rest)) = raw.split_once("::") {
        if workspace.trim().is_empty() || rest.trim().is_empty() {
            return Err(name_invalid(
                "workspace-qualified name must include workspace and target",
            ));
        }
        (Some(PathBuf::from(workspace)), rest.trim())
    } else {
        (None, raw)
    };

    if name.contains("//") {
        return Err(name_invalid("name contains an empty path segment"));
    }

    let target = if name.contains('/') {
        let parts = name.split('/').collect::<Vec<_>>();
        if parts.len() != 2 || !valid_component(parts[0]) || !valid_component(parts[1]) {
            return Err(name_invalid("expected <team>/<agent> or <team>/leader"));
        }
        ParsedTarget::TeamEntity {
            team: parts[0].to_string(),
            entity: parts[1].to_string(),
        }
    } else if name.contains(':') {
        let parts = name.split(':').collect::<Vec<_>>();
        if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
            return Err(name_invalid("expected <session>:<window>"));
        }
        ParsedTarget::SessionWindow {
            session: parts[0].to_string(),
            window: parts[1].to_string(),
        }
    } else {
        if !valid_component(name) {
            return Err(name_invalid("expected a non-empty agent id"));
        }
        ParsedTarget::BareAgent(name.to_string())
    };
    Ok(ParsedNamedAddress { workspace, target })
}

fn valid_component(raw: &str) -> bool {
    !raw.trim().is_empty()
        && !raw.contains(char::is_whitespace)
        && !raw.contains('/')
        && !raw.contains(':')
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
        ParsedTarget::TeamEntity { team, entity } if entity == "leader" => resolve_leader(
            sender_workspace,
            target_workspace,
            state,
            team,
            parsed,
            transport,
        ),
        ParsedTarget::TeamEntity { team, entity } => resolve_worker(
            sender_workspace,
            target_workspace,
            state,
            team,
            entity,
            parsed,
            transport,
        ),
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
    let team_entry = team_entry(state, team).ok_or_else(|| name_not_resolvable_team(team))?;
    let agent_entry = team_entry
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|agents| agents.get(agent))
        .ok_or_else(|| name_not_resolvable_agent(team, agent))?;
    let session = string_field(team_entry, "session_name")
        .ok_or_else(|| name_not_resolvable("team is missing session_name"))?;
    let window = string_field(agent_entry, "window")
        .or_else(|| string_field(agent_entry, "window_name"))
        .unwrap_or(agent);
    let targets = list_targets(transport)?;
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
        0 => Err(name_not_live_worker(
            team,
            agent,
            session,
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

fn resolve_leader(
    sender_workspace: &Path,
    target_workspace: &Path,
    state: &Value,
    team: &str,
    parsed: &ParsedNamedAddress,
    transport: &dyn Transport,
) -> Result<ResolvedNamedAddress, NamedAddressError> {
    let receiver = if let Some(team_entry) = team_entry(state, team) {
        team_entry.get("leader_receiver")
    } else {
        state.get("leader_receiver")
    }
    .ok_or_else(|| name_not_live_leader(team, None, None, None, transport.tmux_endpoint()))?;
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
        .ok_or_else(|| name_not_live_leader(team, None, None, None, transport.tmux_endpoint()))?;
    let session = string_field(receiver, "session_name");
    let window = string_field(receiver, "window_name");
    let socket = string_field(receiver, "tmux_socket")
        .map(str::to_string)
        .or_else(|| transport.tmux_endpoint());
    let targets = list_targets(transport)?;
    let matches = targets
        .iter()
        .filter(|target| target.pane_id.as_str() == pane_id)
        .collect::<Vec<_>>();
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
        0 => Err(name_not_live_leader(
            team,
            Some(pane_id),
            session,
            window,
            socket,
        )),
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
        if entity == "leader" {
            if let Some(socket) = team_entry(state, team)
                .and_then(|entry| entry.get("leader_receiver"))
                .or_else(|| state.get("leader_receiver"))
                .and_then(|receiver| string_field(receiver, "tmux_socket"))
            {
                return Box::new(crate::tmux_backend::TmuxBackend::for_tmux_endpoint(socket));
            }
        }
        if let Some(entry) = team_entry(state, team) {
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
