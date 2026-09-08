//! Seven-field, read-only status projection backed by one bounded nodeprobe sample.
//!
//! This intentionally does not reuse RuntimeSnapshot: the legacy snapshot reads
//! coordinator/db/history and carries diagnostic fields that are outside the
//! status brief contract.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const NODEPROBE_TIMEOUT: Duration = Duration::from_secs(2);
const NODEPROBE_OUTPUT_LIMIT: u64 = 1024 * 1024;
const NODEPROBE_KILL_GRACE: Duration = Duration::from_millis(100);
const NODEPROBE_RECEIPT_LIMIT: u64 = 64 * 1024;
const NODEPROBE_BINARY_LIMIT: u64 = 64 * 1024 * 1024;
const NODEPROBE_READ_CHUNK: usize = 64 * 1024;
const NODEPROBE_NAME: &str = "nodeprobe";
const NODEPROBE_RECEIPT_SUFFIX: &str = ".capability.json";
const NODEPROBE_RECEIPT_SCHEMA: &str = "nodeprobe-capability-v1";
const NODEPROBE_SOURCE_REPO: &str = "Florious95/team-agent-scratch/nodeprobe";
const NODEPROBE_SOURCE_COMMIT: &str = "ff316dc0afe8ab280e61d30934e7624579be6224";
const NODEPROBE_SOURCE_TREE: &str = "5217a41aa914ddcb72c27f39f1b4af9ead68b1b6";
const NODEPROBE_REPORT_SCHEMA: u64 = 1;
const NODEPROBE_CAPABILITIES: &[&str] = &["tmux.list-panes", "ps.pid_ppid_stat_comm"];
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisteredNode {
    name: String,
    provider: String,
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
    runtime_status: String,
    activity: String,
    health: String,
    session_name: Option<String>,
    tmux_command: Option<String>,
}

#[derive(Debug)]
struct ProbeResult {
    nodes: Vec<ProbeNode>,
}

#[derive(Debug, Deserialize)]
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
    let registered = registered_nodes(state, agent);
    let probes = probe_registered_endpoints(&registered);
    let nodes = registered
        .iter()
        .map(|node| project_node(node, probes.get(node.endpoint.as_deref().unwrap_or(""))))
        .collect::<Vec<_>>();
    json!({
        "nodes": nodes.into_iter().map(BriefNode::into_json).collect::<Vec<_>>()
    })
}

/// Human output is the exact same seven-field projection as JSON, rendered in
/// stable field order. Null values remain explicit rather than becoming a
/// diagnostic or an inferred value.
pub(crate) fn format_status_brief(
    workspace: &Path,
    state: &Value,
    agent: Option<&str>,
) -> String {
    let value = status_brief_scoped(workspace, state, agent);
    value
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(format_brief_node)
        .collect::<Vec<_>>()
        .join("\n")
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
    func()
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
        "name: {} provider: {} runtime_status: {} activity: {} health: {} session_name: {} tmux_command: {}",
        value.get("name")?.as_str()?,
        value.get("provider")?.as_str()?,
        value.get("runtime_status")?.as_str()?,
        value.get("activity")?.as_str()?,
        value.get("health")?.as_str()?,
        display_optional(value.get("session_name")?),
        display_optional(value.get("tmux_command")?),
    ))
}

fn display_optional(value: &Value) -> String {
    value
        .as_str()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn registered_nodes(state: &Value, selected: Option<&str>) -> Vec<RegisteredNode> {
    let agents = state
        .get("agents")
        .and_then(Value::as_object)
        .or_else(|| {
            let key = state.get("active_team_key").and_then(Value::as_str)?;
            state
                .get("teams")
                .and_then(|teams| teams.get(key))
                .and_then(|team| team.get("agents"))
                .and_then(Value::as_object)
        });
    let Some(agents) = agents else {
        return Vec::new();
    };
    agents
        .iter()
        .filter(|(name, _)| selected.is_none_or(|selected| selected == name.as_str()))
        .map(|(name, value)| registered_node(name, value, state))
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
    let deadline = Instant::now() + NODEPROBE_TIMEOUT;
    let mut endpoints = BTreeMap::new();
    let mut selected_binary = None;
    let mut resolved = false;
    for endpoint in nodes.iter().filter_map(|node| node.endpoint.as_deref()) {
        if endpoints.contains_key(endpoint) {
            continue;
        }
        if !resolved {
            selected_binary = select_nodeprobe_binary(deadline);
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

fn select_nodeprobe_binary(deadline: Instant) -> Option<PathBuf> {
    for candidate in nodeprobe_candidates() {
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(binary) = validate_nodeprobe_candidate(&candidate, deadline) {
            return Some(binary);
        }
    }
    None
}

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

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

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

fn nodeprobe_receipt_path(binary: &Path) -> PathBuf {
    let name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(NODEPROBE_NAME);
    binary.with_file_name(format!("{name}{NODEPROBE_RECEIPT_SUFFIX}"))
}

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
        .filter_map(parse_probe_node)
        .collect();
    Ok(ProbeResult { nodes })
}

fn probe_has_error(value: &Value) -> bool {
    match value.get("error") {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
        Some(_) => true,
    }
}

fn parse_probe_node(value: &Value) -> Option<ProbeNode> {
    Some(ProbeNode {
        socket: non_empty(value, "socket")?,
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

fn non_empty(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn project_node(node: &RegisteredNode, probe: Option<&Option<ProbeResult>>) -> BriefNode {
    let stopped = matches!(
        node.lifecycle.as_deref(),
        Some("stopped" | "done" | "failed" | "error" | "terminated")
    );
    let matched = match_result(node, probe);
    if stopped {
        return match matched {
            MatchResult::Missing => BriefNode {
                name: node.name.clone(),
                provider: node.provider.clone(),
                runtime_status: "stopped".to_string(),
                activity: "unknown".to_string(),
                health: "unknown".to_string(),
                session_name: None,
                tmux_command: None,
            },
            MatchResult::Unique(_) | MatchResult::Ambiguous => BriefNode::unknown(node),
        };
    }
    let MatchResult::Unique(observed) = matched else {
        return BriefNode::unknown(node);
    };
    if !node.provider.eq_ignore_ascii_case("unknown")
        && !node.provider.eq_ignore_ascii_case(&observed.provider)
    {
        return BriefNode::unknown(node);
    }
    let pi_channel = observed.provider.eq_ignore_ascii_case("pi")
        && observed.evidence_method.as_deref() == Some("pi_activity_channel");
    let activity = if observed.provider.eq_ignore_ascii_case("pi") && !pi_channel {
        "unknown"
    } else {
        valid_activity(&observed.activity)
    };
    let health = if observed.provider.eq_ignore_ascii_case("pi") && !pi_channel {
        "unknown"
    } else {
        valid_health(&observed.health)
    };
    BriefNode {
        name: node.name.clone(),
        provider: if node.provider.eq_ignore_ascii_case("unknown") {
            observed.provider.clone()
        } else {
            node.provider.clone()
        },
        runtime_status: "running".to_string(),
        activity: activity.to_string(),
        health: health.to_string(),
        session_name: observed.session_name.clone(),
        tmux_command: tmux_command(node, observed),
    }
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
        && node.window.as_deref() == Some(observed.window.as_str())
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn registered() -> RegisteredNode {
        RegisteredNode {
            name: "worker".to_string(),
            provider: "pi".to_string(),
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
