//! Owned native stdio entry. Invocation arguments identify an expected record;
//! only actual executable/birth/parent and current instance checks authorize it.
use std::time::{Duration, Instant};
use team_agent_contract::contract::types::*;
use team_agent_contract::host::process::{capture_process, sample_process, ProcessState};
use team_agent_contract::orchestration::{
    mcp::*,
    protocol::ConnectionRecord,
    store::{ContractStore, SeatRecord, SeatStatus},
    Error,
};

use super::backend::{Backend, BackendError};

fn arguments(
    args: &[String],
) -> Result<(std::path::PathBuf, String, SeatId, InstanceId, Generation), BackendError> {
    let mut values = std::collections::BTreeMap::new();
    if !args.len().is_multiple_of(2) {
        return Err(BackendError::Metadata);
    }
    for pair in args.chunks_exact(2) {
        if !matches!(
            pair[0].as_str(),
            "--workspace" | "--team" | "--seat" | "--instance" | "--generation"
        ) || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(BackendError::Metadata);
        }
    }
    if values.len() != 5 {
        return Err(BackendError::Metadata);
    }
    let get = |key| values.get(key).copied().ok_or(BackendError::Metadata);
    Ok((
        get("--workspace")?.into(),
        get("--team")?.into(),
        SeatId::new(get("--seat")?)?,
        InstanceId::new(get("--instance")?)?,
        Generation(
            get("--generation")?
                .parse()
                .map_err(|_| BackendError::Metadata)?,
        ),
    ))
}
fn wait_target(store: &ContractStore, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let seat = store.assert_current(identity)?;
        if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
            return Err(Error::Fence);
        }
        if seat.physical.is_some() {
            return Ok(seat);
        }
        if Instant::now() >= deadline {
            return Err(Error::Host("native target publication timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn run(args: &[String]) -> Result<(), BackendError> {
    let (workspace, team, seat, instance, generation) = arguments(args)?;
    let mut backend = Backend::open(&workspace, &team)?;
    let identity = InstanceIdentity {
        scope: backend.binding.scope.clone(),
        seat,
        instance,
        generation,
    };
    let seat = wait_target(&backend.store, &identity)?;
    let target = seat
        .physical
        .as_ref()
        .ok_or(Error::Invalid("native target missing"))?;
    let process = capture_process(
        std::process::id(),
        &backend.binding.candidate,
        backend.binding.candidate_sha256,
        Duration::from_secs(10),
    )?;
    if process.parent != target.process.identity.pid
        || sample_process(&target.process) != ProcessState::Alive
    {
        return Err(Error::Fence.into());
    }
    let connection = backend.store.propose_identity(&identity.seat)?.instance;
    let record = ConnectionRecord {
        identity: identity.clone(),
        connection: connection.clone(),
        server_key: seat.server_key.clone(),
        binding_key: seat.binding_key.clone(),
        process,
        native_process: target.process.clone(),
        closed: false,
    };
    backend.store.register_connection(&record)?;
    let result = serve_with_context(
        &mut backend.store,
        |store, request| {
            if sample_process(&record.process) != ProcessState::Alive
                || sample_process(&record.native_process) != ProcessState::Alive
            {
                return Err(Error::Fence);
            }
            let current = store.assert_current(&identity)?;
            if current.binding_key != record.binding_key
                || matches!(current.status, SeatStatus::Stopped | SeatStatus::Unknown)
                || current
                    .physical
                    .as_ref()
                    .is_none_or(|target| target.process != record.native_process)
            {
                return Err(Error::Fence);
            }
            let business =
                request.get("method").and_then(serde_json::Value::as_str) == Some("tools/call");
            let mut task = String::new();
            if business {
                // A native callback can race the physical receipt's DB commit. Wait
                // for that exact generation's submitted turn, never a queued row.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match store.submitted_task(&identity) {
                        Ok(Some(bound)) => {
                            task = bound;
                            break;
                        }
                        Ok(None) | Err(Error::Conflict) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        Ok(None) => return Err(Error::Invalid("no submitted native turn")),
                        Err(error) => return Err(error),
                    }
                }
            }
            if sample_process(&record.process) != ProcessState::Alive
                || sample_process(&record.native_process) != ProcessState::Alive
            {
                return Err(Error::Fence);
            }
            Ok(CallContext {
                identity: identity.clone(),
                binding_key: record.binding_key.clone(),
                connection_id: connection.clone(),
                task_id: task,
            })
        },
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    );
    let closed = backend.store.close_connection(&identity, &connection);
    result?;
    closed?;
    Ok(())
}
