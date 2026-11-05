//! Identity, address/channel resolution, presentation and communication diagnostics.
//!
//! Durable messages are handled by `station::send` and `inbox_store`, not a
//! coordinator-driven SQLite delivery engine.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use thiserror::Error;

#[cfg(test)]
use crate::event_log::EventLog;
#[cfg(test)]
use crate::model::enums::Provider;
#[cfg(test)]
use crate::model::ids::{LeaderSessionUuid, OwnerEpoch, TaskId, TeamKey};
#[cfg(test)]
use crate::transport::{PaneId, Target, Transport};

pub use crate::model::routing::{route_task, RouteResult};

pub mod address;
pub mod delivery;
pub mod helpers;
pub mod leader_channel;
pub mod leader_receiver;
pub mod presentation;
pub mod selftest;
pub mod send;
pub mod types;
pub mod watchers;

pub use address::{parse_logical_address, LogicalAddress, LogicalAddressTarget};
pub use leader_channel::{
    resolve_live_leader_channel, DirectTmuxLeaderChannel, LeaderChannelResolution,
    LeaderChannelUnbound, LiveLeaderChannel,
};
pub use selftest::{run_comms_selftest, CommsSelftestDriver};
pub use send::{MessageTarget, TrustedSender};
pub use types::{
    CheckEvidence, CheckKind, CheckStatus, ContractSuiteCheck,
    ProviderSdkCalls, SelftestCheck, SelftestReport,
};
pub use watchers::{format_result_watcher_notification, result_id_from_text};

/// Errors from current messaging diagnostics and retained public refusals.
#[derive(Debug, Error)]
pub enum MessagingError {
    #[error("db: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("transport: {0}")]
    Transport(#[from] crate::transport::TransportError),
    #[error("state: {0}")]
    State(#[from] crate::state::StateError),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    #[error("validation: {0}")]
    Validation(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests;
