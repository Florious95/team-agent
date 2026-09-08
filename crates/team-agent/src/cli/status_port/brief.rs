//! Seven-field, read-only status projection backed by one bounded nodeprobe sample.
//!
//! This intentionally does not reuse RuntimeSnapshot: the legacy snapshot reads
//! coordinator/db/history and carries diagnostic fields that are outside the
//! status brief contract.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const NODEPROBE_TIMEOUT: Duration = Duration::from_secs(2);
const NODEPROBE_OUTPUT_LIMIT: u64 = 1024 * 1024;

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
    let provider = string_at(value, &["provider"])
        .or_else(|| string_at(state, &["provider"]))
        .unwrap_or_else(|| "unknown".to_string());
    RegisteredNode {
        name: name.to_string(),
        provider,
        endpoint: endpoint(value).or_else(|| endpoint(state)),
        session: first_string(value, &["session_name", "session"])
            .or_else(|| first_string(value.pointer("/display").unwrap_or(&Value::Null), &["session_name", "session"]))
            .or_else(|| first_string(value.pointer("/target").unwrap_or(&Value::Null), &["session_name", "session"]))
            .or_else(|| first_string(state, &["session_name"])),
        window: first_string(value, &["window_name", "window", "layout_window"])
            .or_else(|| {
                first_string(
                    value.pointer("/display").unwrap_or(&Value::Null),
                    &["window_name", "window", "layout_window"],
                )
            })
            .or_else(|| {
                first_string(
                    value.pointer("/target").unwrap_or(&Value::Null),
                    &["window_name", "window", "layout_window"],
                )
            }),
        pane: first_string(value, &["pane_id"])
            .or_else(|| first_string(value.pointer("/display").unwrap_or(&Value::Null), &["pane_id"]))
            .or_else(|| first_string(value.pointer("/target").unwrap_or(&Value::Null), &["pane_id"])),
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
    let mut endpoints = BTreeMap::new();
    for endpoint in nodes.iter().filter_map(|node| node.endpoint.as_deref()) {
        endpoints.entry(endpoint.to_string()).or_insert_with(|| {
            run_nodeprobe(endpoint)
                .and_then(|bytes| parse_probe(&bytes).ok())
        });
    }
    endpoints
}

fn run_nodeprobe(endpoint: &str) -> Option<Vec<u8>> {
    let mut child = spawn_nodeprobe(endpoint)?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(NODEPROBE_OUTPUT_LIMIT + 1).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + NODEPROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
    let bytes = reader.join().ok()?;
    if bytes.len() as u64 > NODEPROBE_OUTPUT_LIMIT {
        return None;
    }
    Some(bytes)
}

fn spawn_nodeprobe(endpoint: &str) -> Option<Child> {
    let flag = if Path::new(endpoint).is_absolute() { "-S" } else { "-L" };
    let mut command = Command::new("nodeprobe");
    command
        .arg(flag)
        .arg(endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match command.spawn() {
        Ok(child) => Some(child),
        Err(_) => {
            let home = std::env::var_os("HOME")?;
            let fallback_path: PathBuf = PathBuf::from(home).join(".local/bin/nodeprobe");
            let mut fallback = Command::new(fallback_path);
            fallback
                .arg(flag)
                .arg(endpoint)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            fallback.spawn().ok()
        }
    }
}

fn parse_probe(bytes: &[u8]) -> Result<ProbeResult, ()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
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
    let matched = probe.and_then(|probe| {
        let probe = probe.as_ref()?;
        let matches = probe
            .nodes
            .iter()
            .filter(|observed| matches_registered(node, observed))
            .collect::<Vec<_>>();
        (matches.len() == 1).then_some(matches[0])
    });
    let provider = node.provider.clone();
    if stopped {
        return BriefNode {
            name: node.name.clone(),
            provider,
            runtime_status: "stopped".to_string(),
            activity: "unknown".to_string(),
            health: "unknown".to_string(),
            session_name: None,
            tmux_command: None,
        };
    }
    let Some(observed) = matched else {
        return BriefNode::unknown(node);
    };
    if !provider.eq_ignore_ascii_case("unknown")
        && !provider.eq_ignore_ascii_case(&observed.provider)
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
        provider: if provider.eq_ignore_ascii_case("unknown") {
            observed.provider.clone()
        } else {
            provider
        },
        runtime_status: "running".to_string(),
        activity: activity.to_string(),
        health: health.to_string(),
        session_name: observed.session_name.clone(),
        tmux_command: tmux_command(node, observed),
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
    let flag = if Path::new(endpoint).is_absolute() { "-S" } else { "-L" };
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
    fn parse_probe_rejects_non_envelope() {
        assert!(parse_probe(br"{\"error\":\"missing\"}").is_err());
    }
}
