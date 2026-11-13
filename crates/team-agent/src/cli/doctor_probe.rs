//! Foreground diagnostic DAG. No messages, lifecycle repair, or authority writes.
use super::{CmdOutput, CmdResult, DoctorArgs};
use crate::transport::PaneInfo;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BUDGET: Duration = Duration::from_millis(500);

#[cfg(test)]
mod tests;

/// Evidence method; `status` still determines whether it ran or was BLOCKED.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum Proof {
    Probed,
    Exercised,
}

#[derive(Clone, Serialize)]
struct Link {
    id: String,
    #[serde(rename = "link")]
    name: &'static str,
    status: &'static str,
    proof: Proof,
    message: String,
}
impl Link {
    fn new(n: usize, status: &'static str, message: impl Into<String>) -> Self {
        Self {
            id: format!("L{n}"),
            name: ["scope", "host", "seats", "storage", "dispatch", "return"][n - 1],
            status,
            proof: if n == 4 {
                Proof::Exercised
            } else {
                Proof::Probed
            },
            message: message.into(),
        }
    }
    fn pass(&self) -> bool {
        self.status == "PASS"
    }
}
fn short(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(180)
        .collect()
}
fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn socket(state: &Value) -> String {
    crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(state))
        .map(|(socket, _)| socket)
        .unwrap_or_default()
}
fn receiver_socket(state: &Value) -> String {
    state
        .pointer("/leader_receiver/tmux_socket")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| socket(state))
}
fn running(v: &Value) -> bool {
    matches!(field(v, "status"), "running" | "starting" | "ready")
}
#[derive(serde::Serialize)]
struct Action {
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    argv: Vec<String>,
}
impl Action {
    fn manual(context: String) -> Self {
        Self {
            context: Some(context),
            argv: Vec::new(),
        }
    }
    fn display(&self) -> String {
        self.argv
            .iter()
            .map(|arg| super::shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
fn action(command: &str, workspace: &Path, team: &str) -> Action {
    let mut argv = vec![
        "team-agent".into(),
        command.into(),
        "--workspace".into(),
        workspace.to_string_lossy().into_owned(),
        "--team".into(),
        team.into(),
    ];
    if command == "claim-leader" {
        argv.push("--confirm".into());
    }
    argv.push("--json".into());
    Action {
        context: (command == "claim-leader").then(|| "from the current leader terminal".into()),
        argv,
    }
}
fn restart_action(workspace: &Path, team: &str, seat: Option<&str>) -> Action {
    let mut action = action("restart", workspace, team);
    if let Some(seat) = seat { action.argv.insert(2, seat.into()); }
    action
}
fn address(pane: &str, socket: &str) -> String {
    format!("{pane}@{socket}")
}
fn caller() -> Value {
    let mut v = json!({"cwd": std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()});
    if let Ok(tmux) = std::env::var("TMUX") {
        v["socket"] = json!(tmux.split(',').next().unwrap_or(""));
    }
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        v["pane"] = json!(pane);
    }
    if !field(&v, "socket").is_empty() && !field(&v, "pane").is_empty() {
        if let Ok(panes) = crate::transport_factory::bounded_endpoint_transport(
            field(&v, "socket"),
            Instant::now() + BUDGET,
        )
        .list_targets()
        {
            if let Some(pane) = panes
                .iter()
                .find(|p| p.pane_id.as_str() == field(&v, "pane"))
            {
                v["session"] = json!(pane.session.as_str());
            }
        }
    }
    v
}
fn caller_team(raw: &Value, caller: &Value) -> Option<String> {
    let endpoint = field(caller, "socket");
    let pane = field(caller, "pane");
    let session = field(caller, "session");
    // The session is observed on this exact socket/pane. A live leader-only
    // team needs no running worker to establish the caller's scope.
    if endpoint.is_empty() || pane.is_empty() || session.is_empty() {
        return None;
    }
    let mut matches = raw
        .get("teams")?
        .as_object()?
        .iter()
        .filter_map(|(name, state)| {
            let team_socket = socket(state);
            let worker = state
                .get("agents")
                .and_then(Value::as_object)
                .is_some_and(|agents| {
                    agents.values().filter(|row| running(row)).any(|row| {
                        let worker_socket = field(row, "native_endpoint");
                        field(row, "pane_id") == pane
                            && crate::topology::same_endpoint(
                                if worker_socket.is_empty() {
                                    &team_socket
                                } else {
                                    worker_socket
                                },
                                endpoint,
                            )
                    })
                });
            let anchor = ["leader_receiver", "team_owner"].iter().any(|key| {
                let record = &state[*key];
                let recorded_socket = field(record, "tmux_socket");
                field(record, "pane_id") == pane
                    && crate::topology::same_endpoint(
                        if recorded_socket.is_empty() {
                            &team_socket
                        } else {
                            recorded_socket
                        },
                        endpoint,
                    )
            });
            let session_matches = ["session_name", "tmux_session"]
                .iter()
                .any(|key| field(state, key) == session)
                && crate::topology::same_endpoint(&team_socket, endpoint);
            (!worker && (anchor || session_matches)).then(|| name.clone())
        });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

#[derive(Clone)]
struct Process {
    parent: u32,
    status: String,
}
fn processes() -> Result<HashMap<u32, Process>, String> {
    let argv = ["ps", "-axo", "pid=,ppid=,stat="].map(str::to_owned);
    let result = crate::tmux_backend::RealCommandRunner
        .run_with_budget(&argv, BUDGET)
        .map_err(|e| e.to_string())?;
    if !result.success {
        return Err("process table unavailable".into());
    }
    Ok(result
        .stdout
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let pid = columns.next()?.parse().ok()?;
            let parent = columns.next()?.parse().ok()?;
            let status = columns.next()?.to_owned();
            Some((pid, Process { parent, status }))
        })
        .collect())
}
fn process_provider(table: &HashMap<u32, Process>, root: u32) -> Option<crate::provider::Provider> {
    // Claude can set its tmux command/title to a version string. Observe kernel
    // executable paths and executable argv operands, never pane titles.
    for pid in std::iter::once(root).chain(
        table
            .keys()
            .copied()
            .filter(|pid| *pid != root && descendant(table, *pid, root)),
    ) {
        let path = team_agent_contract::host::process::process_executable_path(pid).ok();
        let mut evidence: Vec<String> = path
            .map(|p| p.to_string_lossy().into_owned())
            .into_iter()
            .collect();
        evidence.extend(
            crate::platform::argv::argv_tokens(pid)
                .unwrap_or_default()
                .into_iter()
                .take(2),
        );
        for item in evidence {
            if item.contains("pi-coding-agent") {
                return Some(crate::provider::Provider::Pi);
            }
            if let Some(provider) = crate::leader::attribute_command_provider(&item) {
                return Some(provider);
            }
        }
    }
    None
}

fn descendant(table: &HashMap<u32, Process>, mut pid: u32, root: u32) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        let Some(p) = table.get(&pid) else {
            return false;
        };
        if p.parent == pid || p.parent <= 1 {
            return false;
        }
        pid = p.parent;
    }
    false
}

