//! Read-only status: native runtime samples plus accepted launch model/effort.
//!
//! This intentionally does not reuse RuntimeSnapshot: the legacy snapshot reads
//! coordinator/db/history and carries diagnostic fields that are outside the
//! status brief contract.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

#[cfg(test)]
use serde::Deserialize;
#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::Read;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::process::{Child, ExitStatus, Stdio};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::mpsc;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::thread;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
const NODEPROBE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const NODEPROBE_OUTPUT_LIMIT: u64 = 1024 * 1024;
#[cfg(test)]
const NODEPROBE_KILL_GRACE: Duration = Duration::from_millis(100);
#[cfg(test)]
const NODEPROBE_RECEIPT_LIMIT: u64 = 64 * 1024;
#[cfg(test)]
const NODEPROBE_BINARY_LIMIT: u64 = 64 * 1024 * 1024;
#[cfg(test)]
const NODEPROBE_READ_CHUNK: usize = 64 * 1024;
#[cfg(test)]
const NODEPROBE_NAME: &str = "nodeprobe";
#[cfg(test)]
const NODEPROBE_RECEIPT_SUFFIX: &str = ".capability.json";
#[cfg(test)]
const NODEPROBE_RECEIPT_SCHEMA: &str = "nodeprobe-capability-v1";
#[cfg(test)]
const NODEPROBE_SOURCE_REPO: &str = "Florious95/team-agent-scratch/nodeprobe";
#[cfg(test)]
const NODEPROBE_SOURCE_COMMIT: &str = "ff316dc0afe8ab280e61d30934e7624579be6224";
#[cfg(test)]
const NODEPROBE_SOURCE_TREE: &str = "5217a41aa914ddcb72c27f39f1b4af9ead68b1b6";
#[cfg(test)]
const NODEPROBE_REPORT_SCHEMA: u64 = 1;
#[cfg(test)]
const NODEPROBE_CAPABILITIES: &[&str] = &["tmux.list-panes", "ps.pid_ppid_stat_comm"];
#[cfg(test)]
const NODEPROBE_FORBIDDEN: &[&str] = &[
    "tmux.capture-pane",
    "tmux.attach",
    "tmux.send-keys",
    "process.argv",
    "pane_body",
];

