//! Read-only addressing and a synchronous tmux actuator. No coordinator, ledger or receipts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct Endpoint {
    pub agent: String,
    pub socket: String,
    pub pane_id: String,
    pub dialect: String,
}

pub struct Directory {
    pub workspace: PathBuf,
    pub team: Option<String>,
    pub state: Value,
}

/// Read the published JSON snapshot without migrations, locks, SQLite or self-healing writes.
pub fn snapshot(workspace: &Path) -> Result<Value, String> {
    let runtime = workspace.join(".team/runtime/state.json");
    let path = if runtime.exists() {
        runtime
    } else {
        workspace.join(".team/state.json")
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn directory(workspace: &Path, team: Option<&str>) -> Result<Directory, String> {
    let workspace =
        crate::model::paths::canonical_run_workspace(workspace).map_err(|e| e.to_string())?;
    let snapshot = snapshot(&workspace)?;
    let Some(teams) = snapshot
        .get("teams")
        .and_then(Value::as_object)
        .filter(|teams| !teams.is_empty())
    else {
        if let Some(requested) = team {
            let matches = ["team_id", "team_name", "tmux_session", "team_dir"]
                .iter()
                .any(|key| field(&snapshot, key) == Some(requested));
            if !matches {
                return Err(format!("unknown team: {requested}"));
            }
        }
        return Ok(Directory {
            workspace,
            team: team.map(str::to_string),
            state: snapshot,
        });
    };
    let requested = team.or_else(|| field(&snapshot, "active_team_key"));
    let matches: Vec<_> = teams
        .iter()
        .filter(|(key, value)| {
            requested.map_or(teams.len() == 1, |requested| {
                key.as_str() == requested
                    || ["team_id", "team_name", "tmux_session", "team_dir"]
                        .iter()
                        .any(|field_name| field(value, field_name) == Some(requested))
            })
        })
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "team selection is unknown or ambiguous: {}",
            requested.unwrap_or("(none)")
        ));
    }
    let (key, selected) = matches[0];
    let mut state = selected.clone();
    if let Some(object) = state.as_object_mut() {
        for name in ["tmux_socket", "leader"] {
            if name == "leader"
                && teams.len() > 1
                && field(&snapshot, "active_team_key") != Some(key.as_str())
            {
                continue;
            }
            if !object.contains_key(name) {
                if let Some(value) = snapshot.get(name) {
                    object.insert(name.into(), value.clone());
                }
            }
        }
    }
    Ok(Directory {
        workspace,
        team: Some(key.clone()),
        state,
    })
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

impl Directory {
    pub fn endpoint(&self, agent: &str) -> Result<Endpoint, String> {
        let info = if agent == "leader" {
            self.state.get("leader")
        } else {
            self.state
                .get("agents")
                .and_then(|agents| agents.get(agent))
        }
        .ok_or_else(|| format!("unknown recipient: {agent}"))?;
        let pane_id = field(info, "pane_id").ok_or_else(|| format!("{agent} has no tmux pane"))?;
        let socket = field(info, "tmux_socket")
            .or_else(|| field(&self.state, "tmux_socket"))
            .unwrap_or("default");
        Ok(Endpoint {
            agent: agent.to_string(),
            socket: socket.to_string(),
            pane_id: pane_id.to_string(),
            dialect: field(info, "dialect")
                .or_else(|| field(info, "provider"))
                .unwrap_or("enter")
                .to_string(),
        })
    }

    pub fn agents(&self) -> Vec<String> {
        self.state
            .get("agents")
            .and_then(Value::as_object)
            .map(|agents| agents.keys().cloned().collect())
            .unwrap_or_default()
    }
}

fn tmux(socket: &str) -> Command {
    let mut command = Command::new("tmux");
    if socket != "default" && !socket.is_empty() {
        command
            .arg(if socket.contains('/') { "-S" } else { "-L" })
            .arg(socket);
    }
    command
}

fn check_output(output: std::process::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "tmux delivery failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Physical success means paste + submit only, never provider acceptance or task completion.
/// Provider names use the normal Enter dialect; double-enter is an explicit primitive dialect.
pub fn direct_send(socket: &str, pane_id: &str, dialect: &str, text: &str) -> Result<(), String> {
    if !pane_id
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("a concrete tmux pane id (%N) is required".into());
    }
    if text.is_empty() {
        return Err("message must not be empty".into());
    }
    // Never allow payload bytes to terminate bracketed paste and inject a submit key.
    if text.contains('\x1b') || text.contains('\0') {
        return Err("message must not contain ESC or NUL bytes".into());
    }
    static NEXT_BUFFER: AtomicU64 = AtomicU64::new(0);
    let buffer = format!(
        "team-agent-direct-{}-{}",
        std::process::id(),
        NEXT_BUFFER.fetch_add(1, Ordering::Relaxed)
    );
    let mut child = tmux(socket)
        .args(["load-buffer", "-b", &buffer, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("tmux: {e}"))?;
    let write_result = child.stdin.take().unwrap().write_all(text.as_bytes());
    let loaded = child.wait_with_output().map_err(|e| e.to_string())?;
    if let Err(error) = write_result {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
        return Err(error.to_string());
    }
    check_output(loaded)?;
    let mut command = tmux(socket);
    command.args([
        "paste-buffer",
        "-p",
        "-r",
        "-d",
        "-b",
        &buffer,
        "-t",
        pane_id,
        ";",
        "send-keys",
        "-t",
        pane_id,
        "Enter",
    ]);
    if dialect == "double-enter" {
        command.args([";", "send-keys", "-t", pane_id, "Enter"]);
    }
    let result = command
        .output()
        .map_err(|e| e.to_string())
        .and_then(check_output);
    if result.is_err() {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
    }
    result
}

pub fn send(endpoint: &Endpoint, text: &str) -> Result<Value, String> {
    direct_send(&endpoint.socket, &endpoint.pane_id, &endpoint.dialect, text)?;
    Ok(
        json!({"status": "sent", "recipient": endpoint.agent, "pane_id": endpoint.pane_id,
        "delivery": "physical", "accepted": null}),
    )
}

/// Enumerate each published team, retaining only physically live panes.
pub fn locate(workspace: &Path, team: Option<&str>) -> Result<Value, String> {
    let snapshot = snapshot(workspace)?;
    let selectors: Vec<Option<String>> = if let Some(team) = team {
        vec![Some(team.to_string())]
    } else if let Some(teams) = snapshot
        .get("teams")
        .and_then(Value::as_object)
        .filter(|t| !t.is_empty())
    {
        teams.keys().cloned().map(Some).collect()
    } else {
        vec![None]
    };
    let mut endpoints = Vec::new();
    for team in selectors {
        let directory = directory(workspace, team.as_deref())?;
        for agent in std::iter::once("leader".to_string()).chain(directory.agents()) {
            let Ok(endpoint) = directory.endpoint(&agent) else {
                continue;
            };
            let output = tmux(&endpoint.socket)
                .args([
                    "display-message",
                    "-p",
                    "-t",
                    &endpoint.pane_id,
                    "#{pane_dead}",
                ])
                .output()
                .map_err(|e| e.to_string())?;
            if output.status.success() && output.stdout == b"0\n" {
                endpoints.push(
                    json!({"workspace": directory.workspace, "team": directory.team,
                    "agent": endpoint.agent, "socket": endpoint.socket,
                    "pane_id": endpoint.pane_id, "dialect": endpoint.dialect}),
                );
            }
        }
    }
    Ok(json!({"endpoints": endpoints}))
}
