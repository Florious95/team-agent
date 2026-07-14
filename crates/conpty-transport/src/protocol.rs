//! ConPTY named-pipe wire protocol.
//!
//! Design §Named Pipe Control Protocol (design.md:245-330).
//!
//! Framing: length-prefixed UTF-8 JSON. 4-byte little-endian length header,
//! then that many bytes of JSON. NOT newline-delimited so payloads may
//! contain arbitrary newlines (provider TUI escape sequences etc.).
//!
//! Schema field is `1` for the MVP. Future schema changes must fail-closed
//! at both ends (CR C-7 schema-skew constraint).

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Wire schema version. Bump AND fail-closed on both ends when the
/// protocol becomes incompatible (CR C-7).
pub const PROTOCOL_SCHEMA: u32 = 1;

/// Maximum single-frame length. Keeps a malformed / malicious length
/// prefix from causing an unbounded allocation.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The 15 operations from design §Request Shape:287-304. Serialised as
/// lowercase snake_case wire strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Hello,
    Spawn,
    Inject,
    SendKeys,
    Capture,
    Query,
    Liveness,
    HasPane,
    ListTargets,
    HasSession,
    ListWindows,
    SetSessionEnv,
    KillSession,
    KillWindow,
    KillPane,
    Shutdown,
}

/// Request envelope. The `payload` value is `serde_json::Value` so each
/// op can carry its own shape without a giant tagged enum at this layer.
///
/// CR C-1: `pipe_token` is only ever set in memory — the caller passes it
/// from an in-memory `PipeClient` and the shim compares it against its
/// in-memory rotation state. Neither side persists this field to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub schema: u32,
    pub request_id: String,
    pub workspace_hash: String,
    pub team_key: String,
    /// In-memory only. Never written to state.json / any persisted file
    /// (CR C-1 grep guard: no `pipe_token` field appears in state.rs).
    pub pipe_token: String,
    pub op: Op,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub payload: serde_json::Value,
}

impl Request {
    /// Build an untargeted request (hello / list_targets / shutdown).
    pub fn new(
        request_id: impl Into<String>,
        workspace_hash: impl Into<String>,
        team_key: impl Into<String>,
        pipe_token: impl Into<String>,
        op: Op,
    ) -> Self {
        Self {
            schema: PROTOCOL_SCHEMA,
            request_id: request_id.into(),
            workspace_hash: workspace_hash.into(),
            team_key: team_key.into(),
            pipe_token: pipe_token.into(),
            op,
            payload: serde_json::Value::Null,
        }
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Response envelope. `ok=true` carries `result` (per-op JSON); `ok=false`
/// carries `error` (a mapped `TransportError` shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub schema: u32,
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub result: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl Response {
    pub fn ok(request_id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            schema: PROTOCOL_SCHEMA,
            request_id: request_id.into(),
            ok: true,
            result,
            error: None,
        }
    }

    pub fn err(request_id: impl Into<String>, error: ProtocolError) -> Self {
        Self {
            schema: PROTOCOL_SCHEMA,
            request_id: request_id.into(),
            ok: false,
            result: serde_json::Value::Null,
            error: Some(error),
        }
    }
}

/// Error kinds emitted by the shim. Mapped to `TransportError` by the
/// backend layer (design §Request Shape:332-338 + CR C-5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolError {
    /// The request's `pipe_token` did not match the shim's current token.
    /// The caller MUST NOT silently retry with a rotated token — that
    /// would let a stale connection hijack a live shim. Map to
    /// `TransportError::MuxUnavailable` (CR C-5).
    PipeTokenMismatch { message: String },
    /// Schema field on the wire did not match `PROTOCOL_SCHEMA`. Both
    /// sides fail-closed on skew (CR C-7).
    SchemaSkew {
        message: String,
        sent: u32,
        expected: u32,
    },
    /// The named pane/target does not exist in the shim registry.
    TargetNotFound { message: String },
    /// Physical spawn (CreatePseudoConsole / CreateProcessW) failed.
    Spawn { message: String },
    /// Physical write to the ConPTY input pipe failed.
    Inject { message: String, stage: String },
    /// Read from the ConPTY output ring buffer failed.
    Capture { message: String },
    /// Shim itself is refusing to serve (e.g. shutdown in progress).
    ShimUnavailable { message: String },
    /// Anything not classified above; opaque to the backend.
    Other { message: String },
}