/// Only allowlisted MCP identity fields leave the bounded event-log reader.
fn helpers(workspace: &Path, team: &str, agents: &serde_json::Map<String, Value>) -> BTreeMap<String, (u32, String)> {
    let mut latest = BTreeMap::new();
    let Ok(mut file) = File::open(workspace.join(".team/logs/events.jsonl")) else {
        return latest;
    };
    let Ok(meta) = file.metadata() else {
        return latest;
    };
    let start = meta.len().saturating_sub(1024 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return latest;
    }
    let mut bytes = Vec::new();
    if file.take(1024 * 1024).read_to_end(&mut bytes).is_err() {
        return latest;
    }
    for (i, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if start > 0 && i == 0 {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if field(&event, "event") != "mcp.server_started" || field(&event, "owner_team_id") != team
        {
            continue;
        }
        let agent = field(&event, "agent_id");
        if agents.get(agent).is_some_and(crate::contract_runtime::framework::is_contract) {
            continue; // Retained legacy logs are not authority for a native incarnation.
        }
        if let Some(pid) = event
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
        {
            latest.insert(
                agent.to_owned(),
                (pid, field(&event, "birth_identity").to_owned()),
            );
        }
    }
    latest
}

fn reference_executable() -> Result<PathBuf, String> {
    let explicit = std::env::args_os()
        .next()
        .is_some_and(|arg| Path::new(&arg).components().count() > 1);
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    if explicit {
        return Ok(current);
    }
    // A public PATH wrapper may be a script; current_exe is its resolved image,
    // not the wrapper's text. Never compare an ELF/Mach-O hash to shell source.
    let public = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("team-agent"))
            .find(|path| path.is_file())
    });
    Ok(public
        .filter(|p| p.canonicalize().ok() == current.canonicalize().ok())
        .unwrap_or(current))
}

