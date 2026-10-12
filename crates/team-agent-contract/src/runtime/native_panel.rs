//! Native modal inspection and exactly one guarded, journaled Escape. A session
//! panel is not a tool registry; both reuse the same owned close/observe operator.
use super::journal::*;
use crate::contract::{delivery::*, hooks::InteractionHook, probe::*, types::*};
use crate::host::{clock::Clock, digest, transport::*, HostError, HostErrorKind};
use std::time::Duration;

pub struct NativePanelPolicy {
    pub profile_id: &'static str,
    pub policy_sha256: Digest,
    pub panel_predicates: &'static [&'static str],
    pub bound_predicate: &'static str,
    pub server_key: &'static str,
}
pub struct SessionPanelPolicy {
    pub profile_id: &'static str,
    pub policy_sha256: Digest,
    pub predicate: &'static str,
}
pub struct PanelRequest<'a> {
    pub target: &'a TargetReceipt,
    pub interaction: &'a dyn InteractionHook,
    pub policy: &'a NativePanelPolicy,
    pub operation: &'a OperationId,
    pub clock: &'a dyn Clock,
    pub deadline: Duration,
    pub freshness: Duration,
}
pub struct SessionPanelRequest<'a> {
    pub target: &'a TargetReceipt,
    pub interaction: &'a dyn InteractionHook,
    pub policy: &'a SessionPanelPolicy,
    pub operation: &'a OperationId,
    pub clock: &'a dyn Clock,
    pub deadline: Duration,
    pub freshness: Duration,
}
struct Close<'a> {
    target: &'a TargetReceipt,
    interaction: &'a dyn InteractionHook,
    profile: &'a str,
    hash: Digest,
    operation: &'a OperationId,
    kind: Operation,
    clock: &'a dyn Clock,
    deadline: Duration,
    freshness: Duration,
}
impl Close<'_> {
    fn capture(&self, host: &mut dyn PhysicalTransport) -> Result<CaptureFrame, HostError> {
        let capture = host.capture(self.target, self.clock, self.deadline, self.freshness)?;
        if capture.mode != PaneMode::Normal { return Err(invalid()); }
        Ok(CaptureFrame { scope: capture.scope, text: capture.text, cwd: Some(self.target.cwd.path.clone()), input: None,
            baseline: None, profile_id: self.profile.into(), operation: self.kind, message: None, attempt: None,
            after_step: None, paste_latch: PasteLatch::NeverSeen })
    }
    /// Private operator: caller holds the exact native lane and has validated the
    /// current H6 panel predicate. No retry and no business submission.
    fn once(&self, host: &mut dyn PhysicalTransport, frame: &CaptureFrame, surface: InputSurface, journal: &mut dyn AttemptJournal) -> Result<(), HostError> {
        let attempt = AttemptId::new(format!("panel-close-{}", self.operation.as_str())).map_err(|_| invalid())?;
        journal.begin(&AttemptMetadata { owner: self.target.identity().clone(), attempt,
            correlation: Correlation::NativeControl(self.operation.clone()), operation: self.kind,
            policy_sha256: self.hash, payload_sha256: digest(&[]), payload_bytes: 0 })?;
        let record = |kind, ordinal, outcome| JournalRecord {
            kind, ordinal, at: self.clock.now(), effect: DeliveryEffect::NoEffect, step: None, outcome,
            sequence: Some(frame.scope.sequence), surface: Some(surface),
            code: Some(if self.kind == Operation::ToolInspect { "registry-panel-escape-once" } else { "session-panel-escape-once" }), control_paste: None,
        };
        journal.append(&record(JournalKind::ActionIntent, 0, None))?;
        let action = host.key(self.target, PhysicalKey::Escape, self.clock, self.deadline);
        journal.append(&record(JournalKind::ActionResult, 1, Some(action.outcome)))?;
        if action.outcome != StepOutcome::Confirmed || action.error.is_some() { return Err(invalid()); }
        loop {
            let closed = self.capture(host)?;
            let observed = self.interaction.interpret(&closed);
            if observed.scope != closed.scope { return Err(invalid()); }
            if observed.surface == InputSurface::ComposerReady { break; }
            if self.clock.now() >= self.deadline { return Err(invalid()); }
            self.clock.sleep(Duration::from_millis(100));
        }
        journal.append(&record(JournalKind::Complete, 2, None))?;
        Ok(())
    }
}
fn invalid() -> HostError { HostError::new("native panel unverified", HostErrorKind::Unknown) }
pub fn capture_and_close(host: &mut dyn PhysicalTransport, request: PanelRequest<'_>, journal: &mut dyn AttemptJournal) -> Result<Probe<ClientBindingEvidence>, HostError> {
    let PanelRequest { target, interaction, policy, operation, clock, deadline, freshness } = request;
    let _lane = host.acquire_lane(target)?;
    let close = Close { target, interaction, profile: policy.profile_id, hash: policy.policy_sha256,
        operation, kind: Operation::ToolInspect, clock, deadline, freshness };
    let frame = close.capture(host)?;
    let observation = interaction.interpret(&frame);
    if observation.scope != frame.scope || !observation.predicate.as_deref().is_some_and(|predicate| policy.panel_predicates.contains(&predicate)) { return Err(invalid()); }
    let bound = observation.predicate.as_deref() == Some(policy.bound_predicate);
    close.once(host, &frame, observation.surface, journal)?;
    Ok(Probe { scope: frame.scope, outcome: if bound {
        ProbeOutcome::Observed(ClientBindingEvidence { server_key: policy.server_key.into(), tools: TEAM_TOOLS.to_vec(), source: ClientBindingSource::NativeRegistry })
    } else { ProbeOutcome::Unknown(Reason { code: "native-registry-incomplete", message: "The current native panel did not expose all three tools from the bound server" }) } })
}
/// Called only after H4 has validated the captured session ID and exact cwd.
/// Revalidate the panel under the lane immediately before sending Escape.
pub fn close_session_panel(host: &mut dyn PhysicalTransport, request: SessionPanelRequest<'_>, journal: &mut dyn AttemptJournal) -> Result<(), HostError> {
    let SessionPanelRequest { target, interaction, policy, operation, clock, deadline, freshness } = request;
    let _lane = host.acquire_lane(target)?;
    let close = Close { target, interaction, profile: policy.profile_id, hash: policy.policy_sha256,
        operation, kind: Operation::SessionInspect, clock, deadline, freshness };
    let frame = close.capture(host)?;
    let observation = interaction.interpret(&frame);
    if observation.scope != frame.scope || observation.predicate.as_deref() != Some(policy.predicate) { return Err(invalid()); }
    close.once(host, &frame, observation.surface, journal)
}
