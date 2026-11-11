//! Positive seat ownership/liveness evidence shared by planning and locked start admission.
use super::LifecycleError;
use crate::state::selector::SelectedTeam;
use crate::transport::{PaneId, Target, Transport};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SeatObservation {
    Live,
    StoppedVerified,
    ProviderExited,
    Unstarted,
    NativeReconcile,
    Unknown,
}

fn refusal(message: impl Into<String>) -> LifecycleError {
    LifecycleError::RequirementUnmet(message.into())
}
fn checked<T, E: std::fmt::Display>(result: Result<T, E>) -> Result<T, LifecycleError> {
    result.map_err(|error| refusal(error.to_string()))
}
fn provider(row: &Value) -> &str {
    row.get("provider").and_then(Value::as_str).unwrap_or("pi")
}
fn same_provider(left: &str, right: &str) -> bool {
    match (
        crate::provider::wire::parse_provider(left),
        crate::provider::wire::parse_provider(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => crate::contract_runtime::registry::same_provider(left, right),
    }
}

#[cfg(unix)]
pub(crate) fn native_observation(
    status: team_agent_contract::orchestration::store::SeatStatus,
    process: team_agent_contract::host::process::ProcessState,
    root_status: Option<&str>,
) -> SeatObservation {
    use team_agent_contract::{host::process::ProcessState, orchestration::store::SeatStatus};
    match (status, process) {
        (SeatStatus::Ready, ProcessState::Alive) if root_status == Some("running") => {
            SeatObservation::Live
        }
        (SeatStatus::Ready, ProcessState::Exited) => SeatObservation::StoppedVerified,
        (SeatStatus::Stopped, ProcessState::Exited) if root_status == Some("stopped") => {
            SeatObservation::StoppedVerified
        }
        _ => SeatObservation::Unknown,
    }
}

fn shell_carrier(command: &str) -> bool {
    matches!(command, "sh" | "bash" | "zsh" | "fish")
}
pub(crate) fn surface_liveness(command: &str, native_frame: bool, live_child: bool) -> bool {
    !command.is_empty() && native_frame && (!shell_carrier(command) || live_child)
}
pub(crate) fn argv0_names(argv0: &str, native: crate::provider::Provider) -> bool {
    Path::new(argv0).file_name().and_then(|name| name.to_str())
        == Some(crate::provider::wire::command_name(native))
}
/// Native installers may exec a version-named file (Claude Code:
/// `~/.local/bin/claude` -> `versions/<semver>`), so tmux reports the version
/// as the pane command. The exact argv[0] of the pane process, or of a live
/// child under its carrier, still names the provider. Unobservable is not
/// positive evidence.
pub(crate) fn launched_as_provider(pid: u32, native: crate::provider::Provider) -> bool {
    let named = |pid: u32| {
        crate::platform::argv::argv_tokens(pid)
            .and_then(|argv| argv.into_iter().next())
            .is_some_and(|argv0| argv0_names(&argv0, native))
    };
    named(pid)
        || crate::platform::process::process_tree(pid)
            .unwrap_or_default()
            .into_iter()
            .any(|child| {
                child != pid && named(child) && crate::platform::process::pid_is_alive(child)
            })
}

/// Positive native/store or exact carrier+pane+provider observations only.
pub(crate) fn observe(
    selected: &SelectedTeam,
    id: &str,
    row: &Value,
) -> Result<SeatObservation, LifecycleError> {
    if !row.is_object()
        || row
            .get("provider")
            .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(refusal("invalid persisted provider metadata"));
    }
    #[cfg(unix)]
    if crate::contract_runtime::framework::is_contract(row)
        || crate::contract_runtime::registry::declared(provider(row)).is_some()
    {
        use team_agent_contract::{contract::types::SeatId, host::process::sample_process};
        if crate::contract_runtime::framework::is_paused_unstarted(row) {
            return Ok(SeatObservation::Unstarted);
        }
        if row.get("contract_instance").is_none_or(Value::is_null)
            && checked(crate::contract_runtime::backend::seat_authority_absent(
                &selected.run_workspace,
                &selected.team_key,
                &checked(SeatId::new(id))?,
            ))?
        {
            // A failed legacy migration may have written only the declaration.
            // Do not infer death from that provider string or from a missing PID.
            if let Some(pid) = super::carrier::pane_pid(row).filter(|pid| *pid > 1)
            {
                return Ok(if super::carrier::observe(row, pid).state.gone() {
                    SeatObservation::StoppedVerified
                } else {
                    SeatObservation::Unknown
                });
            }
            return Ok(
                if row.get("pane_pid").is_none_or(Value::is_null)
                    && row.get("status").and_then(Value::as_str) == Some("stopped")
                    && row.get("explicit_stop").and_then(Value::as_bool) == Some(true)
                {
                    SeatObservation::StoppedVerified
                } else {
                    SeatObservation::Unknown
                },
            );
        }
        if checked(
            crate::contract_runtime::backend::start_needs_reconciliation(
                &selected.run_workspace,
                &selected.team_key,
                &checked(SeatId::new(id))?,
                row,
            ),
        )? {
            return Ok(SeatObservation::NativeReconcile);
        }
        let backend = checked(crate::contract_runtime::backend::Backend::open_readonly(
            &selected.run_workspace,
            &selected.team_key,
        ))?;
        let seat = checked(backend.store.seat(&checked(SeatId::new(id))?))?
            .ok_or_else(|| refusal("missing canonical native seat"))?;
        if !same_provider(provider(row), seat.provider.as_str())
            || row.get("contract_scope").and_then(Value::as_str)
                != Some(seat.identity.scope.as_str())
            || row.get("contract_instance").and_then(Value::as_str)
                != Some(seat.identity.instance.as_str())
            || row.get("generation").and_then(Value::as_u64) != Some(seat.identity.generation.0)
        {
            return Ok(SeatObservation::Unknown);
        }
        let Some(target) = seat.physical.as_ref() else {
            return Ok(SeatObservation::Unknown);
        };
        if sample_process(&target.process) == team_agent_contract::host::process::ProcessState::Alive {
            checked(crate::contract_runtime::backend::validate_delivery_target(
                &backend.binding, &seat, std::time::Duration::from_secs(2),
            ))?;
            let transport = crate::transport_factory::bounded_endpoint_transport(
                target.endpoint.to_str().ok_or_else(|| refusal("non-UTF8 native endpoint"))?, std::time::Instant::now() + std::time::Duration::from_secs(2),
            );
            if checked(transport.provider_exit_status(&PaneId::new(&target.pane)))?.is_some() {
                return Ok(SeatObservation::ProviderExited);
            }
        }
        return Ok(native_observation(
            seat.status,
            sample_process(&target.process),
            row.get("status").and_then(Value::as_str),
        ));
    }
    let native = crate::provider::wire::parse_provider(provider(row))
        .ok_or_else(|| refusal("unknown provider"))?;
    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
    if !["running", "stopped", "dead"]
        .iter()
        .any(|known| status.eq_ignore_ascii_case(known))
    {
        return Ok(SeatObservation::Unknown);
    }
    let Some(pid) = super::carrier::pane_pid(row).filter(|pid| *pid > 1)
    else {
        return Ok(if status.eq_ignore_ascii_case("stopped")
            && row.get("pane_pid").is_none_or(Value::is_null)
            && row.get("pane_id").is_none_or(Value::is_null)
            && row.get("window").is_none_or(Value::is_null)
        { SeatObservation::StoppedVerified } else { SeatObservation::Unknown });
    };
    let carrier = super::carrier::observe(row, pid);
    if carrier.state.gone() {
        return Ok(SeatObservation::StoppedVerified);
    }
    if carrier.state != super::carrier::CarrierState::Possible
        || !status.eq_ignore_ascii_case("running") {
        return Ok(SeatObservation::Unknown);
    }
    #[cfg(unix)]
    let Some(carrier_birth) = carrier.birth else {
        return Ok(SeatObservation::Unknown);
    };
    let backend = checked(
        crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
            &selected.run_workspace,
            &selected.state,
        ),
    )?
    .backend;
    let panes = checked(backend.list_targets())?;
    let pane = row.get("pane_id").and_then(Value::as_str);
    let session = selected.state.get("session_name").and_then(Value::as_str);
    let window = row.get("window").and_then(Value::as_str);
    let matches: Vec<_> = panes
        .iter()
        .filter(|p| {
            Some(p.pane_id.as_str()) == pane
                && p.pane_pid == Some(pid)
                && Some(p.session.as_str()) == session
                && p.window_name.as_ref().map(|w| w.as_str()) == window
        })
        .collect();
    if matches.len() != 1 {
        return Ok(SeatObservation::Unknown);
    }
    let Some(session) = session else {
        return Ok(SeatObservation::Unknown);
    };
    let owner = checked(backend.session_owner(&crate::transport::SessionName::new(session)))?;
    let generation = crate::state::projection::session_generation(&selected.state);
    if owner.as_ref().is_none_or(|owner| {
        Path::new(&owner.workspace) != selected.run_workspace
            || owner.team != selected.team_key
            || Some(owner.generation.as_str()) != generation
    }) {
        return Ok(SeatObservation::Unknown);
    }
    // Consult the exit receipt only after exact pane/PID/session ownership.
    if checked(backend.provider_exit_status(&PaneId::new(pane.unwrap_or(""))))?.is_some() {
        #[cfg(unix)]
        if checked(team_agent_contract::host::process::process_birth_identity(pid))? != carrier_birth {
            return Ok(SeatObservation::Unknown);
        }
        return Ok(SeatObservation::ProviderExited);
    }
    let command = matches[0].current_command.as_deref().unwrap_or("");
    let positive_command = command == crate::provider::wire::command_name(native)
        || crate::leader::provider_attribution::command_matches_provider(native, command)
        // A shell-supervised worker retains its native argv[0] in this exact
        // pane's live process tree; shell-only/stale-screen carriers still fail.
        || (!command.is_empty() && launched_as_provider(pid, native));
    let positive_surface = if !positive_command && !command.is_empty() {
        let frame = checked(
            backend.observe_native_input(&Target::Pane(PaneId::new(pane.unwrap_or(""))), native),
        )?;
        // Original start/restart supervises the native child with a shell carrier.
        // A shell alone (including its inert post-exit tail) is never Live.
        let mut live_child = false;
        if frame.is_some() && shell_carrier(command) {
            for child in checked(crate::platform::process::process_tree(pid))? {
                if child != pid
                    && matches!(
                        checked(crate::platform::process::pid_liveness(child))?,
                        crate::platform::process::ProcessLiveness::Live
                    )
                {
                    live_child = true;
                    break;
                }
            }
        }
        surface_liveness(command, frame.is_some(), live_child)
    } else {
        false
    };
    if !positive_command && !positive_surface {
        return Ok(SeatObservation::Unknown);
    }
    #[cfg(unix)]
    if checked(team_agent_contract::host::process::process_birth_identity(
        pid,
    ))? != carrier_birth
    {
        return Ok(SeatObservation::Unknown);
    }
    Ok(SeatObservation::Live)
}
