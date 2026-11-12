//! Canonical lifecycle surface: pure syntax and finite plans over existing handlers.
//! Legacy entry points keep their own defaults and receipts.
use super::*;
use crate::lifecycle::role_config::RoleConfigPatch;
use crate::model::ids::AgentId;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod runtime;
#[cfg(test)]
mod dynamic;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn explicit_fresh_team_restart_preserves_frozen_legacy_route() {
    let sandbox = tests::Sandbox::new();
    assert!(run("restart", &[], sandbox.path()).is_none());
    let args = vec!["--allow-fresh".into(), "--workspace".into(),
        sandbox.path().to_string_lossy().into_owned(), "--team".into(), "missing".into()];
    assert!(run("restart", &args, sandbox.path()).is_none());
    let mut team_args = vec!["restart".into()];
    team_args.extend(args);
    assert!(run("team", &team_args, sandbox.path()).is_some());
    assert_eq!(std::fs::read_dir(sandbox.path()).unwrap().count(), 0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Surface {
    Canonical,
    FrozenLegacy,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Subject {
    Team { source: Option<PathBuf> },
    Agent(AgentId),
}
use crate::lifecycle::seat_start::SessionPolicy;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Verb {
    Start,
    Stop,
    Restart,
}
#[derive(Clone, Debug)]
pub(super) struct Intent {
    surface: Surface,
    verb: Verb,
    subject: Subject,
    workspace: PathBuf,
    pinned_workspace: bool,
    team: Option<String>,
    session: SessionPolicy,
    session_converge_deadline_ms: Option<u64>,
    dry_run: bool,
    json: bool,
    role_file: Option<PathBuf>,
    patch: RoleConfigPatch,
}
#[derive(Clone, Debug, Serialize)]
pub(super) struct ResolvedScope {
    workspace: PathBuf,
    team: Option<String>,
}
pub(super) use crate::lifecycle::seat_observation::SeatObservation as Observation;
#[derive(Clone, Debug)]
pub(super) enum Step {
    RestoreSpec {
        workspace: PathBuf,
        team: String,
        compiled: crate::model::yaml::Value,
    },
    CreateTeam(QuickStartArgs),
    StartAgent(StartAgentArgs),
    UnifiedStart { request: crate::lifecycle::seat_start::StartRequest, description: crate::lifecycle::seat_start::StartDescription },
    StopAgent(StopAgentArgs),
    RestartTeam(RestartArgs),
    Shutdown(ShutdownArgs),
}
impl Step {
    fn name(&self) -> &'static str {
        match self {
            Self::RestoreSpec { .. } => "restore_runtime_spec",
            Self::CreateTeam(_) => "quick_start",
            Self::StartAgent(_) => "start_agent",
            Self::UnifiedStart { .. } => "start",
            Self::StopAgent(_) => "stop_agent",
            Self::RestartTeam(_) => "restart_team",
            Self::Shutdown(_) => "shutdown",
        }
    }
    fn agent(&self) -> Option<&str> {
        match self {
            Self::StartAgent(a) => Some(&a.agent),
            Self::UnifiedStart { request, .. } => Some(request.agent.as_str()),
            Self::StopAgent(a) => Some(&a.agent),
            _ => None,
        }
    }
    fn execute(&self) -> Result<CmdResult, CliError> {
        match self {
            Self::RestoreSpec {
                workspace,
                team,
                compiled,
            } => {
                let path = crate::lifecycle::restart::restore_missing_runtime_spec(
                    workspace, team, compiled,
                )
                .map_err(|e| CliError::Runtime(e.to_string()))?;
                Ok(CmdResult::from_json(
                    json!({"ok":true,"status":"metadata_restored","spec_path":path,
                    "effects":{"native_processes":"not_attempted","native_input":"not_attempted"}}),
                    true,
                ))
            }
            Self::CreateTeam(a) => cmd_quick_start(a),
            Self::StartAgent(a) => cmd_start_agent(a),
            Self::UnifiedStart { request, .. } => cmd_start_request(request, true),
            Self::StopAgent(a) => cmd_stop_agent(a),
            Self::RestartTeam(a) => cmd_restart(a),
            Self::Shutdown(a) => cmd_shutdown(a),
        }
    }
}
#[derive(Debug)]
pub(super) struct Plan {
    intent: Intent,
    scope: ResolvedScope,
    steps: Vec<Step>,
    skipped: Vec<Value>,
    no_eligible: Option<&'static str>,
    files: Vec<(PathBuf, Vec<u8>)>,
    expected: BTreeMap<String, Value>,
    topology: Option<Value>,
}

const START_SOURCE_HELP: &str = "\n\nNo files are required for direct command-line start:\n  team-agent start reviewer --team release-review --provider pi --prompt \"Review the changes\"\nOptional --from TEAMDIR layout (three-line templates; the role ID is its filename):\nTEAM.md:\n---\nname: release-review\n---\nagents/reviewer.md:\n---\nprovider: pi\n---\nAppend role instructions after the closing ---; other fields are optional.";

pub(super) fn help(command: &str) -> String {
    let (purpose, usage, options, examples, next) = match command {
        "start" => (
            "Start a team or one agent without replacing verified running processes.",
            "[AGENT [PROMPT...]|./TEAMDIR] [--from TEAMDIR] [--workspace PATH] [--team NAME] [--provider PROVIDER] [--model MODEL] [--effort EFFORT] [--profile PROFILE] [--bypass true|false] [--role-file PATH] [--prompt TEXT] [--session preserve|auto|fresh] [--allow-fresh] [--dry-run] [--json]\n       --team NAME creates the team if it does not exist.",
            "--team NAME selects a team and creates it if it does not exist (for example: team-agent start reviewer --team release-review --provider pi).\n--provider PROVIDER, --model MODEL, --effort EFFORT, --profile PROFILE, --bypass true|false, --role-file PATH and --prompt TEXT configure a direct agent start.\n--from selects TEAM.md and agents/; all relative paths use the calling directory.\nAgent IDs such as reviewer.v2 stay agent IDs even when a same-named directory exists.\nTeam start preserves explicitly stopped seats; use agent start ID to reactivate one seat.\nMissing agents are created automatically (leader provider, pi if unknown; native default model; bypass=false); an uninitialized workspace uses team current.\n--role-file and explicit provider/model/effort/bypass/prompt/profile fields resolve one declaration; receipts identify defaulted fields.\nStopped seats may change provider with --session fresh; their identity and history are retained. Running seats refuse configuration changes or fresh sessions until stopped.\nSession defaults to preserve for existing resumable context. Contextless seats and legacy providers without resume support start fresh and report op=fresh; launch/pending metadata alone is not context. Auto permits legal fresh fallback; fresh explicitly starts a new native session. --allow-fresh cannot be combined with preserve or fresh.\n--dry-run performs no lifecycle action; final locked admission remains deferred.\nAlready running does not claim role-file synchronization or business readiness.",
            "  team-agent start --from . --dry-run --json\n  team-agent start reviewer --dry-run --json\n  team-agent start reviewer --provider cursor --model composer-2.5 --session fresh",
            "Wait for pushed replies. Use team-agent status and team-agent inbox leader -n 3 only for a deliberate one-shot inspection; do not poll.",
        ),
        "stop" => (
            "Stop the selected team or one registered agent while retaining session associations.",
            "[AGENT] [--workspace PATH] [--team KEY] [--json]",
            "Without an agent, use the original team shutdown and its residual-resource checks.\nAn agent target uses the original single-agent stop.\nUse --workspace for another directory; a positional path is rejected.\nNo reset, force, role edits or automatic takeover is performed.",
            "  team-agent stop --workspace . --json\n  team-agent stop reviewer --workspace . --json",
            "Use team-agent agent start ID to explicitly reactivate a stopped seat.",
        ),
        "team" => (
            "Select a team lifecycle operation explicitly.",
            "<start|stop|restart|status> [OPTIONS]",
            "start [TEAMDIR] or --from TEAMDIR creates once or starts stopped members.\nstop delegates to the original team shutdown; status keeps the original read-only output.\nrestart defaults to --session auto and preserves paused or explicitly stopped seats.\nLegacy bare or explicit-path restart still defaults to preserve and keeps its old receipt.\n--workspace PATH and --team KEY select the team; no ambiguous directory is guessed.\n--dry-run on start/restart is read-only; native refusals never fall back to legacy.\nauto is not reset and cannot invent an unsupported fresh capability.",
            "  team-agent team start . --dry-run --json\n  team-agent team restart --workspace . --dry-run --json",
            "Use team-agent team status; only an explicit agent start clears that seat's stop intent.",
        ),
        "agent" => (
            "Select an agent lifecycle operation explicitly without guessing a team directory.",
            "<start|stop|restart|remove|reset|clone|fork> AGENT [OPTIONS]",
            "--workspace PATH and --team KEY select the team.\nstart creates, adopts or resumes a seat with explicit defaults and session policy; matching live seats are unchanged.\nrestart defaults to --session auto; --session preserve keeps the original refusal policy.\nSingle-agent restart checks roles and capabilities before stop, then uses original stop/start.\n--dry-run on start/restart is read-only; role patches are not accepted by restart.\nremove/reset/clone/fork retain their capability and confirmation flags.\nreset requires --discard-session; fork requires --as NEW_ID; --in-place is not supported.",
            "  team-agent agent start reviewer --dry-run --json\n  team-agent agent restart reviewer --session preserve --dry-run --json",
            "Inspect team-agent status before using agent start after a partial result; an unknown replacement must not be retried blindly.",
        ),
        _ => return super::emit::command_help(Some(command)),
    };
    let source_help = if command == "start" { START_SOURCE_HELP } else { "" };
    let subteam_help = if matches!(command, "start" | "stop") {
        "\nSub-teams: A teammate can start and lead a sub-team from its own terminal via `team-agent start AGENT --team <NAME>`.\nTeardown: `team-agent stop --team <NAME>` cascades deepest-first to all recorded descendant sub-teams; unrelated teams and the calling leader remain running."
    } else { "" };
    format!("Purpose:\n{purpose}\nUsage: team-agent {command} {usage}\n\nOptions:\n{options}{subteam_help}{source_help}\n\nExamples:\n{examples}\n\nNext Action:\n{next}")
}

fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}
fn refusal(message: impl Into<String>) -> CliError {
    CliError::Runtime(message.into())
}
fn path_syntax(value: &str) -> bool {
    super::emit::explicit_path(value)
}
fn logical_agent_id(value: &str) -> Result<AgentId, CliError> {
    if value.is_empty()
        || value.starts_with('-')
        || path_syntax(value)
        || !value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(usage("invalid agent ID"));
    }
    Ok(AgentId::new(value))
}
fn existing_path(cwd: &Path, value: &str) -> Result<PathBuf, CliError> {
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    path.canonicalize()
        .map_err(|e| usage(format!("invalid explicit path {}: {e}", path.display())))
}
fn options(tokens: &[String]) -> (Vec<String>, BTreeMap<String, String>) {
    let mut pos = Vec::new();
    let mut flags = BTreeMap::new();
    let mut literal = false;
    for token in tokens {
        if literal {
            pos.push(token.clone());
            continue;
        }
        if token == "--" {
            literal = true;
            continue;
        }
        if let Some((key, value)) = token
            .split_once('=')
            .filter(|(key, _)| key.starts_with("--"))
        {
            flags.insert(key.to_owned(), value.to_owned());
        } else if token.starts_with("--") {
            flags.insert(token.clone(), String::new());
        } else {
            pos.push(token.clone());
        }
    }
    (pos, flags)
}

