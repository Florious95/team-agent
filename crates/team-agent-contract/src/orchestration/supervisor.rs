//! One shared durable outbox consumer. It schedules work; the physical executor
//! owns all key counts, timing, native queue decisions and effect journaling.
use super::store::{current, next_id, unleased, ContractStore, SeatRecord, SeatStatus};
use super::Error;
use crate::contract::delivery::*;
use crate::contract::probe::*;
use crate::contract::types::*;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::time::Duration;

pub struct ReadinessSample {
    pub process: Probe<ProcessAliveEvidence>,
    pub pane: Probe<PaneReadyEvidence>,
    pub binding: Probe<ClientBindingEvidence>,
    pub server: Probe<ServerHandshakeEvidence>,
    pub minimum_sequence: u64,
    pub now: Duration,
}

pub struct DeliveryJob {
    pub target: SeatRecord,
    pub envelope: LogicalEnvelope,
    pub attempt: AttemptId,
    pub operation: Operation,
    pub channel: Channel,
    pub policy_sha256: Digest,
}

/// A framework port into K2, not an adapter hook. Implementations must recheck
/// live evidence at the executor boundary and return its scoped typed receipt.
pub trait DeliveryHost {
    fn deliver(&mut self, job: &DeliveryJob) -> Result<DeliveryReceipt, Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tick {
    Empty,
    Blocked,
    Recorded {
        message: MessageId,
        effect: DeliveryEffect,
        uncertain: bool,
    },
}

pub struct DeliveryAuthorization {
    pub allow_first_business_bootstrap: bool,
    pub require_server_handshake: bool,
    pub channel: Channel,
    pub policy_sha256: Digest,
}

pub fn evaluate_readiness(
    seat: &SeatRecord,
    sample: &ReadinessSample,
    authorization: &DeliveryAuthorization,
) -> Result<SendGate, Error> {
    let process = seat
        .process
        .as_ref()
        .ok_or(Error::Invalid("process receipt missing"))?;
    let expected = EvidenceExpectation {
        identity: &seat.identity,
        endpoint: &seat.endpoint,
        pane: &seat.pane,
        binding: &seat.binding_key,
        session: seat.session.as_ref().map(|s| &s.native_session),
        minimum_sequence: sample.minimum_sequence,
        now: sample.now,
        process,
        profile_id: &seat.profile,
        server_key: &seat.server_key,
    };
    if authorization.require_server_handshake
        && (!sample.server.scope.is_current(&expected)
            || !matches!(&sample.server.outcome,ProbeOutcome::Observed(e) if e.initialize_response_written && e.tools_list_response_written))
    {
        return Ok(SendGate::Blocked(vec![]));
    }
    // A current contrary server fact blocks even when positive T3a is optional.
    if sample.server.scope.is_current(&expected)
        && matches!(sample.server.outcome, ProbeOutcome::Negative(_))
    {
        return Ok(SendGate::Blocked(vec![]));
    }
    Ok(evaluate_send_gate(
        &sample.process,
        &sample.pane,
        &sample.binding,
        &expected,
        if seat.bootstrap_used {
            Operation::OrdinarySend
        } else {
            Operation::FirstBusiness
        },
        if authorization.allow_first_business_bootstrap && !seat.bootstrap_used {
            BootstrapAllowance::AuthorizedFirstBusinessOnce { remaining: 1 }
        } else {
            BootstrapAllowance::Disabled
        },
    ))
}

pub fn tick(
    store: &mut ContractStore,
    identity: &InstanceIdentity,
    sample: &ReadinessSample,
    authorization: &DeliveryAuthorization,
    host: &mut dyn DeliveryHost,
) -> Result<Tick, Error> {
    let seat = store.assert_current(identity)?;
    if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
        return Ok(Tick::Blocked);
    }
    if !matches!(
        evaluate_readiness(&seat, sample, authorization)?,
        SendGate::ReadyForInput { .. }
    ) {
        return Ok(Tick::Blocked);
    }
    let scope = store.scope().clone();
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut target = current(&tx, &scope, identity)?;
    if target != seat {
        return Err(Error::Fence);
    }
    unleased(&tx, &identity.seat)?;
    let encoded_identity = serde_json::to_string(identity)?;
    let next:Option<(String,String,String,Option<String>)> = tx.query_row(
        "SELECT m.message_id,m.sender,m.content,m.task_id FROM messages m JOIN contract_outbox o USING(message_id)
         WHERE m.owner_team_id=?1 AND m.recipient=?2 AND o.state='queued' AND (o.target=?3 OR o.target='null') ORDER BY m.rowid LIMIT 1",
        params![scope.as_str(),identity.seat.as_str(),encoded_identity],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((id, sender, content, task)) = next else {
        return Ok(Tick::Empty);
    };
    let attempt = AttemptId::new(next_id(&tx, "attempt")?)?;
    let operation = if target.bootstrap_used {
        Operation::OrdinarySend
    } else {
        Operation::FirstBusiness
    };
    if operation == Operation::FirstBusiness {
        tx.execute(
            "INSERT INTO contract_bootstrap VALUES(?1,?2,0)",
            params![encoded_identity, attempt.as_str()],
        )?;
    }
    // Admission + bootstrap consumption + serialized target lease + write-ahead
    // physical intent are one commit. Losing contenders never call the host.
    target.bootstrap_used = true;
    target.status = SeatStatus::Ready;
    tx.execute(
        "UPDATE contract_seats SET record=?1 WHERE seat=?2",
        params![serde_json::to_string(&target)?, identity.seat.as_str()],
    )?;
    tx.execute(
        "INSERT INTO contract_leases VALUES(?1,?2)",
        params![identity.seat.as_str(), attempt.as_str()],
    )?;
    tx.execute("UPDATE contract_outbox SET target=?1,state='in_flight',attempt=?2,effect='\"MayHaveSubmitted\"' WHERE message_id=?3 AND state='queued'",
        params![encoded_identity,attempt.as_str(),id])?;
    tx.execute("UPDATE messages SET status='target_resolved',delivery_attempts=delivery_attempts+1 WHERE message_id=?1",[&id])?;
    tx.commit()?;
    let job = DeliveryJob {
        target,
        envelope: LogicalEnvelope {
            message: MessageId::new(&id)?,
            sender: SeatId::new(sender)?,
            task,
            content,
        },
        attempt,
        operation,
        channel: authorization.channel,
        policy_sha256: authorization.policy_sha256,
    };
    let receipt = host.deliver(&job);
    let (effect, uncertain) = match receipt {
        Ok(receipt)
            if receipt.identity == *identity
                && receipt.message == job.envelope.message
                && receipt.attempt == job.attempt
                && receipt.operation == job.operation
                && receipt.channel == job.channel
                && receipt.policy_sha256 == job.policy_sha256 =>
        {
            (
                DeliveryEffect::MayHaveSubmitted.retain_floor(receipt.effect_floor),
                receipt.failure.is_some()
                    || receipt.persistence != PersistenceState::Durable
                    || receipt.effect_floor != DeliveryEffect::Submitted,
            )
        }
        _ => (DeliveryEffect::MayHaveSubmitted, true),
    };
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    current(&tx, &scope, identity)?;
    let n = tx.execute("UPDATE contract_outbox SET state=?1,effect=?2 WHERE message_id=?3 AND attempt=?4 AND state='in_flight' AND target=?5",
        params![if uncertain {"uncertain"} else {"submitted"},serde_json::to_string(&effect)?,id,job.attempt.as_str(),encoded_identity])?;
    if n != 1 {
        return Err(Error::Fence);
    }
    tx.execute(
        "UPDATE messages SET status=?1 WHERE message_id=?2",
        params![
            if uncertain {
                "submitted_unverified"
            } else {
                "submitted_pending_acceptance"
            },
            id
        ],
    )?;
    if !uncertain {
        tx.execute(
            "DELETE FROM contract_leases WHERE seat=?1 AND operation=?2",
            params![identity.seat.as_str(), job.attempt.as_str()],
        )?;
    }
    // Uncertain effects quarantine the target; no automatic replay or queue flush.
    tx.commit()?;
    Ok(Tick::Recorded {
        message: job.envelope.message,
        effect,
        uncertain,
    })
}

/// The physical executor's one-use confirmation of K3's already durable claim.
/// Owns another connection to the same scoped database, never a second queue.
pub struct OutboxBootstrap {
    store: ContractStore,
}
impl OutboxBootstrap {
    pub fn new(store: ContractStore) -> Self {
        Self { store }
    }
    fn consume_claim(
        &mut self,
        owner: &InstanceIdentity,
        attempt: &AttemptId,
    ) -> Result<(), Error> {
        let scope = self.store.scope().clone();
        let tx = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seat = current(&tx, &scope, owner)?;
        if !seat.bootstrap_used {
            return Err(Error::Fence);
        }
        let lease: Option<String> = tx
            .query_row(
                "SELECT operation FROM contract_leases WHERE seat=?1",
                [owner.seat.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if lease.as_deref() != Some(attempt.as_str()) {
            return Err(Error::Fence);
        }
        let target = serde_json::to_string(owner)?;
        let count: u64 = tx.query_row("SELECT COUNT(*) FROM contract_outbox WHERE target=?1 AND attempt=?2 AND state='in_flight'", params![target, attempt.as_str()], |r| r.get(0))?;
        if count != 1 {
            return Err(Error::Fence);
        }
        let confirmed = tx.execute("UPDATE contract_bootstrap SET confirmed=1 WHERE owner=?1 AND attempt=?2 AND confirmed=0", params![target, attempt.as_str()])?;
        if confirmed != 1 {
            return Err(Error::Conflict);
        }
        tx.commit()?;
        Ok(())
    }
}
impl crate::runtime::delivery::BootstrapCommit for OutboxBootstrap {
    fn consume(
        &mut self,
        owner: &InstanceIdentity,
        attempt: &AttemptId,
        clock: &dyn crate::host::clock::Clock,
        deadline: Duration,
    ) -> Result<(), crate::host::HostError> {
        use crate::host::{HostError, HostErrorKind};
        let budget = crate::host::clock::remaining(clock, deadline)
            .ok_or_else(|| HostError::new("bootstrap deadline", HostErrorKind::Deadline))?;
        self.store
            .connection
            .busy_timeout(budget.min(Duration::from_secs(5)))
            .map_err(|_| HostError::new("bootstrap store wait", HostErrorKind::Unknown))?;
        self.consume_claim(owner, attempt)
            .map_err(|_| HostError::new("bootstrap claim fence", HostErrorKind::Ownership))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDelivery {
    pub message: MessageId,
    pub state: String,
    pub attempt: Option<AttemptId>,
    pub effect: DeliveryEffect,
}
impl ContractStore {
    pub fn has_queued_for(&self, identity: &InstanceIdentity) -> Result<bool, Error> {
        self.assert_current(identity)?;
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM contract_outbox WHERE state='queued' AND target=?1)",
            [serde_json::to_string(identity)?], |row| row.get(0))?)
    }
    /// Registry inspection shares the same seat lease as business delivery and
    /// lifecycle. An interrupted inspection is fenced, never silently replayed.
    pub fn begin_observation(&mut self, identity: &InstanceIdentity, operation: &OperationId) -> Result<(), Error> {
        let scope = self.scope().clone();
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, &scope, identity)?;
        unleased(&tx, &identity.seat)?;
        tx.execute("INSERT INTO contract_leases VALUES(?1,?2)", params![identity.seat.as_str(), operation.as_str()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn finish_observation(&mut self, identity: &InstanceIdentity, operation: &OperationId) -> Result<(), Error> {
        let scope = self.scope().clone();
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, &scope, identity)?;
        if tx.execute("DELETE FROM contract_leases WHERE seat=?1 AND operation=?2", params![identity.seat.as_str(), operation.as_str()])? != 1 { return Err(Error::Fence); }
        tx.commit()?;
        Ok(())
    }
    pub fn deliveries(&self) -> Result<Vec<StoredDelivery>, Error> {
        let mut stmt = self.connection.prepare(
            "SELECT message_id,state,attempt,effect FROM contract_outbox ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, state, attempt, effect) = row?;
            Ok(StoredDelivery {
                message: MessageId::new(id)?,
                state,
                attempt: attempt.map(AttemptId::new).transpose()?,
                effect: serde_json::from_str(&effect)?,
            })
        })
        .collect()
    }
    pub fn results(&self) -> Result<Vec<serde_json::Value>, Error> {
        let mut stmt = self
            .connection
            .prepare("SELECT envelope FROM results ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn protocol_facts(&self) -> Result<Vec<String>, Error> {
        let mut stmt = self
            .connection
            .prepare("SELECT fact FROM contract_facts ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
