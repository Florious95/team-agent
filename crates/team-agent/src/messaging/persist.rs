//! Durable boundary between resolved logical addressing and delivery/recovery.

use std::path::{Path, PathBuf};

use crate::db::message_store::{MessageStore, PersistMessageInput};
use crate::model::ids::{AgentId, TaskId, TeamKey};

use super::send::TrustedSender;
use super::MessagingError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalSendKind {
    Delivery,
    LeaderNotification,
    OfflineMailbox,
    Selftest,
    Watcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOrigin {
    Cli,
    Mcp,
    Internal(InternalSendKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalRecipient {
    Worker(AgentId),
    Leader,
}

impl LogicalRecipient {
    pub fn from_resolved(value: impl Into<String>) -> Self {
        let value = value.into();
        if value == "leader" {
            Self::Leader
        } else {
            Self::Worker(AgentId::new(value))
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Worker(agent) => agent.as_str(),
            Self::Leader => "leader",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryBlocker {
    CoordinatorUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialDisposition {
    Accepted,
    QueuedUntilLeaderAttach,
    Blocked(DeliveryBlocker),
}

impl InitialDisposition {
    fn row_status(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::QueuedUntilLeaderAttach => "queued_until_leader_attach",
            Self::Blocked(DeliveryBlocker::CoordinatorUnavailable) => {
                "queued_coordinator_unavailable"
            }
        }
    }

    fn error(self) -> Option<&'static str> {
        match self {
            Self::Blocked(DeliveryBlocker::CoordinatorUnavailable) => {
                Some("coordinator_unavailable")
            }
            Self::Accepted | Self::QueuedUntilLeaderAttach => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSendIntent {
    pub origin: SendOrigin,
    pub target_workspace: PathBuf,
    pub owner_team_id: Option<TeamKey>,
    pub recipient: LogicalRecipient,
    pub sender: TrustedSender,
    pub task_id: Option<TaskId>,
    pub content: String,
    pub reply_to: Option<String>,
    pub requires_ack: bool,
    pub requested_message_id: Option<String>,
    pub initial_disposition: InitialDisposition,
}

impl ResolvedSendIntent {
    #[allow(clippy::too_many_arguments)]
    pub fn accepted(
        origin: SendOrigin,
        workspace: &Path,
        owner_team_id: Option<TeamKey>,
        recipient: LogicalRecipient,
        sender: TrustedSender,
        task_id: Option<TaskId>,
        content: impl Into<String>,
        reply_to: Option<String>,
        requires_ack: bool,
        requested_message_id: Option<String>,
    ) -> Self {
        Self {
            origin,
            target_workspace: workspace.to_path_buf(),
            owner_team_id,
            recipient,
            sender,
            task_id,
            content: content.into(),
            reply_to,
            requires_ack,
            requested_message_id,
            initial_disposition: InitialDisposition::Accepted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedSend {
    pub message_id: String,
    pub owner_team_id: Option<TeamKey>,
    pub recipient: LogicalRecipient,
    pub row_status: String,
    pub blocker: Option<DeliveryBlocker>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistResolution {
    Persisted(PersistedSend),
    Duplicate(String),
}

/// The only production entry point that creates a durable message row.
pub fn persist_resolved_send(
    intent: &ResolvedSendIntent,
) -> Result<PersistResolution, MessagingError> {
    let store = MessageStore::open(&intent.target_workspace)?;
    if let Some(requested) = intent.requested_message_id.as_deref() {
        if store.message_exists(requested)? {
            return Ok(PersistResolution::Duplicate(requested.to_string()));
        }
    }
    let status = intent.initial_disposition.row_status();
    let message_id = store.persist_message(PersistMessageInput {
        message_id: intent.requested_message_id.as_deref(),
        owner_team_id: intent.owner_team_id.as_ref().map(TeamKey::as_str),
        task_id: intent.task_id.as_ref().map(TaskId::as_str),
        sender: intent.sender.as_str(),
        recipient: intent.recipient.as_str(),
        reply_to: intent.reply_to.as_deref(),
        requires_ack: intent.requires_ack,
        status,
        content: &intent.content,
        error: intent.initial_disposition.error(),
    })?;
    Ok(PersistResolution::Persisted(PersistedSend {
        message_id,
        owner_team_id: intent.owner_team_id.clone(),
        recipient: intent.recipient.clone(),
        row_status: status.to_string(),
        blocker: match intent.initial_disposition {
            InitialDisposition::Blocked(blocker) => Some(blocker),
            InitialDisposition::Accepted | InitialDisposition::QueuedUntilLeaderAttach => None,
        },
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_internal_send(
    workspace: &Path,
    kind: InternalSendKind,
    owner_team_id: Option<&str>,
    task_id: Option<&str>,
    sender: &str,
    recipient: &str,
    content: &str,
    reply_to: Option<&str>,
    requires_ack: bool,
    requested_message_id: Option<&str>,
    initial_disposition: InitialDisposition,
) -> Result<PersistResolution, MessagingError> {
    let mut intent = ResolvedSendIntent::accepted(
        SendOrigin::Internal(kind),
        workspace,
        owner_team_id.map(TeamKey::new),
        LogicalRecipient::from_resolved(recipient),
        TrustedSender::from_runtime_identity(AgentId::new(sender)),
        task_id.map(TaskId::new),
        content,
        reply_to.map(ToOwned::to_owned),
        requires_ack,
        requested_message_id.map(ToOwned::to_owned),
    );
    intent.initial_disposition = initial_disposition;
    persist_resolved_send(&intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ta-send-persist-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn coordinator_blocker_is_durable_and_not_delivered() {
        let workspace = workspace("coordinator-blocker");
        let mut intent = ResolvedSendIntent::accepted(
            SendOrigin::Cli,
            &workspace,
            Some(TeamKey::new("carc")),
            LogicalRecipient::from_resolved("w1"),
            TrustedSender::leader(),
            None,
            "probe",
            None,
            true,
            None,
        );
        intent.initial_disposition =
            InitialDisposition::Blocked(DeliveryBlocker::CoordinatorUnavailable);
        let PersistResolution::Persisted(persisted) = persist_resolved_send(&intent).unwrap()
        else {
            panic!("fresh intent must persist")
        };
        assert_eq!(persisted.row_status, "queued_coordinator_unavailable");
        assert_eq!(
            persisted.blocker,
            Some(DeliveryBlocker::CoordinatorUnavailable)
        );
        let store = MessageStore::open(&workspace).unwrap();
        let connection = crate::db::schema::open_db(store.db_path()).unwrap();
        let (status, delivered_at, error): (String, Option<String>, Option<String>) = connection
            .query_row(
                "select status, delivered_at, error from messages where message_id = ?1",
                [&persisted.message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "queued_coordinator_unavailable");
        assert_eq!(delivered_at, None);
        assert_eq!(error.as_deref(), Some("coordinator_unavailable"));
    }

    #[test]
    fn production_row_creation_is_owned_by_persisted_primitive() {
        for (name, source) in [
            ("send", include_str!("send.rs")),
            ("leader_receiver", include_str!("leader_receiver.rs")),
            ("delivery", include_str!("delivery.rs")),
            ("watchers", include_str!("watchers.rs")),
            ("helpers", include_str!("helpers.rs")),
            ("selftest", include_str!("selftest.rs")),
        ] {
            assert!(
                !source.contains(".create_message(")
                    && !source.contains(".create_message_with_id("),
                "{name} still creates message rows outside persist_resolved_send"
            );
        }
    }
}
