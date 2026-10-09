//! Framework-owned routing to a recipient outside this native store. These are
//! durable forwarding intents, not a second physical queue for that recipient.
//! The framework port owns scope resolution and uses an exact idempotency key.
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    store::{load_seat, ContractStore},
    Error,
};
use crate::contract::types::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameworkPeer {
    pub recipient: String,
    /// Opaque framework-owned route key, never a tool-supplied workspace/pane.
    pub route: String,
    pub provider: String,
}

// Payloads intentionally have no Debug representation.
#[derive(Clone, Serialize, Deserialize)]
pub enum ForwardPayload {
    Message {
        message: MessageId,
        task: Option<String>,
        sender: String,
        recipient: String,
        content: String,
        mailbox: bool,
    },
    Result {
        result_id: String,
        envelope: Value,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForwardState {
    Pending,
    InFlight,
    Accepted,
    Refused,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ForwardIntent {
    pub id: String,
    pub source: InstanceIdentity,
    pub route: String,
    pub payload: ForwardPayload,
    pub state: ForwardState,
    pub receipt: Option<Value>,
}

pub(crate) fn peer(tx: &Transaction<'_>, recipient: &str) -> Result<Option<FrameworkPeer>, Error> {
    let row: Option<String> = tx
        .query_row(
            "SELECT record FROM contract_framework_peers WHERE recipient=?1",
            [recipient],
            |row| row.get(0),
        )
        .optional()?;
    row.map(|row| serde_json::from_str(&row).map_err(Error::from))
        .transpose()
}
pub(crate) fn insert(tx: &Transaction<'_>, intent: &ForwardIntent) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO contract_forwards(id,state,record) VALUES(?1,'Pending',?2)",
        params![intent.id, serde_json::to_string(intent)?],
    )?;
    Ok(())
}

impl ContractStore {
    /// Only a framework controller supplies this roster. Existing native seats
    /// cannot be shadowed by external peers. Reconfiguration does not retarget
    /// already accepted intents: each one retains its captured route.
    pub fn set_framework_routes(
        &mut self,
        peers: &[FrameworkPeer],
        result_route: Option<&str>,
    ) -> Result<(), Error> {
        if peers.iter().any(|peer| {
            !nonblank(&peer.recipient)
                || peer.recipient.chars().any(char::is_control)
                || !nonblank(&peer.route)
                || peer.route.chars().any(char::is_control)
                || !nonblank(&peer.provider)
        }) || result_route
            .is_some_and(|route| !nonblank(route) || route.chars().any(char::is_control))
        {
            return Err(Error::Invalid("framework route"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut names = std::collections::BTreeSet::new();
        for peer in peers {
            if !names.insert(&peer.recipient) {
                return Err(Error::Conflict);
            }
            if let Ok(id) = SeatId::new(&peer.recipient) {
                if load_seat(&tx, &id)?.is_some() {
                    return Err(Error::Conflict);
                }
            }
        }
        tx.execute("DELETE FROM contract_framework_peers", [])?;
        for peer in peers {
            tx.execute(
                "INSERT INTO contract_framework_peers VALUES(?1,?2)",
                params![peer.recipient, serde_json::to_string(peer)?],
            )?;
        }
        tx.execute("UPDATE contract_meta SET result_route=?1", [result_route])?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_forwards(&self, limit: usize) -> Result<Vec<ForwardIntent>, Error> {
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("forward limit"));
        }
        let mut statement = self.connection.prepare(
            "SELECT record FROM contract_forwards WHERE state='Pending' ORDER BY rowid LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn forward(&self, id: &str) -> Result<ForwardIntent, Error> {
        let raw: String = self.connection.query_row(
            "SELECT record FROM contract_forwards WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Reserve before calling the framework port. Another consumer, a crashed
    /// call or an unknown prior effect cannot be implicitly retried here.
    pub fn claim_forward(&mut self, id: &str) -> Result<ForwardIntent, Error> {
        let scope = self.scope().clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: String = tx
            .query_row(
                "SELECT record FROM contract_forwards WHERE id=?1 AND state='Pending'",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Conflict)?;
        let mut intent: ForwardIntent = serde_json::from_str(&raw)?;
        if intent.id != id || intent.state != ForwardState::Pending || intent.source.scope != scope
        {
            return Err(Error::Fence);
        }
        intent.state = ForwardState::InFlight;
        let changed = tx.execute("UPDATE contract_forwards SET state='InFlight',record=?1 WHERE id=?2 AND state='Pending'", params![serde_json::to_string(&intent)?, id])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        tx.commit()?;
        Ok(intent)
    }

    /// Accepted means the destination's authoritative port accepted/persisted
    /// the request. It does not mean native consumption or leader presentation.
    pub fn finish_forward(
        &mut self,
        id: &str,
        state: ForwardState,
        receipt: Value,
    ) -> Result<(), Error> {
        if !matches!(
            state,
            ForwardState::Accepted | ForwardState::Refused | ForwardState::Unknown
        ) {
            return Err(Error::Invalid("forward completion"));
        }
        let scope = self.scope().clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: String = tx
            .query_row(
                "SELECT record FROM contract_forwards WHERE id=?1 AND state='InFlight'",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Conflict)?;
        let mut intent: ForwardIntent = serde_json::from_str(&raw)?;
        if intent.id != id || intent.state != ForwardState::InFlight || intent.source.scope != scope
        {
            return Err(Error::Fence);
        }
        if let ForwardPayload::Message { message, .. } = &intent.payload {
            let status = match state {
                ForwardState::Accepted => "forward_accepted",
                ForwardState::Refused => "forward_refused",
                ForwardState::Unknown => "forward_unknown",
                _ => return Err(Error::Invalid("forward completion")),
            };
            tx.execute(
                "UPDATE messages SET status=?1 WHERE message_id=?2",
                params![status, message.as_str()],
            )?;
        }
        intent.state = state;
        intent.receipt = Some(receipt);
        tx.execute(
            "UPDATE contract_forwards SET state=?1,record=?2 WHERE id=?3 AND state='InFlight'",
            params![format!("{state:?}"), serde_json::to_string(&intent)?, id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
