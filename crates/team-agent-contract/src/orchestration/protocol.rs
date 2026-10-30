//! Captured native connections are observations, never process-liveness authority.
use super::{
    projection::Key,
    store::{current, ContractStore, SeatStatus},
    Error,
};
use crate::contract::types::*;
use crate::host::process::{verify_native_ancestry, ProcessStamp};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRecord {
    pub identity: InstanceIdentity,
    pub connection: InstanceId,
    pub server_key: String,
    pub binding_key: String,
    pub process: ProcessStamp,
    pub native_process: ProcessStamp,
    pub closed: bool,
}
impl ContractStore {
    pub fn register_connection(&mut self, record: &ConnectionRecord) -> Result<(), Error> {
        let key = Key::new("connections", record.connection.as_str());
        let index = Key::new("connection_index", record.identity.instance.as_str());
        let mut tx = self.files.write([
            key.clone(),
            index.clone(),
            Key::new("seats", record.identity.seat.as_str()),
        ])?;
        let seat = current(&tx, self.scope(), &record.identity)?;
        let target = seat
            .physical
            .as_ref()
            .ok_or(Error::Invalid("native target unpublished"))?;
        if record.closed
            || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
            || record.server_key != seat.server_key
            || record.binding_key != seat.binding_key
            || record.native_process != target.process
            || record.process.identity.pid == target.process.identity.pid
            || record.process.identity.executable_sha256 != target.candidate_sha256
            || verify_native_ancestry(&record.process, &target.process).is_err()
        {
            return Err(Error::Fence);
        }
        if tx.get::<ConnectionRecord>(&key)?.is_some() {
            return Err(Error::Conflict);
        }
        let mut ids: Vec<InstanceId> = tx.get(&index)?.unwrap_or_default();
        ids.push(record.connection.clone());
        tx.put(key, record)?;
        tx.put(index, &ids)?;
        tx.commit()
    }
    pub fn connections(&self, identity: &InstanceIdentity) -> Result<Vec<ConnectionRecord>, Error> {
        self.assert_current(identity)?;
        let ids: Vec<InstanceId> = self
            .files
            .get(&Key::new("connection_index", identity.instance.as_str()))?
            .unwrap_or_default();
        ids.into_iter()
            .map(|id| {
                let record: ConnectionRecord = self
                    .files
                    .get(&Key::new("connections", id.as_str()))?
                    .ok_or(Error::Corrupt)?;
                if record.identity != *identity || record.connection != id {
                    return Err(Error::Fence);
                }
                Ok(record)
            })
            .collect()
    }
    pub fn completed_connection_tools(
        &mut self,
        identity: &InstanceIdentity,
        connection: &InstanceId,
    ) -> Result<Vec<LogicalTool>, Error> {
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
        let prefix = format!("{}:", connection.as_str());
        evidence
            .calls
            .into_iter()
            .map(|(name, key)| {
                let suffix = key.strip_prefix(&prefix).ok_or(Error::Fence)?;
                serde_json::from_str::<serde_json::Value>(suffix)?;
                let address = super::mcp::call_address(identity, &key)?;
                let call: super::mcp::CallRecord = self
                    .files
                    .get(&Key::new("calls", &address))?
                    .ok_or(Error::Corrupt)?;
                let facts: super::mcp::FactRecord = self
                    .files
                    .get(&Key::new("facts", address))?
                    .ok_or(Error::Corrupt)?;
                if call.caller != *identity
                    || call.key != key
                    || facts.caller != *identity
                    || facts.key != key
                    || call.request["name"].as_str() != Some(&name)
                    || !facts.facts.contains("invocation_received")
                    || !facts.facts.contains("response_written")
                {
                    return Err(Error::Fence);
                }
                match name.as_str() {
                    "send_message" => Ok(LogicalTool::SendMessage),
                    "report_result" => Ok(LogicalTool::ReportResult),
                    "get_team_status" => Ok(LogicalTool::GetTeamStatus),
                    _ => Err(Error::Corrupt),
                }
            })
            .collect()
    }
    pub fn close_connection(
        &mut self,
        identity: &InstanceIdentity,
        connection: &InstanceId,
    ) -> Result<(), Error> {
        if identity.scope != *self.scope() {
            return Err(Error::Fence);
        }
        let key = Key::new("connections", connection.as_str());
        let mut tx = self.files.write([key.clone()])?;
        let mut record: ConnectionRecord = tx.get(&key)?.ok_or(Error::Fence)?;
        if record.identity != *identity || record.connection != *connection {
            return Err(Error::Fence);
        }
        record.closed = true;
        tx.put(key, &record)?;
        tx.commit()
    }
}