pub(super) fn canonical_start_command(command: &str) -> &str {
    match command { "add-agent" | "start-agent" => "start", other => other }
}

/// None deliberately leaves frozen legacy parsing, defaults and output untouched.
pub(super) fn run(
    command: &str,
    args: &[String],
    cwd: &Path,
) -> Option<Result<CmdResult, CliError>> {
    let command = canonical_start_command(command);
    if !matches!(command, "start" | "stop" | "restart" | "team" | "agent") {
        return None;
    }
    if command == "restart" {
        // Schema parsing is needed to distinguish a positional from a flag VALUE.
        let checked = match super::arguments::parse("intent-restart", args) {
            Ok(c) => c,
            Err(e) => return Some(Err(e)),
        };
        let (pos, flags) = options(&checked.tokens);
        let surface = if !flags.contains_key("--session")
            && !flags.contains_key("--dry-run")
            && pos.first().is_none_or(|p| path_syntax(p))
        {
            Surface::FrozenLegacy
        } else {
            Surface::Canonical
        };
        if surface == Surface::FrozenLegacy {
            return None;
        }
    }
    Some(match run_canonical(command, args, cwd) {
        Err(error) if !matches!(error, CliError::Usage(_)) => {
            // New surfaces use one effect-free refusal envelope. In particular,
            // never reparse a --prompt payload as an error-log workspace.
            let mut result = CmdResult::from_json(
                json!({"ok":false,"action":command,"scope":null,"phase":"planning_or_selection","error":crate::redaction::redact_external_text(&error.to_string())}),
                super::arguments::has_flag(command, args, "--json"),
            );
            if !result.as_json {
                if let CmdOutput::Json(value) = &result.output {
                    result.output = CmdOutput::Human(intent_human(value));
                }
            }
            Ok(result)
        }
        result => result,
    })
}
fn run_canonical(command: &str, args: &[String], cwd: &Path) -> Result<CmdResult, CliError> {
    let (verb, args, explicit_agent, explicit_team) = if matches!(command, "team" | "agent") {
        if args.is_empty()
            || args
                .first()
                .is_some_and(|a| matches!(a.as_str(), "--help" | "-h"))
        {
            return Ok(CmdResult::human(help(command)));
        }
        let sub = if command == "agent" && args[0] == "add" { "start" } else { args[0].as_str() };
        if command == "team" && sub == "status" {
            return super::emit::intent_alias("status", &args[1..], cwd);
        }
        if command == "agent" && matches!(sub, "remove" | "reset" | "clone" | "fork") {
            let original = match sub {
                "remove" => "remove-agent",
                "reset" => "reset-agent",
                "clone" => "clone-agent",
                _ => "fork-agent",
            };
            return super::emit::intent_alias(original, &args[1..], cwd);
        }
        (sub, &args[1..], command == "agent", command == "team")
    } else {
        (command, args, false, false)
    };
    let verb = match verb {
        "start" => Verb::Start,
        "stop" => Verb::Stop,
        "restart" => Verb::Restart,
        _ => return Err(usage("unknown team/agent operation; see --help")),
    };
    let schema = match verb {
        Verb::Start => "intent-start",
        Verb::Stop => "intent-stop",
        Verb::Restart => "intent-restart",
    };
    let checked = super::arguments::parse(schema, args)?;
    if checked.help {
        return Ok(CmdResult::human(help(command)));
    }
    let intent = lifecycle_intent(
        command,
        verb,
        &checked.tokens,
        cwd,
        explicit_agent,
        explicit_team,
    )?;
    if intent.verb == Verb::Start {
        if matches!(intent.subject, Subject::Team { .. }) && intent.session == SessionPolicy::Fresh {
            return Err(usage("--session fresh requires an explicit agent; team start preserves stopped seats"));
        }
    }
    if !intent.dry_run && std::env::var_os("TEAM_AGENT_ID").is_none() {
        if let Ok(selected) = crate::state::selector::resolve_active_team_readonly(
            &intent.workspace, intent.team.as_deref(), crate::state::selector::SelectorMode::RuntimeOnly,
        ) {
            let target = match &intent.subject {
                Subject::Agent(id) => Some(id),
                _ => None,
            };
            let owner_gate = if intent.verb == Verb::Stop && target.is_none() {
                crate::lifecycle::launch::ensure_stop_owner_allowed_for_state
            } else { crate::lifecycle::launch::ensure_owner_allowed_for_state };
            if let Err(crate::lifecycle::LifecycleError::OwnerRefused(reason)) = owner_gate(&selected.state, target) {
                if serde_json::from_str::<Value>(&reason).ok()
                    .is_some_and(|refusal| refusal["reason"] == "team_owner_mismatch")
                {
                    // Reuse the endpoint-aware, non-forcing claim CAS. Planning
                    // rereads ownership and still refuses if the claim failed.
                    let _ = crate::leader::claim_leader(&selected.run_workspace, Some(&selected.team_key), false);
                }
            }
        }
    }
    #[cfg(unix)]
    if intent.verb == Verb::Restart && matches!(intent.subject, Subject::Team { .. }) {
        if let Some(result) = crate::contract_runtime::recovery::restart(
            &intent.workspace, intent.team.as_deref(),
            checked.tokens.iter().any(|token| token == "--allow-fresh"), intent.dry_run, intent.json,
        )? { return Ok(result); }
    }
    #[cfg(unix)]
    if !intent.dry_run {
        if let Ok(selected) = crate::state::selector::resolve_active_team_readonly(
            &intent.workspace, intent.team.as_deref(), crate::state::selector::SelectorMode::RuntimeOnly,
        ) {
            if let Ok(binding) = crate::contract_runtime::backend::read_binding(&selected.run_workspace, &selected.team_key) {
                crate::contract_runtime::backend::retire_inactive_siblings(&binding, &selected.state)
                    .map_err(|error| refusal(error.to_string()))?;
                if crate::contract_runtime::backend::temporary_authority(&binding) {
                    match crate::contract_runtime::backend::migrate_if_required(&selected.run_workspace, &selected.team_key) {
                        Ok(_) | Err(crate::contract_runtime::backend::BackendError::AuthorityLost { .. }) => {}
                        Err(error) => return Err(refusal(error.to_string())),
                    }
                }
            }
        }
    }
    let mut plan = runtime::plan(intent)?;
    let upgrade_workspace = plan.scope.workspace.clone();
    let upgrade = !plan.intent.dry_run && plan.no_eligible.is_none();
    if upgrade && crate::coordinator::retire_legacy_coordinator(&upgrade_workspace).map_err(CliError::Runtime)? {
        plan = runtime::plan(plan.intent)?;
    }
    #[cfg(unix)]
    if plan.intent.verb == Verb::Stop && !plan.intent.dry_run && plan.no_eligible.is_none() {
        if let Some(team) = plan.scope.team.as_deref() {
            match crate::contract_runtime::backend::migrate_if_required(&plan.scope.workspace, team) {
                Ok(true) => plan = runtime::plan(plan.intent)?,
                Ok(false) | Err(crate::contract_runtime::backend::BackendError::AuthorityLost { .. }) => {},
                Err(error) => return Err(refusal(error.to_string())),
            }
        }
    }
    let concise_start = plan.intent.verb == Verb::Start && !plan.intent.json && !plan.intent.dry_run;
    let mut result = execute_plan(plan, &mut |step| step.execute(), &mut runtime::recheck)?;
    if upgrade {
        if let Err(error) = crate::coordinator::retire_legacy_coordinator(&upgrade_workspace) {
            if let CmdOutput::Json(value) = &mut result.output {
                value["ok"] = json!(false);
                value["status"] = json!("partial");
                value["upgrade_error"] = json!(error);
                value["current_state"] = json!("legacy_coordinator_not_retired");
            }
            result.exit = ExitCode::Error;
        }
    }
    if concise_start {
        if let CmdOutput::Json(value) = &result.output {
            result.output = CmdOutput::Human(intent_human(value));
        }
    }
    Ok(result)
}

