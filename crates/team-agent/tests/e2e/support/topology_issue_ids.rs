// label migration需同步此处: these ids are public diagnose/API contract.
pub const WORKER_PANE_BINDING_STALE: &str = "worker_pane_binding_stale";
pub const TMUX_ENDPOINT_SOCKET_CONFLICT: &str = "tmux_endpoint_socket_conflict";
pub const LEADER_RECEIVER_SOCKET_MISMATCH: &str = "leader_receiver_socket_mismatch";
pub const ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET: &str = "orphan_team_session_on_ignored_socket";
pub const TEAM_SESSION_MISSING_ON_CANONICAL_SOCKET: &str =
    "team_session_missing_on_canonical_socket";
pub const RECENT_COORDINATOR_SESSION_MISSING: &str = "recent_coordinator_session_missing";
pub const LEADER_PANE_ID_COLLIDES_WITH_AGENT: &str = "LeaderPaneIdCollidesWithAgent";
