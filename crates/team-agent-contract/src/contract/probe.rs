use std::path::PathBuf;
use std::time::Duration;

use super::delivery::InputSurface;
use super::session::NativeSessionId;
use super::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceScope {
    pub identity: InstanceIdentity,
    pub endpoint: String,
    pub pane: String,
    pub binding: String,
    pub session: Option<NativeSessionId>,
    pub sequence: u64,
    pub observed_at: Duration,
    pub valid_for: Duration,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContraryEvidence {
    ProcessExited { pid: u32, exit_code: Option<i32> },
    SurfaceBlocked(InputSurface),
    BindingRemoved { server_key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeOutcome<E> {
    Observed(E),
    Pending { deadline: Duration },
    Negative(ContraryEvidence),
    Unknown(Reason),
    Unsupported(Reason),
    TimedOut { last_observation: Option<String> },
    Error { code: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe<E> {
    pub scope: EvidenceScope,
    pub outcome: ProbeOutcome<E>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub birth_identity: String,
    pub executable: PathBuf,
    pub executable_sha256: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessAliveEvidence {
    pub process: ProcessIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneReadyEvidence {
    pub profile_id: String,
    pub surface: InputSurface,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerHandshakeEvidence {
    pub server_instance: InstanceId,
    pub initialize_response_written: bool,
    pub tools_list_response_written: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientBindingSource {
    NativeRegistry,
    NativeClientDiscovery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientBindingEvidence {
    pub server_key: String,
    pub tools: Vec<LogicalTool>,
    pub source: ClientBindingSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundTripFact {
    InvocationReceived,
    ResponseWritten,
    ClientConsumptionObserved,
    PresentationObserved,
}

/// Facts are a set, not a rank. ResponseWritten never implies client consumption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeRoundTripEvidence {
    pub message: Option<MessageId>,
    pub attempt: Option<AttemptId>,
    pub call_id: String,
    pub tool: LogicalTool,
    pub facts: Vec<RoundTripFact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticEvidence {
    Busy,
    Idle,
    Fault { code: String },
}

pub struct EvidenceExpectation<'a> {
    pub identity: &'a InstanceIdentity,
    pub endpoint: &'a str,
    pub pane: &'a str,
    pub binding: &'a str,
    pub session: Option<&'a NativeSessionId>,
    pub minimum_sequence: u64,
    pub now: Duration,
    pub process: &'a ProcessIdentity,
    pub profile_id: &'a str,
    pub server_key: &'a str,
}

impl EvidenceScope {
    pub fn is_current(&self, expected: &EvidenceExpectation<'_>) -> bool {
        &self.identity == expected.identity
            && self.endpoint == expected.endpoint
            && self.pane == expected.pane
            && self.binding == expected.binding
            && self.session.as_ref() == expected.session
            && self.sequence >= expected.minimum_sequence
            && nonblank(&self.endpoint)
            && nonblank(&self.pane)
            && nonblank(&self.binding)
            && nonblank(&self.source)
            && self.observed_at <= expected.now
            && expected.now - self.observed_at <= self.valid_for
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeLayer {
    Process,
    Pane,
    ClientBinding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateFailure {
    StaleOrMismatched,
    Pending,
    Negative,
    Unknown,
    Unsupported,
    TimedOut,
    Error,
    WrongProcess,
    NotComposer,
    WrongTools,
    NotBusinessOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateIssue {
    pub layer: ProbeLayer,
    pub failure: GateFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapAllowance {
    Disabled,
    AuthorizedFirstBusinessOnce { remaining: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendGate {
    ReadyForInput { consume_bootstrap: bool },
    Blocked(Vec<GateIssue>),
}

fn current<'a, E>(
    probe: &'a Probe<E>,
    expected: &EvidenceExpectation<'_>,
) -> Result<&'a E, GateFailure> {
    if !probe.scope.is_current(expected) {
        return Err(GateFailure::StaleOrMismatched);
    }
    match &probe.outcome {
        ProbeOutcome::Observed(evidence) => Ok(evidence),
        ProbeOutcome::Pending { .. } => Err(GateFailure::Pending),
        ProbeOutcome::Negative(_) => Err(GateFailure::Negative),
        ProbeOutcome::Unknown(_) => Err(GateFailure::Unknown),
        ProbeOutcome::Unsupported(_) => Err(GateFailure::Unsupported),
        ProbeOutcome::TimedOut { .. } => Err(GateFailure::TimedOut),
        ProbeOutcome::Error { .. } => Err(GateFailure::Error),
    }
}

/// Pure business-input eligibility; native controls/startup have separate operation guards.
/// Neither consumes a bootstrap allowance nor promotes any probe.
/// T3c is deliberately not an input: the first business message must not wait for its reply.
pub fn evaluate_send_gate(
    process: &Probe<ProcessAliveEvidence>,
    pane: &Probe<PaneReadyEvidence>,
    binding: &Probe<ClientBindingEvidence>,
    expected: &EvidenceExpectation<'_>,
    operation: Operation,
    bootstrap: BootstrapAllowance,
) -> SendGate {
    if !matches!(
        operation,
        Operation::FirstBusiness | Operation::OrdinarySend
    ) {
        return SendGate::Blocked(vec![GateIssue {
            layer: ProbeLayer::Pane,
            failure: GateFailure::NotBusinessOperation,
        }]);
    }
    let mut issues = Vec::new();
    let process_result = current(process, expected).and_then(|e| {
        if e.process == *expected.process
            && e.process.pid > 0
            && nonblank(&e.process.birth_identity)
            && e.process.executable.is_absolute()
        {
            Ok(())
        } else {
            Err(GateFailure::WrongProcess)
        }
    });
    if let Err(failure) = process_result {
        issues.push(GateIssue {
            layer: ProbeLayer::Process,
            failure,
        });
    }
    let pane_result = current(pane, expected).and_then(|e| {
        if e.profile_id == expected.profile_id && e.surface == InputSurface::ComposerReady {
            Ok(())
        } else {
            Err(GateFailure::NotComposer)
        }
    });
    if let Err(failure) = pane_result {
        issues.push(GateIssue {
            layer: ProbeLayer::Pane,
            failure,
        });
    }
    let binding_result = current(binding, expected).and_then(|e| {
        if e.server_key == expected.server_key
            && nonblank(&e.server_key)
            && e.tools.len() == TEAM_TOOLS.len()
            && TEAM_TOOLS.iter().all(|t| e.tools.contains(t))
        {
            Ok(())
        } else {
            Err(GateFailure::WrongTools)
        }
    });
    let consume_bootstrap = matches!(
        bootstrap,
        BootstrapAllowance::AuthorizedFirstBusinessOnce { remaining: 1 }
    ) && operation == Operation::FirstBusiness
        && matches!(
            binding_result,
            Err(GateFailure::Pending | GateFailure::Unknown)
        );
    if !consume_bootstrap {
        if let Err(failure) = binding_result {
            issues.push(GateIssue {
                layer: ProbeLayer::ClientBinding,
                failure,
            });
        }
    }
    if issues.is_empty() {
        SendGate::ReadyForInput { consume_bootstrap }
    } else {
        SendGate::Blocked(issues)
    }
}
