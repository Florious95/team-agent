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
mod tests;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SessionPolicy {
    Auto,
    Preserve,
}
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
    team: Option<String>,
    session: SessionPolicy,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Observation {
    Live,
    StoppedVerified,
    Unstarted,
    Unknown,
}
#[derive(Clone, Debug)]
pub(super) enum Step {
    CreateTeam(QuickStartArgs),
    StartAgent(StartAgentArgs),
    AddAgent(AddAgentArgs),
    StopAgent(StopAgentArgs),
    RestartTeam(RestartArgs),
    Shutdown(ShutdownArgs),
}
impl Step {
    fn name(&self) -> &'static str {
        match self {
            Self::CreateTeam(_) => "quick_start",
            Self::StartAgent(_) => "start_agent",
            Self::AddAgent(_) => "add_agent",
            Self::StopAgent(_) => "stop_agent",
            Self::RestartTeam(_) => "restart_team",
            Self::Shutdown(_) => "shutdown",
        }
    }
    fn agent(&self) -> Option<&str> {
        match self {
            Self::StartAgent(a) => Some(&a.agent),
            Self::AddAgent(a) => Some(&a.agent),
            Self::StopAgent(a) => Some(&a.agent),
            _ => None,
        }
    }
    fn execute(&self) -> Result<CmdResult, CliError> {
        match self {
            Self::CreateTeam(a) => cmd_quick_start(a),
            Self::StartAgent(a) => cmd_start_agent(a),
            Self::AddAgent(a) => cmd_add_agent(a),
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
    files: Vec<(PathBuf, Vec<u8>)>,
    expected: BTreeMap<String, Value>,
}

pub(super) const HELP: &str = "Usage: team-agent start [AGENT|./TEAMDIR] [--from TEAMDIR] [--workspace PATH] [--team KEY] [--session preserve|auto] [--dry-run] [--json]\n       team-agent stop [AGENT] [--workspace PATH] [--team KEY] [--json]\n       team-agent restart AGENT [--session auto|preserve] [--dry-run] [--json]\n       team-agent team <start|stop|restart|status> [TEAMDIR] [OPTIONS]\n       team-agent agent <start|stop|restart|add|remove|reset|clone|fork> AGENT [OPTIONS]\n\nstart is idempotent only after verified physical identity; running roles are not edited.\nstart activates the non-paused roster, including explicitly stopped seats.\nteam restart preserves paused/explicitly stopped seats; its default is session auto.\nLegacy bare/path restart keeps preserve and original receipts. auto is not reset/fresh.\nAgent IDs (including reviewer.v2) are never inferred from directory existence.\nPaths use ./DIR, --from DIR or team start DIR; stop paths require --workspace.\nDry-run is read-only; original lock/native admission remains deferred.\nExamples:\n  team-agent start --from ./roles\n  team-agent start reviewer\n  team-agent team restart --session preserve\n  team-agent agent restart reviewer --dry-run --json";

fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}
fn refusal(message: impl Into<String>) -> CliError {
    CliError::Runtime(message.into())
}
fn path_syntax(value: &str) -> bool {
    matches!(value, "." | "..")
        || value.contains('/')
        || value.contains('\\')
        || value.as_bytes().get(1) == Some(&b':')
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
    for token in tokens {
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

/// None deliberately leaves frozen legacy parsing, defaults and output untouched.
pub(super) fn run(
    command: &str,
    args: &[String],
    cwd: &Path,
) -> Option<Result<CmdResult, CliError>> {
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
        if !flags.contains_key("--session")
            && !flags.contains_key("--dry-run")
            && pos.first().is_none_or(|p| path_syntax(p))
        {
            let surface = Surface::FrozenLegacy;
            debug_assert_eq!(surface, Surface::FrozenLegacy);
            return None;
        }
    }
    Some(run_canonical(command, args, cwd))
}
fn run_canonical(command: &str, args: &[String], cwd: &Path) -> Result<CmdResult, CliError> {
    let (verb, args, explicit_agent, explicit_team) = if matches!(command, "team" | "agent") {
        if args.is_empty()
            || args
                .first()
                .is_some_and(|a| matches!(a.as_str(), "--help" | "-h"))
        {
            return Ok(CmdResult::human(HELP));
        }
        let sub = args[0].as_str();
        if command == "team" && sub == "status" {
            return super::emit::intent_alias("status", &args[1..], cwd);
        }
        if command == "agent" && matches!(sub, "add" | "remove" | "reset" | "clone" | "fork") {
            let original = match sub {
                "add" => "add-agent",
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
        return Ok(CmdResult::human(HELP));
    }
    let (pos, flags) = options(&checked.tokens);
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
        Subject::Team {
            source: Some(existing_path(cwd, source)?),
        }
    } else if restart_path.is_some() {
        Subject::Team { source: None }
    } else if let Some(id) = pos.first() {
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            || id.starts_with('-')
            || id.is_empty()
        {
            return Err(usage("invalid agent ID"));
        }
        if verb == Verb::Restart && !explicit_agent && cwd.join(id).is_dir() {
            return Err(usage(
                "ambiguous restart target: use agent restart ID or --workspace PATH",
            ));
        }
        Subject::Agent(AgentId::new(id))
    } else {
        Subject::Team { source: None }
    };
    let session = match flags.get("--session").map(String::as_str) {
        Some("preserve") if flags.contains_key("--allow-fresh") => {
            return Err(usage("--allow-fresh conflicts with --session preserve"))
        }
        Some("preserve") => SessionPolicy::Preserve,
        Some("auto") => SessionPolicy::Auto,
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
            } => crate::model::paths::team_workspace(source).map_err(|e| usage(e.to_string()))?,
            _ => path_workspace.clone().unwrap_or(cwd.canonicalize()?),
        },
    };
    if !workspace.is_dir() {
        return Err(usage("workspace must be an existing directory"));
    }
    if path_workspace.is_some_and(|path| path != workspace) {
        return Err(usage("positional workspace conflicts with --workspace"));
    }
    let role_file = flags
        .get("--role-file")
        .map(|path| existing_path(cwd, path))
        .transpose()?;
    let patch = if verb == Verb::Start {
        let mut role_args = vec!["intent-agent".to_string()];
        for token in &checked.tokens {
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
    if matches!(subject, Subject::Team { .. })
        && (patch != RoleConfigPatch::default() || role_file.is_some())
    {
        return Err(usage(
            "team start does not apply per-agent role patches; use start AGENT",
        ));
    }
    let intent = Intent {
        surface: Surface::Canonical,
        verb,
        subject,
        workspace,
        team: flags.get("--team").cloned(),
        session,
        dry_run: flags.contains_key("--dry-run"),
        json: flags.contains_key("--json"),
        role_file,
        patch,
    };
    let plan = runtime::plan(intent)?;
    execute_plan(plan, &mut |step| step.execute(), &mut runtime::recheck)
}

fn execute_plan(
    plan: Plan,
    execute: &mut impl FnMut(&Step) -> Result<CmdResult, CliError>,
    recheck: &mut impl FnMut(&Plan, &Step, bool) -> Result<(), CliError>,
) -> Result<CmdResult, CliError> {
    let descriptions: Vec<_> = plan
        .steps
        .iter()
        .map(|s| json!({"action":s.name(),"agent":s.agent()}))
        .collect();
    if plan.intent.dry_run {
        return Ok(CmdResult::from_json(
            json!({"ok":true,"action":plan.intent.verb,"surface":plan.intent.surface,"scope":plan.scope,"session_policy":plan.intent.session,"dry_run":true,"steps":descriptions,"skipped":plan.skipped,"deferred":["original lock-held owner/cohort admission","native/session admission not performed by the readonly planner"]}),
            plan.intent.json,
        ));
    }
    let mut results = Vec::new();
    let mut ok = true;
    let mut stop_succeeded = false;
    for step in &plan.steps {
        if let Err(error) = recheck(&plan, step, stop_succeeded) {
            ok = false;
            results.push(json!({"action":step.name(),"agent":step.agent(),"ok":false,"phase":"recheck","error":error.to_string()}));
            break;
        }
        let result = execute(step);
        let (success, receipt) = match result {
            Ok(result) => (
                result.exit == ExitCode::Ok,
                match result.output {
                    CmdOutput::Json(v) => v,
                    CmdOutput::Human(text) => json!({"text":text}),
                    CmdOutput::None => Value::Null,
                },
            ),
            Err(error) => (false, json!({"ok":false,"error":error.to_string()})),
        };
        results.push(json!({"action":step.name(),"agent":step.agent(),"ok":success,"mode":receipt.get("start_mode").or_else(||receipt.get("mode")).cloned().unwrap_or(json!("unknown")),"receipt":receipt}));
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
    Ok(CmdResult::from_json(
        json!({"ok":ok,"action":plan.intent.verb,"surface":plan.intent.surface,"scope":plan.scope,"session_policy":plan.intent.session,"status":if !ok && stop_succeeded {"partial_stopped"} else if !ok {"failed"} else if results.is_empty() {"already_running"} else {"completed"},"steps":results,"skipped":plan.skipped,"configuration_note":"Running processes were not edited; role body synchronization and business readiness are not claimed.","recovery":if !ok && stop_succeeded {Some("Use start AGENT in the same explicit workspace/team after resolving the reported failure")} else {None}}),
        plan.intent.json,
    ))
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
    if team && verb == Verb::Restart && explicit_stop {
        return Ok(vec![]);
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
        (Verb::Start, _) => Ok(vec!["start"]),
        (Verb::Restart, Observation::Live) => Ok(vec!["stop", "start"]),
        (Verb::Restart, Observation::StoppedVerified) => Ok(vec!["start"]),
        (Verb::Restart, _) => Err(refusal(
            "restart requires a registered, previously started agent",
        )),
        (Verb::Stop, Observation::StoppedVerified | Observation::Unstarted) => Ok(vec![]),
        (Verb::Stop, Observation::Live) => Ok(vec!["stop"]),
        _ => Err(refusal("unsupported lifecycle observation")),
    }
}