#[cfg(test)]
thread_local! {
    static TEST_NODEPROBE: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
    static TEST_RESOLVER_DELAY: std::cell::RefCell<Option<TestResolverDelay>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
static NODEPROBE_RESOLVER_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisteredNode {
    name: String,
    provider: String,
    model: Option<String>,
    effort: Option<String>,
    endpoint: Option<String>,
    session: Option<String>,
    window: Option<String>,
    pane: Option<String>,
    lifecycle: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProbeNode {
    socket: String,
    session: String,
    window: String,
    pane: String,
    provider: String,
    activity: String,
    health: String,
    session_name: Option<String>,
    evidence_method: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BriefNode {
    name: String,
    provider: String,
    model: Option<String>,
    effort: Option<String>,
    runtime_status: String,
    activity: String,
    health: String,
    session_name: Option<String>,
    tmux_command: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AttachContext {
    endpoint: String,
    session: String,
    window: String,
    pane: String,
}

#[derive(Debug)]
struct ProbeResult {
    nodes: Vec<ProbeNode>,
}

#[derive(Debug, Deserialize)]
#[cfg(test)]
struct NodeprobeCapabilityReceipt {
    schema: String,
    binary: String,
    binary_sha256: String,
    source_repo: String,
    source_commit: String,
    source_tree: String,
    target: String,
    report_schema: u64,
    capabilities: Vec<String>,
    forbidden: Vec<String>,
}

enum MatchResult<'a> {
    Missing,
    Unique(&'a ProbeNode),
    Ambiguous,
}

/// The CLI's concise JSON projection. `agent` filters the registered list but
/// never synthesizes an unregistered node.
pub(crate) fn status_brief_scoped(
    _workspace: &Path,
    state: &Value,
    agent: Option<&str>,
) -> Value {
    let nodes = project_status_nodes(state, agent);
    json!({
        "nodes": nodes
            .into_iter()
            .map(|(node, _)| node.into_json())
            .collect::<Vec<_>>()
    })
}

/// Human output keeps the legacy single-node form and groups shared attach
/// context only when multiple nodes make that repetition useful.
pub(crate) fn format_status_brief(
    _workspace: &Path,
    state: &Value,
    agent: Option<&str>,
) -> String {
    let nodes = project_status_nodes(state, agent);
    if nodes.len() <= 1 {
        return nodes
            .into_iter()
            .filter_map(|(node, _)| format_brief_node(&node.into_json()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    format_multi_node_brief(nodes)
}

fn project_status_nodes(
    state: &Value,
    agent: Option<&str>,
) -> Vec<(BriefNode, Option<AttachContext>)> {
    let registered = registered_nodes(state, agent);
    let probes = probe_registered_endpoints(&registered);
    registered
        .iter()
        .map(|node| {
            project_node_with_attach(node, probes.get(node.endpoint.as_deref().unwrap_or("")))
        })
        .collect()
}

pub(crate) fn registered_agent_exists(state: &Value, agent: &str) -> bool {
    registered_nodes(state, None)
        .iter()
        .any(|node| node.name == agent)
}

#[cfg(test)]
pub(crate) fn with_test_nodeprobe<R>(path: PathBuf, func: impl FnOnce() -> R) -> R {
    with_test_nodeprobe_candidates(vec![path], func)
}

#[cfg(test)]
pub(crate) fn with_test_nodeprobe_unbound<R>(path: PathBuf, func: impl FnOnce() -> R) -> R {
    with_test_nodeprobe_candidate(vec![path], func)
}

#[cfg(test)]
pub(crate) fn with_test_nodeprobe_candidates<R>(
    paths: Vec<PathBuf>,
    func: impl FnOnce() -> R,
) -> R {
    for path in &paths {
        write_test_capability_receipt(path);
    }
    with_test_nodeprobe_candidate(paths, func)
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ResolverDelaySignal {
    entered: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

#[cfg(test)]
impl ResolverDelaySignal {
    pub(crate) fn entered(&self) -> bool {
        self.entered.load(Ordering::Acquire)
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[derive(Clone)]
struct TestResolverDelay {
    delay: Duration,
    signal: ResolverDelaySignal,
}

#[cfg(test)]
pub(crate) fn with_test_nodeprobe_resolver_delay<R>(
    delay: Duration,
    func: impl FnOnce(&ResolverDelaySignal) -> R,
) -> R {
    struct Guard(Option<TestResolverDelay>);
    impl Drop for Guard {
        fn drop(&mut self) {
            TEST_RESOLVER_DELAY.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let config = TestResolverDelay {
        delay,
        signal: ResolverDelaySignal {
            entered: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
        },
    };
    let signal = config.signal.clone();
    let previous = TEST_RESOLVER_DELAY.with(|slot| slot.replace(Some(config)));
    let _guard = Guard(previous);
    func(&signal)
}

#[cfg(test)]
fn with_test_nodeprobe_candidate<R>(
    paths: Vec<PathBuf>,
    func: impl FnOnce() -> R,
) -> R {
    struct Guard(Option<Vec<PathBuf>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            TEST_NODEPROBE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let previous = TEST_NODEPROBE.with(|slot| slot.replace(Some(paths)));
    let _guard = Guard(previous);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(func));
    let deadline = Instant::now() + NODEPROBE_TIMEOUT;
    while NODEPROBE_RESOLVER_ACTIVE.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::yield_now();
    }
    let resolver_idle = !NODEPROBE_RESOLVER_ACTIVE.load(Ordering::Acquire);
    match outcome {
        Ok(result) => {
            assert!(
                resolver_idle,
                "test nodeprobe fixture ended while resolver worker was still active"
            );
            result
        }
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(test)]
fn write_test_capability_receipt(path: &Path) {
    let binary_sha256 =
        sha256_file(path, Instant::now() + NODEPROBE_TIMEOUT).expect("test nodeprobe fixture hash");
    let receipt = json!({
        "schema": NODEPROBE_RECEIPT_SCHEMA,
        "binary": NODEPROBE_NAME,
        "binary_sha256": binary_sha256,
        "source_repo": NODEPROBE_SOURCE_REPO,
        "source_commit": NODEPROBE_SOURCE_COMMIT,
        "source_tree": NODEPROBE_SOURCE_TREE,
        "target": current_nodeprobe_target().expect("test target"),
        "report_schema": NODEPROBE_REPORT_SCHEMA,
        "capabilities": NODEPROBE_CAPABILITIES,
        "forbidden": NODEPROBE_FORBIDDEN,
    });
    std::fs::write(nodeprobe_receipt_path(path), serde_json::to_vec(&receipt).unwrap())
        .expect("write test nodeprobe capability receipt");
}

fn format_brief_node(value: &Value) -> Option<String> {
    Some(format!(
        "name: {} provider: {} model: {} effort: {} runtime_status: {} activity: {} health: {} session_name: {} tmux_command: {}",
        value.get("name")?.as_str()?,
        value.get("provider")?.as_str()?,
        display_optional(value.get("model")?),
        display_optional(value.get("effort")?),
        value.get("runtime_status")?.as_str()?,
        value.get("activity")?.as_str()?,
        value.get("health")?.as_str()?,
        display_optional(value.get("session_name")?),
        display_optional(value.get("tmux_command")?),
    ))
}

fn format_multi_node_brief(nodes: Vec<(BriefNode, Option<AttachContext>)>) -> String {
    let mut groups = Vec::<(String, String)>::new();
    for (_, context) in &nodes {
        let Some(context) = context else { continue };
        if !groups.iter().any(|(endpoint, session)| {
            endpoint == &context.endpoint && session == &context.session
        }) {
            groups.push((context.endpoint.clone(), context.session.clone()));
        }
    }

    let mut lines = groups
        .iter()
        .enumerate()
        .map(|(index, (endpoint, session))| {
            format_attach_template(index + 1, endpoint, session)
        })
        .collect::<Vec<_>>();
    if !groups.is_empty() {
        lines.push(
            "TMUX templates: replace <target> with the window.pane part of the matching ATTACH value."
                .to_string(),
        );
    }
    lines.push(
        "NAME\tPROVIDER\tMODEL\tEFFORT\tRUNTIME_STATUS\tACTIVITY\tHEALTH\tSESSION_NAME\tATTACH"
            .to_string(),
    );
    for (node, context) in nodes {
        let attach = context
            .as_ref()
            .and_then(|context| {
                groups
                    .iter()
                    .position(|(endpoint, session)| {
                        endpoint == &context.endpoint && session == &context.session
                    })
                    .map(|index| format!("{}:{}.{}", index + 1, context.window, context.pane))
            })
            .unwrap_or_else(|| "null".to_string());
        lines.push(
            [
                escape_human_cell(&node.name),
                escape_human_cell(&node.provider),
                escape_human_cell(node.model.as_deref().unwrap_or("null")),
                escape_human_cell(node.effort.as_deref().unwrap_or("null")),
                escape_human_cell(&node.runtime_status),
                escape_human_cell(&node.activity),
                escape_human_cell(&node.health),
                escape_human_cell(node.session_name.as_deref().unwrap_or("null")),
                escape_human_cell(&attach),
            ]
            .join("\t"),
        );
    }
    lines.join("\n")
}

fn format_attach_template(group: usize, endpoint: &str, session: &str) -> String {
    let flag = if Path::new(endpoint).is_absolute() {
        "-S"
    } else {
        "-L"
    };
    let target = format!("{session}:<target>");
    format!(
        "TMUX[{group}] tmux {flag} {} attach -t {}",
        shell_quote(endpoint),
        shell_quote(&target)
    )
}

fn escape_human_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn display_optional(value: &Value) -> String {
    value
        .as_str()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn registered_nodes(state: &Value, selected: Option<&str>) -> Vec<RegisteredNode> {
    // A projected state can still carry stale top-level agents. Once the
    // selector identified a canonical team entry, that entry is authoritative;
    // an empty/missing nested roster must not fall back to the stale projection.
    let canonical_team = state
        .get("active_team_key")
        .and_then(Value::as_str)
        .and_then(|key| state.get("teams").and_then(|teams| teams.get(key)));
    let (agents, lifecycle, defaults) = match canonical_team {
        Some(team) => (
            team.get("agents").and_then(Value::as_object),
            team.get("agent_lifecycle").and_then(Value::as_object),
            team,
        ),
        None => (
            state.get("agents").and_then(Value::as_object),
            state.get("agent_lifecycle").and_then(Value::as_object),
            state,
        ),
    };
    let Some(agents) = agents else {
        return Vec::new();
    };
    agents
        .iter()
        .filter(|(name, _)| selected.is_none_or(|selected| selected == name.as_str()))
        .filter(|(name, _)| {
            !lifecycle
                .and_then(|entries| entries.get(*name))
                .and_then(|entry| entry.get("state"))
                .and_then(Value::as_str)
                .is_some_and(|state| state == "retired")
        })
        .map(|(name, value)| registered_node(name, value, defaults))
        .collect()
}

fn registered_node(name: &str, value: &Value, state: &Value) -> RegisteredNode {
    let display = value.pointer("/display").unwrap_or(&Value::Null);
    let target = value.pointer("/target").unwrap_or(&Value::Null);
    let provider = string_at(value, &["provider"])
        .or_else(|| string_at(state, &["provider"]))
        .unwrap_or_else(|| "unknown".to_string());
    RegisteredNode {
        name: name.to_string(),
        provider,
        // Accepted launch settings, not pending role-file edits or guessed
        // provider defaults. Runtime liveness remains a separate signal.
        model: string_at(value, &["model"]),
        effort: string_at(value, &["effort"]),
        endpoint: endpoint(value).or_else(|| endpoint(state)),
        session: first_string(value, &["session_name", "session"])
            .or_else(|| {
                first_string(
                    display,
                    &["target_worker_session", "display_session", "linked_session"],
                )
            })
            .or_else(|| first_string(target, &["session_name", "session"]))
            .or_else(|| first_string(state, &["session_name"])),
        window: first_string(value, &["window_name", "window", "layout_window"])
            .or_else(|| first_string(display, &["window", "window_name", "layout_window"]))
            .or_else(|| first_string(target, &["window_name", "window", "layout_window"])),
        pane: first_string(value, &["pane_id"])
            .or_else(|| first_string(display, &["pane_id"]))
            .or_else(|| first_string(target, &["pane_id"])),
        lifecycle: string_at(value, &["status"]).map(|status| status.to_ascii_lowercase()),
    }
}

fn endpoint(value: &Value) -> Option<String> {
    first_string(value, &["tmux_endpoint", "tmux_socket", "socket"]).or_else(|| {
        first_string(
            value.pointer("/transport").unwrap_or(&Value::Null),
            &["tmux_endpoint", "tmux_socket", "endpoint", "socket"],
        )
    })
}

fn string_at(value: &Value, keys: &[&str]) -> Option<String> {
    first_string(value, keys)
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn probe_registered_endpoints(nodes: &[RegisteredNode]) -> BTreeMap<String, Option<ProbeResult>> {
    #[cfg(test)]
    {
        // Keep the legacy fixture seam for existing hermetic tests; production
        // status never searches for or executes nodeprobe.
        if TEST_NODEPROBE.with(|slot| slot.borrow().is_some()) {
            return probe_registered_endpoints_with_test_nodeprobe(nodes);
        }
    }
    let mut endpoints = BTreeMap::new();
    for endpoint in nodes.iter().filter_map(|node| node.endpoint.as_deref()) {
        if endpoints.contains_key(endpoint) {
            continue;
        }
        endpoints.insert(
            endpoint.to_string(),
            sample_native_endpoint(endpoint, nodes),
        );
    }
    endpoints
}

fn sample_native_endpoint(
    endpoint: &str,
    registered: &[RegisteredNode],
) -> Option<ProbeResult> {
    let output = run_native_tmux_list_panes(endpoint)?;
    Some(sample_native_panes(&output, endpoint, registered, process_snapshots))
}

fn sample_native_panes(
    output: &str,
    endpoint: &str,
    registered: &[RegisteredNode],
    snapshots: impl FnOnce(&BTreeSet<u32>) -> BTreeMap<u32, ProcessSnapshot>,
) -> ProbeResult {
    let panes = output.lines().filter_map(native_pane_fields).collect::<Vec<_>>();
    let pids = panes.iter().filter_map(|fields| native_pane_pid(fields[3])).collect::<BTreeSet<_>>();
    let processes = if pids.is_empty() { BTreeMap::new() } else { snapshots(&pids) };
    let nodes = panes.into_iter()
        .filter_map(|fields| parse_native_pane(fields, endpoint, registered, &processes))
        .collect();
    ProbeResult { nodes }
}

fn native_pane_fields(line: &str) -> Option<[&str; 5]> {
    let mut fields = line.split('\t');
    let session = fields.next()?;
    let window = fields.next()?;
    let pane = fields.next()?;
    let pid = fields.next().unwrap_or_default();
    let command = fields.next().unwrap_or_default();
    if session.is_empty() || window.is_empty() || pane.is_empty() { return None; }
    Some([session, window, pane, pid, command])
}

fn native_pane_pid(pid: &str) -> Option<u32> {
    pid.parse::<u32>().ok().filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
}

fn run_native_tmux_list_panes(endpoint: &str) -> Option<String> {
    const FORMAT: &str = "#{session_name}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}";
    let flag = if Path::new(endpoint).is_absolute() { "-S" } else { "-L" };
    let output = Command::new("tmux")
        .arg(flag)
        .arg(endpoint)
        .args(["list-panes", "-a", "-F", FORMAT])
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8(output.stdout).ok())?
}

fn parse_native_pane(
    fields: [&str; 5],
    endpoint: &str,
    registered: &[RegisteredNode],
    processes: &BTreeMap<u32, ProcessSnapshot>,
) -> Option<ProbeNode> {
    let [session, window, pane, pid, command] = fields;
    let session = session.to_string();
    let window = window.to_string();
    let pane = pane.to_string();
    let matched = registered.iter().find(|node| {
        node.endpoint.as_deref() == Some(endpoint)
            && node.session.as_deref() == Some(session.as_str())
            && node.window.as_deref().is_none_or(|value| value == window)
            && node.pane.as_deref() == Some(pane.as_str())
    });
    if !pid.is_empty() {
        let snapshot = processes.get(&native_pane_pid(pid)?)?;
        if snapshot.zombie {
            return None;
        }
    }
    let provider = matched
        .map(|node| node.provider.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let activity = if command.is_empty() {
        "unknown"
    } else if matches!(command, "bash" | "fish" | "sh" | "zsh" | "-bash" | "-zsh") {
        "idle"
    } else {
        "working"
    };
    Some(ProbeNode {
        socket: endpoint.to_string(),
        session: session.clone(),
        window: window.clone(),
        pane,
        provider,
        activity: activity.to_string(),
        health: "normal".to_string(),
        session_name: Some(session),
        evidence_method: Some("native_tmux".to_string()),
    })
}

struct ProcessSnapshot {
    zombie: bool,
}

fn process_snapshot_command(pids: &BTreeSet<u32>) -> Command {
    let pids = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let mut command = Command::new("ps");
    command.args(["-o", "pid=,ppid=,stat=,comm=", "-p", &pids]);
    command
}

fn process_snapshots(pids: &BTreeSet<u32>) -> BTreeMap<u32, ProcessSnapshot> {
    if pids.is_empty() { return BTreeMap::new(); }
    let Ok(output) = process_snapshot_command(pids).output() else { return BTreeMap::new(); };
    if !output.status.success() { return BTreeMap::new(); }
    parse_process_snapshots(&output.stdout, pids)
}

fn parse_process_snapshots(bytes: &[u8], requested: &BTreeSet<u32>) -> BTreeMap<u32, ProcessSnapshot> {
    let mut snapshots = BTreeMap::<u32, ProcessSnapshot>::new();
    let output = String::from_utf8_lossy(bytes);
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(native_pane_pid) else { continue; };
        if !requested.contains(&pid) { continue; }
        let Some(_ppid) = fields.next() else { continue; };
        let Some(stat) = fields.next() else { continue; };
        let zombie = stat.contains('Z');
        snapshots.entry(pid)
            .and_modify(|snapshot| snapshot.zombie |= zombie)
            .or_insert(ProcessSnapshot { zombie });
    }
    snapshots
}

#[cfg(test)]
fn probe_registered_endpoints_with_test_nodeprobe(
    nodes: &[RegisteredNode],
) -> BTreeMap<String, Option<ProbeResult>> {
    let deadline = Instant::now() + NODEPROBE_TIMEOUT;
    let candidates = nodeprobe_candidates();
    let resolver_options = resolver_options();
    let mut endpoints = BTreeMap::new();
    let mut selected_binary = None;
    let mut resolved = false;
    for endpoint in nodes.iter().filter_map(|node| node.endpoint.as_deref()) {
        if endpoints.contains_key(endpoint) {
            continue;
        }
        if !resolved {
            selected_binary = resolve_nodeprobe_binary(
                candidates.clone(),
                deadline,
                resolver_options.clone(),
            );
            resolved = true;
        }
        let sampled = selected_binary.as_deref().and_then(|binary| {
            run_nodeprobe(binary, endpoint, deadline)
                .and_then(|bytes| parse_probe(&bytes, endpoint).ok())
        });
        endpoints.insert(endpoint.to_string(), sampled);
    }
    endpoints
}

#[cfg(test)]
fn run_nodeprobe(binary: &Path, endpoint: &str, deadline: Instant) -> Option<Vec<u8>> {
    if Instant::now() >= deadline {
        return None;
    }
    let mut child = spawn_nodeprobe(binary, endpoint)?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_nodeprobe(&mut child);
            return None;
        }
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take(NODEPROBE_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes);
        let _ = tx.send(read.map(|_| bytes));
    });
    loop {
        let now = Instant::now();
        if now >= deadline {
            terminate_nodeprobe(&mut child);
            let _ = rx.recv_timeout(NODEPROBE_KILL_GRACE);
            return None;
        }
        let slice = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(20));
        match rx.recv_timeout(slice) {
            Ok(Ok(bytes)) => {
                let status = wait_child_bounded(&mut child, deadline)?;
                if !status.success() || bytes.len() as u64 > NODEPROBE_OUTPUT_LIMIT {
                    terminate_nodeprobe(&mut child);
                    return None;
                }
                return Some(bytes);
            }
            Ok(Err(_)) => {
                terminate_nodeprobe(&mut child);
                return None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        terminate_nodeprobe(&mut child);
                        let leftover = deadline.saturating_duration_since(Instant::now());
                        let _ = rx.recv_timeout(leftover.min(NODEPROBE_KILL_GRACE));
                        return None;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    terminate_nodeprobe(&mut child);
                    return None;
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                terminate_nodeprobe(&mut child);
                return None;
            }
        }
    }
}

#[cfg(test)]
fn wait_child_bounded(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                terminate_nodeprobe(child);
                return None;
            }
            Err(_) => {
                terminate_nodeprobe(child);
                return None;
            }
        }
    }
}

#[cfg(test)]
fn terminate_nodeprobe(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        let _ = unsafe { libc::killpg(pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let deadline = Instant::now() + NODEPROBE_KILL_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[cfg(test)]
fn spawn_nodeprobe(binary: &Path, endpoint: &str) -> Option<Child> {
    let flag = if Path::new(endpoint).is_absolute() {
        "-S"
    } else {
        "-L"
    };
    let mut command = Command::new(binary);
    command.arg(flag).arg(endpoint);
    configure_nodeprobe_command(&mut command);
    command.spawn().ok()
}

#[derive(Clone, Default)]
#[cfg(test)]
struct ResolverOptions {
    #[cfg(test)]
    delay_after_selection: Option<TestResolverDelay>,
}

#[cfg(test)]
fn resolver_options() -> ResolverOptions {
    #[cfg(test)]
    {
        return ResolverOptions {
            delay_after_selection: TEST_RESOLVER_DELAY.with(|slot| slot.borrow().clone()),
        };
    }
    #[cfg(not(test))]
    {
        ResolverOptions::default()
    }
}

// A resolver that is stuck in an uninterruptible filesystem syscall may outlive
// the caller deadline. It owns only its candidate work; the active gate prevents
// repeated status calls from accumulating more detached resolver threads.
#[cfg(test)]
struct ResolverActiveGuard {
    #[cfg(test)]
    finished_signal: Option<ResolverDelaySignal>,
}

#[cfg(test)]
impl Drop for ResolverActiveGuard {
    fn drop(&mut self) {
        NODEPROBE_RESOLVER_ACTIVE.store(false, Ordering::Release);
        #[cfg(test)]
        if let Some(signal) = self.finished_signal.take() {
            signal.finished.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
fn resolve_nodeprobe_binary(
    candidates: Vec<PathBuf>,
    deadline: Instant,
    options: ResolverOptions,
) -> Option<PathBuf> {
    #[cfg(not(test))]
    let _ = options;
    if Instant::now() >= deadline
        || NODEPROBE_RESOLVER_ACTIVE.swap(true, Ordering::AcqRel)
    {
        return None;
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("team-agent-nodeprobe-resolver".to_string())
        .spawn(move || {
            #[cfg(test)]
            let delay_signal = options
                .delay_after_selection
                .as_ref()
                .map(|config| config.signal.clone());
            let _guard = ResolverActiveGuard {
                #[cfg(test)]
                finished_signal: delay_signal,
            };
            let result = select_nodeprobe_binary(candidates, deadline);
            if result.is_some() {
                #[cfg(test)]
                if let Some(config) = options.delay_after_selection {
                    config.signal.entered.store(true, Ordering::Release);
                    thread::sleep(config.delay);
                }
            }
            let _ = tx.send(result);
        });
    if worker.is_err() {
        NODEPROBE_RESOLVER_ACTIVE.store(false, Ordering::Release);
        return None;
    }
    let timeout = deadline.saturating_duration_since(Instant::now());
    // Only the caller may turn a timely result into a child process. A late
    // resolver result is dropped with the receiver and can never spawn probe.
    match rx.recv_timeout(timeout) {
        Ok(Some(binary)) if Instant::now() < deadline => Some(binary),
        Ok(_) | Err(_) => None,
    }
}

#[cfg(test)]
fn select_nodeprobe_binary(
    candidates: Vec<PathBuf>,
    deadline: Instant,
) -> Option<PathBuf> {
    for candidate in candidates {
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(binary) = validate_nodeprobe_candidate(&candidate, deadline) {
            return Some(binary);
        }
    }
    None
}

#[cfg(test)]
fn nodeprobe_candidates() -> Vec<PathBuf> {
    #[cfg(test)]
    {
        let override_bins = TEST_NODEPROBE.with(|slot| slot.borrow().clone());
        if let Some(paths) = override_bins {
            return paths;
        }
    }
    let mut candidates = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|entry| entry.join(NODEPROBE_NAME))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin").join(NODEPROBE_NAME));
    }
    candidates
}

#[cfg(test)]
fn validate_nodeprobe_candidate(path: &Path, deadline: Instant) -> Option<PathBuf> {
    if Instant::now() >= deadline {
        return None;
    }
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return None;
    }
    let receipt_path = nodeprobe_receipt_path(path);
    let receipt_metadata = std::fs::metadata(&receipt_path).ok()?;
    if !receipt_metadata.is_file() || receipt_metadata.len() > NODEPROBE_RECEIPT_LIMIT {
        return None;
    }
    let receipt: NodeprobeCapabilityReceipt =
        serde_json::from_slice(&read_bounded_file(
            &receipt_path,
            NODEPROBE_RECEIPT_LIMIT,
            deadline,
        )?)
        .ok()?;
    if !valid_nodeprobe_receipt(&receipt, path) || Instant::now() >= deadline {
        return None;
    }
    let digest = sha256_file(path, deadline)?;
    (digest == receipt.binary_sha256).then_some(path.to_path_buf())
}

#[cfg(test)]
fn valid_nodeprobe_receipt(receipt: &NodeprobeCapabilityReceipt, binary: &Path) -> bool {
    receipt.schema == NODEPROBE_RECEIPT_SCHEMA
        && binary.file_name().and_then(|name| name.to_str()) == Some(NODEPROBE_NAME)
        && receipt.binary == NODEPROBE_NAME
        && valid_sha256(&receipt.binary_sha256)
        && receipt.source_repo == NODEPROBE_SOURCE_REPO
        && receipt.source_commit == NODEPROBE_SOURCE_COMMIT
        && receipt.source_tree == NODEPROBE_SOURCE_TREE
        && current_nodeprobe_target().is_some_and(|target| receipt.target == target)
        && receipt.report_schema == NODEPROBE_REPORT_SCHEMA
        && same_string_set(&receipt.capabilities, NODEPROBE_CAPABILITIES)
        && same_string_set(&receipt.forbidden, NODEPROBE_FORBIDDEN)
}

#[cfg(test)]
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
fn same_string_set(actual: &[String], expected: &[&str]) -> bool {
    let mut actual = actual.to_vec();
    actual.sort_unstable();
    let mut expected = expected
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    actual == expected
}

#[cfg(test)]
fn nodeprobe_receipt_path(binary: &Path) -> PathBuf {
    let name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(NODEPROBE_NAME);
    binary.with_file_name(format!("{name}{NODEPROBE_RECEIPT_SUFFIX}"))
}

#[cfg(test)]
fn read_bounded_file(path: &Path, limit: u64, deadline: Instant) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut file = open_readonly_nonblocking(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut chunk = [0u8; NODEPROBE_READ_CHUNK];
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let read = file.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() as u64 > limit {
            return None;
        }
    }
}

#[cfg(test)]
fn sha256_file(path: &Path, deadline: Instant) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > NODEPROBE_BINARY_LIMIT {
        return None;
    }
    let mut file = open_readonly_nonblocking(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; NODEPROBE_READ_CHUNK];
    let mut total = 0u64;
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let read = file.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(format!("{:x}", hasher.finalize()));
        }
        total = total.saturating_add(read as u64);
        if total > NODEPROBE_BINARY_LIMIT {
            return None;
        }
        hasher.update(&chunk[..read]);
    }
}

#[cfg(test)]
fn open_readonly_nonblocking(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path).ok()
}

#[cfg(test)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

#[cfg(test)]
fn current_nodeprobe_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

#[cfg(test)]
fn configure_nodeprobe_command(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}

#[cfg(test)]
fn parse_probe(bytes: &[u8], requested_endpoint: &str) -> Result<ProbeResult, ()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(());
    }
    if probe_has_error(&value) {
        return Err(());
    }
    let report_socket = value.get("socket").and_then(Value::as_str).ok_or(())?;
    if report_socket != requested_endpoint {
        return Err(());
    }
    let nodes = value
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or(())?
        .iter()
        .filter_map(|node| parse_probe_node(node, report_socket))
        .collect();
    Ok(ProbeResult { nodes })
}

#[cfg(test)]
fn probe_has_error(value: &Value) -> bool {
    match value.get("error") {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
fn parse_probe_node(value: &Value, report_socket: &str) -> Option<ProbeNode> {
    Some(ProbeNode {
        // The accepted producer binds the socket once at the report envelope
        // and omits the redundant per-node field. An explicit node socket is
        // still preserved for strict mismatch rejection below.
        socket: non_empty(value, "socket").unwrap_or_else(|| report_socket.to_string()),
        session: non_empty(value, "session")?,
        window: non_empty(value, "window_name")?,
        pane: non_empty(value, "pane_id")?,
        provider: non_empty(value, "provider")?,
        activity: non_empty(value, "activity")?,
        health: non_empty(value, "health")?,
        session_name: value
            .get("session_name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        evidence_method: value
            .pointer("/evidence/method")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
fn non_empty(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
fn project_node(node: &RegisteredNode, probe: Option<&Option<ProbeResult>>) -> BriefNode {
    project_node_with_attach(node, probe).0
}

fn project_node_with_attach(
    node: &RegisteredNode,
    probe: Option<&Option<ProbeResult>>,
) -> (BriefNode, Option<AttachContext>) {
    let stopped = matches!(
        node.lifecycle.as_deref(),
        Some("stopped" | "done" | "failed" | "error" | "terminated")
    );
    let matched = match_result(node, probe);
    if stopped {
        let projected = match matched {
            MatchResult::Missing => BriefNode {
                name: node.name.clone(),
                provider: node.provider.clone(),
                model: node.model.clone(),
                effort: node.effort.clone(),
                runtime_status: "stopped".to_string(),
                activity: "unknown".to_string(),
                health: "unknown".to_string(),
                session_name: None,
                tmux_command: None,
            },
            MatchResult::Unique(_) | MatchResult::Ambiguous => BriefNode::unknown(node),
        };
        return (projected, None);
    }
    let MatchResult::Unique(observed) = matched else {
        return (BriefNode::unknown(node), None);
    };
    if !node.provider.eq_ignore_ascii_case("unknown")
        && !node.provider.eq_ignore_ascii_case(&observed.provider)
    {
        return (BriefNode::unknown(node), None);
    }
    let pi_channel = observed.provider.eq_ignore_ascii_case("pi")
        && observed.evidence_method.as_deref() == Some("pi_activity_channel");
    let native_tmux = observed.evidence_method.as_deref() == Some("native_tmux");
    let activity = if observed.provider.eq_ignore_ascii_case("pi")
        && !pi_channel
        && !native_tmux
    {
        "unknown"
    } else {
        valid_activity(&observed.activity)
    };
    let health = if observed.provider.eq_ignore_ascii_case("pi") && !pi_channel && !native_tmux {
        "unknown"
    } else {
        valid_health(&observed.health)
    };
    let tmux_command = tmux_command(node, observed);
    let attach = tmux_command.as_ref().map(|_| AttachContext {
        endpoint: observed.socket.clone(),
        session: observed.session.clone(),
        window: observed.window.clone(),
        pane: observed.pane.clone(),
    });
    (
        BriefNode {
            name: node.name.clone(),
            provider: if node.provider.eq_ignore_ascii_case("unknown") {
                observed.provider.clone()
            } else {
                node.provider.clone()
            },
            model: node.model.clone(),
            effort: node.effort.clone(),
            runtime_status: "running".to_string(),
            activity: activity.to_string(),
            health: health.to_string(),
            session_name: observed.session_name.clone(),
            tmux_command,
        },
        attach,
    )
}

fn match_result<'a>(
    node: &RegisteredNode,
    probe: Option<&'a Option<ProbeResult>>,
) -> MatchResult<'a> {
    let Some(probe) = probe.and_then(|probe| probe.as_ref()) else {
        return MatchResult::Missing;
    };
    let mut matches = probe
        .nodes
        .iter()
        .filter(|observed| matches_registered(node, observed));
    let Some(first) = matches.next() else {
        return MatchResult::Missing;
    };
    if matches.next().is_some() {
        MatchResult::Ambiguous
    } else {
        MatchResult::Unique(first)
    }
}

fn matches_registered(node: &RegisteredNode, observed: &ProbeNode) -> bool {
    node.endpoint.as_deref() == Some(observed.socket.as_str())
        && node.session.as_deref() == Some(observed.session.as_str())
        // Older registrations may not persist a window. The endpoint,
        // session, and pane tuple remains the narrow compatibility key; a
        // present window is still an exact constraint.
        && node
            .window
            .as_deref()
            .is_none_or(|window| window == observed.window)
        && node.pane.as_deref() == Some(observed.pane.as_str())
}

fn valid_activity(value: &str) -> &str {
    match value {
        "working" | "idle" => value,
        _ => "unknown",
    }
}

fn valid_health(value: &str) -> &str {
    match value {
        "normal" | "abnormal" | "unknown" => value,
        _ => "unknown",
    }
}

fn tmux_command(node: &RegisteredNode, observed: &ProbeNode) -> Option<String> {
    let endpoint = node.endpoint.as_deref()?;
    let target = format!("{}:{}.{}", observed.session, observed.window, observed.pane);
    let flag = if Path::new(endpoint).is_absolute() {
        "-S"
    } else {
        "-L"
    };
    Some(format!(
        "tmux {flag} {} attach -t {}",
        shell_quote(endpoint),
        shell_quote(&target)
    ))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl BriefNode {
    fn unknown(node: &RegisteredNode) -> Self {
        Self {
            name: node.name.clone(),
            provider: node.provider.clone(),
            model: node.model.clone(),
            effort: node.effort.clone(),
            runtime_status: "unknown".to_string(),
            activity: "unknown".to_string(),
            health: "unknown".to_string(),
            session_name: None,
            tmux_command: None,
        }
    }

    fn into_json(self) -> Value {
        let mut object = Map::new();
        object.insert("name".to_string(), json!(self.name));
        object.insert("provider".to_string(), json!(self.provider));
        object.insert("model".to_string(), json!(self.model));
        object.insert("effort".to_string(), json!(self.effort));
        object.insert("runtime_status".to_string(), json!(self.runtime_status));
        object.insert("activity".to_string(), json!(self.activity));
        object.insert("health".to_string(), json!(self.health));
        object.insert(
            "session_name".to_string(),
            self.session_name.map(Value::String).unwrap_or(Value::Null),
        );
        object.insert(
            "tmux_command".to_string(),
            self.tmux_command.map(Value::String).unwrap_or(Value::Null),
        );
        Value::Object(object)
    }
}

#[cfg(test)]
mod native_ps_batch_regressions {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn registered(endpoint: &str, pane: &str) -> RegisteredNode {
        RegisteredNode {
            name: format!("{endpoint}-{pane}"),
            provider: "pi".into(),
            model: Some("fixture-model".into()),
            effort: Some("high".into()),
            endpoint: Some(endpoint.into()),
            session: Some("hp".into()),
            window: Some("w".into()),
            pane: Some(pane.into()),
            lifecycle: None,
        }
    }

    #[test]
    fn eight_panes_use_one_endpoint_query() {
        let registered = (0..8).map(|i| registered("e1", &format!("%{i}"))).collect::<Vec<_>>();
        let panes = (0..8).map(|i| format!("hp\tw\t%{i}\t{}\tpi\n", 100 + i)).collect::<String>();
        let rows = (0..8).map(|i| format!("{} 1 S pi\n", 100 + i)).collect::<String>();
        let mut calls = Vec::new();
        let result = sample_native_panes(&panes, "e1", &registered, |pids| {
            calls.push(pids.iter().copied().collect::<Vec<_>>());
            parse_process_snapshots(rows.as_bytes(), pids)
        });
        assert_eq!(calls, vec![(100..108).collect::<Vec<_>>()]);
        assert_eq!(result.nodes.len(), 8);
        assert!(result.nodes.iter().all(|node| node.provider == "pi" && node.activity == "working" && node.health == "normal"));
    }

    #[test]
    fn pid_deduplication_and_command_preserve_the_privacy_whitelist() {
        let mut calls = Vec::new();
        let result = sample_native_panes("hp\tw\t%1\t11\tsh\nhp\tw\t%2\t011\tsh\n", "e1", &[], |pids| {
            calls.push(pids.iter().copied().collect::<Vec<_>>());
            parse_process_snapshots(b"11 1 S sh\n", pids)
        });
        assert_eq!(calls, vec![vec![11]]);
        assert_eq!(result.nodes.len(), 2);
        let pids = BTreeSet::from([22, 11]);
        let command = process_snapshot_command(&pids);
        assert_eq!(command.get_program(), "ps");
        assert_eq!(
            command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            vec!["-o", "pid=,ppid=,stat=,comm=", "-p", "11,22"]
        );
        let command = process_snapshot_command(&BTreeSet::from([11]));
        assert_eq!(command.get_args().last().unwrap(), "11");
    }

    #[cfg(unix)]
    #[test]
    fn native_ps_partial_selection_preserves_a_live_pid() {
        let pid = std::process::id();
        let requested = BTreeSet::from([pid, i32::MAX as u32]);
        let command = process_snapshot_command(&requested);
        // Absolute ps avoids unrelated PATH-fixture mutation in parallel unit tests.
        let output = Command::new("/bin/ps").args(command.get_args()).output().unwrap();
        assert!(output.status.success(), "partial PID selection must not poison the live row");
        let snapshots = parse_process_snapshots(&output.stdout, &requested);
        assert!(!snapshots.get(&pid).unwrap().zombie);
        assert!(!snapshots.contains_key(&(i32::MAX as u32)));
    }

    #[test]
    fn single_pane_keeps_its_native_health_and_activity() {
        let registered = vec![registered("e1", "%1")];
        let result = sample_native_panes("hp\tw\t%1\t11\tsh\n", "e1", &registered, |pids| {
            assert_eq!(pids, &BTreeSet::from([11]));
            parse_process_snapshots(b"11 1 S sh\n", pids)
        });
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].provider, "pi");
        assert_eq!(result.nodes[0].activity, "idle");
        assert_eq!(result.nodes[0].health, "normal");
    }

    #[test]
    fn missing_rows_and_zombies_omit_only_their_panes() {
        let panes = "hp\tw\t%1\t11\tsh\nhp\tw\t%2\t22\tpi\nhp\tw\t%3\t33\tpi\nhp\tw\t%4\t\tsh\n";
        let result = sample_native_panes(panes, "e1", &[], |pids| {
            parse_process_snapshots(b"22 1 Z+ dead\n11 1 S sh\n999 1 S unrelated\n33 1\n", pids)
        });
        assert_eq!(result.nodes.iter().map(|node| node.pane.as_str()).collect::<Vec<_>>(), vec!["%1", "%4"]);
        assert!(result.nodes.iter().all(|node| node.activity == "idle" && node.health == "normal"));
    }

    #[test]
    fn empty_or_invalid_pid_sets_do_not_run_ps() {
        let panes = "hp\tw\t%1\t\tsh\nhp\tw\t%2\t0\tsh\nhp\tw\t%3\t1,2\tsh\nhp\tw\t%4\t4294967295\tsh\n\tw\t%5\t99\tsh\n";
        let result = sample_native_panes(panes, "e1", &[], |_| panic!("no valid PID means no ps query"));
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].pane, "%1");
        assert_eq!(result.nodes[0].health, "normal");
    }

    #[test]
    fn snapshot_failure_and_endpoint_results_are_not_shared() {
        let registered = vec![registered("e1", "%1"), registered("e2", "%1")];
        let mut calls = Vec::new();
        let first = sample_native_panes("hp\tw\t%1\t88\tsh\n", "e1", &registered, |pids| {
            calls.push(("e1", pids.iter().copied().collect::<Vec<_>>()));
            BTreeMap::new()
        });
        let second = sample_native_panes("hp\tw\t%1\t88\tsh\n", "e2", &registered, |pids| {
            calls.push(("e2", pids.iter().copied().collect::<Vec<_>>()));
            parse_process_snapshots(b"88 1 S sh\n", pids)
        });
        assert_eq!(calls, vec![("e1", vec![88]), ("e2", vec![88])]);
        assert!(first.nodes.is_empty());
        assert_eq!(second.nodes.len(), 1);
        let failed = Some(first);
        assert_eq!(project_node_with_attach(&registered[0], Some(&failed)).0.health, "unknown");
        let live = Some(second);
        assert_eq!(project_node_with_attach(&registered[1], Some(&live)).0.health, "normal");
    }

    #[test]
    fn duplicate_process_rows_cannot_hide_a_zombie() {
        let requested = BTreeSet::from([11]);
        for rows in [b"11 1 Z dead\n11 1 S sh\n".as_slice(), b"11 1 S sh\n11 1 Z dead\n".as_slice()] {
            assert!(parse_process_snapshots(rows, &requested).get(&11).unwrap().zombie);
        }
        assert!(parse_process_snapshots(b"not a process row\n", &requested).is_empty());
    }

    #[test]
    fn json_and_human_keep_nine_fields_without_ps_details() {
        let registered = vec![registered("e1", "%1"), registered("e1", "%2")];
        let probe = Some(sample_native_panes(
            "hp\tw\t%1\t11\tsh\nhp\tw\t%2\t22\tpi\n", "e1", &registered,
            |pids| parse_process_snapshots(b"22 1 R PRIVATE_COMMAND_DETAIL\n11 1 S PRIVATE_COMMAND_DETAIL\n", pids),
        ));
        let projected = registered.iter().map(|node| project_node_with_attach(node, Some(&probe))).collect::<Vec<_>>();
        let keys = BTreeSet::from(["name", "provider", "model", "effort", "runtime_status", "activity", "health", "session_name", "tmux_command"]);
        for (node, _) in &projected {
            let json = node.clone().into_json();
            assert_eq!(json.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>(), keys);
            assert_eq!(json["model"], "fixture-model");
            assert_eq!(json["effort"], "high");
            assert_eq!(json["runtime_status"], "running");
            assert_eq!(json["health"], "normal");
            assert!(!json.to_string().contains("PRIVATE_COMMAND_DETAIL"));
        }
        let human = format_multi_node_brief(projected);
        let mut lines = human.lines().skip_while(|line| !line.starts_with("NAME\t"));
        assert_eq!(lines.next().unwrap().split('\t').count(), 9);
        assert!(lines.all(|line| line.split('\t').count() == 9));
        assert!(!human.contains("PRIVATE_COMMAND_DETAIL"));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn registered() -> RegisteredNode {
        RegisteredNode {
            name: "worker".to_string(),
            provider: "pi".to_string(),
            model: None,
            effort: None,
            endpoint: Some("/tmp/tmux socket/o'k".to_string()),
            session: Some("sess".to_string()),
            window: Some("win".to_string()),
            pane: Some("%7".to_string()),
            lifecycle: None,
        }
    }

    fn probe(method: &str) -> ProbeNode {
        ProbeNode {
            socket: "/tmp/tmux socket/o'k".to_string(),
            session: "sess".to_string(),
            window: "win".to_string(),
            pane: "%7".to_string(),
            provider: "pi".to_string(),
            activity: "idle".to_string(),
            health: "normal".to_string(),
            session_name: Some("pi-session".to_string()),
            evidence_method: Some(method.to_string()),
        }
    }

    fn producer_error_envelope() -> &'static [u8] {
        br#"{"schema_version":1,"socket":"/tmp/sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[],"error":{"kind":"tmux_inventory","message":"missing"}}"#
    }

    #[test]
    fn status_model_effort_project_accepted_launch_settings_in_all_runtime_states() {
        for settings in [(None, None), (Some("openai-codex/gpt-6-luna"), Some("max"))] {
            let mut node = registered();
            node.model = settings.0.map(str::to_string);
            node.effort = settings.1.map(str::to_string);
            for lifecycle in [None, Some("stopped")] {
                node.lifecycle = lifecycle.map(str::to_string);
                for sample in [None, Some(ProbeResult { nodes: vec![probe("pi_activity_channel")] })] {
                    let value = project_node(&node, Some(&sample)).into_json();
                    assert_eq!(value["model"], json!(settings.0));
                    assert_eq!(value["effort"], json!(settings.1));
                    let human = format_brief_node(&value).unwrap();
                    assert!(human.contains(&format!("model: {} effort: {}", settings.0.unwrap_or("null"), settings.1.unwrap_or("null"))));
                    assert_eq!(value.as_object().unwrap().len(), 9);
                }
            }
        }
    }

    #[test]
    fn status_model_effort_use_canonical_runtime_roster_not_stale_projection() {
        let state = json!({
            "active_team_key": "selected",
            "agents": {"worker": {"model": "stale-model", "effort": "low"}},
            "teams": {"selected": {"agents": {
                "worker": {"model": "accepted-model", "effort": "max"},
                "plain": {}
            }}}
        });
        let selected = registered_nodes(&state, Some("worker"));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].model.as_deref(), Some("accepted-model"));
        assert_eq!(selected[0].effort.as_deref(), Some("max"));
        let plain = registered_nodes(&state, Some("plain"));
        assert!(plain[0].model.is_none());
        assert!(plain[0].effort.is_none());
    }

    #[test]
    fn pi_pane_title_is_unknown_not_idle() {
        let node = registered();
        let observed = probe("pane_title");
        let projected = project_node(&node, Some(&Some(ProbeResult { nodes: vec![observed] })));
        assert_eq!(projected.runtime_status, "running");
        assert_eq!(projected.activity, "unknown");
        assert_eq!(projected.health, "unknown");
    }

    #[test]
    fn exact_probe_generates_quoted_window_pane_command() {
        let node = registered();
        let observed = probe("pi_activity_channel");
        let projected = project_node(&node, Some(&Some(ProbeResult { nodes: vec![observed] })));
        assert_eq!(projected.activity, "idle");
        assert_eq!(
            projected.tmux_command.as_deref(),
            Some("tmux -S '/tmp/tmux socket/o'\\''k' attach -t 'sess:win.%7'"),
        );
    }

    #[test]
    fn stopped_registration_is_retained_without_sample() {
        let mut node = registered();
        node.lifecycle = Some("stopped".to_string());
        let projected = project_node(&node, None);
        assert_eq!(projected.runtime_status, "stopped");
        assert_eq!(projected.activity, "unknown");
        assert!(projected.tmux_command.is_none());
    }

    #[test]
    fn stopped_with_live_sample_is_unknown_conflict() {
        let mut node = registered();
        node.lifecycle = Some("stopped".to_string());
        let observed = probe("pi_activity_channel");
        let projected = project_node(&node, Some(&Some(ProbeResult { nodes: vec![observed] })));
        assert_eq!(projected.runtime_status, "unknown");
        assert!(projected.tmux_command.is_none());
    }

    #[test]
    fn stopped_with_duplicate_live_samples_is_unknown_conflict() {
        let mut node = registered();
        node.lifecycle = Some("stopped".to_string());
        let projected = project_node(
            &node,
            Some(&Some(ProbeResult {
                nodes: vec![probe("pi_activity_channel"), probe("pi_activity_channel")],
            })),
        );
        assert_eq!(projected.runtime_status, "unknown");
        assert!(projected.tmux_command.is_none());
    }

    #[test]
    fn zero_matches_is_unknown_not_panic() {
        let node = registered();
        let mut observed = probe("pi_activity_channel");
        observed.pane = "%9".to_string();
        let projected = project_node(&node, Some(&Some(ProbeResult { nodes: vec![observed] })));
        assert_eq!(projected.runtime_status, "unknown");
    }

    #[test]
    fn multiple_matches_is_unknown_not_indexed() {
        let node = registered();
        let projected = project_node(
            &node,
            Some(&Some(ProbeResult {
                nodes: vec![probe("pi_activity_channel"), probe("pi_activity_channel")],
            })),
        );
        assert_eq!(projected.runtime_status, "unknown");
    }

    // Frozen 1257e8e3 used `br"{\"error\":\"missing\"}"`, which is not a valid
    // raw byte string (VERIFY compile gate: unknown start of token `\\`).
    // Keep that failure as history; this regression uses a real producer envelope.
    #[test]
    fn parse_probe_rejects_producer_error_envelope() {
        assert!(parse_probe(producer_error_envelope(), "/tmp/sock").is_err());
    }

    #[test]
    fn parse_probe_rejects_exit_zero_error_with_nodes() {
        let bytes = br#"{"schema_version":1,"socket":"/tmp/sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[{"socket":"/tmp/sock","session":"sess","window_name":"win","pane_id":"%7","provider":"pi","activity":"idle","health":"normal","evidence":{"method":"pi_activity_channel"}}],"error":{"kind":"corpus_error","message":"providers corpus failed"}}"#;
        assert!(parse_probe(bytes, "/tmp/sock").is_err());
    }

    #[test]
    fn parse_probe_rejects_wrong_report_socket() {
        let bytes = br#"{"schema_version":1,"socket":"/tmp/other","sampled_at":"2026-01-01T00:00:00Z","nodes":[]}"#;
        assert!(parse_probe(bytes, "/tmp/sock").is_err());
    }

    #[test]
    fn parse_probe_accepts_matching_producer_report() {
        let bytes = br#"{"schema_version":1,"socket":"/tmp/sock","sampled_at":"2026-01-01T00:00:00Z","nodes":[{"socket":"/tmp/sock","session":"sess","window_name":"win","pane_id":"%7","provider":"pi","activity":"idle","health":"normal","session_name":"pi-session","evidence":{"method":"pi_activity_channel"}}]}"#;
        let parsed = parse_probe(bytes, "/tmp/sock").expect("valid producer report");
        assert_eq!(parsed.nodes.len(), 1);
        assert_eq!(parsed.nodes[0].pane, "%7");
    }
}
