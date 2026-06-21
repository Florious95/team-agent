//! unit-9 (Stage 4) — tmux endpoint selection policy, separated from the
//! concrete `TmuxBackend` execution layer.
//!
//! Today the endpoint-selection rule lives inside `crate::tmux_backend`
//! (private fn `runtime_tmux_endpoint_from_state` + helpers) intertwined
//! with the concrete command-execution backend. The policy is independently
//! testable — given a `state.json` value, pick which endpoint string to
//! bind a tmux client to — but you can't reach it without spinning up the
//! whole backend.
//!
//! This module exposes the policy as a small, pure, layout-layer concern.
//! Concrete `TmuxBackend` stays in `tmux_backend.rs` for command execution.
//! Callers that only want to KNOW the endpoint (cli printers, diagnostics,
//! event-log fields) can ask this module instead of constructing a backend.
//!
//! Migration: additive. The existing `tmux_backend` API is unchanged; the
//! policy here mirrors it byte-for-byte (verified by `endpoint_priority_matches_backend`
//! contract tests).

use std::path::Path;
use serde_json::Value;

/// Which state field (or fallback) supplied the chosen endpoint.
///
/// Mirrors `crate::tmux_backend::RuntimeTmuxEndpointSource` 1:1 — this
/// public surface lets layout-layer callers reason about the source
/// without depending on a `pub(crate)` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxEndpointSource {
    /// `state.tmux_endpoint` (highest priority).
    StateTmuxEndpoint,
    /// `state.tmux_socket` (fallback when `tmux_endpoint` is absent).
    StateTmuxSocket,
    /// Derived from the workspace path (lowest priority, used when state
    /// has no endpoint info at all — typical for never-quick-started ws).
    WorkspaceFallback,
}

impl TmuxEndpointSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StateTmuxEndpoint => "state.tmux_endpoint",
            Self::StateTmuxSocket => "state.tmux_socket",
            Self::WorkspaceFallback => "workspace_fallback",
        }
    }
}

/// Selected endpoint description: the string to pass to tmux + which state
/// field it came from. The endpoint may be either a short socket name
/// (`-L <name>`) or a path (`-S <path>`); callers map that to flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxEndpointSelection {
    pub endpoint: String,
    pub source: TmuxEndpointSource,
}

/// Pure policy: pick the tmux endpoint to use given a (maybe-present)
/// `state.json` Value.
///
/// Priority — keep in sync with `crate::tmux_backend::
/// runtime_tmux_endpoint_from_state`:
///   1. `state.tmux_endpoint` (non-empty string)
///   2. `state.tmux_socket` (non-empty string)
///   3. workspace fallback (caller supplies the derived value)
///
/// Returns `None` when state has neither field; callers fall back to a
/// workspace-derived value via [`select_endpoint_for_workspace`].
pub fn select_endpoint_from_state(state: Option<&Value>) -> Option<TmuxEndpointSelection> {
    let state = state?;
    if let Some(endpoint) = state
        .get("tmux_endpoint")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(TmuxEndpointSelection {
            endpoint: endpoint.to_string(),
            source: TmuxEndpointSource::StateTmuxEndpoint,
        });
    }
    if let Some(socket) = state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(TmuxEndpointSelection {
            endpoint: socket.to_string(),
            source: TmuxEndpointSource::StateTmuxSocket,
        });
    }
    None
}

/// State-first endpoint selection with workspace fallback. Equivalent to
/// what `tmux_backend_for_runtime_state_or_workspace` does internally,
/// minus the backend construction.
pub fn select_endpoint_for_workspace(
    workspace: &Path,
    state: Option<&Value>,
) -> TmuxEndpointSelection {
    if let Some(sel) = select_endpoint_from_state(state) {
        return sel;
    }
    TmuxEndpointSelection {
        endpoint: crate::tmux_backend::socket_name_for_workspace(workspace),
        source: TmuxEndpointSource::WorkspaceFallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn state_tmux_endpoint_wins_over_socket() {
        let state = json!({
            "tmux_endpoint": "/private/tmp/tmux-501/abc",
            "tmux_socket": "abc",
        });
        let sel = select_endpoint_from_state(Some(&state)).unwrap();
        assert_eq!(sel.endpoint, "/private/tmp/tmux-501/abc");
        assert_eq!(sel.source, TmuxEndpointSource::StateTmuxEndpoint);
    }

    #[test]
    fn state_tmux_socket_used_when_endpoint_absent() {
        let state = json!({ "tmux_socket": "ta-abc" });
        let sel = select_endpoint_from_state(Some(&state)).unwrap();
        assert_eq!(sel.endpoint, "ta-abc");
        assert_eq!(sel.source, TmuxEndpointSource::StateTmuxSocket);
    }

    #[test]
    fn empty_strings_are_ignored() {
        let state = json!({ "tmux_endpoint": "", "tmux_socket": "" });
        assert!(select_endpoint_from_state(Some(&state)).is_none());
    }

    #[test]
    fn none_state_returns_none() {
        assert!(select_endpoint_from_state(None).is_none());
    }

    #[test]
    fn workspace_fallback_kicks_in_with_no_state_endpoint() {
        let ws = PathBuf::from("/tmp/ta-unit9-fallback");
        let sel = select_endpoint_for_workspace(&ws, None);
        assert_eq!(sel.source, TmuxEndpointSource::WorkspaceFallback);
        assert!(!sel.endpoint.is_empty());
    }

    #[test]
    fn workspace_fallback_skipped_when_state_carries_endpoint() {
        let ws = PathBuf::from("/tmp/ta-unit9-prefer-state");
        let state = json!({ "tmux_endpoint": "state-endpoint-x" });
        let sel = select_endpoint_for_workspace(&ws, Some(&state));
        assert_eq!(sel.endpoint, "state-endpoint-x");
        assert_eq!(sel.source, TmuxEndpointSource::StateTmuxEndpoint);
    }
}
