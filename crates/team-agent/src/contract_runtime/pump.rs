//! One coordinator-owned consumer of the K3 outbox. No CLI send side channel,
//! implicit business retry, queue flush, or replay of uncertain messages.
use std::path::Path;
use std::time::Duration;
use serde_json::Value;
use team_agent_contract::contract::{delivery::InputSurface, probe::*, types::*};
use team_agent_contract::host::{clock::{Clock, RealClock}, process::{sample_process, ProcessState}};
use team_agent_contract::orchestration::{physical::*, store::*, supervisor::*};
use team_agent_contract::runtime::probes::*;
use team_agent_contract::kiro::{KIRO_DESCRIPTOR, interaction};
use super::{backend::{self, Backend, BackendError}, forward, framework};

fn protocol(backend: &mut Backend, seat: &SeatRecord, clock: &RealClock) -> Result<ProtocolSnapshot, BackendError> {
    let target = seat.physical.as_ref().ok_or(BackendError::Metadata)?;
    let active:Vec<_> = backend.store.connections(&seat.identity)?.into_iter().filter(|record| {
        !record.closed && record.native_process == target.process
            && sample_process(&record.process) == ProcessState::Alive
            && sample_process(&record.native_process) == ProcessState::Alive
    }).collect();
    if active.len() != 1 {
        return Ok(collect_protocol(target, &seat.identity.instance, &seat.server_key, &[], clock.now(), Duration::from_secs(2)));
    }
    let connection = &active[0];
    let facts = backend.store.connection_facts(&seat.identity, &connection.connection)?;
    let mut events = Vec::new();
    // T3a is connection state, sampled against BOTH captured process births.
    // Completed server writes are never promoted to client discovery.
    for (fact, protocol) in [("initialize_response_written", ProtocolFact::InitializeResponseWritten { server_instance:connection.connection.clone() }),
        ("tools_list_response_written", ProtocolFact::ToolsListResponseWritten { server_instance:connection.connection.clone() })] {
        if facts.iter().any(|value| value == fact) {
            events.push(ProtocolEvent { scope:target.scope(events.len() as u64, clock.now(), Duration::from_secs(2), "live-owned-stdio-state"), fact:protocol });
        }
    }
    Ok(collect_protocol(target, &connection.connection, &seat.server_key, &events, clock.now(), Duration::from_secs(2)))
}

fn deliver_one(backend: &mut Backend, seat: &SeatRecord) -> Result<(), BackendError> {
    if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) || !backend.store.has_queued_for(&seat.identity)? { return Ok(()); }
    let clock = RealClock::new();
    let adapter = backend::adapter(&backend.binding, &seat.identity)?;
    let hooks = adapter.hooks();
    let operation = OperationId::new(format!("deliver-{}", backend.store.propose_identity(&seat.identity.seat)?.instance.as_str()))?;
    let mut physical = PhysicalRuntime::new(Registration { descriptor:&KIRO_DESCRIPTOR, hooks:&hooks, controls:interaction::CONTROLS },
        backend::settings(&backend.binding), operation.clone(), &clock)?;
    physical.evidence = backend::policy_evidence(&seat.native, backend.binding.candidate_sha256);
    physical.protocol = Some(protocol(backend, seat, &clock)?);
    let sample = physical.readiness(seat)?;
    if !matches!(sample.pane.outcome, ProbeOutcome::Observed(ref pane) if pane.surface == InputSurface::ComposerReady)
        || !matches!(sample.server.outcome, ProbeOutcome::Observed(ref server) if server.initialize_response_written && server.tools_list_response_written) {
        return Ok(());
    }
    backend.store.begin_observation(&seat.identity, &operation)?;
    // An uncertain panel operation retains its exact seat lease. No retry and
    // no blind Escape to clear an unrelated approval/composer are permitted.
    let binding = physical.client_binding(seat, &interaction::TOOL_PANEL)?;
    backend.store.finish_observation(&seat.identity, &operation)?;
    if !matches!(binding.outcome, ProbeOutcome::Observed(_)) { return Ok(()); }
    let mut current_protocol = protocol(backend, seat, &clock)?;
    current_protocol.binding = binding;
    physical.protocol = Some(current_protocol);
    let mut bootstrap = OutboxBootstrap::new(ContractStore::open(&backend.binding.transport_root, backend.binding.scope.clone(), &backend.binding.endpoint)?);
    physical.bootstrap = Some(&mut bootstrap);
    let sample = physical.readiness(seat)?;
    let authorization = DeliveryAuthorization { allow_first_business_bootstrap:false, require_server_handshake:true,
        channel:Channel::Tmux, policy_sha256:interaction::POLICY };
    let _ = team_agent_contract::orchestration::supervisor::tick(&mut backend.store, &seat.identity, &sample, &authorization, &mut physical)?;
    Ok(())
}

pub fn tick(workspace: &Path, state: &mut Value) -> Result<(), BackendError> {
    if !framework::has_contract(state) { return Ok(()); }
    let team = crate::state::projection::team_state_key(state);
    let mut backend = Backend::open(workspace, &team)?;
    let ids:Vec<_> = state.get("agents").and_then(Value::as_object).into_iter().flat_map(|agents| agents.iter())
        .filter(|(_, agent)| framework::is_contract(agent)).map(|(id, _)| id.clone()).collect();
    let mut failure = None;
    for id in ids {
        let Some(seat) = backend.store.seat(&SeatId::new(&id)?)? else { continue; };
        if let Err(error) = deliver_one(&mut backend, &seat) { failure = Some(error); }
        if let Some(agent) = state.get_mut("agents").and_then(|agents| agents.get_mut(&id)) {
            match framework::project_seat(workspace, &team, &id) {
                Ok(projected) => *agent = projected,
                Err(error) => failure = Some(error),
            }
        }
    }
    // One blocked seat must not prevent already committed results from reaching
    // the shared framework task-finalization/leader-presentation path.
    forward::drain(&backend.binding.framework(), &mut backend.store, 32)?;
    match failure { Some(error) => Err(error), None => Ok(()) }
}