fn intent_human(value: &Value) -> String {
    let action = value.get("action").and_then(Value::as_str).unwrap_or("Lifecycle");
    let status = value.get("status").and_then(Value::as_str).unwrap_or("unknown");
    let status = if status == "actions_completed" { "completed" } else { status };
    let team = value.pointer("/scope/team").and_then(Value::as_str).unwrap_or("workspace");
    let mut lines = vec![format!("{action} {team}: {status}")];
    if let Some(summary) = value.get("summary").and_then(Value::as_str) { lines.push(summary.into()); }
    let steps = value.get("steps").and_then(Value::as_array).into_iter().flatten().collect::<Vec<_>>();
    let completed = steps.iter().filter(|step| step.get("ok").and_then(Value::as_bool) == Some(true)).count();
    if !steps.is_empty() { lines.push(format!("{completed}/{} actions completed; task completion is not implied.", steps.len())); }
    let binding = value.get("leader_bind_ok").and_then(Value::as_bool).or_else(|| steps.iter().find_map(|step| step.pointer("/receipt/leader_bind_ok").and_then(Value::as_bool)));
    if let Some(bound) = binding { lines.push(format!("Leader: {}.", if bound { "bound" } else { "binding pending" })); }
    if steps.iter().any(|step| step.pointer("/receipt/readiness/ready").and_then(Value::as_bool) == Some(false)) {
        lines.push("Worker tools: not yet verified; send a task and inspect the actual reply.".into());
    }
    if let Some(error) = value.get("error").or_else(|| value.get("reason")).and_then(Value::as_str).or_else(|| steps.iter().find_map(|step| step.get("error").or_else(|| step.pointer("/receipt/error")).and_then(Value::as_str))) {
        let message: String = error.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(360).collect();
        lines.insert(0, format!("error: {message}"));
    }
    for blocker in value.get("blockers").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        lines.push(format!("Blocked: {blocker}"));
    }
    if let Some(action) = value.get("recovery").and_then(Value::as_str).or_else(|| value.get("next_actions").and_then(Value::as_array).and_then(|actions| actions.first()).and_then(Value::as_str)) {
        lines.push(format!("Next: {action}"));
    }
    lines.join("\n")
}

