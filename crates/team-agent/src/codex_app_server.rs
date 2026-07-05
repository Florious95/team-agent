//! Minimal Codex app-server Unix-socket WebSocket client for leader delivery.
//!
//! 0.5.x Windows portability Batch 1: this client is fundamentally
//! Unix-only (`UnixStream` WebSocket handshake, uid/mode socket
//! ownership check). On Windows we still expose the same public API
//! surface so `messaging/delivery.rs`, `cli/send.rs`, `cli/named_address.rs`,
//! `leader/lease.rs` compile identically. Windows implementations of
//! `attach_probe` + `submit_to_bound_thread` return
//! `AppServerError::SocketUnreachable("codex_app_server unix socket not supported on this platform (Windows)")`
//! (N38 three-line typed unsupported: code+reason+action are baked into
//! `AppServerError::code()` and the `Display` impl). Truth source:
//! `.team/artifacts/0.5.x-windows-portability-survey-design.md` §Batch 1.
//!
//! Portable pieces below (types, JSON helpers) stay unconditional so
//! `receiver_is_app_server` / `binding_from_receiver` work identically
//! everywhere.

use serde_json::{json, Value};
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;

pub const APP_SERVER_PROTOCOL_FIXTURE_CLI_MIN: &str = "0.139.0";
pub const APP_SERVER_PROTOCOL_FIXTURE_CLI_MAX: &str = "0.139.x";
pub const APP_SERVER_PROTOCOL_REQUIRES_USER_AGENT: bool = true;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerBinding {
    pub socket: String,
    pub thread_id: String,
    pub session_id: String,
    pub cwd: String,
    pub cli_version: String,
    pub bound_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerSubmit {
    pub turn_id: String,
    pub turn_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppServerError {
    SocketUnreachable(String),
    SocketOwnershipInvalid(String),
    ThreadNotLive(String),
    ThreadStale { expected: Value, actual: Value },
    LeaderBusy(String),
    ApprovalUnsupported(String),
    ProtocolMismatch(String),
    MissingUserAgent,
    Io(String),
    Json(String),
}

impl AppServerError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SocketUnreachable(_) => "socket_unreachable",
            Self::SocketOwnershipInvalid(_) => "socket_ownership_invalid",
            Self::ThreadNotLive(_) => "thread_not_live",
            Self::ThreadStale { .. } => "app_server_thread_stale",
            Self::LeaderBusy(_) => "leader_busy",
            Self::ApprovalUnsupported(_) => "approval_unsupported",
            Self::ProtocolMismatch(_) => "protocol_mismatch",
            Self::MissingUserAgent => "protocol_mismatch_missing_user_agent",
            Self::Io(_) => "io_error",
            Self::Json(_) => "json_error",
        }
    }
}

impl std::fmt::Display for AppServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SocketUnreachable(msg)
            | Self::SocketOwnershipInvalid(msg)
            | Self::ThreadNotLive(msg)
            | Self::LeaderBusy(msg)
            | Self::ApprovalUnsupported(msg)
            | Self::ProtocolMismatch(msg)
            | Self::Io(msg)
            | Self::Json(msg) => write!(f, "{}:{msg}", self.code()),
            Self::ThreadStale { expected, actual } => {
                write!(f, "{}:expected={expected},actual={actual}", self.code())
            }
            Self::MissingUserAgent => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for AppServerError {}

