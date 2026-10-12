//! Captured native stdio connections. A row is not liveness or discovery:
//! producers/consumers must freshly verify the captured processes and bounded
//! native ancestry before use; a previously accepted chain is not cached authority.
use rusqlite::{params, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::{
    store::{current, ContractStore, SeatStatus},
    Error,
};
use crate::contract::types::*;
use crate::host::process::{verify_native_ancestry, ProcessStamp};

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
            || record.process.identity.pid == target.process.identity.pid
            || record.process.identity.executable_sha256 != target.candidate_sha256
            || verify_native_ancestry(&record.process, &target.process).is_err()
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

    /// Historical successful tool-call pairs on exactly one connection and
    /// generation. This is NOT liveness or client consumption: the root must
    /// freshly verify the captured native ancestry, SID and open connection
    /// before using these names as NativeToolRoundtrip binding evidence.
    pub fn completed_connection_tools(
        &mut self,
        identity: &InstanceIdentity,
        connection: &InstanceId,
    ) -> Result<Vec<LogicalTool>, Error> {
        let scope = self.scope().clone();
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let seat = current(&tx, &scope, identity)?;
        if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
            return Err(Error::Fence);
        }
        let caller = serde_json::to_string(identity)?;
        let prefix = format!("{}:", connection.as_str());
        let tools = {
            // Both facts must pair with the SAME completed invocation; a bare
            // response write, error, other call ID or tools/list cannot qualify.
            // The suffix is one serialized RPC ID, preventing an opaque
            // connection such as `a:child` from being accumulated into `a`.
            // Only names leave SQL; arguments/results are not diagnostics.
            let mut statement = tx.prepare(
                "SELECT DISTINCT json_extract(c.request,'$.name') FROM contract_calls c
                 JOIN contract_facts i ON i.caller=c.caller AND i.call_key=c.call_key AND i.fact='invocation_received'
                 JOIN contract_facts w ON w.caller=c.caller AND w.call_key=c.call_key AND w.fact='response_written'
                 WHERE c.caller=?1 AND substr(c.call_key,1,length(?2))=?2
                 AND json_valid(substr(c.call_key,length(?2)+1)) ORDER BY 1",
            )?;
            let rows = statement.query_map(params![caller, prefix], |row| row.get::<_, String>(0))?;
            rows.map(|row| match row?.as_str() {
                "send_message" => Ok(LogicalTool::SendMessage),
                "report_result" => Ok(LogicalTool::ReportResult),
                "get_team_status" => Ok(LogicalTool::GetTeamStatus),
                _ => Err(Error::Corrupt),
            }).collect::<Result<Vec<_>, Error>>()?
        };
        tx.commit()?;
        Ok(tools)
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