fn lifecycle_intent(
    command: &str,
    verb: Verb,
    tokens: &[String],
    cwd: &Path,
    explicit_agent: bool,
    explicit_team: bool,
) -> Result<Intent, CliError> {
    let (pos, flags) = options(tokens);
    if verb == Verb::Stop && pos.first().is_some_and(|p| path_syntax(p)) {
        return Err(usage(
            "stop accepts an agent ID; select a workspace with --workspace",
        ));
    }
    let source = flags.get("--from").map(String::as_str);
    if source.is_some() && !pos.is_empty() {
        return Err(usage("--from conflicts with a positional target"));
    }
    if explicit_agent && (source.is_some() || pos.first().is_some_and(|p| path_syntax(p))) {
        return Err(usage(
            "agent operation requires an agent ID, not a team source",
        ));
    }
    if explicit_agent && pos.is_empty() {
        return Err(usage("missing agent"));
    }
    if explicit_team && verb != Verb::Start && !pos.is_empty() {
        return Err(usage(
            "team stop/restart accepts --workspace, not a positional source",
        ));
    }
    let restart_path = (verb == Verb::Restart)
        .then(|| pos.first())
        .flatten()
        .filter(|p| path_syntax(p));
    let team_source = source.or_else(|| {
        pos.first()
            .map(String::as_str)
            .filter(|p| explicit_team || (verb == Verb::Start && path_syntax(p)))
    });
    let subject = if let Some(source) = team_source {
        let source = existing_path(cwd, source)?;
        if !source.is_dir() {
            return Err(usage("team source must be an existing directory"));
        }
        Subject::Team {
            source: Some(source),
        }
    } else if restart_path.is_some() {
        Subject::Team { source: None }
    } else if let Some(id) = pos.first() {
        if verb == Verb::Restart && !explicit_agent && cwd.join(id).is_dir() {
            return Err(usage(
                "ambiguous restart target: use agent restart ID or --workspace PATH",
            ));
        }
        Subject::Agent(logical_agent_id(id)?)
    } else {
        Subject::Team { source: None }
    };
    let session = match flags.get("--session").map(String::as_str) {
        Some("preserve" | "fresh") if flags.contains_key("--allow-fresh") => {
            return Err(usage("--allow-fresh conflicts with explicit preserve/fresh session policy"))
        }
        Some("preserve") => SessionPolicy::Preserve,
        Some("auto") => SessionPolicy::Auto,
        Some("fresh") => SessionPolicy::Fresh,
        _ if flags.contains_key("--allow-fresh")
            || (verb == Verb::Restart
                && (command != "restart" || matches!(subject, Subject::Agent(_)))) =>
        {
            SessionPolicy::Auto
        }
        _ => SessionPolicy::Preserve,
    };
    let path_workspace = restart_path.map(|p| existing_path(cwd, p)).transpose()?;
    let workspace = match flags.get("--workspace") {
        Some(path) => existing_path(cwd, path)?,
        None => match &subject {
            Subject::Team {
                source: Some(source),
            } => super::emit::intent_source_workspace(source, cwd)?,
            _ if path_workspace.is_none()
                && cwd.join("TEAM.md").is_file()
                && cwd.join("agents").is_dir() =>
            {
                super::emit::intent_source_workspace(cwd, cwd)?
            }
            _ => path_workspace.clone().unwrap_or(cwd.canonicalize()?),
        },
    };
    let pinned_workspace = flags.contains_key("--workspace")
        || path_workspace.is_some()
        || matches!(&subject, Subject::Team { source: Some(_) })
        || (cwd.join("TEAM.md").is_file() && cwd.join("agents").is_dir());
    if !workspace.is_dir() {
        return Err(usage("workspace must be an existing directory"));
    }
    if path_workspace.is_some_and(|path| path != workspace) {
        return Err(usage("positional workspace conflicts with --workspace"));
    }
    if matches!(subject, Subject::Agent(_)) && flags.contains_key("--session-converge-deadline") {
        return Err(usage(
            "--session-converge-deadline applies only to team restart",
        ));
    }
    let session_converge_deadline_ms = flags
        .get("--session-converge-deadline")
        .map(|value| {
            super::emit::parse_seconds_ms(value).ok_or_else(|| usage("invalid session deadline"))
        })
        .transpose()?;
    let role_file = flags
        .get("--role-file")
        .map(|path| existing_path(cwd, path))
        .transpose()?;
    if role_file.as_ref().is_some_and(|path| !path.is_file()) {
        return Err(usage("--role-file must be an existing regular file"));
    }
    if pos.len() > 1 && (verb != Verb::Start || !matches!(subject, Subject::Agent(_)) || flags.contains_key("--prompt")) {
        return Err(usage("trailing prompt requires an agent start and cannot be combined with --prompt"));
    }
    let mut patch = if verb == Verb::Start {
        let mut role_args = vec!["intent-agent".to_string()];
        for token in tokens.iter().take_while(|token| token.as_str() != "--") {
            if token.split_once('=').is_some_and(|(k, _)| {
                matches!(
                    k,
                    "--provider" | "--model" | "--effort" | "--bypass" | "--prompt" | "--profile"
                )
            }) {
                role_args.push(token.clone());
            }
        }
        super::emit::intent_role_patch(&role_args)?
    } else {
        RoleConfigPatch::default()
    };
    if pos.len() > 1 {
        patch.prompt = Some(pos[1..].join(" "));
    }
    if matches!(subject, Subject::Team { .. })
        && (patch != RoleConfigPatch::default() || role_file.is_some())
    {
        return Err(usage(
            "team start does not apply per-agent role patches; use start AGENT",
        ));
    }
    Ok(Intent {
        surface: Surface::Canonical,
        verb,
        subject,
        workspace,
        pinned_workspace,
        team: flags.get("--team").cloned(),
        session,
        session_converge_deadline_ms,
        dry_run: flags.contains_key("--dry-run"),
        json: flags.contains_key("--json"),
        role_file,
        patch,
    })
}