#[cfg(unix)]
pub fn attach_probe(endpoint: &str, thread_id: &str) -> Result<AppServerBinding, AppServerError> {
    check_socket_ownership(endpoint)?;
    let socket = socket_path(endpoint)?;
    let mut client = AppServerClient::connect(&socket)?;
    let user_agent = client.initialize()?;
    let resume = client.resume(thread_id)?;
    Ok(AppServerBinding {
        socket: endpoint.to_string(),
        thread_id: resume.thread_id,
        session_id: resume.session_id,
        cwd: resume.cwd,
        cli_version: user_agent,
        bound_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Batch 1 Windows N38 typed-unsupported stub. Callers see the same
/// `AppServerError` shape as Unix — no silent success, no panic.
#[cfg(not(unix))]
pub fn attach_probe(
    _endpoint: &str,
    _thread_id: &str,
) -> Result<AppServerBinding, AppServerError> {
    Err(AppServerError::SocketUnreachable(
        "codex_app_server unix-socket client not supported on this platform \
         (Windows); use codex CLI stdio or ConPTY worker delivery instead"
            .to_string(),
    ))
}

#[cfg(unix)]
pub fn submit_to_bound_thread(
    binding: &AppServerBinding,
    message_id: &str,
    rendered: &str,
) -> Result<AppServerSubmit, AppServerError> {
    let socket = socket_path(&binding.socket)?;
    let mut client = AppServerClient::connect(&socket)?;
    let user_agent = client.initialize()?;
    let resume = client.resume(&binding.thread_id)?;
    validate_tuple(binding, &user_agent, &resume)?;
    client.turn_start(&binding.thread_id, message_id, rendered)
}

#[cfg(not(unix))]
pub fn submit_to_bound_thread(
    _binding: &AppServerBinding,
    _message_id: &str,
    _rendered: &str,
) -> Result<AppServerSubmit, AppServerError> {
    Err(AppServerError::SocketUnreachable(
        "codex_app_server unix-socket submit not supported on this platform \
         (Windows); use codex CLI stdio or ConPTY worker delivery instead"
            .to_string(),
    ))
}

pub fn binding_from_receiver(receiver: &Value) -> Result<AppServerBinding, AppServerError> {
    let app = receiver
        .get("app_server")
        .ok_or_else(|| AppServerError::ProtocolMismatch("missing app_server".to_string()))?;
    Ok(AppServerBinding {
        socket: required_str(app, "socket")?.to_string(),
        thread_id: required_str(app, "thread_id")?.to_string(),
        session_id: required_str(app, "session_id")?.to_string(),
        cwd: required_str(app, "cwd")?.to_string(),
        cli_version: required_str(app, "cli_version")?.to_string(),
        bound_at: required_str(app, "bound_at")?.to_string(),
    })
}

pub fn receiver_is_app_server(receiver: &Value) -> bool {
    receiver.get("transport_kind").and_then(Value::as_str) == Some("codex_app_server")
        || receiver.get("mode").and_then(Value::as_str) == Some("codex_app_server")
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, AppServerError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppServerError::ProtocolMismatch(format!("missing {field}")))
}

#[cfg(unix)]
fn validate_tuple(
    binding: &AppServerBinding,
    user_agent: &str,
    resume: &ThreadResume,
) -> Result<(), AppServerError> {
    if user_agent != binding.cli_version
        || resume.thread_id != binding.thread_id
        || resume.session_id != binding.session_id
        || resume.cwd != binding.cwd
    {
        return Err(AppServerError::ThreadStale {
            expected: json!({
                "cli_version": binding.cli_version,
                "thread_id": binding.thread_id,
                "session_id": binding.session_id,
                "cwd": binding.cwd,
            }),
            actual: json!({
                "cli_version": user_agent,
                "thread_id": resume.thread_id,
                "session_id": resume.session_id,
                "cwd": resume.cwd,
            }),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn check_socket_ownership(endpoint: &str) -> Result<(), AppServerError> {
    let path = socket_path(endpoint)?;
    let meta = std::fs::metadata(&path)
        .map_err(|e| AppServerError::SocketUnreachable(format!("{}:{e}", path.display())))?;
    if !meta.file_type().is_socket() {
        return Err(AppServerError::SocketOwnershipInvalid(format!(
            "not a unix socket: {}",
            path.display()
        )));
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(AppServerError::SocketOwnershipInvalid(format!(
            "socket owner uid {} != current uid {}",
            meta.uid(),
            unsafe { libc::geteuid() }
        )));
    }
    if meta.permissions().mode() & 0o002 != 0 {
        return Err(AppServerError::SocketOwnershipInvalid(format!(
            "socket is world-writable: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn socket_path(endpoint: &str) -> Result<PathBuf, AppServerError> {
    let raw = endpoint.strip_prefix("unix://").unwrap_or(endpoint);
    if raw.is_empty() {
        return Err(AppServerError::SocketUnreachable(
            "missing unix socket path".to_string(),
        ));
    }
    Ok(PathBuf::from(raw))
}

#[cfg(unix)]
struct ThreadResume {
    thread_id: String,
    session_id: String,
    cwd: String,
}

#[cfg(unix)]
struct AppServerClient {
    stream: UnixStream,
    next_id: u64,
}

#[cfg(unix)]
impl AppServerClient {
    fn connect(path: &Path) -> Result<Self, AppServerError> {
        let mut stream = UnixStream::connect(path)
            .map_err(|e| AppServerError::SocketUnreachable(format!("{}:{e}", path.display())))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| AppServerError::Io(e.to_string()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| AppServerError::Io(e.to_string()))?;
        let request = concat!(
            "GET / HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Upgrade: websocket\r\n",
            "Connection: Upgrade\r\n",
            "Sec-WebSocket-Key: dGVhbS1hZ2VudC1jb2RleA==\r\n",
            "Sec-WebSocket-Version: 13\r\n",
            "\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| AppServerError::Io(e.to_string()))?;
        let response = read_http_response(&mut stream)?;
        if !response.starts_with("HTTP/1.1 101") && !response.starts_with("HTTP/1.0 101") {
            return Err(AppServerError::ProtocolMismatch(format!(
                "websocket upgrade failed: {}",
                response.lines().next().unwrap_or("")
            )));
        }
        Ok(Self { stream, next_id: 1 })
    }

    fn initialize(&mut self) -> Result<String, AppServerError> {
        let result = self.request(json!({
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "codex-appserver-team-agent",
                    "title": "Team Agent",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true,
                    "requestAttestation": false
                }
            }
        }))?;
        let _ = self.notify(json!({"method": "initialized"}));
        let user_agent = result
            .get("userAgent")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(AppServerError::MissingUserAgent)?;
        if !user_agent.to_ascii_lowercase().contains("codex") {
            return Err(AppServerError::ProtocolMismatch(format!(
                "unexpected userAgent: {user_agent}"
            )));
        }
        Ok(user_agent.to_string())
    }

    fn resume(&mut self, thread_id: &str) -> Result<ThreadResume, AppServerError> {
        let result = self.request(json!({
            "method": "thread/resume",
            "params": {"threadId": thread_id}
        }))?;
        let thread = result
            .get("thread")
            .ok_or_else(|| AppServerError::ProtocolMismatch("missing thread".to_string()))?;
        let actual_thread_id = required_str(thread, "id")?.to_string();
        let session_id = required_str(thread, "sessionId")?.to_string();
        let cwd = result
            .get("cwd")
            .and_then(Value::as_str)
            .or_else(|| thread.get("cwd").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppServerError::ProtocolMismatch("missing cwd".to_string()))?
            .to_string();
        Ok(ThreadResume {
            thread_id: actual_thread_id,
            session_id,
            cwd,
        })
    }

    fn turn_start(
        &mut self,
        thread_id: &str,
        message_id: &str,
        rendered: &str,
    ) -> Result<AppServerSubmit, AppServerError> {
        let result = self.request(json!({
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "clientUserMessageId": message_id,
                "input": [{
                    "type": "text",
                    "text": rendered,
                    "text_elements": []
                }]
            }
        }))?;
        let turn = result
            .get("turn")
            .ok_or_else(|| AppServerError::ProtocolMismatch("missing turn".to_string()))?;
        let turn_id = required_str(turn, "id")?.to_string();
        let status = required_str(turn, "status")?.to_string();
        if status != "inProgress" {
            return Err(AppServerError::ProtocolMismatch(format!(
                "turn/start returned status {status}"
            )));
        }
        Ok(AppServerSubmit {
            turn_id,
            turn_status: status,
        })
    }

    fn notify(&mut self, mut payload: Value) -> Result<(), AppServerError> {
        if let Some(obj) = payload.as_object_mut() {
            obj.remove("id");
        }
        write_ws_text(&mut self.stream, &payload.to_string())
    }

    fn request(&mut self, mut payload: Value) -> Result<Value, AppServerError> {
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("id".to_string(), json!(id));
        }
        write_ws_text(&mut self.stream, &payload.to_string())?;
        loop {
            let frame = read_ws_text(&mut self.stream)?;
            let value: Value = serde_json::from_str(&frame)
                .map_err(|e| AppServerError::Json(format!("{e}: {frame}")))?;
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                if is_approval_method(method) {
                    return Err(AppServerError::ApprovalUnsupported(method.to_string()));
                }
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("app-server request failed")
                    .to_string();
                return match method.as_str() {
                    "thread/resume" => Err(AppServerError::ThreadNotLive(message)),
                    "turn/start" if looks_busy(&message) => {
                        Err(AppServerError::LeaderBusy(message))
                    }
                    "turn/start" => Err(AppServerError::ThreadNotLive(message)),
                    _ => Err(AppServerError::ProtocolMismatch(format!(
                        "{method} failed: {message}"
                    ))),
                }
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| AppServerError::ProtocolMismatch("missing result".to_string()));
        }
    }
}

#[cfg(unix)]
fn is_approval_method(method: &str) -> bool {
    method.contains("requestApproval")
        || method.contains("requestUserInput")
        || method.contains("elicitation/request")
}

#[cfg(unix)]
fn looks_busy(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("active turn") || lower.contains("already has") || lower.contains("busy")
}

#[cfg(unix)]
fn read_http_response(stream: &mut UnixStream) -> Result<String, AppServerError> {
    let mut data = Vec::new();
    let mut buf = [0u8; 1];
    while !data.ends_with(b"\r\n\r\n") {
        stream
            .read_exact(&mut buf)
            .map_err(|e| AppServerError::Io(e.to_string()))?;
        data.push(buf[0]);
        if data.len() > 64 * 1024 {
            return Err(AppServerError::ProtocolMismatch(
                "oversized websocket handshake".to_string(),
            ));
        }
    }
    Ok(String::from_utf8_lossy(&data).to_string())
}

#[cfg(unix)]
fn write_ws_text(stream: &mut UnixStream, text: &str) -> Result<(), AppServerError> {
    let payload = text.as_bytes();
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    let mask = [0x54, 0x41, 0x47, 0x54];
    frame.extend_from_slice(&mask);
    for (idx, byte) in payload.iter().enumerate() {
        frame.push(*byte ^ mask[idx % 4]);
    }
    stream
        .write_all(&frame)
        .map_err(|e| AppServerError::Io(e.to_string()))
}

#[cfg(unix)]
fn read_ws_text(stream: &mut UnixStream) -> Result<String, AppServerError> {
    loop {
        let mut header = [0u8; 2];
        stream
            .read_exact(&mut header)
            .map_err(|e| AppServerError::Io(e.to_string()))?;
        let opcode = header[0] & 0x0f;
        if opcode == 0x8 {
            return Err(AppServerError::ProtocolMismatch(
                "websocket closed".to_string(),
            ));
        }
        let masked = header[1] & 0x80 != 0;
        let mut len = u64::from(header[1] & 0x7f);
        if len == 126 {
            let mut ext = [0u8; 2];
            stream
                .read_exact(&mut ext)
                .map_err(|e| AppServerError::Io(e.to_string()))?;
            len = u64::from(u16::from_be_bytes(ext));
        } else if len == 127 {
            let mut ext = [0u8; 8];
            stream
                .read_exact(&mut ext)
                .map_err(|e| AppServerError::Io(e.to_string()))?;
            len = u64::from_be_bytes(ext);
        }
        let mut mask = [0u8; 4];
        if masked {
            stream
                .read_exact(&mut mask)
                .map_err(|e| AppServerError::Io(e.to_string()))?;
        }
        let size = usize::try_from(len).map_err(|_| {
            AppServerError::ProtocolMismatch("websocket frame too large".to_string())
        })?;
        let mut payload = vec![0u8; size];
        stream
            .read_exact(&mut payload)
            .map_err(|e| AppServerError::Io(e.to_string()))?;
        if masked {
            for (idx, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[idx % 4];
            }
        }
        if opcode == 0x1 {
            return Ok(String::from_utf8_lossy(&payload).to_string());
        }
    }
}
