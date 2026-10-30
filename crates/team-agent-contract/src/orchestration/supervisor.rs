//! Readiness evidence and explicitly scoped observation leases. The legacy
//! outbox executor is retired and refuses calls before any filesystem access.
use super::{
    projection::Key,
    store::{current, unleased, ContractStore, SeatRecord, SeatStatus},
    Error,
};
use crate::contract::{delivery::*, probe::*, types::*};
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
    _store: &mut ContractStore,
    _identity: &InstanceIdentity,
    _sample: &ReadinessSample,
    _authorization: &DeliveryAuthorization,
    _host: &mut dyn DeliveryHost,
) -> Result<Tick, Error> {
    Err(Error::Invalid("legacy outbox retired; use station send"))
}
pub struct OutboxBootstrap;
impl OutboxBootstrap {
    pub fn new(_store: ContractStore) -> Self {
        Self
    }
}
impl crate::runtime::delivery::BootstrapCommit for OutboxBootstrap {
    fn consume(
        &mut self,
        _owner: &InstanceIdentity,
        _attempt: &AttemptId,
        _clock: &dyn crate::host::clock::Clock,
        _deadline: Duration,
    ) -> Result<(), crate::host::HostError> {
        Err(crate::host::HostError::new(
            "legacy bootstrap retired",
            crate::host::HostErrorKind::Ownership,
        ))
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
    pub fn has_queued_for(&self, _identity: &InstanceIdentity) -> Result<bool, Error> {
        Ok(false)
    }
    pub fn begin_observation(
        &mut self,
        identity: &InstanceIdentity,
        operation: &OperationId,
    ) -> Result<(), Error> {
        let lease = Key::new("leases", identity.seat.as_str());
        let mut tx = self
            .files
            .write([lease.clone(), Key::new("seats", identity.seat.as_str())])?;
        current(&tx, self.scope(), identity)?;
        unleased(&tx, &identity.seat)?;
        tx.put(lease, operation)?;
        tx.commit()
    }
    pub fn finish_observation(
        &mut self,
        identity: &InstanceIdentity,
        operation: &OperationId,
    ) -> Result<(), Error> {
        self.end_observation(identity, operation, false)
    }
    pub fn fail_observation(
        &mut self,
        identity: &InstanceIdentity,
        operation: &OperationId,
    ) -> Result<(), Error> {
        self.end_observation(identity, operation, true)
    }
    fn end_observation(
        &mut self,
        identity: &InstanceIdentity,
        operation: &OperationId,
        uncertain: bool,
    ) -> Result<(), Error> {
        let lease = Key::new("leases", identity.seat.as_str());
        let seat_key = Key::new("seats", identity.seat.as_str());
        let mut tx = self.files.write([lease.clone(), seat_key.clone()])?;
        let mut seat = current(&tx, self.scope(), identity)?;
        if tx.get::<String>(&lease)?.as_deref() != Some(operation.as_str()) {
            return Err(Error::Fence);
        }
        tx.delete(lease)?;
        if uncertain {
            seat.status = SeatStatus::Unknown;
            tx.put(seat_key, &seat)?;
        }
        tx.commit()
    }
    /// The retired queue/result plane is empty; these are not C3 history views.
    pub fn deliveries(&self) -> Result<Vec<StoredDelivery>, Error> {
        Ok(Vec::new())
    }
    pub fn results(&self) -> Result<Vec<serde_json::Value>, Error> {
        Ok(Vec::new())
    }
    pub fn protocol_facts(&self) -> Result<Vec<String>, Error> {
        Ok(self
            .files
            .list::<super::mcp::FactRecord>("facts")?
            .into_iter()
            .flat_map(|r| r.facts)
            .collect())
    }
}