fn execute_plan(
    mut plan: Plan,
    execute: &mut impl FnMut(&Step) -> Result<CmdResult, CliError>,
    recheck: &mut impl FnMut(&Plan, &Step, bool) -> Result<(), CliError>,
) -> Result<CmdResult, CliError> {
    if let Some(reason) = plan.no_eligible {
        return Ok(CmdResult::from_json(
            json!({"ok":false,"action":plan.intent.verb,"surface":plan.intent.surface,"scope":plan.scope,"session_policy":plan.intent.session,"dry_run":plan.intent.dry_run,"status":"no_eligible_agents","phase":"planning","error":reason,"steps":[],"skipped":plan.skipped}),
            plan.intent.json,
        ));
    }
    let descriptions: Vec<_> = plan
        .steps
        .iter()
        .map(|s| {
            let mut value = json!({"action":s.name(),"agent":s.agent()});
            if let Step::UnifiedStart { request, description } = s {
                value["op"] = json!(description.op);
                value["declaration"] = description.declaration.clone();
                value["defaulted"] = description.defaulted.clone();
                if plan.intent.dry_run {
                    // Reuse the read-only admission; refusals remain deferred,
                    // and a prediction never grants the later lock-held start.
                    if let Some(orphan) = crate::lifecycle::seat_start::start(request, true)
                        .ok().and_then(|preview| preview.get("orphan_cleanup").cloned())
                    {
                        value["orphan_cleanup"] = orphan;
                    }
                }
            }
            value
        })
        .collect();
    if plan.intent.dry_run {
        let creates_team = plan.steps.iter().any(|step| match step {
            Step::CreateTeam(_) => true,
            Step::UnifiedStart { description, .. } => description.creates_team,
            _ => false,
        });
        let summary = creates_team.then(|| format!("creates team {} led by this leader", plan.scope.team.as_deref().unwrap_or("current")));
        return Ok(CmdResult::from_json(
            json!({"ok":true,"action":plan.intent.verb,"surface":plan.intent.surface,"scope":plan.scope,"session_policy":plan.intent.session,"dry_run":true,"summary":summary,"steps":descriptions,"skipped":plan.skipped,"deferred":["original lock-held owner/cohort admission","native/session admission not performed by the readonly planner"]}),
            plan.intent.json,
        ));
    }
    let mut results = Vec::new();
    let mut actual_scope = plan.scope.clone();
    let mut pending = false;
    let mut ok = true;
    let mut stop_succeeded = false;
    let mut effects = Vec::new();
    let mut partial_effect = false;
    let mut handler_effect_confirmed = false;
    let mut known_no_effect = true;
    let mut refusal_hint = None;
    let steps = plan.steps.clone();
    for step in &steps {
        // Resolve the bounded write set before the handler can change its row.
        let started_files = match recheck(&plan, step, stop_succeeded)
            .and_then(|_| runtime::started_file_indices(&plan, step)) {
            Ok(files) => files,
            Err(error) => {
                ok = false;
                known_no_effect = false;
                results.push(json!({"action":step.name(),"agent":step.agent(),"ok":false,"phase":"recheck","error":crate::redaction::redact_external_text(&error.to_string())}));
                break;
            }
        };
        let result = execute(step);
        let (success, receipt) = match result {
            Ok(result) => (
                result.exit == ExitCode::Ok,
                match result.output {
                    CmdOutput::Json(v) => v,
                    CmdOutput::Human(text) => json!({"text":text}),
                },
            ),
            Err(error) => (
                false,
                json!({"ok":false,"error":crate::redaction::redact_external_text(&error.to_string())}),
            ),
        };
        partial_effect |= receipt.get("status").and_then(Value::as_str) == Some("partial");
        known_no_effect &= matches!(
            receipt.get("status").and_then(Value::as_str),
            Some("preflight_blocked" | "blocked")
        ) && receipt["effects"]["coordinator_start"] == "not_attempted"
            && receipt["effects"]["worker_panes_started"]
                .as_array()
                .is_some_and(Vec::is_empty);
        if let Some(effect) = receipt.get("effects") {
            let workers = effect
                .get("worker_panes_started")
                .and_then(Value::as_array)
                .is_some_and(|workers| !workers.is_empty());
            let coordinator = effect.get("coordinator_start").and_then(Value::as_str);
            handler_effect_confirmed |= workers || coordinator == Some("running_confirmed");
            partial_effect |=
                workers || matches!(coordinator, Some("running_confirmed" | "attempted_unknown"));
            effects.push(json!({"action": step.name(), "agent": step.agent(), "effects": effect}));
        }
        let stop_unconfirmed = plan.intent.verb == Verb::Restart
            && matches!(step, Step::StopAgent(_))
            && success
            && receipt.get("stopped").and_then(Value::as_bool) != Some(true);
        let success = success && !stop_unconfirmed;
        if !success {
            refusal_hint = receipt
                .get("recovery")
                .and_then(Value::as_str)
                .or_else(|| {
                    receipt
                        .get("next_actions")
                        .and_then(Value::as_array)
                        .and_then(|actions| actions.first())
                        .and_then(Value::as_str)
                })
                .map(str::to_string);
        }
        if matches!(step, Step::CreateTeam(_)) {
            if let Some(team) = receipt
                .get("team")
                .and_then(Value::as_str)
                .filter(|team| !team.is_empty())
            {
                actual_scope.team = Some(team.to_string());
            }
        }
        pending |= success
            && (receipt.get("ready").and_then(Value::as_bool) == Some(false)
                || receipt.get("status").and_then(Value::as_str) == Some("pending_tool_load"));
        results.push(json!({"action":step.name(),"agent":step.agent(),"ok":success,"phase":if stop_unconfirmed {"stop_not_confirmed"} else {"handler"},"mode":receipt.get("start_mode").or_else(||receipt.get("mode")).cloned().unwrap_or(json!("unknown")),"receipt":receipt}));
        if let Step::UnifiedStart { description, .. } = step {
            if let Some(result) = results.last_mut() {
                result["op"] = receipt.get("op").cloned().unwrap_or(json!(description.op));
                result["declaration"] = receipt.get("declaration").cloned().unwrap_or_else(|| description.declaration.clone());
                result["defaulted"] = receipt.get("defaulted").cloned().unwrap_or_else(|| description.defaulted.clone());
            }
        }
        if success {
            let refreshed = started_files.into_iter().map(|index|
                std::fs::read(&plan.files[index].0).map(|bytes| (index, bytes)))
                .collect::<Result<Vec<_>, _>>();
            match refreshed {
                Ok(files) => for (index, bytes) in files { plan.files[index].1 = bytes; },
                Err(error) => {
                    ok = false;
                    known_no_effect = false;
                    results.push(json!({"action":step.name(),"agent":step.agent(),"ok":false,"phase":"file_refresh","error":crate::redaction::redact_external_text(&error.to_string())}));
                    break;
                }
            }
            if let Err(error) = runtime::advance_topology(&mut plan, step, &receipt) {
                ok = false;
                known_no_effect = false;
                results.push(json!({"action":step.name(),"agent":step.agent(),"ok":false,"phase":"topology_refresh","error":crate::redaction::redact_external_text(&error.to_string())}));
                break;
            }
        }
        if !success {
            ok = false;
            if plan.intent.verb != Verb::Start {
                break;
            }
        }
        if matches!(step, Step::StopAgent(_)) {
            if !success {
                break;
            }
            stop_succeeded = true;
        }
    }
    let partial = !ok
        && (stop_succeeded
            || partial_effect
            || results
                .iter()
                .any(|result| result.get("ok").and_then(Value::as_bool) == Some(true)));
    let recovery = if partial || (!ok && !known_no_effect) {
        let team = actual_scope
            .team
            .as_deref()
            .map(|team| format!(" --team {}", shell_quote(team)))
            .unwrap_or_default();
        Some(format!("team-agent doctor --workspace {}{team} --json; inspect confirmed/unknown effects before retrying; no automatic rollback is claimed", shell_quote(&actual_scope.workspace.to_string_lossy())))
    } else if !ok {
        refusal_hint
    } else {
        None
    };
    let mut value = json!({"ok":ok,"action":plan.intent.verb,"surface":plan.intent.surface,"scope":actual_scope,"session_policy":plan.intent.session,"status":if partial {"partial"} else if !ok {"failed"} else if pending {"pending"} else if results.is_empty() {if plan.intent.verb == Verb::Stop {"already_stopped"} else {"already_running"}} else {"actions_completed"},"steps":results,"skipped":plan.skipped,"effects":effects,"configuration_note":"Running processes were not edited; role body synchronization and business readiness are not claimed.","last_confirmed_effect":if stop_succeeded {Some("original_instance_stopped")} else if handler_effect_confirmed {Some("handler_effects_confirmed")} else {None},"current_state":if !ok {Some(if known_no_effect && !partial {"unchanged"} else {"unknown"})} else {None},"recovery":recovery});
    if let Some(receipt) = value["steps"].as_array().into_iter().flatten()
        .find(|step| step["ok"] == false)
        .and_then(|step| step.get("receipt")).cloned()
    {
        for field in ["blockers", "next_actions"] {
            if let Some(detail) = receipt.get(field) { value[field] = detail.clone(); }
        }
    }
    append_restart_binding_status(&mut value);
    Ok(CmdResult::from_json(value, plan.intent.json))
}

