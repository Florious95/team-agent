//! Bounded read-only native registry observation. The registered H6 parser owns
//! all screen grammar. Closing the inspected panel is exactly one guarded Escape,
//! separately journaled; it never retries or submits a business message.
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

pub struct PanelRequest<'a> {
    pub target: &'a TargetReceipt,
    pub interaction: &'a dyn InteractionHook,
    pub policy: &'a NativePanelPolicy,
    pub operation: &'a OperationId,
    pub clock: &'a dyn Clock,
    pub deadline: Duration,
    pub freshness: Duration,
}
pub fn capture_and_close(
    host: &mut dyn PhysicalTransport,
    request: PanelRequest<'_>,
    journal: &mut dyn AttemptJournal,
) -> Result<Probe<ClientBindingEvidence>, HostError> {
    let PanelRequest {
        target,
        interaction,
        policy,
        operation,
        clock,
        deadline,
        freshness,
    } = request;
    let invalid = || HostError::new("native registry panel unverified", HostErrorKind::Unknown);
    let _lane = host.acquire_lane(target)?;
    let capture = host.capture(target, clock, deadline, freshness)?;
    if capture.mode != PaneMode::Normal {
        return Err(invalid());
    }
    let frame = CaptureFrame {
        scope: capture.scope,
        text: capture.text,
        baseline: None,
        profile_id: policy.profile_id.into(),
        operation: Operation::ToolInspect,
        message: None,
        attempt: None,
        after_step: None,
        paste_latch: PasteLatch::NeverSeen,
    };
    let observation = interaction.interpret(&frame);
    if observation.scope != frame.scope
        || !observation
            .predicate
            .as_deref()
            .is_some_and(|predicate| policy.panel_predicates.contains(&predicate))
    {
        return Err(invalid());
    }
    let bound = observation.predicate.as_deref() == Some(policy.bound_predicate);
    let attempt =
        AttemptId::new(format!("panel-close-{}", operation.as_str())).map_err(|_| invalid())?;
    journal.begin(&AttemptMetadata {
        owner: target.identity().clone(),
        attempt,
        correlation: Correlation::NativeControl(operation.clone()),
        operation: Operation::ToolInspect,
        policy_sha256: policy.policy_sha256,
        payload_sha256: digest(&[]),
        payload_bytes: 0,
    })?;
    let record = |kind, ordinal, outcome| JournalRecord {
        kind,
        ordinal,
        at: clock.now(),
        effect: DeliveryEffect::NoEffect,
        step: None,
        outcome,
        sequence: Some(frame.scope.sequence),
        surface: Some(observation.surface),
        code: Some("registry-panel-escape-once"),
    };
    journal.append(&record(JournalKind::ActionIntent, 0, None))?;
    let action = host.key(target, PhysicalKey::Escape, clock, deadline);
    journal.append(&record(JournalKind::ActionResult, 1, Some(action.outcome)))?;
    if action.outcome != StepOutcome::Confirmed || action.error.is_some() {
        return Err(invalid());
    }
    loop {
        let capture = host.capture(target, clock, deadline, freshness)?;
        let closed = CaptureFrame {
            scope: capture.scope,
            text: capture.text,
            baseline: None,
            profile_id: policy.profile_id.into(),
            operation: Operation::ToolInspect,
            message: None,
            attempt: None,
            after_step: None,
            paste_latch: PasteLatch::NeverSeen,
        };
        if capture.mode == PaneMode::Normal
            && interaction.interpret(&closed).surface == InputSurface::ComposerReady
        {
            break;
        }
        if clock.now() >= deadline {
            return Err(invalid());
        }
        clock.sleep(Duration::from_millis(100));
    }
    journal.append(&record(JournalKind::Complete, 2, None))?;
    Ok(Probe {
        scope: frame.scope,
        outcome: if bound {
            ProbeOutcome::Observed(ClientBindingEvidence {
                server_key: policy.server_key.into(),
                tools: TEAM_TOOLS.to_vec(),
                source: ClientBindingSource::NativeRegistry,
            })
        } else {
            ProbeOutcome::Unknown(Reason {
                code: "native-registry-incomplete",
                message:
                    "The current native panel did not expose all three tools from the bound server",
            })
        },
    })
}