fn host_images(
    workspace: &Path,
    team: &str,
    agents: &serde_json::Map<String, Value>,
    state: &Value,
) -> (&'static str, String, Option<String>) {
    let evidence = helpers(workspace, team, agents);
    let expected = reference_executable().and_then(|p| {
        team_agent_contract::host::process::fingerprint_file(&p, 256 * 1024 * 1024, BUDGET)
            .map_err(|e| e.to_string())
    });
    let mut checked = 0;
    let mut unknown = Vec::new();
    for (agent, row) in agents.iter().filter(|(_, v)| running(v) && !crate::contract_runtime::framework::is_contract(v)) {
        if field(row, "provider") == "fake" && !evidence.contains_key(agent) {
            continue;
        }
        // Reuse the production birth/image/ancestry receipt. A disk pathname,
        // current file hash, or alive PID alone cannot attest an old loaded image.
        #[cfg(unix)]
        if let Ok(Some(admission)) =
            crate::mcp_server::readiness::preflight(workspace, Some(team), agent, state)
        {
            if let Ok(expected) = &expected {
                if *expected != admission.server().identity.executable_sha256 {
                    return (
                        "FAIL",
                        format!("binary_skew: {agent} helper differs from invoked executable"),
                        Some(agent.clone()),
                    );
                }
                checked += 1;
                continue;
            }
        }
        unknown.push(format!("{agent}: helper loaded image unverified"));
    }
    if unknown.is_empty() {
        (
            "PASS",
            format!("tmux responsive; {checked} helper images verified"),
            None,
        )
    } else {
        ("WARN", short(&unknown.join("; ")), None)
    }
}

fn filesystem_probe(
    workspace: &Path,
    team: &str,
    agents: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    let runtime = workspace.join(".team/runtime");
    let mut busy = Vec::new();
    let mut locks = vec![
        runtime.join("state-save.lock"),
        runtime.join("result-collection.lock"),
    ];
    for agent in agents.keys() {
        if let Some(manifest) = crate::station::projection::target(workspace, team, agent)? {
            locks.push(
                crate::station::seat_store::SeatStore::new(workspace, manifest.scope)?.lock_path(),
            );
        }
    }
    for path in locks {
        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => {
                if !crate::platform::file_lock::try_lock_once_nonblocking(&file)
                    .map_err(|e| e.to_string())?
                {
                    busy.push("runtime lock contended");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let parent = runtime.join(".doctor");
    fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let owned = parent.join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir(&owned).map_err(|e| e.to_string())?;
    let first = owned.join("write");
    let final_path = owned.join("committed");
    let payload = format!("doctor:{}:{stamp}", std::process::id());
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&first)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&first, &final_path)?;
        if fs::read(&final_path)? != payload.as_bytes() {
            return Err(std::io::Error::other("probe readback differs"));
        }
        fs::remove_file(&final_path)?;
        #[cfg(unix)]
        File::open(&owned)?.sync_all()?;
        Ok(())
    })();
    // Only these two exclusively-created filenames belong to this invocation.
    for path in [&first, &final_path] {
        let _ = fs::remove_file(path);
    }
    let cleanup = fs::remove_dir(&owned);
    let parent_cleanup = fs::remove_dir(&parent);
    result.map_err(|e| e.to_string())?;
    cleanup.map_err(|e| e.to_string())?;
    match parent_cleanup {
        Ok(()) => {}
        Err(error) if matches!(error.kind(), std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error.to_string()),
    }
    let mut checked = 0;
    for recipient in std::iter::once("leader").chain(agents.keys().map(String::as_str)) {
        match crate::inbox_store::probe_tail(workspace, recipient, Some(team)) {
            Ok((count, partial)) => {
                checked += count;
                if partial {
                    busy.push("inbox tail incomplete or unverified");
                }
            }
            Err(error) if error == "inbox_append_busy" => busy.push("inbox append contended"),
            Err(error) => return Err(error),
        }
    }
    if !busy.is_empty() {
        return Err(format!("INCONCLUSIVE: {}", busy.join("; ")));
    }
    Ok(format!(
        "write/read/rename/delete exercised; {checked} tail records decoded; C3 checksums checked"
    ))
}

