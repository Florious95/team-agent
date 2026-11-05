//!
//! step 14a · mcp_server::helpers — pure regularization + shared free helpers.

use std::io::Write as _;
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use crate::messaging::MessageTarget;
use crate::state::persist::load_runtime_state;

use super::types::{NormalizedReportEnvelope, ToolError, ToolErrorReason};

// ═══════════════════════════════════════════════════════════════════════════
// MODULE HELPERS (tools.py:16-69) — pure regularization, contract-callable.
// ═══════════════════════════════════════════════════════════════════════════

pub(crate) fn tool_error_reason_wire(reason: ToolErrorReason) -> &'static str {
    match reason {
        ToolErrorReason::UnknownTool => "unknown_tool",
        ToolErrorReason::InvalidToolArguments => "invalid_tool_arguments",
        ToolErrorReason::InternalRuntimeError => "internal_runtime_error",
        ToolErrorReason::PeerNotInScope => "peer_not_in_scope",
        ToolErrorReason::SelfTarget => "self_target",
        ToolErrorReason::NameNotResolvable => "name_not_resolvable",
        ToolErrorReason::NameAmbiguous => "name_ambiguous",
        ToolErrorReason::NameInvalid => "name_invalid",
        ToolErrorReason::NameNotLive => "name_not_live",
        ToolErrorReason::TeamKeyNotFound => "team_key_not_found",
        ToolErrorReason::EmptyTargetList => "empty_target_list",
        ToolErrorReason::McpScopeRefused => "mcp.scope_refused",
    }
}

pub(crate) fn normalize_token(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
}

pub(crate) fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(text_of_value)
}

pub(crate) fn text_of_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => non_empty_string(s).map(ToString::to_string),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

pub(crate) fn items_from_value(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other.clone()],
    }
}

pub(crate) fn non_empty_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(crate) fn json_dumps_default(value: &Value) -> String {
    let mut bytes = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut bytes, PythonJsonFormatter);
    if value.serialize(&mut ser).is_err() {
        return "{}".to_string();
    }
    String::from_utf8(bytes).unwrap_or_else(|_| "{}".to_string())
}

struct PythonJsonFormatter;

impl serde_json::ser::Formatter for PythonJsonFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

pub(crate) fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }
}

pub(crate) fn tool_runtime_error(err: impl std::fmt::Display) -> ToolError {
    ToolError::new(
        ToolErrorReason::InternalRuntimeError,
        ToolError::public_exception_message(&err.to_string(), "RuntimeError"),
        "RuntimeError",
    )
}

pub(crate) fn object_fields(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("ok".to_string(), Value::Bool(true));
            map.insert("value".to_string(), other);
            map
        }
    }
}
