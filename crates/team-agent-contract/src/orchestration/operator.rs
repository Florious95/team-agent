//! The former SQL business ingress is retired. All CLI/MCP business writes use
//! station::send and the C3 inbox; rejection here is purely in memory.
use super::{
    store::{ContractStore, SeatStatus},
    Error,
};
use crate::contract::types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorSend {
    pub recipient: SeatId,
    pub content: String,
    pub task: Option<String>,
    pub message: Option<MessageId>,
    pub mailbox: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReceipt {
    pub message: MessageId,
    pub task: String,
    pub target: InstanceIdentity,
    pub mailbox: bool,
}
impl ContractStore {
    pub fn send_from_operator(&mut self, _request: &OperatorSend) -> Result<SendReceipt, Error> {
        Err(Error::Invalid("legacy outbox retired; use station send"))
    }
    pub fn send_from_framework(
        &mut self,
        _request: &OperatorSend,
        _sender: &SeatId,
    ) -> Result<SendReceipt, Error> {
        Err(Error::Invalid("legacy outbox retired; use station send"))
    }
    /// No live turn can originate from the retired outbox. C3 owns submitted tasks.
    pub fn submitted_task(
        &mut self,
        _identity: &InstanceIdentity,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }
    pub fn connection_facts(
        &mut self,
        identity: &InstanceIdentity,
        connection: &InstanceId,
    ) -> Result<Vec<String>, Error> {
        let seat = self.assert_current(identity)?;
        if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
            return Err(Error::Fence);
        }
        let Some(evidence) =
            self.files
                .get::<super::mcp::ToolEvidence>(&super::mcp::ToolEvidence::key(
                    identity, connection,
                )?)?
        else {
            return Ok(Vec::new());
        };
        if evidence.caller != *identity || evidence.connection != *connection {
            return Err(Error::Fence);
        }
        Ok(evidence.facts.into_iter().collect())
    }
    pub fn inbox(&self, _recipient: &SeatId, _limit: usize) -> Result<Vec<Value>, Error> {
        Err(Error::Invalid("legacy inbox retired; use the C3 inbox"))
    }
}