fn render(
    links: Vec<Link>,
    workspace: &Path,
    team: &str,
    caller: Value,
    seats: (usize, usize),
    fix: Option<Action>,
    avoid: &str,
    as_json: bool,
    started: Instant,
) -> CmdResult {
    let ok = links.iter().all(Link::pass);
    let mut value = json!({"schema":"team-agent.doctor.v1","ok":ok,"team":team,"workspace":workspace,"links":links});
    if !field(&caller, "socket").is_empty() {
        value["caller"] = caller;
    }
    if let Ok(reference) = reference_executable() {
        value["binary_reference"] = json!(reference);
        if let Some(entry) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("team-agent"))
                .find(|p| p.is_file())
        }) {
            if entry.canonicalize().ok() != reference.canonicalize().ok() {
                value["path_entry"] = json!(entry);
            }
        }
    }
    if let Some(fix) = &fix {
        value["next_action"] = json!(fix);
    }
    if !ok {
        value["avoid"] = json!(avoid);
    }
    let mut result = CmdResult::from_json(value.clone(), as_json);
    if as_json {
        return result;
    } else if ok {
        result.output = CmdOutput::Human(format!("team-agent doctor: all 6 links passed — team {team} healthy ({}/{} seats; {}; {}) [{:.2}s]", seats.0, seats.1, links[5].message, links[4].message, started.elapsed().as_secs_f64()));
    } else {
        let mut lines = Vec::new();
        for link in &links {
            if link.status == "BLOCKED" && !links[0].pass() {
                continue;
            }
            let icon = match link.status {
                "PASS" => "✓",
                "FAIL" => "✗",
                "WARN" => "!",
                _ => "-",
            };
            lines.push(format!(
                "{icon} {} {} [{}]: {}",
                link.id, link.name, link.status, link.message
            ));
        }
        if !links[0].pass() {
            lines.push("- L2–L6 BLOCKED by L1 (scope not resolved)".into());
        }
        if let Some(fix) = fix {
            if let Some(context) = &fix.context {
                lines.push(format!("context: {context}"));
            }
            if !fix.argv.is_empty() {
                lines.push(format!("fix: {}", fix.display()));
            }
        }
        lines.push(format!("avoid: {avoid}"));
        result.output = CmdOutput::Human(lines.join("\n"));
    }
    if let CmdOutput::Human(text) = &mut result.output {
        *text = text.lines().map(human_line).collect::<Vec<_>>().join("\n");
    }
    result
}

fn human_line(text: &str) -> String {
    let clean: String = text.chars().filter(|ch| !ch.is_control()).collect();
    if clean.len() <= 160 {
        return clean;
    }
    let end = clean.char_indices().map(|(index, _)| index)
        .take_while(|index| *index <= 157).last().unwrap_or(0);
    format!("{}...", &clean[..end])
}