fn append_restart_binding_status(value: &mut Value) {
    let binding = value.get("steps").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|step| step.get("receipt"))
        .find(|receipt| receipt.get("leader_bind_ok").and_then(Value::as_bool) == Some(false))
        .cloned();
    let Some(binding) = binding else { return; };
    for field in ["leader_bind_ok", "leader_bind_reason", "warnings", "next_actions"] {
        if let Some(detail) = binding.get(field) { value[field] = detail.clone(); }
    }
    if value.get("ok").and_then(Value::as_bool) == Some(true)
        && binding.get("status").and_then(Value::as_str) == Some("restarted_binding_pending") {
        value["status"] = json!("restarted_binding_pending");
        value["current_state"] = json!("running");
    }
}

/// Pure registered-seat truth table. Caller supplies physical evidence, not status guesses.
fn seat_actions(
    verb: Verb,
    observation: Observation,
    paused: bool,
    explicit_stop: bool,
    patch: bool,
    team: bool,
) -> Result<Vec<&'static str>, CliError> {
    if paused {
        return if team {
            Ok(vec![])
        } else {
            Err(refusal("paused agent: start/restart does not unpause"))
        };
    }
    if team && matches!(verb, Verb::Start | Verb::Restart) && explicit_stop {
        return Ok(vec![]);
    }
    if verb == Verb::Start && observation == Observation::Live && explicit_stop {
        return Err(refusal("live process contradicts the explicit-stop fence; inspect the selected agent before start"));
    }
    if observation == Observation::Unknown {
        return Err(refusal(
            "unknown physical identity; no lifecycle action performed",
        ));
    }
    match (verb, observation) {
        (Verb::Start, Observation::Live) if patch => Err(refusal(
            "running agent: stop it before applying role settings",
        )),
        (Verb::Start, Observation::Live) => Ok(vec![]),
        (Verb::Start, Observation::ProviderExited) => Ok(vec!["stop", "start"]),
        (Verb::Start, _) => Ok(vec!["start"]),
        (Verb::Restart, Observation::Live | Observation::NativeReconcile | Observation::ProviderExited) => Ok(vec!["stop", "start"]),
        (Verb::Restart, Observation::StoppedVerified) => Ok(vec!["start"]),
        (Verb::Restart, _) => Err(refusal(
            "restart requires a registered, previously started agent",
        )),
        (Verb::Stop, Observation::StoppedVerified | Observation::Unstarted) => Ok(vec![]),
        (Verb::Stop, Observation::Live | Observation::NativeReconcile | Observation::ProviderExited) => Ok(vec!["stop"]),
        _ => Err(refusal("unsupported lifecycle observation")),
    }
}
