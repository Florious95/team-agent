use std::collections::BTreeMap;
use std::time::Duration;

use crate::contract::delivery::{CaptureFrame, InputSurface};
use crate::contract::hooks::InteractionHook;
use crate::contract::probe::*;
use crate::contract::types::*;
use crate::host::clock::Clock;
use crate::host::process::{sample_process, ProcessState};
use crate::host::transport::TargetReceipt;

const UNKNOWN: Reason = Reason { code: "no-current-evidence", message: "No current scoped evidence establishes this fact" };

pub fn collect_process(target: &TargetReceipt, clock: &dyn Clock, freshness: Duration) -> Probe<ProcessAliveEvidence> {
    let outcome = match sample_process(&target.process) {
        ProcessState::Alive => ProbeOutcome::Observed(ProcessAliveEvidence { process: target.process.identity.clone() }),
        ProcessState::Exited => ProbeOutcome::Negative(ContraryEvidence::ProcessExited { pid: target.process.identity.pid, exit_code: None }),
        ProcessState::Replaced => ProbeOutcome::Unknown(Reason { code: "process-replaced", message: "The PID no longer denotes the captured native instance" }),
        ProcessState::Unknown(error) => ProbeOutcome::Error { code: error.operation.to_string() },
    };
    Probe { scope: target.scope(0, clock.now(), freshness, "host-process-identity"), outcome }
}