pub(super) fn run(args: &DoctorArgs) -> CmdResult {
    let started = Instant::now();
    let caller = caller();
    let ambiguity = args.team.is_none().then(|| crate::direct_delivery::directory_with_reason(&args.workspace, None).err())
        .flatten().filter(|error| error.code == "name_ambiguous");
    let valid_workspace = args.workspace.is_dir();
    let selected = if !valid_workspace {
        Err(crate::state::StateError::TeamSelect(format!("invalid workspace: {}", args.workspace.display())))
    } else if let Some(error) = ambiguity {
        Err(crate::state::StateError::TeamSelect(format!("ambiguous team selection: {}", error.message)))
    } else { crate::state::selector::resolve_active_team_readonly(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    ) };
    let scope = selected
        .map_err(|e| e.to_string())
        .and_then(|mut selected| {
            let raw = crate::direct_delivery::snapshot(&selected.run_workspace)?;
            // General lifecycle selection may silently skip terminal/leader-only
            // teams. Doctor must diagnose a recorded default before that fallback.
            if args.team.is_none() {
                if let Some(active) = raw
                    .get("active_team_key")
                    .and_then(Value::as_str)
                    .filter(|active| {
                        raw.get("teams")
                            .and_then(|teams| teams.get(*active))
                            .is_some()
                    })
                    .filter(|active| *active != selected.team_key)
                {
                    selected = crate::state::selector::resolve_active_team_readonly(
                        &selected.run_workspace,
                        Some(active),
                        crate::state::selector::SelectorMode::RuntimeOnly,
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            crate::model::paths::encode_scope_component(&selected.team_key)
                .map_err(str::to_owned)?;
            if raw.as_object().is_none_or(|v| v.is_empty()) {
                return Err("runtime scope missing".into());
            }
            if let Some(spec) = &args.spec {
                let text = fs::read_to_string(spec).map_err(|e| e.to_string())?;
                crate::model::yaml::loads(&text).map_err(|e| e.to_string())?;
            }
            let directory = crate::direct_delivery::directory(
                &selected.run_workspace,
                Some(&selected.team_key),
            )?;
            if args.team.is_none() {
                if let Some(live) =
                    caller_team(&raw, &caller).filter(|live| live != &selected.team_key)
                {
                    let endpoint = socket(&selected.state);
                    let active = crate::transport_factory::bounded_endpoint_transport(
                        &endpoint,
                        Instant::now() + BUDGET,
                    )
                    .list_targets()
                    .is_ok_and(|panes| !panes.is_empty())
                        && directory.probe("leader", Instant::now() + BUDGET).is_ok();
                    // A surviving workspace/worker server does not make a dead
                    // team's registered leader live (shared-host stale selector).
                    if !active {
                        return Err(format!(
                            "stale active scope {}; caller leads {live}; select --team {live}",
                            selected.team_key
                        ));
                    }
                }
            }
            Ok((selected, directory))
        });
    let (selected, directory) = match scope {
        Ok(scope) => scope,
        Err(error) => {
            let raw = crate::direct_delivery::snapshot(&args.workspace).unwrap_or(Value::Null);
            let ambiguous = error.starts_with("ambiguous team selection:");
            let caller_scope = (args.team.is_none() && !ambiguous).then(|| caller_team(&raw, &caller)).flatten();
            let team = args.team.clone().or_else(|| caller_scope.clone())
                .unwrap_or_else(|| if ambiguous { "<unselected>" } else { "current" }.into());
            let missing_runtime = valid_workspace && [".team/runtime/state.json", ".team/state.json"].iter().all(|path| {
                fs::metadata(args.workspace.join(path)).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            });
            let missing_team = valid_workspace && (missing_runtime || args.team.as_deref().is_some_and(|team| {
                error.starts_with(&format!("team '{team}' not found."))
            }));
            let error = if missing_runtime {
                format!("team '{team}' has no runtime state in this workspace")
            } else {
                error
            };
            let create = format!("team-agent start AGENT --team {} --workspace {} ...", super::shell_quote(&team), super::shell_quote(&args.workspace.to_string_lossy()));
            let links = (1..=6)
                .map(|n| {
                    Link::new(
                        n,
                        if n == 1 { "FAIL" } else { "BLOCKED" },
                        if n == 1 {
                            if missing_team { format!("{}; create it with `{create}`", short(&error)) }
                            else if !valid_workspace { error.clone() } else { short(&error) }
                        } else {
                            "requires L1".into()
                        },
                    )
                })
                .collect();
            return render(
                links,
                &args.workspace,
                &team,
                caller,
                (0, 0),
                Some(if !valid_workspace { Action::manual("correct --workspace to an existing directory".into()) }
                else if ambiguous { Action::manual(format!("select a team explicitly: team-agent doctor --workspace {} --team <NAME> --json", super::shell_quote(&args.workspace.to_string_lossy()))) }
                else if missing_team { Action::manual(create) } else { action(
                    if caller_scope.is_some() { "doctor" } else { "status" },
                    &args.workspace,
                    &team,
                ) }),
                "do not restart or claim a different team",
                args.json,
                started,
            );
        }
    };
    let workspace = &selected.run_workspace;
    let team = &selected.team_key;
    let empty = serde_json::Map::new();
    let agents = selected
        .state
        .get("agents")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut links = vec![Link::new(
        1,
        "PASS",
        format!("{team} resolved; cross-workspace inspection permitted"),
    )];
    let mut fix = None;
    #[cfg(unix)]
    if let Ok(binding) = crate::contract_runtime::backend::read_binding(workspace, team) {
        if crate::contract_runtime::backend::temporary_authority(&binding)
            && matches!(crate::contract_runtime::backend::authority_check(workspace, team),
                Ok(()) | Err(crate::contract_runtime::backend::BackendError::Migration(_)))
        {
            links[0] = Link::new(1, "WARN", format!("{team}: native authority is temporary: {}; stop native seats and run team-agent migrate --team {team}", binding.transport_root.display()));
            fix = Some(action("migrate", workspace, team));
        }
    }
    // Authority is a prerequisite for lifecycle repair. Missing panes cannot be
    // repaired by restart when the native store itself was purged.
    #[cfg(unix)]
    if agents.values().any(crate::contract_runtime::framework::is_contract) {
        if let Err(error) = crate::contract_runtime::backend::authority_check(workspace, team) {
            let next = if matches!(error, crate::contract_runtime::backend::BackendError::Migration(_)) {
                action("migrate", workspace, team)
            } else if matches!(error, crate::contract_runtime::backend::BackendError::AuthorityLost { .. }) {
                Action::manual(crate::contract_runtime::backend::authority_recovery_action(workspace, team, ""))
            } else {
                Action::manual(error.to_string())
            };
            links.push(Link::new(2, "FAIL", error.to_string()));
            for n in 3..=6 { links.push(Link::new(n, "BLOCKED", "native authority unavailable")); }
            return render(links, workspace, team, caller, (0, agents.len()), Some(next),
                "native authority must be restored before lifecycle repair", args.json, started);
        }
    }
    let mut panes: HashMap<String, Vec<PaneInfo>> = HashMap::new();
    let mut host_error = None;
    let target_socket = socket(&selected.state);
    let mut endpoints = vec![if target_socket.is_empty() {
        crate::tmux_backend::socket_name_for_workspace(workspace)
    } else {
        target_socket.clone()
    }];
    for (agent, _) in agents.iter().filter(|(_, v)| running(v)) {
        if let Ok(endpoint) = directory.endpoint(agent) {
            endpoints.push(endpoint.socket);
        }
    }
    endpoints.sort();
    endpoints.dedup();
    for endpoint in endpoints {
        let began = Instant::now();
        let transport =
            crate::transport_factory::bounded_endpoint_transport(&endpoint, began + BUDGET);
        match transport.list_targets() {
            Ok(observed) if began.elapsed() < BUDGET => {
                panes.insert(endpoint, observed);
            }
            Ok(_) => host_error = Some(format!("{endpoint}: host latency exceeds 500ms")),
            Err(error) => host_error = Some(format!("{endpoint}: host unreachable: {error}")),
        }
    }
    let table = processes();
    if let Some(error) = &host_error {
        links.push(Link::new(2, "FAIL", short(error)));
        fix = Some(action("restart", workspace, team));
    } else {
        let (status, message, failed_seat) = match &table {
            Ok(_) => host_images(workspace, team, agents, &selected.state),
            Err(error) => ("WARN", error.clone(), None),
        };
        links.push(Link::new(2, status, format!("{target_socket}: {message}")));
        if status == "FAIL" {
            fix = Some(restart_action(workspace, team, failed_seat.as_deref()));
        }
    }
    let mut live = 0;
    let total = agents.values().filter(|v| running(v)).count();
    let mut faults = Vec::new();
    let mut failed_seats = std::collections::BTreeSet::new();
    let mut unknown = Vec::new();
    let helper_records = helpers(workspace, team, agents);
    for (agent, state) in agents.iter().filter(|(_, v)| running(v)) {
        let endpoint = directory.endpoint(agent);
        let pane = endpoint.as_ref().ok().and_then(|e| {
            panes
                .get(&e.socket)
                .and_then(|all| all.iter().find(|p| p.pane_id.as_str() == e.pane_id))
        });
        let Some(pane) = pane else {
            failed_seats.insert(agent.clone());
            faults.push(format!("{agent}: pane missing"));
            continue;
        };
        let pid = pane.pane_pid.unwrap_or(0);
        let expected = state.get("pane_pid").and_then(Value::as_u64).unwrap_or(0);
        let birth = team_agent_contract::host::process::process_birth_identity(pid).ok();
        let recorded_birth = field(state, "pane_birth_identity");
        if (expected != 0 && expected != u64::from(pid))
            || birth.is_none()
            || (!recorded_birth.is_empty() && birth.as_deref() != Some(recorded_birth))
        {
            failed_seats.insert(agent.clone());
            faults.push(format!("{agent}: ghost/PID identity mismatch"));
            continue;
        }
        if let Ok(table) = &table {
            if table
                .get(&pid)
                .is_some_and(|p| p.status.starts_with(['T', 'Z']))
            {
                failed_seats.insert(agent.clone());
                faults.push(format!("{agent}: process stopped/zombie"));
                continue;
            }
            if let Some((helper, recorded_birth)) = helper_records.get(agent) {
                let birth =
                    team_agent_contract::host::process::process_birth_identity(*helper).ok();
                if !recorded_birth.is_empty() && birth.as_ref() != Some(recorded_birth) {
                    failed_seats.insert(agent.clone());
                    faults.push(format!("{agent}: MCP birth identity mismatch"));
                    continue;
                }
                #[cfg(unix)]
                if recorded_birth.is_empty()
                    && !matches!(
                        crate::mcp_server::readiness::preflight(
                            workspace,
                            Some(team),
                            agent,
                            &selected.state
                        ),
                        Ok(Some(_))
                    )
                {
                    unknown.push(format!("{agent}: MCP birth unverified"));
                }
                if !table.contains_key(helper) || !descendant(table, *helper, pid) {
                    failed_seats.insert(agent.clone());
                    faults.push(format!("{agent}: recorded MCP process missing/reused"));
                    continue;
                }
                if table
                    .get(helper)
                    .is_some_and(|p| p.status.starts_with(['T', 'Z']))
                {
                    failed_seats.insert(agent.clone());
                    faults.push(format!("{agent}: MCP process stopped/zombie"));
                    continue;
                }
            }
        }
        if field(state, "provider") != "fake" {
            match table
                .as_ref()
                .ok()
                .and_then(|table| process_provider(table, pid))
            {
                None => unknown.push(format!("{agent}: executable/argv provider unverified")),
                Some(observed) => {
                    if let Some(expected) =
                        crate::provider::wire::parse_provider(field(state, "provider"))
                    {
                        if !crate::leader::provider_matches(observed, expected) {
                            failed_seats.insert(agent.clone());
                            faults.push(format!("{agent}: provider process mismatch"));
                            continue;
                        }
                    }
                }
            }
        }
        live += 1;
    }
    let seat_status = if host_error.is_some() {
        "BLOCKED"
    } else if !faults.is_empty() {
        "FAIL"
    } else if !unknown.is_empty() {
        "WARN"
    } else {
        "PASS"
    };
    let seat_message = if seat_status == "BLOCKED" {
        "requires responsive seat host".into()
    } else if !faults.is_empty() {
        short(&faults.join("; "))
    } else if !unknown.is_empty() {
        short(&unknown.join("; "))
    } else {
        format!(
            "{live}/{total} running ({}); pane/PID birth verified",
            agents
                .iter()
                .filter(|(_, v)| running(v))
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    links.push(Link::new(3, seat_status, seat_message));
    if seat_status == "FAIL" && fix.is_none() {
        fix = Some(restart_action(workspace, team, failed_seats.first().map(String::as_str)));
    }

    // L4 depends only on scope, not host/seat/dispatch health.
    links.push(match filesystem_probe(workspace, team, agents) {
        Ok(message) => Link::new(4, "PASS", message),
        Err(message) => Link::new(
            4,
            if message.starts_with("INCONCLUSIVE:") {
                "WARN"
            } else {
                "FAIL"
            },
            short(&message),
        ),
    });
    let dispatch = if host_error.is_some() || seat_status == "FAIL" || !links[3].pass() {
        Link::new(
            5,
            "BLOCKED",
            "requires live worker endpoints and verified storage",
        )
    } else {
        let mut errors = Vec::new();
        let mut dialects = Vec::new();
        for (agent, state) in agents.iter().filter(|(_, v)| running(v)) {
            let provider = field(state, "provider");
            if crate::provider::wire::parse_provider(provider).is_none()
                && crate::contract_runtime::registry::descriptor(provider).is_none()
            {
                errors.push(format!("{agent}: unknown input dialect"));
                continue;
            }
            match directory.probe(agent, Instant::now() + BUDGET) {
                Ok(endpoint) => dialects.push(endpoint.dialect),
                Err(error) => errors.push(format!("{agent}: {}", short(&error))),
            }
        }
        dialects.sort();
        dialects.dedup();
        if errors.is_empty() {
            Link::new(
                5,
                "PASS",
                format!(
                    "{total} routes: {} dialect; identity/composer guards; no input submitted",
                    dialects.join(",")
                ),
            )
        } else {
            Link::new(5, "FAIL", short(&errors.join("; ")))
        }
    };
    links.push(dispatch);

    // L6 uses the same fail-closed caller gate as start, then independently
    // probes the receiver. A reachable receiver cannot validate a stale caller.
    let pane = std::env::var("TMUX_PANE").ok().filter(|pane| !pane.is_empty());
    let current_endpoint = crate::tmux_backend::socket_name_from_tmux_env();
    // Like start's caller_tmux_endpoint, scope identity to the server used to
    // observe the caller, not the independently selected team's receiver socket.
    let caller_resolution = crate::layout::worker_env::caller_provider_resolution(
        pane.as_deref(),
        current_endpoint.as_deref(),
        current_endpoint.as_deref(),
    );
    let registered = directory
        .endpoint("leader")
        .map(|endpoint| address(&endpoint.pane_id, &endpoint.socket))
        .unwrap_or_else(|_| address("unbound", &receiver_socket(&selected.state)));
    let caller_address = address(field(&caller, "pane"), field(&caller, "socket"));
    let return_link = match directory.probe("leader", Instant::now() + BUDGET) {
        _ if caller_resolution == crate::layout::worker_env::CallerProviderResolution::Invalid => {
            if fix.is_none() {
                fix = Some(Action::manual(crate::layout::worker_env::CALLER_CONTEXT_RECOVERY.into()));
            }
            Link::new(6, "FAIL", crate::layout::worker_env::invalid_caller_tuple_message(
                pane.as_deref(),
                current_endpoint.as_deref(),
                current_endpoint.as_deref(),
            ))
        }
        Err(error) => {
            if fix.is_none() {
                fix = Some(action("claim-leader", workspace, team));
            }
            Link::new(
                6,
                "FAIL",
                format!(
                    "registered {registered}; caller {caller_address}; leader unreachable: {}",
                    short(&error)
                ),
            )
        }
        Ok(endpoint) => {
            let index = crate::leader::registry::registry_dir().map(|dir| {
                dir.join(format!(
                    "{}__{team}.json",
                    crate::leader::registry::workspace_hash(workspace)
                ))
            });
            if !index.is_some_and(|path| {
                fs::read(path)
                    .ok()
                    .and_then(|b| {
                        serde_json::from_slice::<crate::leader::registry::LeaderRegistryEntry>(&b)
                            .ok()
                    })
                    .is_some_and(|entry| {
                        selected
                            .state
                            .get("leader_receiver")
                            .is_some_and(|receiver| {
                                crate::lifecycle::launch::readiness::canonical_identity_aligned(
                                    workspace,
                                    team,
                                    &selected.state,
                                    receiver,
                                    &entry,
                                )
                            })
                    })
            }) {
                if fix.is_none() {
                    fix = Some(action("claim-leader", workspace, team));
                }
                Link::new(
                    6,
                    "WARN",
                    format!("leader {registered} reachable; caller {caller_address}; discovery index missing/stale"),
                )
            } else {
                let same = field(&caller, "pane") == endpoint.pane_id
                    && crate::topology::same_endpoint(field(&caller, "socket"), &endpoint.socket);
                Link::new(
                    6,
                    "PASS",
                    format!(
                        "leader {registered} reachable; caller {caller_address} {}",
                        if same { "matches" } else { "differs (allowed)" }
                    ),
                )
            }
        }
    };
    links.push(return_link);
    if fix.is_none() {
        fix = links.iter().find(|link| !link.pass()).map(|link| Action::manual(match link.id.as_str() {
            "L4" => format!("restore writable, uncontended storage at {}; preserve inbox records", super::shell_quote(&workspace.join(".team/runtime").to_string_lossy())),
            "L5" => format!("leave copy/edit mode or resolve the draft/menu in the named {team} worker pane; do not submit pending text"),
            _ => format!("verify the reported process/image evidence for team {team}; do not infer health from PID existence"),
        }));
    }
    render(
        links,
        workspace,
        team,
        caller,
        (live, total),
        fix,
        if live == total && host_error.is_none() {
            "do not restart healthy workers or replay inboxes"
        } else {
            "no forced takeover, inbox replay, or broad process kill"
        },
        args.json,
        started,
    )
}