impl ProtocolError {
    pub fn message(&self) -> &str {
        match self {
            Self::PipeTokenMismatch { message }
            | Self::SchemaSkew { message, .. }
            | Self::TargetNotFound { message }
            | Self::Spawn { message }
            | Self::Inject { message, .. }
            | Self::Capture { message }
            | Self::ShimUnavailable { message }
            | Self::Other { message } => message,
        }
    }
}

/// Write a single frame: 4-byte little-endian length header, then the
/// JSON bytes. Returns the number of bytes written (header + body).
pub fn write_frame<W: Write>(writer: &mut W, json_bytes: &[u8]) -> io::Result<usize> {
    if json_bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "frame body {} bytes exceeds MAX_FRAME_BYTES {}",
                json_bytes.len(),
                MAX_FRAME_BYTES
            ),
        ));
    }
    let len_prefix = (json_bytes.len() as u32).to_le_bytes();
    writer.write_all(&len_prefix)?;
    writer.write_all(json_bytes)?;
    Ok(4 + json_bytes.len())
}

/// Read one frame: parse the 4-byte header, then read exactly that many
/// bytes. Returns the raw JSON body; caller does `serde_json::from_slice`.
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "frame header claims {} bytes, exceeds MAX_FRAME_BYTES {}",
                len, MAX_FRAME_BYTES
            ),
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// Serialise a `Request` to a length-prefixed frame in one call.
pub fn write_request<W: Write>(writer: &mut W, req: &Request) -> io::Result<usize> {
    let json =
        serde_json::to_vec(req).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(writer, &json)
}