pub fn collect_pane(frame: &CaptureFrame, interaction: &dyn InteractionHook) -> Probe<PaneReadyEvidence> {
    let observation = interaction.interpret(frame);
    let outcome = if observation.scope != frame.scope {
        ProbeOutcome::Unknown(Reason { code: "interaction-scope-mismatch", message: "H6 evidence must preserve the captured scope" })
    } else {
        ProbeOutcome::Observed(PaneReadyEvidence { profile_id: frame.profile_id.clone(), surface: observation.surface })
    };
    Probe { scope: frame.scope.clone(), outcome }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallIdentity {
    pub call_id: String,
    pub tool: LogicalTool,
    pub message: Option<MessageId>,
    pub attempt: Option<AttemptId>,
}

/// Framework event inputs. Configured is deliberately not a client-binding event.
/// Producers must carry captured scope/epoch; a marker without identity stays Unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolFact {
    Configured,
    InitializeResponseWritten { server_instance: InstanceId },
    ToolsListResponseWritten { server_instance: InstanceId },
    ServerExited { server_instance: InstanceId, pid: u32, exit_code: Option<i32> },
    ClientBound { server_instance: InstanceId, evidence: ClientBindingEvidence },
    ClientUnbound { server_instance: InstanceId, server_key: String },
    Call { server_instance: InstanceId, identity: CallIdentity, fact: RoundTripFact },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolEvent { pub scope: EvidenceScope, pub fact: ProtocolFact }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolSnapshot {
    pub server: Probe<ServerHandshakeEvidence>,
    pub binding: Probe<ClientBindingEvidence>,
    pub calls: Vec<Probe<NativeRoundTripEvidence>>,
}

fn same_subject(scope: &EvidenceScope, target: &TargetReceipt) -> bool {
    scope.identity == *target.identity() && scope.endpoint == target.endpoint.to_string_lossy()
        && scope.pane == target.pane && scope.binding == target.binding && scope.session == target.native_session
        && !scope.source.trim().is_empty()
}

fn fresh(scope: &EvidenceScope, now: Duration) -> bool {
    scope.observed_at <= now && now - scope.observed_at <= scope.valid_for
}

/// Bounded caller-supplied event window, not a hidden reader of configuration, HOME or DB.
/// Facts do not imply each other; an uncorrelated call remains uncorrelated.
pub fn collect_protocol(
    target: &TargetReceipt, server_instance: &InstanceId, server_key: &str,
    events: &[ProtocolEvent], now: Duration, freshness: Duration,
) -> ProtocolSnapshot {
    let empty_scope = target.scope(0, now, freshness, "protocol-collector");
    let mut server = Probe { scope: empty_scope.clone(), outcome: ProbeOutcome::Unknown(UNKNOWN) };
    let mut binding = Probe { scope: empty_scope, outcome: ProbeOutcome::Unknown(UNKNOWN) };
    let mut initialized = false;
    let mut listed = false;
    let mut exited = false;
    let mut last_unbound = None;
    let mut relevant: Vec<_> = events.iter().filter(|event| same_subject(&event.scope, target) && event.scope.observed_at <= now).collect();
    relevant.sort_by_key(|event| event.scope.sequence);
    let mut calls: BTreeMap<String, (CallIdentity, Vec<RoundTripFact>, EvidenceScope, bool)> = BTreeMap::new();
    for event in relevant {
        match &event.fact {
            ProtocolFact::Configured => {},
            ProtocolFact::ServerExited { server_instance: id, pid, exit_code } if id == server_instance => {
                // An exit for this exact server instance remains true; TTL expiry must not revive it.
                exited = true;
                server = Probe { scope: target.scope(event.scope.sequence, now, freshness, "latched-server-exit"),
                    outcome: ProbeOutcome::Negative(ContraryEvidence::ProcessExited { pid: *pid, exit_code: *exit_code }) };
            }
            ProtocolFact::InitializeResponseWritten { server_instance: id } if id == server_instance && !exited => {
                if fresh(&event.scope, now) { initialized = true; server.scope = event.scope.clone(); }
            }
            ProtocolFact::ToolsListResponseWritten { server_instance: id } if id == server_instance && !exited => {
                if fresh(&event.scope, now) { listed = true; server.scope = event.scope.clone(); }
            }
            ProtocolFact::ClientBound { server_instance: id, evidence } if id == server_instance
                && evidence.server_key == server_key && last_unbound.is_none_or(|sequence| event.scope.sequence > sequence) => {
                binding = Probe { scope: event.scope.clone(), outcome: if fresh(&event.scope, now) {
                    ProbeOutcome::Observed(evidence.clone())
                } else { ProbeOutcome::Unknown(UNKNOWN) } };
            }
            ProtocolFact::ClientUnbound { server_instance: id, server_key: key } if id == server_instance && key == server_key => {
                last_unbound = Some(event.scope.sequence);
                binding = Probe { scope: target.scope(event.scope.sequence, now, freshness, "latched-client-unbind"),
                    outcome: ProbeOutcome::Negative(ContraryEvidence::BindingRemoved { server_key: key.clone() }) };
            }
            ProtocolFact::Call { server_instance: id, identity, fact } if id == server_instance && fresh(&event.scope, now) && !identity.call_id.is_empty() => {
                let entry = calls.entry(identity.call_id.clone()).or_insert_with(|| (identity.clone(), Vec::new(), event.scope.clone(), false));
                if entry.0 != *identity { entry.3 = true; }
                if !entry.1.contains(fact) { entry.1.push(*fact); }
                entry.2 = event.scope.clone();
            }
            _ => {},
        }
    }
    if !exited && (initialized || listed) {
        server.outcome = ProbeOutcome::Observed(ServerHandshakeEvidence { server_instance: server_instance.clone(), initialize_response_written: initialized, tools_list_response_written: listed });
    }
    let calls = calls.into_values().map(|(identity, facts, scope, conflict)| Probe {
        scope, outcome: if conflict { ProbeOutcome::Unknown(Reason { code: "call-correlation-conflict", message: "Call identity changed; no latest-message inference is permitted" }) }
        else { ProbeOutcome::Observed(NativeRoundTripEvidence { message: identity.message, attempt: identity.attempt, call_id: identity.call_id, tool: identity.tool, facts }) },
    }).collect();
    ProtocolSnapshot { server, binding, calls }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolRequirement { ClientBinding, ServerAndClientBinding }

/// Additional protocol/startup checks supplement K1's business gate; they do not mutate facts.
pub fn evaluate_protocol_gate(
    process: &Probe<ProcessAliveEvidence>, pane: &Probe<PaneReadyEvidence>, protocol: &ProtocolSnapshot,
    expected: &EvidenceExpectation<'_>, operation: Operation, policy: ProtocolRequirement, bootstrap: BootstrapAllowance,
) -> SendGate {
    let server_current = protocol.server.scope.is_current(expected);
    if server_current && matches!(protocol.server.outcome, ProbeOutcome::Negative(_)) {
        return SendGate::Blocked(vec![GateIssue { layer: ProbeLayer::ClientBinding, failure: GateFailure::Negative }]);
    }
    if policy == ProtocolRequirement::ServerAndClientBinding {
        let ready = server_current && matches!(&protocol.server.outcome,
            ProbeOutcome::Observed(evidence) if evidence.initialize_response_written && evidence.tools_list_response_written);
        if !ready { return SendGate::Blocked(vec![GateIssue { layer: ProbeLayer::ClientBinding, failure: GateFailure::Unknown }]); }
    }
    evaluate_send_gate(process, pane, &protocol.binding, expected, operation, bootstrap)
}

pub fn control_surface_allowed(operation: Operation, surface: InputSurface) -> bool {
    match operation {
        Operation::StartupBypassAck => surface == InputSurface::StartupRiskWarning,
        Operation::SessionInspect | Operation::InWindowBranch | Operation::Stop | Operation::Shutdown => surface == InputSurface::ComposerReady,
        _ => false,
    }
}
