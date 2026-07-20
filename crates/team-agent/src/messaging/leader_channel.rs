use serde_json::Value;

use crate::codex_app_server::AppServerBinding;
use crate::transport::{PaneInfo, Transport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveLeaderChannel {
    DirectTmux(DirectTmuxLeaderChannel),
    CodexAppServer(AppServerBinding),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectTmuxLeaderChannel {
    pub pane_id: String,
    pub tmux_socket: Option<String>,
    pub observed: PaneInfo,
    pub metadata_drift: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderChannelResolution {
    Live(LiveLeaderChannel),
    Unbound(LeaderChannelUnbound),
    ProbeFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderChannelUnbound {
    ReceiverNotAttached,
    TransportConflict,
    MissingPaneId,
    NonCanonicalTmuxSocket,
    EndpointMismatch,
    PaneNotLive,
    AppServerBindingInvalid,
}

/// Resolve the canonical receiver's live physical channel without mutating
/// ownership. For direct tmux, `(absolute socket, pane id)` is the authority;
/// session/window/tty/fingerprint are observations and can only report drift.
pub fn resolve_live_leader_channel(
    receiver: &Value,
    transport: &dyn Transport,
) -> LeaderChannelResolution {
    if receiver.get("status").and_then(Value::as_str) != Some("attached") {
        return LeaderChannelResolution::Unbound(LeaderChannelUnbound::ReceiverNotAttached);
    }
    if receiver_transport_conflicts(receiver) {
        return LeaderChannelResolution::Unbound(LeaderChannelUnbound::TransportConflict);
    }
    if crate::codex_app_server::receiver_is_app_server(receiver) {
        return match crate::codex_app_server::binding_from_receiver(receiver) {
            Ok(binding) => {
                LeaderChannelResolution::Live(LiveLeaderChannel::CodexAppServer(binding))
            }
            Err(_) => {
                LeaderChannelResolution::Unbound(LeaderChannelUnbound::AppServerBindingInvalid)
            }
        };
    }

    let Some(pane_id) = receiver
        .get("pane_id")
        .and_then(Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return LeaderChannelResolution::Unbound(LeaderChannelUnbound::MissingPaneId);
    };
    let tmux_socket = receiver
        .get("tmux_socket")
        .and_then(Value::as_str)
        .filter(|socket| !socket.is_empty());
    if tmux_socket.is_some_and(|socket| !std::path::Path::new(socket).is_absolute()) {
        return LeaderChannelResolution::Unbound(LeaderChannelUnbound::NonCanonicalTmuxSocket);
    }
    if let Some(expected) = tmux_socket {
        if transport.tmux_endpoint().as_deref() != Some(expected) {
            return LeaderChannelResolution::Unbound(LeaderChannelUnbound::EndpointMismatch);
        }
    }
    let targets = match transport.list_targets() {
        Ok(targets) => targets,
        Err(error) => return LeaderChannelResolution::ProbeFailed(error.to_string()),
    };
    let Some(observed) = targets
        .into_iter()
        .find(|target| target.pane_id.as_str() == pane_id)
    else {
        return LeaderChannelResolution::Unbound(LeaderChannelUnbound::PaneNotLive);
    };
    let metadata_drift = receiver_metadata_drift(receiver, &observed);
    LeaderChannelResolution::Live(LiveLeaderChannel::DirectTmux(DirectTmuxLeaderChannel {
        pane_id: pane_id.to_string(),
        tmux_socket: tmux_socket.map(str::to_string),
        observed,
        metadata_drift,
    }))
}

fn receiver_transport_conflicts(receiver: &Value) -> bool {
    let mode = receiver.get("mode").and_then(Value::as_str);
    let transport_kind = receiver.get("transport_kind").and_then(Value::as_str);
    matches!((mode, transport_kind), (Some(mode), Some(kind)) if !mode.is_empty() && !kind.is_empty() && mode != kind)
}

fn receiver_metadata_drift(receiver: &Value, observed: &PaneInfo) -> Vec<&'static str> {
    let mut drift = Vec::new();
    if string_field(receiver, "session_name")
        .is_some_and(|expected| expected != observed.session.as_str())
    {
        drift.push("session_name");
    }
    if string_field(receiver, "window_name").is_some_and(|expected| {
        observed.window_name.as_ref().map(|value| value.as_str()) != Some(expected)
    }) {
        drift.push("window_name");
    }
    if string_field(receiver, "pane_tty")
        .is_some_and(|expected| observed.tty.as_deref() != Some(expected))
    {
        drift.push("pane_tty");
    }
    let observed_fingerprint = format!(
        "{}|{}|{}|{}",
        observed.session.as_str(),
        observed
            .window_index
            .map_or_else(String::new, |value| value.to_string()),
        observed
            .pane_index
            .map_or_else(String::new, |value| value.to_string()),
        observed.tty.as_deref().unwrap_or("")
    );
    if string_field(receiver, "fingerprint")
        .is_some_and(|expected| expected != observed_fingerprint)
    {
        drift.push("fingerprint");
    }
    drift
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}
