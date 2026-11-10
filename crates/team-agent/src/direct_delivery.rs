//! Read-only addressing and a synchronous tmux actuator. No coordinator, ledger or receipts.

use std::io::Write;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing { Framed, Bare }

pub(crate) fn validate_bare_dialect(dialect: &str) -> Result<(), String> {
    if matches!(canonical_dialect(dialect).as_str(), "claude" | "codex" | "pi") { Ok(()) }
    else { Err(format!("--bare does not support dialect {dialect}")) }
}

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

#[derive(Debug, Clone)]
pub(crate) struct AddressError {
    pub code: &'static str,
    pub message: String,
    pub candidates: Vec<Value>,
}
impl AddressError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), candidates: Vec::new() }
    }
}

pub fn directory(workspace: &Path, team: Option<&str>) -> Result<Directory, String> {
    directory_with_reason(workspace, team).map_err(|error| error.message)
}

pub(crate) fn directory_with_reason(workspace: &Path, team: Option<&str>) -> Result<Directory, AddressError> {
    select_directory(workspace, team, None)
}

pub(crate) fn directory_for_recipient(workspace: &Path, team: Option<&str>, recipient: &str) -> Result<Directory, AddressError> {
    select_directory(workspace, team, Some(recipient))
}

fn select_directory(workspace: &Path, team: Option<&str>, recipient: Option<&str>) -> Result<Directory, AddressError> {
    let workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| AddressError::new("internal_runtime_error", e.to_string()))?;
    let snapshot = snapshot(&workspace)
        .map_err(|e| AddressError::new("internal_runtime_error", e))?;
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
                return Err(AddressError::new("team_key_not_found", format!("unknown team: {requested}")));
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
    let matches_selector = |requested: &str, key: &str, value: &Value| {
        crate::state::projection::team_selector_matches(requested, key, value)
            || ["team_id", "team_name", "tmux_session", "team_dir"].iter()
                .any(|name| field(value, name) == Some(requested))
    };
    // active_team_key is a projection pointer, not caller authority. Inbox and
    // send share the same alive-team predicate as the read-only status gate.
    let alive = crate::state::projection::team_state_candidates(&snapshot);
    // Bare operator recipients are resolved by the current roster, not by a
    // stale launch/default team. Explicit scopes (including worker seats) are
    // never widened, and leader/broadcast still require unambiguous team scope.
    let matching: Vec<_> = alive.iter().filter(|(_, state)| {
        team.is_none() && recipient.is_some_and(|id| id != "leader" && id != "*"
            && state.get("agents").and_then(|agents| agents.get(id)).is_some())
    }).collect();
    if matching.len() > 1 {
        let mut error = AddressError::new("name_ambiguous", "recipient exists in multiple active teams; use <team>/<agent>");
        error.candidates = matching.iter().map(|(key, _)| json!({"team_key":key,"advisory":true})).collect();
        return Err(error);
    }
    let team = team.or_else(|| matching.first().map(|(key, _)| key.as_str()));
    if team.is_none() && alive.len() > 1 {
        let mut error = AddressError::new("name_ambiguous",
            "workspace has multiple alive teams; pass --team <key> to choose one");
        error.candidates = alive.keys().map(|key| json!({"team_key":key,"advisory":true})).collect();
        return Err(error);
    }
    // A stale default pointer cannot make the sole remaining team ambiguous.
    // Explicit selectors and genuinely multi-team workspaces still fail closed.
    let requested = team.or_else(|| field(&snapshot, "active_team_key")
        .filter(|active| teams.iter().any(|(key, value)| matches_selector(active, key, value))));
    // Canonical keys are authoritative, even when a sibling reuses one as an alias.
    let matches: Vec<_> = match requested.and_then(|key| teams.get_key_value(key)) {
        Some(exact) => vec![exact],
        None => teams.iter()
            .filter(|(key, value)| requested.map_or(teams.len() == 1, |requested| matches_selector(requested, key, value)))
            .collect(),
    };
    if matches.len() != 1 {
        let code = if matches.is_empty() && requested.is_some() { "team_key_not_found" } else { "name_ambiguous" };
        let mut error = AddressError::new(code, format!(
            "team selection is unknown or ambiguous: {}", requested.unwrap_or("(none)")
        ));
        error.candidates = teams.keys().filter(|key| matches.is_empty() || matches.iter().any(|(candidate, _)| *candidate == *key))
            .map(|key| json!({"team_key":key,"advisory":true})).collect();
        return Err(error);
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
fn legacy_startup_pending(row: &Value) -> bool {
    if field(row, "provider") == Some("fake") { return false; }
    let timestamp = |key| field(row, key).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let Some(spawned) = timestamp("spawned_at") else { return false; };
    timestamp("first_send_at").is_none_or(|sent| sent < spawned)
}

#[cfg(all(test, unix))]
#[test]
fn cold_readiness_is_incarnation_scoped() {
    let mut row = json!({"provider":"pi","spawned_at":"2026-01-02T00:00:00Z", "first_send_at":"2026-01-01T00:00:00Z"});
    assert!(legacy_startup_pending(&row));
    row["first_send_at"] = json!("2026-01-02T00:00:01Z");
    assert!(!legacy_startup_pending(&row));
    assert!(!legacy_startup_pending(&json!({"provider":"fake","spawned_at":"2026-01-02T00:00:00Z"})));
}

#[cfg(unix)]
fn native_recipient(info: &Value) -> bool {
    crate::contract_runtime::framework::is_contract(info)
        || field(info, "provider").is_some_and(crate::contract_runtime::registry::recognizes)
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
        let backend = match crate::contract_runtime::backend::Backend::open_readonly(&self.workspace, team) {
            Ok(backend) => backend,
            Err(error) => {
                if matches!(&error, crate::contract_runtime::backend::BackendError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
                    && self.verified_legacy_recipient(agent, team)
                { return Ok(None); }
                return Err(crate::contract_runtime::backend::observation_error(team, agent, &error));
            }
        };
        let id = SeatId::new(agent).map_err(|e| e.to_string())?;
        let seat = match backend.store.seat(&id).map_err(|e| e.to_string())? {
            Some(seat) => seat,
            None if self.verified_legacy_recipient(agent, team) => return Ok(None),
            None => return Err("native recipient missing".into()),
        };
        let target = seat.physical_target().map_err(|e| e.to_string())?;
        // Committed native seats can remain Starting; Ready is not a commit flag.
        if !matches!(seat.status, SeatStatus::Starting | SeatStatus::Ready)
            || backend.store.leased(&id).map_err(|e| e.to_string())?
            || backend.store.unfinished().map_err(|e| e.to_string())?.iter()
                .any(|operation| crate::contract_runtime::backend::involves_seat(operation, &id)) {
            return Err("recipient_not_running: native lifecycle not settled".into());
        }
        Ok(Some((backend.binding, seat)))
    }

    // Legacy provider names remain usable after recovery, but a bare snapshot
    // is never authority to route a native leaf to an arbitrary pane.
    #[cfg(unix)]
    fn verified_legacy_recipient(&self, agent: &str, team: &str) -> bool {
        let Ok(info) = self.recipient(agent) else { return false; };
        if crate::contract_runtime::framework::is_contract(info) || field(info, "status") != Some("running") {
            return false;
        }
        let session = crate::compiler::default_session_name(&self.workspace, team);
        if field(&self.state, "session_name") != Some(session.as_str()) { return false; }
        let Some(pid) = info.get("pane_pid").and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()).filter(|pid| *pid > 1) else { return false; };
        if !matches!(crate::platform::process::pid_liveness(pid), Ok(crate::platform::process::ProcessLiveness::Live)) {
            return false;
        }
        let Ok(endpoint) = self.snapshot_endpoint(agent, info) else { return false; };
        let transport = crate::transport_factory::bounded_endpoint_transport(
            &endpoint.socket, Instant::now() + Duration::from_secs(2),
        );
        let Ok(panes) = transport.list_targets() else { return false; };
        if !panes.iter().any(|pane| pane.pane_id.as_str() == endpoint.pane_id
            && pane.pane_pid == Some(pid) && pane.session.as_str() == session
            && pane.window_name.as_ref().is_some_and(|window| window.as_str() == field(info, "window").unwrap_or(agent)))
        { return false; }
        transport.session_owner(&crate::transport::SessionName::new(session)).ok().flatten()
            .is_some_and(|owner| owner.team == team
                && Path::new(&owner.workspace).canonicalize().is_ok_and(|owned|
                    self.workspace.canonicalize().is_ok_and(|workspace| owned == workspace)))
    }

    fn verify_endpoint(&self, agent: &str, expected: &Endpoint, deadline: Instant) -> Result<(), String> {
        let current = self.endpoint(agent)?;
        if current.socket != expected.socket || current.pane_id != expected.pane_id || current.dialect != expected.dialect {
            return Err("identity_mismatch: recipient address changed".into());
        }
        if agent == "leader" {
            // Re-read the binding and worker ownership after acquiring the input lock.
            // A missing legacy nonce is allowed; a recorded pane lifetime is not optional.
            let latest = directory(&self.workspace, self.team.as_deref())?;
            let address = latest.endpoint(agent)?;
            if !crate::topology::same_endpoint(&address.socket, &expected.socket)
                || address.pane_id != expected.pane_id || address.dialect != expected.dialect {
                return Err("leader_identity_mismatch: recipient address changed".into());
            }
            let transport = crate::transport_factory::bounded_endpoint_transport(&expected.socket, deadline);
            let observed = transport.list_targets().map_err(|error| error.to_string())?
                .into_iter().find(|pane| pane.pane_id.as_str() == expected.pane_id)
                .ok_or("leader_identity_unavailable: registered pane is not live")?;
            let receiver = latest.recipient(agent)?;
            if receiver.get("pane_pid").and_then(|pid| pid.as_u64().or_else(|| pid.as_str().and_then(|s| s.parse().ok())))
                .is_some_and(|pid| observed.pane_pid.map(u64::from) != Some(pid)) {
                return Err("leader_identity_pid_mismatch: registered pane instance changed".into());
            }
            let observed_nonce = observed.leader_env.get(crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY)
                .map(String::as_str).filter(|nonce| !nonce.is_empty());
            if field(receiver, "binding_nonce").zip(observed_nonce)
                .is_some_and(|(expected, observed)| expected != observed) {
                return Err("leader_identity_nonce_mismatch: registered pane binding changed".into());
            }
            if let Some(worker) = crate::topology::active_worker_on_endpoint(&latest.state, &expected.socket, &observed) {
                return Err(format!("leader_identity_worker_collision: registered pane belongs to active worker {worker}"));
            }
        }
        #[cfg(unix)]
        if let Some((binding, seat)) = self.native_seat(agent)? {
            crate::contract_runtime::backend::validate_delivery_target(&binding, &seat, deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| format!("identity_mismatch: {e}"))?;
        }
        Ok(())
    }

    pub(crate) fn recovery_action(&self, agent: &str, error: &str) -> Option<String> {
        if agent == "leader" && error.starts_with("leader_identity_") {
            return Some(format!("team-agent claim-leader --workspace {}{}",
                crate::cli::shell_quote(&self.workspace.to_string_lossy()),
                self.team.as_deref().map(|team| format!(" --team {}", crate::cli::shell_quote(team))).unwrap_or_default()));
        }
        #[cfg(unix)]
        if error.starts_with("native_authority_missing:") {
            return self.team.as_deref().map(|team|
                crate::contract_runtime::backend::authority_recovery_action(&self.workspace, team, agent));
        }
        #[cfg(not(unix))]
        let _ = (agent, error);
        None
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
        self.snapshot_endpoint(agent, info)
    }

    fn snapshot_endpoint(&self, agent: &str, info: &Value) -> Result<Endpoint, String> {
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
    /// Execute normal addressing/identity/composer guards without committing or submitting input.
    pub(crate) fn probe(&self, agent: &str, deadline: Instant) -> Result<Endpoint, String> {
        let endpoint = self.endpoint(agent)?;
        self.verify_endpoint(agent, &endpoint, deadline)?;
        if agent != "leader" {
            let observed = pane_identity(&endpoint.socket, &endpoint.pane_id, deadline)?;
            let _input = input_lock(&observed, &endpoint.pane_id, deadline)?;
            let pid = self.recipient(agent)?.get("pane_pid").and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok());
            verify_identity(&endpoint.socket, &endpoint.pane_id, &observed, pid, deadline)?;
            let mode = native_output(tmux(&endpoint.socket).args(["display-message", "-p", "-t", &endpoint.pane_id, "#{pane_in_mode}"]), deadline)?;
            if !mode.success || mode.stdout.trim() != "0" {
                return Err("input_not_ready: pane is in copy/edit mode".into());
            }
            guard_non_composer(&endpoint.socket, &endpoint.pane_id, deadline)?;
            wait_for_draft(&endpoint.socket, &endpoint.pane_id, deadline)?;
            self.verify_endpoint(agent, &endpoint, deadline)?;
        }
        Ok(endpoint)
    }

    #[cfg(unix)]
    fn wait_for_startup(&self, agent: &str, endpoint: &Endpoint, deadline: Instant) -> Result<(), String> {
        if agent == "leader" { return Ok(()); }
        let row = self.recipient(agent)?;
        let native = self.native_seat(agent)?;
        if native.is_none() && !legacy_startup_pending(row) { return Ok(()); }
        let team = self.team.as_deref().ok_or("input_not_ready: startup scope missing; no input issued")?;
        let backend = native.as_ref().map(|_| crate::contract_runtime::backend::Backend::open_readonly(&self.workspace, team))
            .transpose().map_err(|e| e.to_string())?;
        let observed = pane_identity(&endpoint.socket, &endpoint.pane_id, deadline)?;
        loop {
            if Instant::now() >= deadline {
                return Err("input_not_ready: startup readiness deadline; no input issued; inspect status/doctor before retrying".into());
            }
            verify_identity(&endpoint.socket, &endpoint.pane_id, &observed, None, deadline)?;
            self.verify_endpoint(agent, endpoint, deadline)?;
            let initialized = if let (Some(backend), Some((_, seat))) = (&backend, &native) {
                backend.store.startup_handshake_observed(&seat.identity).map_err(|e| e.to_string())?
            } else {
                crate::mcp_server::readiness::startup_observed(&self.workspace, team, agent, row).unwrap_or(false)
            };
            // Initialization/tools-list flush is startup evidence, not business
            // acceptance. Existing composer/ownership guards still precede input.
            if initialized { return Ok(()); }
            std::thread::sleep(Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    pub fn send(&self, agent: &str, text: &str) -> Result<Value, String> {
        self.send_with_receipt(agent, text, || Ok(()))
    }

    pub(crate) fn send_with_receipt(&self, agent: &str, text: &str, submitted: impl FnOnce() -> Result<(), String>) -> Result<Value, String> {
        self.send_with_framing(agent, text, Framing::Framed, submitted)
    }

    pub(crate) fn send_with_framing(&self, agent: &str, text: &str, framing: Framing, submitted: impl FnOnce() -> Result<(), String>) -> Result<Value, String> {
        #[cfg(unix)]
        let _lifecycle = if agent != "leader" && native_recipient(self.recipient(agent)?) {
            let id = crate::model::ids::AgentId::new(agent);
            Some(crate::lifecycle::lock::acquire_seat_delivery_lock(crate::lifecycle::lock::LifecycleLockRequest {
                workspace: &self.workspace, operation: "native-send", team: self.team.as_deref(), agent_id: Some(&id),
            }).map_err(|e| format!("recipient_lifecycle_busy: {e}"))?)
        } else { None };
        let endpoint = self.endpoint(agent)?;
        let info = self.recipient(agent)?;
        let expected_pid = if agent == "leader" { info.get("pane_pid") }
            else { info.get("pane_pid").or_else(|| info.get("pid")).or_else(|| info.get("native_pid")) };
        let expected_pid = expected_pid.and_then(|value| value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok())))
            .and_then(|pid| u32::try_from(pid).ok()).filter(|pid| *pid > 0);
        #[cfg(unix)]
        let mut startup_checked = false;
        direct_send_checked(&endpoint.socket, &endpoint.pane_id, &endpoint.dialect, text, expected_pid, true, framing, submitted,
            |deadline| {
                if agent == "leader" && leader_target_is_sender(&endpoint,
                    crate::tmux_backend::socket_name_from_tmux_env().as_deref(),
                    std::env::var("TMUX_PANE").ok().as_deref()) {
                    return Err("leader_identity_self_collision: target is the sender's own pane".into());
                }
                self.verify_endpoint(agent, &endpoint, deadline)?;
                #[cfg(unix)]
                if framing == Framing::Framed && !startup_checked {
                    self.wait_for_startup(agent, &endpoint, deadline)?;
                    startup_checked = true;
                }
                Ok(())
            }).map_err(|error| {
                if agent == "leader" && ["identity_mismatch", "pane_gone", "recipient_provider_exited"]
                    .iter().any(|prefix| error.starts_with(prefix)) {
                    format!("leader_identity_mismatch: {error}")
                } else { error }
            })?;
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

pub(crate) fn leader_target_is_sender(
    target: &Endpoint,
    sender_socket: Option<&str>,
    sender_pane: Option<&str>,
) -> bool {
    sender_pane == Some(target.pane_id.as_str())
        && sender_socket.is_some_and(|socket| crate::topology::same_endpoint(socket, &target.socket))
}

fn canonical_dialect(name: &str) -> String {
    let normalized = name.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "claude" | "claude_code" | "claudecode" | "claude_tui" => "claude".into(),
        "pi" | "pi_tui" => "pi".into(),
        "cursor" | "cursor_agent" | "agent" => "cursor".into(),
        "codex" | "codex_cli" | "codex_tui" => "codex".into(),
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
    direct_send_checked(socket, pane_id, dialect, text, None, false, Framing::Framed, || Ok(()), |_| Ok(()))
}

fn direct_send_checked(socket: &str, pane_id: &str, dialect: &str, text: &str, expected_pid: Option<u32>, managed: bool, framing: Framing, submitted: impl FnOnce() -> Result<(), String>, mut verify_owner: impl FnMut(Instant) -> Result<(), String>) -> Result<(), String> {
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
    if framing == Framing::Bare {
        validate_bare_dialect(dialect)?;
        // Release tmux's overlay, not the provider's draft or native input.
        cancel_copy_mode(socket, pane_id, deadline)?;
        guard_bare_composer(socket, pane_id, dialect, None, deadline)?;
    } else if managed && canonical_dialect(dialect) == "claude" {
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
    // Terminate Codex's token within the paste so its mentions popup cannot
    // consume Enter or replace a trailing @file/$skill. Other dialects stay verbatim.
    let text = if framing == Framing::Framed && canonical_dialect(dialect) == "codex" {
        format!("{text}\n")
    } else {
        text.to_owned()
    };
    let write_result = child.stdin.take().unwrap().write_all(text.as_bytes());
    let loaded = child.wait_with_output().map_err(|e| e.to_string())?;
    if let Err(error) = write_result {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
        return Err(error.to_string());
    }
    check_output(loaded)?;
    if let Err(error) = verify_identity(socket, pane_id, &observed, expected_pid, deadline)
        .and_then(|()| if framing == Framing::Bare { guard_bare_composer(socket, pane_id, dialect, None, deadline) }
            else { guard_non_composer(socket, pane_id, deadline) })
        .and_then(|()| verify_owner(deadline))
        .and_then(|()| if framing == Framing::Bare { Ok(()) } else { cancel_copy_mode(socket, pane_id, deadline) }) {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
        return Err(error);
    }
    let mut paste = tmux(socket);
    paste.arg("paste-buffer");
    if framing == Framing::Framed || canonical_dialect(dialect) == "pi" { paste.arg("-p"); }
    let pasted = paste.args(["-r", "-d", "-b", &buffer, "-t", pane_id])
        .output()
        .map_err(|e| e.to_string())
        .and_then(check_output);
    if let Err(error) = pasted {
        let _ = tmux(socket).args(["delete-buffer", "-b", &buffer]).output();
        return Err(format!("uncertain: paste was issued: {error}; do not replay"));
    }
    let (floor, enter_count) = if framing == Framing::Bare { (Duration::from_millis(100), 1) } else { submit_sequence(dialect) };
    if !floor.is_zero() {
        std::thread::sleep(floor);
    }
    verify_identity(socket, pane_id, &observed, expected_pid, deadline)
        .map_err(|error| format!("uncertain: identity changed after paste: {error}; do not replay"))?;
    (if framing == Framing::Bare { guard_bare_composer(socket, pane_id, dialect, Some(&text), deadline) }
        else { guard_non_composer(socket, pane_id, deadline) })
        .map_err(|error| format!("uncertain: surface changed after paste: {error}; no Enter issued; do not replay"))?;
    verify_owner(deadline)
        .map_err(|error| format!("uncertain: ownership changed after paste: {error}; no Enter issued; do not replay"))?;
    if framing == Framing::Framed {
        cancel_copy_mode(socket, pane_id, deadline)
            .map_err(|error| format!("uncertain: mode preparation failed after paste: {error}; no Enter issued; do not replay"))?;
    }
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

fn guard_bare_composer(socket: &str, pane: &str, dialect: &str, expected: Option<&str>, deadline: Instant) -> Result<(), String> {
    use team_agent_contract::contract::delivery::InputSurface;
    use team_agent_contract::providers::{claude, codex, pi};
    let mode = native_output(tmux(socket).args(["display-message", "-p", "-t", pane, "#{pane_in_mode}"]), deadline)?;
    if !mode.success || mode.stdout.trim() != "0" { return Err("bare_composer_refused: pane is in copy mode or unknown".into()); }
    let frame = native_output(tmux(socket).args(["capture-pane", "-p", "-t", pane]), deadline)?;
    if !frame.success { return Err(format!("bare_composer_refused: capture failed: {}", frame.stderr)); }
    let (surface, input) = match canonical_dialect(dialect).as_str() {
        "claude" => (claude::interaction::surface(&frame.stdout),
            claude::interaction::inspect_legacy_composer(&frame.stdout).and_then(|(surface, input)|
                matches!(surface, InputSurface::ComposerReady | InputSurface::ComposerContainsPaste).then_some(input))),
        "codex" => (codex::interaction::surface(&frame.stdout), codex::interaction::composer_text(&frame.stdout)),
        "pi" => (pi::interaction::surface(&frame.stdout), pi::interaction::composer_text(&frame.stdout)),
        _ => return Err("bare_composer_refused: unsupported dialect".into()),
    };
    let normalize = |text: &str| text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let ready = match expected {
        None => surface == InputSurface::ComposerReady && input.as_deref().is_some_and(|input| input.trim().is_empty()),
        Some(expected) => matches!(surface, InputSurface::ComposerReady | InputSurface::ComposerContainsPaste)
            && input.as_deref().is_some_and(|input| normalize(input) == normalize(expected)),
    };
    if ready { Ok(()) } else { Err(format!("bare_composer_refused: {surface:?}; input is not the expected live composer")) }
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
        &format!("#{{pane_dead}}|#{{pane_pid}}|#{{pid}}|#{{socket_path}}|#{{{}}}", crate::tmux_backend::PROVIDER_EXIT_STATUS_OPTION)]), deadline)?;
    if !output.success { return Err("pane_gone".into()); }
    checked_pane_identity(output.stdout.trim().to_owned())
}

fn checked_pane_identity(identity: String) -> Result<String, String> {
    let fields: Vec<_> = identity.split('|').collect();
    if fields.get(4).is_some_and(|exit| exit.trim().parse::<i32>().is_ok()) {
        return Err("recipient_provider_exited".into());
    }
    if fields.len() < 4 || fields[0] != "0" || fields[1].parse::<u32>().ok().filter(|p| *p > 0).is_none() {
        return Err("pane_gone: no live physical identity".into());
    }
    Ok(identity)
}

fn cancel_copy_mode(socket: &str, pane: &str, deadline: Instant) -> Result<(), String> {
    let mode = native_output(tmux(socket).args(["display-message", "-p", "-t", pane, "#{pane_in_mode}"]), deadline)?;
    if !mode.success { return Err("pane_mode_observation_failed: input withheld".into()); }
    match mode.stdout.trim() {
        "0" => return Ok(()),
        "1" => (),
        _ => return Err("pane_mode_observation_failed: invalid mode".into()),
    }
    // This is a tmux mode command, never Escape/C-c to the provider. Cancel
    // before paste to retain bracketed framing, and again before Enter.
    let cancelled = native_output(tmux(socket).args(["send-keys", "-t", pane, "-X", "cancel"]), deadline)?;
    if !cancelled.success { return Err(format!("copy_mode_cancel_failed: {}", cancelled.stderr.trim())); }
    Ok(())
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
            return Err("composer_contention_timeout: draft remained until the input deadline; preserve or clear it before a new send".into());
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
                Err(error) if agent != "leader" || error.starts_with("leader_identity_") => {
                    let action = directory.recovery_action(&agent, &error);
                    unavailable.push(json!({"workspace":directory.workspace,"team":directory.team,"agent":agent,
                        "candidate":null,"delivery_error":error,"action":action}));
                }
                Err(_) => {}
            }
        }
    }
    Ok(json!({"endpoints": endpoints, "unavailable":unavailable}))
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_exit_receipt_overrides_live_wrapper_pid() {
        for exit in ["0", "1", "137"] {
            assert_eq!(super::checked_pane_identity(format!("0|123|456|/tmp/test.sock|{exit}")).unwrap_err(), "recipient_provider_exited");
        }
        assert!(super::checked_pane_identity("0|123|456|/tmp/test.sock|running".into()).is_ok());
        assert!(super::checked_pane_identity("0|123|456|/tmp/test.sock|".into()).is_ok());
        assert!(super::checked_pane_identity("1|123|456|/tmp/test.sock|".into()).is_err());
    }

    use super::*;

    #[test]
    #[serial_test::serial(env)]
    fn legacy_leader_delivery_needs_live_non_worker_not_nonce() {
        let root = std::env::temp_dir().join(format!("ta-local-leader-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
        let endpoint = root.join("leader.sock").to_string_lossy().into_owned();
        let pane = crate::transport::PaneInfo {
            pane_id: crate::transport::PaneId::new("%0"), session: crate::transport::SessionName::new("leader"),
            window_index: None, window_name: None, pane_index: None, tty: None,
            current_command: Some("bash".into()), current_path: Some(root.clone()),
            active: false, pane_pid: Some(123), leader_env: Default::default(),
        };
        let mut state = json!({"team_key":"a", "tmux_socket":endpoint, "agents":{},
            "leader_receiver":{"pane_id":"%0","provider":"pi"}});
        let mut outcomes = Vec::new();
        for case in 0..5 {
            match case {
                1 => state["leader_receiver"]["binding_nonce"] = json!("obsolete"),
                2 => state["agents"]["worker"] = json!({"pane_id":"%0","pane_pid":123,"status":"running"}),
                3 => { state["agents"]["worker"].as_object_mut().unwrap().remove("pane_pid"); },
                4 => state["agents"]["worker"]["tmux_socket"] = json!("/tmp/another-leader-server"),
                _ => {}
            }
            std::fs::write(root.join(".team/runtime/state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
            let transport = crate::transport::test_support::OfflineTransport::default()
                .with_tmux_endpoint(&endpoint).with_targets(vec![pane.clone()]);
            let result = crate::transport_factory::with_leader_endpoint_transport(&endpoint, transport, || {
                let directory = directory(&root, Some("a")).unwrap();
                directory.verify_endpoint("leader", &directory.endpoint("leader").unwrap(), Instant::now()+Duration::from_secs(1))
            });
            outcomes.push(result);
        }
        std::fs::remove_dir_all(&root).unwrap();
        assert!(outcomes[0].is_ok() && outcomes[1].is_ok());
        assert!(outcomes[2].as_ref().unwrap_err().contains("worker_collision"));
        assert!(outcomes[3].as_ref().unwrap_err().contains("worker_collision"));
        assert!(outcomes[4].is_ok());
    }

    #[test]
    #[serial_test::serial(env)]
    fn leader_delivery_fences_recorded_pid_and_conflicting_nonce() {
        let root = std::env::temp_dir().join(format!("ta-leader-fence-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
        let endpoint = root.join("leader.sock").to_string_lossy().into_owned();
        let state = json!({"team_key":"a", "tmux_socket":endpoint, "agents":{},
            "leader_receiver":{"pane_id":"%0","provider":"pi","pane_pid":123,"binding_nonce":"bound"}});
        std::fs::write(root.join(".team/runtime/state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        for (pid, nonce, expected_error) in [
            (Some(123), None, None),
            (Some(123), Some(""), None),
            (Some(123), Some("bound"), None),
            (Some(999), None, Some("leader_identity_pid_mismatch")),
            (None, None, Some("leader_identity_pid_mismatch")),
            (Some(123), Some("foreign"), Some("leader_identity_nonce_mismatch")),
        ] {
            let mut pane = crate::transport::PaneInfo {
                pane_id: crate::transport::PaneId::new("%0"), session: crate::transport::SessionName::new("leader"),
                window_index: None, window_name: None, pane_index: None, tty: None,
                current_command: Some("bash".into()), current_path: Some(root.clone()),
                active: false, pane_pid: pid, leader_env: Default::default(),
            };
            if let Some(nonce) = nonce {
                pane.leader_env.insert(crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY.into(), nonce.into());
            }
            let transport = crate::transport::test_support::OfflineTransport::default()
                .with_tmux_endpoint(&endpoint).with_targets(vec![pane]);
            crate::transport_factory::with_leader_endpoint_transport(&endpoint, transport, || {
                let directory = directory(&root, Some("a")).unwrap();
                let result = directory.verify_endpoint("leader", &directory.endpoint("leader").unwrap(), Instant::now()+Duration::from_secs(1));
                if let Some(expected) = expected_error {
                    let error = result.unwrap_err();
                    assert!(error.starts_with(expected), "{error}");
                    assert!(directory.recovery_action("leader", &error).unwrap().contains("claim-leader"));
                } else { assert!(result.is_ok(), "{result:?}"); }
            });
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_team_key_wins_but_default_delivery_refuses_multiple_alive_teams() {
        let root = std::env::temp_dir().join(format!("ta-team-key-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
        let state = json!({"active_team_key":"regressions-20261101", "teams": {
            "regressions-20261101": {"session_name":"actual-session", "agents":{}},
            "current": {"session_name":"team-regressions-20261101", "team_name":"regressions-20261101", "agents":{"reviewer":{"status":"running"}}}
        }});
        std::fs::write(root.join(".team/runtime/state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        for requested in [Some("regressions-20261101"), Some("actual-session")] {
            let selected = directory_with_reason(&root, requested).unwrap();
            assert_eq!(selected.team.as_deref(), Some("regressions-20261101"));
            assert_eq!(selected.state["session_name"], "actual-session");
        }
        let refused = directory_with_reason(&root, None).err().expect("unscoped multi-team refusal");
        assert_eq!(refused.code, "name_ambiguous");
        assert_eq!(refused.candidates.len(), 2);
        assert!(crate::inbox_store::read(&root, "leader", 10, None).is_err());
        let command = ["send", "leader", "must-not-route", "--mailbox"].map(str::to_string);
        assert_ne!(crate::cli::run(&command, &root), crate::cli::ExitCode::Ok);
        assert!(!root.join(".team/inbox").exists(), "ambiguous send must have no storage effect");
        assert_eq!(directory_for_recipient(&root, None, "reviewer").unwrap().team.as_deref(), Some("current"));
        assert_eq!(directory_for_recipient(&root, Some("regressions-20261101"), "reviewer").unwrap().team.as_deref(), Some("regressions-20261101"));
        let command = ["send", "reviewer", "unique-recipient", "--mailbox"].map(str::to_string);
        assert_eq!(crate::cli::run(&command, &root), crate::cli::ExitCode::Ok);
        assert_eq!(crate::inbox_store::read(&root, "reviewer", 10, Some("current")).unwrap()["messages"][0]["content"], "unique-recipient");
        let command = ["send", "leader", "explicit-team", "--mailbox", "--team", "current"].map(str::to_string);
        assert_eq!(crate::cli::run(&command, &root), crate::cli::ExitCode::Ok);
        assert_eq!(crate::inbox_store::read(&root, "leader", 10, Some("current")).unwrap()["messages"][0]["content"], "explicit-team");
        assert_eq!(directory_with_reason(&root, Some("current")).unwrap().team.as_deref(), Some("current"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn leader_self_exclusion_is_socket_qualified() {
        let target = Endpoint { agent: "leader".into(), socket: "/tmp/leader-a.sock".into(), pane_id: "%0".into(), dialect: "pi".into() };
        assert!(leader_target_is_sender(&target, Some("/tmp/leader-a.sock"), Some("%0")));
        assert!(!leader_target_is_sender(&target, Some("/tmp/leader-b.sock"), Some("%0")));
        assert!(!leader_target_is_sender(&target, Some("/tmp/leader-a.sock"), Some("%1")));
        assert!(!leader_target_is_sender(&target, None, Some("%0")));
    }

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