/// Read + deserialise a `Request` from a length-prefixed frame.
pub fn read_request<R: Read>(reader: &mut R) -> io::Result<Request> {
    let body = read_frame(reader)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_response<W: Write>(writer: &mut W, resp: &Response) -> io::Result<usize> {
    let json =
        serde_json::to_vec(resp).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(writer, &json)
}

pub fn read_response<R: Read>(reader: &mut R) -> io::Result<Response> {
    let body = read_frame(reader)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ─────────────────────────────────────────────────────────────────────────
// Per-op payloads. Each op has a small typed struct that maps to the
// `payload` JSON value; the wire is not strongly-typed at that layer, but
// callers on both sides use these structs so drift is caught at compile
// time.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub session: String,
    pub window: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub env_unset: Vec<String>,
    #[serde(default = "SpawnRequest::default_cols")]
    pub cols: u16,
    #[serde(default = "SpawnRequest::default_rows")]
    pub rows: u16,
}

impl SpawnRequest {
    fn default_cols() -> u16 {
        120
    }
    fn default_rows() -> u16 {
        30
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResult {
    pub pane_id: String,
    pub session: String,
    pub window: String,
    pub child_pid: Option<u32>,
    pub spawn_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectRequest {
    pub pane_id: String,
    pub text: String,
    pub submit_key: Option<String>,
    #[serde(default)]
    pub bracketed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub pane_id: String,
    /// `head:N`, `tail:N`, or `full`.
    pub range: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResult {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub schema: u32,
    pub shim_pid: u32,
    pub shim_version: String,
    pub pipe_token: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Tests (portable — run on macOS/Linux/Windows).
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn hello_request_round_trip_through_length_prefix_frame() {
        let req = Request::new("req-1", "wshash", "team-a", "tok-123", Op::Hello);
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        // Header is 4 bytes little-endian; the body is JSON.
        assert!(buf.len() > 4);
        let mut cursor = Cursor::new(buf);
        let parsed = read_request(&mut cursor).unwrap();
        assert_eq!(parsed.request_id, "req-1");
        assert_eq!(parsed.op, Op::Hello);
        assert_eq!(parsed.pipe_token, "tok-123");
    }

    #[test]
    fn frame_length_prefix_is_little_endian_u32() {
        // Explicit wire test: a 5-byte body must serialise with header
        // [0x05, 0x00, 0x00, 0x00] then the body.
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        assert_eq!(&buf[0..4], &[0x05, 0x00, 0x00, 0x00]);
        assert_eq!(&buf[4..], b"hello");
    }

    #[test]
    fn frame_body_may_contain_newlines_and_control_bytes() {
        // Design §Named Pipe Control Protocol:263 — "Payload text can
        // contain arbitrary newlines and large protocol blocks."
        let payload = b"line1\nline2\r\n\x1b[31mred\x1b[0m\nline3";
        let mut buf = Vec::new();
        write_frame(&mut buf, payload).unwrap();
        let mut cursor = Cursor::new(buf);
        let read = read_frame(&mut cursor).unwrap();
        assert_eq!(read, payload);
    }

    #[test]
    fn frame_reader_rejects_oversized_header() {
        // Header claims 100MB — reader must refuse without allocating.
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&(100_000_000_u32).to_le_bytes());
        buf.extend_from_slice(b"anything");
        let mut cursor = Cursor::new(buf);
        let err = read_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("MAX_FRAME_BYTES"));
    }

    #[test]
    fn response_ok_and_err_serialise_and_deserialise() {
        let ok_resp = Response::ok(
            "req-2",
            serde_json::json!({"hello_result": {"shim_pid": 42}}),
        );
        let mut buf = Vec::new();
        write_response(&mut buf, &ok_resp).unwrap();
        let parsed = read_response(&mut Cursor::new(buf)).unwrap();
        assert!(parsed.ok);
        assert!(parsed.error.is_none());
        assert_eq!(
            parsed.result["hello_result"]["shim_pid"],
            serde_json::json!(42)
        );

        let err_resp = Response::err(
            "req-3",
            ProtocolError::PipeTokenMismatch {
                message: "shim rotated token, caller has stale one".to_string(),
            },
        );
        let mut buf = Vec::new();
        write_response(&mut buf, &err_resp).unwrap();
        let parsed = read_response(&mut Cursor::new(buf)).unwrap();
        assert!(!parsed.ok);
        let err = parsed.error.unwrap();
        assert!(
            matches!(err, ProtocolError::PipeTokenMismatch { .. }),
            "err discriminant round-trip: {err:?}"
        );
    }

    #[test]
    fn op_wire_strings_are_snake_case_stable() {
        // Lock the wire values so a future rename to `Hello` variant name
        // does not silently break shim protocol.
        for (op, wire) in [
            (Op::Hello, "hello"),
            (Op::Spawn, "spawn"),
            (Op::Inject, "inject"),
            (Op::SendKeys, "send_keys"),
            (Op::Capture, "capture"),
            (Op::Query, "query"),
            (Op::Liveness, "liveness"),
            (Op::HasPane, "has_pane"),
            (Op::ListTargets, "list_targets"),
            (Op::HasSession, "has_session"),
            (Op::ListWindows, "list_windows"),
            (Op::SetSessionEnv, "set_session_env"),
            (Op::KillSession, "kill_session"),
            (Op::KillWindow, "kill_window"),
            (Op::KillPane, "kill_pane"),
            (Op::Shutdown, "shutdown"),
        ] {
            let s = serde_json::to_string(&op).unwrap();
            assert_eq!(s, format!("\"{}\"", wire), "op {op:?} wire drift");
        }
    }

    #[test]
    fn protocol_schema_is_pinned_to_one_for_mvp() {
        // Bumping this value must be a conscious cross-repo change
        // (fail-closed on both ends per CR C-7 schema-skew).
        assert_eq!(PROTOCOL_SCHEMA, 1);
    }

    #[test]
    fn pipe_token_mismatch_is_a_distinct_error_variant() {
        // CR C-5 anchor: token mismatch must be structurally
        // distinguishable so the backend never conflates it with a
        // routine target-not-found or a shim-side spawn failure.
        let sample = ProtocolError::PipeTokenMismatch {
            message: "stale".to_string(),
        };
        assert!(matches!(sample, ProtocolError::PipeTokenMismatch { .. }));
        // Round-trip through JSON.
        let j = serde_json::to_string(&sample).unwrap();
        let back: ProtocolError = serde_json::from_str(&j).unwrap();
        assert!(matches!(back, ProtocolError::PipeTokenMismatch { .. }));
        // The JSON representation includes the kind tag.
        assert!(j.contains("\"kind\":\"pipe_token_mismatch\""));
    }
}
