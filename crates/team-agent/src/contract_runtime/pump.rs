//! One coordinator-owned consumer of the K3 outbox. No CLI send side channel,
//! implicit business retry, queue flush, or replay of uncertain messages.
use super::{
    backend::{self, Backend, BackendError},
    framework, registry,
};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use team_agent_contract::contract::{delivery::InputSurface, probe::*, types::*};
use team_agent_contract::host::{
    clock::{Clock, RealClock},
    process::verify_native_ancestry,
};
use team_agent_contract::orchestration::{physical::*, store::*, supervisor::*};
use team_agent_contract::runtime::probes::*;

pub(super) fn protocol(
    backend: &mut Backend,
    seat: &SeatRecord,
    clock: &RealClock,
) -> Result<ProtocolSnapshot, BackendError> {
    let target = seat.physical.as_ref().ok_or(BackendError::Metadata)?;
    let active: Vec<_> = backend
        .store
        .connections(&seat.identity)?
        .into_iter()
        .filter(|record| {
            !record.closed
                && record.identity == seat.identity
                && record.server_key == seat.server_key
                && record.binding_key == seat.binding_key
                && record.native_process == target.process
                && verify_native_ancestry(&record.process, &record.native_process).is_ok()
        })
        .collect();
    if active.len() != 1 {
        return Ok(collect_protocol(
            target,
            &seat.identity.instance,
            &seat.server_key,
            &[],
            clock.now(),
            Duration::from_secs(2),
        ));
    }
    let connection = &active[0];
    let facts = backend
        .store
        .connection_facts(&seat.identity, &connection.connection)?;
    let mut events = Vec::new();
    // T3a is connection state, sampled against BOTH captured process births.
    // Completed server writes are never promoted to client discovery.
    for (fact, protocol) in [
        (
            "initialize_response_written",
            ProtocolFact::InitializeResponseWritten {
                server_instance: connection.connection.clone(),
            },
        ),
        (
            "tools_list_response_written",
            ProtocolFact::ToolsListResponseWritten {
                server_instance: connection.connection.clone(),
            },
        ),
    ] {
        if facts.iter().any(|value| value == fact) {
            events.push(ProtocolEvent {
                scope: target.scope(
                    events.len() as u64,
                    clock.now(),
                    Duration::from_secs(2),
                    "live-owned-stdio-state",
                ),
                fact: protocol,
            });
        }
    }
    if registry::provider(seat.provider.as_str())?.uses_tool_roundtrips() {
        let session = seat.session.as_ref().ok_or(BackendError::Metadata)?;
        if target.identity() != &seat.identity || target.binding != seat.binding_key
            || target.provider != seat.provider || target.native != seat.native
            || target.native_session.as_ref() != Some(&session.native_session)
            || session.source != seat.identity || session.provider != seat.provider
            || session.native != seat.native || session.cwd != target.cwd
            || target.candidate_sha256 != backend.binding.candidate_sha256
        {
            return Err(ContractError::Mismatch("native roundtrip current binding").into());
        }
        // The core returns only per-tool invocation + actual successful
        // write/flush pairs for THIS captured connection and generation.
        // Server list writes or another connection's facts cannot qualify.
        let tools = backend.store.completed_connection_tools(&seat.identity, &connection.connection)?;
        if !tools.is_empty() {
            events.push(ProtocolEvent {
                scope: target.scope(events.len() as u64, clock.now(), Duration::from_secs(2),
                    "live-native-tool-roundtrip-ledger"),
                fact: ProtocolFact::ClientBound {
                    server_instance: connection.connection.clone(),
                    evidence: ClientBindingEvidence {
                        server_key: seat.server_key.clone(), tools,
                        source: ClientBindingSource::NativeToolRoundtrip,
                    },
                },
            });
        }
    }
    Ok(collect_protocol(
        target,
        &connection.connection,
        &seat.server_key,
        &events,
        clock.now(),
        Duration::from_secs(2),
    ))
}

// Messaging and result presentation are owned by station::send, not this
// lifecycle observer. No historical outbox item is replayed on a tick.
pub fn tick(workspace: &Path, state: &mut Value) -> Result<(), BackendError> {
    if !framework::has_contract(state) {
        return Ok(());
    }
    let team = crate::state::projection::team_state_key(state);
    let ids: Vec<_> = state
        .get("agents")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|agents| agents.iter())
        .filter(|(_, agent)| framework::is_contract(agent) && !framework::is_paused_unstarted(agent))
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let mut backend = Backend::open(workspace, &team)?;
    let mut failure = None;
    for id in ids {
        let Some(seat) = backend.store.seat(&SeatId::new(&id)?)? else {
            continue;
        };
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(|agents| agents.get_mut(&id))
        {
            match framework::project_seat(workspace, &team, &id) {
                Ok(projected) => {
                    let row = agent.as_object_mut().ok_or(BackendError::Metadata)?;
                    for (key, value) in projected.as_object().ok_or(BackendError::Metadata)? {
                        row.insert(key.clone(), value.clone());
                    }
                },
                Err(error) => failure = Some(error),
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
