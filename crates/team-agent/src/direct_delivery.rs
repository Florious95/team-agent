//! Read-only addressing and a synchronous tmux actuator. No coordinator, ledger or receipts.

use std::io::Write;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
            let matches = crate::state::projection::team_selector_matches(
                requested, field(&snapshot, "team_key").or_else(|| field(&snapshot, "active_team_key")).unwrap_or(""), &snapshot,
            ) || ["team_id", "team_name", "tmux_session", "team_dir"]
                .iter()
                .any(|key| field(&snapshot, key) == Some(requested));
            if !matches {
                return Err(format!("unknown team: {requested}"));
            }
        }
        let derived = crate::state::projection::team_state_key(&snapshot);
        let canonical = field(&snapshot, "active_team_key")
            .or_else(|| (derived != "current").then_some(derived.as_str()))
            .or_else(|| field(&snapshot, "team_id"))
            .or_else(|| field(&snapshot, "team_name"))
            .or(team).map(str::to_string);
        return Ok(Directory { workspace, team: canonical, state: snapshot });
    };
    let requested = team.or_else(|| field(&snapshot, "active_team_key"));
    let matches: Vec<_> = teams
        .iter()
        .filter(|(key, value)| {
            requested.map_or(teams.len() == 1, |requested| {
                crate::state::projection::team_selector_matches(requested, key, value)
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
        for name in ["tmux_socket", "tmux_endpoint", "leader", "leader_receiver"] {
            if matches!(name, "leader" | "leader_receiver")
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

#[cfg(unix)]
fn native_recipient(info: &Value) -> bool {
    crate::contract_runtime::framework::is_contract(info)
        || field(info, "runtime_family") == Some("contract") || field(info, "contract_scope").is_some()
}

impl Directory {
    fn recipient(&self, agent: &str) -> Result<&Value, String> {
        if agent == "leader" {
            self.state.get("leader_receiver").or_else(|| self.state.get("leader"))
        } else {
            self.state
                .get("agents")
                .and_then(|agents| agents.get(agent))
        }
        .ok_or_else(|| format!("unknown recipient: {agent}"))
    }

    #[cfg(unix)]
    fn native_seat(&self, agent: &str) -> Result<Option<(crate::contract_runtime::backend::Binding, team_agent_contract::orchestration::store::SeatRecord)>, String> {
        use team_agent_contract::{contract::types::SeatId, orchestration::store::SeatStatus};
        if agent == "leader" || !native_recipient(self.recipient(agent)?) { return Ok(None); }
        let team = self.team.as_deref().ok_or("native recipient has no team scope")?;
        let backend = crate::contract_runtime::backend::Backend::open_readonly(&self.workspace, team).map_err(|e| e.to_string())?;
        let id = SeatId::new(agent).map_err(|e| e.to_string())?;
        let seat = backend.store.seat(&id).map_err(|e| e.to_string())?.ok_or("native recipient missing")?;
        let target = seat.physical_target().map_err(|e| e.to_string())?;
        if target.candidate_sha256 != crate::contract_runtime::backend::executing_candidate_sha().map_err(|e| e.to_string())? {
            return Err("seat_requires_restart".into());
        }
        crate::contract_runtime::backend::require_candidate(&backend.binding).map_err(|e| e.to_string())?;
        // Committed native seats can remain Starting; Ready is not a commit flag.
        if !matches!(seat.status, SeatStatus::Starting | SeatStatus::Ready)
            || backend.store.leased(&id).map_err(|e| e.to_string())?
            || backend.store.unfinished().map_err(|e| e.to_string())?.iter()
                .any(|operation| crate::contract_runtime::backend::involves_seat(operation, &id)) {
            return Err("recipient_not_running: native lifecycle not settled".into());
        }
        Ok(Some((backend.binding, seat)))
    }

    fn verify_endpoint(&self, agent: &str, expected: &Endpoint, deadline: Instant) -> Result<(), String> {
        let current = self.endpoint(agent)?;
        if current.socket != expected.socket || current.pane_id != expected.pane_id || current.dialect != expected.dialect {
            return Err("identity_mismatch: recipient address changed".into());
        }
        #[cfg(unix)]
        if let Some((binding, seat)) = self.native_seat(agent)? {
            crate::contract_runtime::backend::validate_delivery_target(&binding, &seat, deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| format!("identity_mismatch: {e}"))?;
        }
        Ok(())
    }

    pub fn endpoint(&self, agent: &str) -> Result<Endpoint, String> {
        let info = self.recipient(agent)?;
        if agent != "leader" && (info.get("paused").and_then(Value::as_bool) == Some(true)
            || info.get("explicit_stop").and_then(Value::as_bool) == Some(true)) {
            return Err(format!("recipient_not_running: {agent}"));
        }
        #[cfg(unix)]
        if let Some((_binding, seat)) = self.native_seat(agent)? {
            return Ok(Endpoint { agent: agent.into(), socket: seat.endpoint, pane_id: seat.pane,
                dialect: endpoint_dialect(Some(seat.provider.as_str()), None) });
        }
        if agent != "leader" && field(info, "status") != Some("running") {
            return Err(format!("recipient_not_running: {agent}"));
        }
        let pane_id = field(info, "pane_id").or_else(|| field(info, "native_pane"))
            .ok_or_else(|| format!("{agent} has no tmux pane"))?;
        let socket = field(info, "native_endpoint").map(str::to_string)
            .or_else(|| crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(info)).map(|(socket, _)| socket))
            .or_else(|| crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(Some(&self.state)).map(|(socket, _)| socket))
            .unwrap_or_else(|| crate::tmux_backend::socket_name_for_workspace(&self.workspace));
        Ok(Endpoint {
            agent: agent.to_string(),
            socket,
            pane_id: pane_id.to_string(),
            dialect: endpoint_dialect(field(info, "provider"), field(info, "dialect")),
        })
    }

    /// Check the selected physical identity while holding only its input lock.
    pub fn send(&self, agent: &str, text: &str) -> Result<Value, String> {
        self.send_with_receipt(agent, text, || Ok(()))
    }

    pub(crate) fn send_with_receipt(&self, agent: &str, text: &str, submitted: impl FnOnce() -> Result<(), String>) -> Result<Value, String> {
        #[cfg(unix)]
        let _lifecycle = if agent != "leader" && native_recipient(self.recipient(agent)?) {
            let id = crate::model::ids::AgentId::new(agent);
            Some(crate::lifecycle::lock::try_acquire_seat_lifecycle_lock(crate::lifecycle::lock::LifecycleLockRequest {
                workspace: &self.workspace, operation: "native-send", team: self.team.as_deref(), agent_id: Some(&id),
            }).map_err(|e| format!("recipient_lifecycle_busy: {e}"))?)
        } else { None };
        let endpoint = self.endpoint(agent)?;
        let info = self.recipient(agent)?;
        let expected_pid = info.get("pane_pid").or_else(|| info.get("pid")).or_else(|| info.get("native_pid"))
            .and_then(|value| value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok())))
            .and_then(|pid| u32::try_from(pid).ok()).filter(|pid| *pid > 0);
        direct_send_checked(&endpoint.socket, &endpoint.pane_id, &endpoint.dialect, text, expected_pid, true, submitted,
            |deadline| self.verify_endpoint(agent, &endpoint, deadline))?;
        Ok(json!({"status":"sent","recipient":endpoint.agent,"pane_id":endpoint.pane_id,
            "dialect":endpoint.dialect,"delivery":"physical","accepted":null}))
    }

    pub fn agents(&self) -> Vec<String> {
        self.state
            .get("agents")
            .and_then(Value::as_object)
            .map(|agents| agents.keys().cloned().collect())
            .unwrap_or_default()
    }
}

fn canonical_dialect(name: &str) -> String {
    let normalized = name.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "claude" | "claude_code" | "claudecode" | "claude_tui" => "claude".into(),
        "pi" | "pi_tui" => "pi".into(),
        "cursor" | "cursor_agent" | "agent" => "cursor".into(),
        "codex" | "codex_cli" => "codex".into(),
        "grok" | "grok_build" => "grok".into(),
        "double_enter" => "double-enter".into(),
        "cursor_double_enter" => "cursor-double-enter".into(),
        _ => name.trim().to_string(),
    }
}

fn endpoint_dialect(provider: Option<&str>, configured: Option<&str>) -> String {
    let provider = canonical_dialect(provider.unwrap_or("enter"));
    match configured.map(canonical_dialect).as_deref() {
        // A key-count override must not discard Cursor's native paste floor. The resolved
        // dialect is self-contained, so `locate` -> raw `send` preserves the same sequence.
        Some("double-enter") if provider == "cursor" => "cursor-double-enter".into(),
        Some("enter") | None => provider,
        Some(dialect) => dialect.to_string(),
    }
}

fn submit_sequence(dialect: &str) -> (Duration, usize) {
    // Keep the legacy tmux executor's ordering: completed paste -> native floor -> Enter.
    // Cursor's in-tree interaction profile and ac30977b's Claude profile specify 100ms,
    // one Enter, and retry-never. The approved direct-actuator map intentionally uses
    // Pi/Codex=0 and Grok=150ms; Grok's contract uses 150ms for *queue flush*, not its
    // 100ms paste floor. Do not copy receipt/queue keys or claim these maps are identical.
    match canonical_dialect(dialect).as_str() {
        "claude" | "cursor" => (Duration::from_millis(100), 1),
        "grok" => (Duration::from_millis(150), 1),
        "cursor-double-enter" => (Duration::from_millis(100), 2),
        "double-enter" => (Duration::ZERO, 2),
        _ => (Duration::ZERO, 1),
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
/// Provider names/aliases select their native paste floor and one Enter. Only an explicit
/// double-enter dialect adds a second key; there is no observation-driven retry or third Enter.
pub fn direct_send(socket: &str, pane_id: &str, dialect: &str, text: &str) -> Result<(), String> {
    direct_send_checked(socket, pane_id, dialect, text, None, false, || Ok(()), |_| Ok(()))
}

fn direct_send_checked(socket: &str, pane_id: &str, dialect: &str, text: &str, expected_pid: Option<u32>, managed: bool, submitted: impl FnOnce() -> Result<(), String>, mut verify_owner: impl FnMut(Instant) -> Result<(), String>) -> Result<(), String> {
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
    let deadline = Instant::now() + Duration::from_secs(60);
    let observed = pane_identity(socket, pane_id, deadline)?;
    let _input_lock = input_lock(&observed, pane_id, deadline)?;
    verify_identity(socket, pane_id, &observed, expected_pid, deadline)?;
    verify_owner(deadline)?;
    if managed && canonical_dialect(dialect) == "claude" {
        wait_for_draft(socket, pane_id, deadline)?;
    } else {
        guard_non_composer(socket, pane_id, deadline)?;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
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
    if let Err(error) = verify_identity(socket, pane_id, &observed, expected_pid, deadline)
        .and_then(|()| guard_non_composer(socket, pane_id, deadline))
        .and_then(|()| verify_owner(deadline)) {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
        return Err(error);
    }
    let pasted = tmux(socket)
        .args([
            "paste-buffer",
            "-p",
            "-r",
            "-d",
            "-b",
            &buffer,
            "-t",
            pane_id,
        ])
        .output()
        .map_err(|e| e.to_string())
        .and_then(check_output);
    if let Err(error) = pasted {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
        return Err(format!("uncertain: paste was issued: {error}; do not replay"));
    }
    let (floor, enter_count) = submit_sequence(dialect);
    if !floor.is_zero() {
        std::thread::sleep(floor);
    }
    verify_identity(socket, pane_id, &observed, expected_pid, deadline)
        .map_err(|error| format!("uncertain: identity changed after paste: {error}; do not replay"))?;
    guard_non_composer(socket, pane_id, deadline)
        .map_err(|error| format!("uncertain: surface changed after paste: {error}; no Enter issued; do not replay"))?;
    verify_owner(deadline)
        .map_err(|error| format!("uncertain: ownership changed after paste: {error}; no Enter issued; do not replay"))?;
    let mut command = tmux(socket);
    command.args(["send-keys", "-t", pane_id, "Enter"]);
    if enter_count == 2 {
        command.args([";", "send-keys", "-t", pane_id, "Enter"]);
    }
    command.output().map_err(|e| e.to_string()).and_then(check_output)
        .map_err(|error| format!("uncertain: submit was issued: {error}; do not replay"))?;
    // The submission receipt/task hint is published before releasing the input
    // lock, so concurrent assignments cannot reverse their attribution order.
    submitted().map_err(|error| format!("uncertain: submission receipt failed: {error}; do not replay"))
}

fn native_output(command: &Command, deadline: Instant) -> Result<crate::tmux_backend::CommandOutput, String> {
    let argv: Vec<String> = std::iter::once(command.get_program()).chain(command.get_args())
        .map(|s| s.to_string_lossy().into_owned()).collect();
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() { return Err("native_observation_timeout".into()); }
    crate::tmux_backend::RealCommandRunner.run_with_budget(&argv, budget).map_err(|e| e.to_string())
}

fn pane_identity(socket: &str, pane: &str, deadline: Instant) -> Result<String, String> {
    let output = native_output(tmux(socket).args(["display-message", "-p", "-t", pane,
        "#{pane_dead}|#{pane_pid}|#{pid}|#{socket_path}"]), deadline)?;
    if !output.success { return Err("pane_gone".into()); }
    let identity = output.stdout.trim().to_owned();
    let fields: Vec<_> = identity.splitn(4, '|').collect();
    if fields.len() != 4 || fields[0] != "0" || fields[1].parse::<u32>().ok().filter(|p| *p > 0).is_none() {
        return Err("pane_gone: no live physical identity".into());
    }
    Ok(identity)
}

fn verify_identity(socket: &str, pane: &str, observed: &str, expected_pid: Option<u32>, deadline: Instant) -> Result<(), String> {
    if pane_identity(socket, pane, deadline)? != observed { return Err("identity_mismatch: pane changed".into()); }
    if let Some(mut pid) = expected_pid {
        let pane_pid = observed.split('|').nth(1).and_then(|s| s.parse::<u32>().ok()).ok_or("invalid pane pid")?;
        // A wrapper shell may own the PTY while the registered provider is its
        // descendant. Never accept merely another live PID elsewhere on the host.
        for _ in 0..32 {
            if pid == pane_pid { return Ok(()); }
            let output = native_output(Command::new("ps").args(["-o", "ppid=", "-p", &pid.to_string()]), deadline)?;
            let Some(parent) = output.stdout.trim().parse::<u32>().ok().filter(|p| *p > 0 && *p != pid) else { break; };
            pid = parent;
        }
        return Err("identity_mismatch: registered process no longer owns this pane".into());
    }
    Ok(())
}

fn reject_non_composer(text: &str) -> Result<(), String> {
    use team_agent_contract::contract::delivery::InputSurface;
    let surface = team_agent_contract::providers::claude::interaction::surface(text);
    if matches!(surface, InputSurface::StartupRiskWarning | InputSurface::Starting | InputSurface::Fault) {
        // A trust decision is not text input. Never select, cancel, or confirm it
        // on the caller's behalf; the durable sender can return immediately.
        return Err(format!("non_composer: {surface:?}; input withheld; resolve the surface manually before a new send"));
    }
    Ok(())
}

fn guard_non_composer(socket: &str, pane: &str, deadline: Instant) -> Result<(), String> {
    let capture = native_output(tmux(socket).args(["capture-pane", "-p", "-t", pane, "-S", "0"]), deadline)?;
    if !capture.success { return Err("composer_surface_observation_failed: input withheld".into()); }
    // Inspect the current screen, not historical startup dialogs in scrollback.
    // This guard also covers raw addressing and dialect/key-count overrides.
    reject_non_composer(&capture.stdout)
}

fn wait_for_draft(socket: &str, pane: &str, deadline: Instant) -> Result<(), String> {
    let mut observed_draft = false;
    loop {
        if Instant::now() >= deadline {
            return Err("composer_contention_timeout: draft remained for 60s; message is safe in inbox; clear the draft before an explicit new send".into());
        }
        let capture = native_output(tmux(socket).args(["capture-pane", "-p", "-t", pane, "-S", "0"]), deadline);
        let draft = match capture {
            Ok(output) if output.success => {
                reject_non_composer(&output.stdout)?;
                team_agent_contract::providers::claude::interaction::inspect_legacy_composer(&output.stdout)
                    .is_some_and(|(_, text)| !text.is_empty())
            },
            _ if observed_draft => true, // a failed observation is not evidence that a known draft cleared
            _ => false, // Canon 1: no proven draft means direct delivery, including raw PTYs.
        };
        if !draft { return Ok(()); }
        observed_draft = true;
        std::thread::sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn input_lock(identity: &str, pane: &str, deadline: Instant) -> Result<std::fs::File, String> {
    #[cfg(unix)] let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))] let uid = 0;
    let root = std::env::temp_dir().join(format!("team-agent-input-{uid}"));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(&root).map_err(|e| e.to_string())?;
    let key = format!("{:x}.lock", Sha256::digest(format!("{identity}\0{pane}").as_bytes()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600).custom_flags(libc::O_NOFOLLOW); }
    let file = options.open(root.join(key)).map_err(|e| e.to_string())?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Err("input_contention_timeout: 60s; message is safe in inbox".into()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub fn send(endpoint: &Endpoint, text: &str) -> Result<Value, String> {
    direct_send(&endpoint.socket, &endpoint.pane_id, &endpoint.dialect, text)?;
    Ok(
        json!({"status": "sent", "recipient": endpoint.agent, "pane_id": endpoint.pane_id,
        "dialect": endpoint.dialect, "delivery": "physical", "accepted": null}),
    )
}

/// List proven live endpoints and explain unavailable worker addresses.
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
    let mut unavailable: Vec<Value> = Vec::new();
    for team in selectors {
        let directory = directory(workspace, team.as_deref())?;
        for agent in std::iter::once("leader".to_string()).chain(directory.agents()) {
            let live = (|| {
                let endpoint = directory.endpoint(&agent)?;
                directory.verify_endpoint(&agent, &endpoint, Instant::now() + Duration::from_secs(5))?;
                let output = tmux(&endpoint.socket)
                    .args(["display-message", "-p", "-t", &endpoint.pane_id, "#{pane_dead}"])
                    .output().map_err(|e| e.to_string())?;
                if !output.status.success() {
                    return Err(format!("tmux observation failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
                }
                if output.stdout != b"0\n" { return Err("pane_not_live".into()); }
                Ok(endpoint)
            })();
            match live {
                Ok(endpoint) => endpoints.push(json!({"workspace":directory.workspace,"team":directory.team,
                    "agent":endpoint.agent,"socket":endpoint.socket,"pane_id":endpoint.pane_id,"dialect":endpoint.dialect})),
                Err(error) if agent != "leader" => {
                    #[cfg(unix)]
                    let action = (error == "seat_requires_restart").then(||
                        crate::contract_runtime::backend::restart_command(&directory.workspace, directory.team.as_deref().unwrap_or(""), &agent));
                    #[cfg(not(unix))]
                    let action: Option<String> = None;
                    unavailable.push(json!({"workspace":directory.workspace,"team":directory.team,"agent":agent,
                        "candidate":(error == "seat_requires_restart").then_some("foreign"),"delivery_error":error,"action":action}));
                }
                Err(_) => {}
            }
        }
    }
    Ok(json!({"endpoints": endpoints, "unavailable":unavailable}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoritative_leader_receiver_and_endpoint_take_precedence() {
        let directory = Directory { workspace: PathBuf::from("/controlled-workspace"), team: Some("a".into()), state: json!({
            "leader": {"pane_id": "%1", "tmux_socket": "stale"},
            "leader_receiver": {"pane_id": "%2", "tmux_endpoint": "/tmp/owned-socket", "tmux_socket": "older"}
        }) };
        let endpoint = directory.endpoint("leader").unwrap();
        assert_eq!(endpoint.pane_id, "%2");
        assert_eq!(endpoint.socket, "/tmp/owned-socket");
    }

    #[test]
    fn missing_socket_uses_the_existing_workspace_namespace_not_the_default_server() {
        let mut directory = Directory { workspace: PathBuf::from("/controlled-workspace"), team: Some("a".into()), state: json!({
            "agents": {"worker": {"pane_id": "%3", "status": "running"}}
        }) };
        assert_eq!(directory.endpoint("worker").unwrap().socket, crate::tmux_backend::socket_name_for_workspace(&directory.workspace));
        directory.state["tmux_socket"] = json!("default");
        assert_eq!(directory.endpoint("worker").unwrap().socket, "default");
    }
}
