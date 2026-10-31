//! Framework directory projections. Legacy SQL forwarding is read-only history;
//! live business ingress is owned by station::send, never an outbox pump.
use super::{
    projection::Key,
    store::{load_seat, ContractStore},
    Error,
};
use crate::contract::types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

impl ContractStore {
    pub fn set_framework_routes(
        &mut self,
        peers: &[FrameworkPeer],
        result_route: Option<&str>,
    ) -> Result<(), Error> {
        if peers.iter().any(|p| {
            !nonblank(&p.recipient)
                || p.recipient.chars().any(char::is_control)
                || !nonblank(&p.route)
                || p.route.chars().any(char::is_control)
                || !nonblank(&p.provider)
        }) || result_route.is_some_and(|v| !nonblank(v) || v.chars().any(char::is_control))
        {
            return Err(Error::Invalid("framework route"));
        }
        let mut tx = self
            .files
            .write([Key::new("framework_config", "result_route")])?;
        let previous: Vec<FrameworkPeer> = self.files.list("peers")?;
        if previous == peers
            && self
                .files
                .get::<Option<String>>(&Key::new("framework_config", "result_route"))?
                == Some(result_route.map(str::to_owned))
        {
            return Ok(());
        }
        tx.lock_additional(
            previous
                .iter()
                .chain(peers)
                .map(|peer| Key::new("peers", &peer.recipient)),
        )?;
        let mut names = std::collections::BTreeSet::new();
        for peer in peers {
            if !names.insert(&peer.recipient) {
                return Err(Error::Conflict);
            }
            if let Ok(id) = SeatId::new(&peer.recipient) {
                if load_seat(&tx, &id)?.is_some()
                    || tx
                        .get::<String>(&Key::new("leases", id.as_str()))?
                        .is_some()
                {
                    return Err(Key::new("peers", id.as_str())
                        .fence("framework_peer_conflicts_with_native_seat_or_reservation"));
                }
            }
        }
        for peer in previous {
            tx.delete(Key::new("peers", peer.recipient))?;
        }
        for peer in peers {
            tx.put(Key::new("peers", &peer.recipient), peer)?;
        }
        tx.put(Key::new("framework_config", "result_route"), &result_route)?;
        tx.commit()
    }
    pub fn pending_forwards(&self, limit: usize) -> Result<Vec<ForwardIntent>, Error> {
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("forward limit"));
        }
        Ok(self
            .files
            .list::<ForwardIntent>("forwards")?
            .into_iter()
            .filter(|v| v.state == ForwardState::Pending)
            .take(limit)
            .collect())
    }
    pub fn forward(&self, id: &str) -> Result<ForwardIntent, Error> {
        self.files
            .get(&Key::new("forwards", id))?
            .ok_or(Error::Fence)
    }
    pub fn claim_forward(&mut self, _id: &str) -> Result<ForwardIntent, Error> {
        Err(Error::Invalid(
            "legacy forwarding retired; use station send",
        ))
    }
    pub fn finish_forward(
        &mut self,
        _id: &str,
        _state: ForwardState,
        _receipt: Value,
    ) -> Result<(), Error> {
        Err(Error::Invalid(
            "legacy forwarding retired; use station send",
        ))
    }
}
