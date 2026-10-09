use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::Error;
use crate::contract::delivery::DeliveryEffect;
use crate::contract::plan::OwnedResourceReceipt;
use crate::contract::probe::ProcessIdentity;
use crate::contract::session::{CwdIdentity, ResumeBinding};
use crate::contract::types::*;

const APPLICATION_ID: i64 = 0x54414333;
const SCHEMA: &str = "
PRAGMA application_id=1413563187;
PRAGMA user_version=1;
CREATE TABLE contract_meta(scope TEXT NOT NULL, endpoint TEXT NOT NULL, root TEXT NOT NULL);
CREATE TABLE contract_seats(seat TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE contract_operations(id TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE contract_leases(seat TEXT PRIMARY KEY, operation TEXT NOT NULL);
CREATE TABLE contract_calls(caller TEXT NOT NULL, call_key TEXT NOT NULL, request TEXT NOT NULL, response TEXT NOT NULL,
 PRIMARY KEY(caller,call_key));
CREATE TABLE contract_facts(caller TEXT NOT NULL, call_key TEXT NOT NULL, fact TEXT NOT NULL,
 PRIMARY KEY(caller,call_key,fact));
CREATE TABLE contract_sequence(id INTEGER PRIMARY KEY CHECK(id=1), value INTEGER NOT NULL);
INSERT INTO contract_sequence VALUES(1,0);
CREATE TABLE messages(message_id TEXT PRIMARY KEY, owner_team_id TEXT NOT NULL, task_id TEXT,
 sender TEXT NOT NULL, recipient TEXT NOT NULL, status TEXT NOT NULL, content TEXT NOT NULL,
 presentation TEXT NOT NULL, delivery_attempts INTEGER NOT NULL DEFAULT 0);
CREATE TABLE results(result_id TEXT PRIMARY KEY, owner_team_id TEXT NOT NULL, task_id TEXT NOT NULL,
 agent_id TEXT NOT NULL, envelope TEXT NOT NULL, status TEXT NOT NULL,
 UNIQUE(owner_team_id,task_id,agent_id));
CREATE TABLE contract_outbox(message_id TEXT PRIMARY KEY REFERENCES messages(message_id), target TEXT NOT NULL,
 state TEXT NOT NULL, attempt TEXT, effect TEXT NOT NULL);
";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeatStatus {
    Starting,
    Ready,
    Stopped,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRecord {
    pub identity: InstanceIdentity,
    pub provider: ProviderId,
    pub native: NativeIdentity,
    pub cwd: CwdIdentity,
    pub endpoint: String,
    pub pane: String,
    pub binding_key: String,
    pub profile: String,
    pub server_key: String,
    pub evidence_kind: EvidenceKind,
    pub status: SeatStatus,
    pub process: Option<ProcessIdentity>,
    pub session: Option<ResumeBinding>,
    pub resources: Vec<OwnedResourceReceipt>,
    pub bootstrap_used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    F0Preflight,
    F1Stage,
    F2Register,
    F3Spawn,
    F4Commit,
    F5Receipt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionKind {
    Startup,
    Restart,
    InWindowBranch,
    NewSeatFullSnapshot,
    NativeNewSeat,
    Teardown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Running,
    Committed,
    Compensated,
    NeedsRecovery,
}

/// Write-ahead `pending` is durable BEFORE invoking a side-effecting host/hook.
/// On recovery it means uncertainty, never permission to replay the action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: OperationId,
    pub kind: TransactionKind,
    pub target: SeatRecord,
    pub parent: Option<SeatRecord>,
    pub phase: Phase,
    pub pending: Option<String>,
    pub resources: Vec<OwnedResourceReceipt>,
    pub effect: DeliveryEffect,
    pub outcome: Outcome,
    pub preserved: Vec<OwnedPath>,
    pub failure: Option<String>,
}

pub struct ContractStore {
    pub(crate) connection: Connection,
    scope: ScopeId,
    root: PathBuf,
    endpoint: String,
}

/// The root is an exclusive, private framework directory. It must not be an old
/// workspace. No migration/adoption path exists. Same-user hostile writers are
/// outside this DB boundary; host owned-I/O must still enforce fresh ownership.
impl ContractStore {
    pub fn create(root: &Path, scope: ScopeId, endpoint: &str) -> Result<Self, Error> {
        require_absolute(root, "store root")?;
        if endpoint.trim().is_empty() {
            return Err(Error::Invalid("endpoint"));
        }
        validate_ancestors(root.parent().ok_or(Error::Invalid("root parent"))?)?;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root)?; // exclusive: even an empty pre-existing root is refused
        let connection = Connection::open_with_flags(
            root.join("contract.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure(&connection)?;
        connection.execute_batch(SCHEMA)?;
        connection.execute(
            "INSERT INTO contract_meta VALUES(?1,?2,?3)",
            params![
                scope.as_str(),
                endpoint,
                root.to_str().ok_or(Error::Invalid("root utf8"))?
            ],
        )?;
        Ok(Self {
            connection,
            scope,
            root: root.to_path_buf(),
            endpoint: endpoint.into(),
        })
    }

    pub fn open(root: &Path, scope: ScopeId, endpoint: &str) -> Result<Self, Error> {
        require_absolute(root, "store root")?;
        validate_ancestors(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(root)?.permissions().mode() & 0o077 != 0 {
                return Err(Error::Fence);
            }
        }
        let path = root.join("contract.db");
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(Error::Fence);
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        // Verify identity before pragmas, migrations, or writes.
        let app: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != APPLICATION_ID || version != 1 {
            return Err(Error::Fence);
        }
        let stored: (String, String, String) =
            connection.query_row("SELECT scope,endpoint,root FROM contract_meta", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if stored
            != (
                scope.as_str().into(),
                endpoint.into(),
                root.to_str().ok_or(Error::Fence)?.into(),
            )
        {
            return Err(Error::Fence);
        }
        configure(&connection)?;
        Ok(Self {
            connection,
            scope,
            root: root.into(),
            endpoint: endpoint.into(),
        })
    }

    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn seat(&self, seat: &SeatId) -> Result<Option<SeatRecord>, Error> {
        load_seat(&self.connection, seat)
    }
    pub fn seats(&self) -> Result<Vec<SeatRecord>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT record FROM contract_seats ORDER BY seat")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn operation(&self, id: &OperationId) -> Result<OperationRecord, Error> {
        let raw: String = self.connection.query_row(
            "SELECT record FROM contract_operations WHERE id=?1",
            [id.as_str()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&raw)?)
    }
    pub fn unfinished(&self) -> Result<Vec<OperationRecord>, Error> {
        let mut statement = self
            .connection
            .prepare("SELECT record FROM contract_operations ORDER BY id")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        let all: Vec<OperationRecord> = rows
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_, Error>>()?;
        Ok(all
            .into_iter()
            .filter(|r| matches!(r.outcome, Outcome::Running | Outcome::NeedsRecovery))
            .collect())
    }
    pub fn assert_current(&self, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
        current(&self.connection, &self.scope, identity)
    }

    pub(crate) fn begin(&mut self, operation: &OperationRecord) -> Result<(), Error> {
        if operation.target.identity.scope != self.scope
            || operation.target.endpoint != self.endpoint
        {
            return Err(Error::Fence);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = &operation.target.identity;
        let old = load_seat(&tx, &target.seat)?;
        match operation.kind {
            TransactionKind::Startup
            | TransactionKind::NewSeatFullSnapshot
            | TransactionKind::NativeNewSeat => {
                if old.is_some() || target.generation.0 != 1 {
                    return Err(Error::Conflict);
                }
            }
            TransactionKind::Restart => {
                let parent = operation.parent.as_ref().ok_or(Error::Fence)?;
                if old.as_ref() != Some(parent)
                    || target.seat != parent.identity.seat
                    || target.instance == parent.identity.instance
                    || parent.identity.generation.0.checked_add(1) != Some(target.generation.0)
                {
                    return Err(Error::Fence);
                }
            }
            TransactionKind::InWindowBranch | TransactionKind::Teardown => {
                if old.as_ref() != Some(&operation.target) {
                    return Err(Error::Fence);
                }
            }
        }
        if let Some(parent) = &operation.parent {
            if current(&tx, &self.scope, &parent.identity)? != *parent {
                return Err(Error::Fence);
            }
        }
        tx.execute(
            "INSERT INTO contract_leases VALUES(?1,?2)",
            params![target.seat.as_str(), operation.id.as_str()],
        )?;
        if let Some(parent) = &operation.parent {
            if parent.identity.seat != target.seat {
                tx.execute(
                    "INSERT INTO contract_leases VALUES(?1,?2)",
                    params![parent.identity.seat.as_str(), operation.id.as_str()],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO contract_operations VALUES(?1,?2)",
            params![operation.id.as_str(), serde_json::to_string(operation)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Lease-CAS protects every phase and registration update. Record + seat +
    /// lease release share a single transaction, including failure compensation.
    pub(crate) fn save(&mut self, operation: &OperationRecord, publish: bool) -> Result<(), Error> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lease: Option<String> = tx
            .query_row(
                "SELECT operation FROM contract_leases WHERE seat=?1",
                [operation.target.identity.seat.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if lease.as_deref() != Some(operation.id.as_str()) {
            return Err(Error::Fence);
        }
        let rollback = operation.outcome == Outcome::Compensated;
        if operation.kind == TransactionKind::Restart
            && (rollback
                || (!publish && matches!(operation.phase, Phase::F0Preflight | Phase::F1Stage)))
        {
            if let Some(parent) = &operation.parent {
                tx.execute(
                    "UPDATE contract_seats SET record=?1 WHERE seat=?2",
                    params![
                        serde_json::to_string(parent)?,
                        parent.identity.seat.as_str()
                    ],
                )?;
            }
        }
        if rollback
            && matches!(
                operation.kind,
                TransactionKind::Startup
                    | TransactionKind::NewSeatFullSnapshot
                    | TransactionKind::NativeNewSeat
            )
        {
            if let Some(current) = load_seat(&tx, &operation.target.identity.seat)? {
                if current.identity != operation.target.identity {
                    return Err(Error::Fence);
                }
                tx.execute(
                    "DELETE FROM contract_seats WHERE seat=?1",
                    [operation.target.identity.seat.as_str()],
                )?;
            }
        } else if publish && !(rollback && operation.kind == TransactionKind::Restart) {
            tx.execute("INSERT INTO contract_seats VALUES(?1,?2) ON CONFLICT(seat) DO UPDATE SET record=excluded.record",
                params![operation.target.identity.seat.as_str(),serde_json::to_string(&operation.target)?])?;
        }
        let n = tx.execute(
            "UPDATE contract_operations SET record=?1 WHERE id=?2",
            params![serde_json::to_string(operation)?, operation.id.as_str()],
        )?;
        if n != 1 {
            return Err(Error::Fence);
        }
        if matches!(operation.outcome, Outcome::Committed | Outcome::Compensated) {
            tx.execute(
                "DELETE FROM contract_leases WHERE operation=?1",
                [operation.id.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn configure(connection: &Connection) -> Result<(), Error> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
    )?;
    Ok(())
}
fn validate_ancestors(path: &Path) -> Result<(), Error> {
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::Fence);
        }
    }
    Ok(())
}
pub(crate) fn load_seat(
    connection: &Connection,
    seat: &SeatId,
) -> Result<Option<SeatRecord>, Error> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT record FROM contract_seats WHERE seat=?1",
            [seat.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
}
pub(crate) fn current(
    connection: &Connection,
    scope: &ScopeId,
    identity: &InstanceIdentity,
) -> Result<SeatRecord, Error> {
    if &identity.scope != scope {
        return Err(Error::Fence);
    }
    let record = load_seat(connection, &identity.seat)?.ok_or(Error::Fence)?;
    if record.identity != *identity {
        return Err(Error::Fence);
    }
    Ok(record)
}
pub(crate) fn unleased(connection: &Connection, seat: &SeatId) -> Result<(), Error> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM contract_leases WHERE seat=?1)",
        [seat.as_str()],
        |r| r.get(0),
    )?;
    if exists {
        Err(Error::Conflict)
    } else {
        Ok(())
    }
}
pub(crate) fn next_id(connection: &Connection, prefix: &str) -> Result<String, Error> {
    let n: i64 = connection.query_row(
        "UPDATE contract_sequence SET value=value+1 WHERE id=1 RETURNING value",
        [],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}_{n}"))
}
