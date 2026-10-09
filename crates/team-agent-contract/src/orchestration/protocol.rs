//! Captured native stdio connections. A row is not liveness or discovery:
//! producers/consumers must freshly sample both process stamps before use.
use rusqlite::{params, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::{
    store::{current, ContractStore, SeatStatus},
    Error,
};
use crate::contract::types::*;
use crate::host::process::ProcessStamp;

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
    /// The stdio entry captures its actual executable/birth/parent. Tools cannot
    /// supply this record. Registration permits protocol setup during F3 but
    /// never produces a client-consumed/tool-discovery fact.
    pub fn register_connection(&mut self, record: &ConnectionRecord) -> Result<(), Error> {
        let scope = self.scope().clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seat = current(&tx, &scope, &record.identity)?;
        let target = seat
            .physical
            .as_ref()
            .ok_or(Error::Invalid("native target unpublished"))?;
        if record.closed
            || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
            || record.server_key != seat.server_key
            || record.binding_key != seat.binding_key
            || record.native_process != target.process
            || record.process.parent != target.process.identity.pid
            || record.process.identity.pid == target.process.identity.pid
            || record.process.identity.executable_sha256 != target.candidate_sha256
        {
            return Err(Error::Fence);
        }
        tx.execute(
            "INSERT INTO contract_connections VALUES(?1,?2,?3)",
            params![
                record.connection.as_str(),
                serde_json::to_string(&record.identity)?,
                serde_json::to_string(record)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn connections(&self, identity: &InstanceIdentity) -> Result<Vec<ConnectionRecord>, Error> {
        self.assert_current(identity)?;
        let mut statement = self
            .connection
            .prepare("SELECT record FROM contract_connections WHERE identity=?1 ORDER BY rowid")?;
        let rows = statement.query_map([serde_json::to_string(identity)?], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn close_connection(
        &mut self,
        identity: &InstanceIdentity,
        connection: &InstanceId,
    ) -> Result<(), Error> {
        // Closing an already accepted connection remains possible after a seat
        // transitions; it cannot publish a fact for the replacement generation.
        if &identity.scope != self.scope() {
            return Err(Error::Fence);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: String = tx.query_row(
            "SELECT record FROM contract_connections WHERE connection=?1 AND identity=?2",
            params![connection.as_str(), serde_json::to_string(identity)?],
            |row| row.get(0),
        )?;
        let mut record: ConnectionRecord = serde_json::from_str(&raw)?;
        if record.identity != *identity || record.connection != *connection {
            return Err(Error::Fence);
        }
        record.closed = true;
        tx.execute(
            "UPDATE contract_connections SET record=?1 WHERE connection=?2",
            params![serde_json::to_string(&record)?, connection.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }
}
