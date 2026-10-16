//! Independent watchdog effects. No native-input state-machine evidence is adopted.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::*;
use crate::lifecycle::pane_input_lock::{acquire_required, PaneInputLockRequest};
use crate::transport::enter_rescue::{
    EnterRescueContext, EnterRescueOutcome, EnterRescueProof, EnterRescueTarget,
};
use crate::transport::native_input::InputFrame;

const STABLE: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(50);
const OBSERVE: Duration = Duration::from_secs(10);

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn marker_count(value: &str, marker: &str) -> usize {
    value
        .lines()
        .map(str::trim)
        .collect::<String>()
        .matches(marker)
        .count()
}
fn current_marker_only(context: &EnterRescueContext, frame: &InputFrame) -> bool {
    let composer: String = frame.composer.lines().map(str::trim).collect();
    composer.matches("[team-agent-token:").count() == 1
        && composer.matches(&context.marker()).count() == 1
}

fn no_effect(reason: &'static str, reobserve: bool) -> EnterRescueOutcome {
    EnterRescueOutcome::NoEffect { reason, reobserve }
}
fn refusal(reason: &str) -> TransportError {
    TransportError::NativeInputNotReady {
        reason: reason.into(),
    }
}

fn pending(context: &EnterRescueContext, frame: &InputFrame) -> bool {
    let Some(proof) = context.proof.as_ref().filter(|_| context.valid_proof()) else {
        return false;
    };
    if frame.busy
        || frame.identity != proof.ui_identity
        || digest(&frame.footer) != proof.footer_sha256
    {
        return false;
    }
    let marker = context.marker();
    if marker_count(&frame.transcript, &marker) != 0 {
        return false;
    }
    (proof.visible_marker && current_marker_only(context, frame))
        || proof.owned_capsule.as_ref().is_some_and(|capsule| {
            capsule.valid()
                && frame.capsule.as_ref() == Some(&capsule.label)
                && frame.composer.trim() == capsule.label
                && !frame
                    .transcript
                    .lines()
                    .any(|line| line.trim() == capsule.label)
        })
}

fn accepted(context: &EnterRescueContext, frame: &InputFrame) -> bool {
    let Some(proof) = context.proof.as_ref().filter(|_| context.valid_proof()) else {
        return false;
    };
    // Invisible whitespace is not an empty composer receipt.
    if !frame.composer.is_empty() || frame.identity != proof.ui_identity {
        return false;
    }
    if proof.visible_marker
        && proof.transcript_marker_absent
        && marker_count(&frame.transcript, &context.marker()) == 1
    {
        return true;
    }
    proof.owned_capsule.as_ref().is_some_and(|capsule| {
        capsule.valid()
            && frame
                .transcript
                .lines()
                .filter(|line| line.trim() == capsule.label)
                .count()
                == 1
    })
}

fn same_editor(left: &InputFrame, right: &InputFrame) -> bool {
    left.composer == right.composer
        && left.identity == right.identity
        && left.footer == right.footer
        && left.capsule == right.capsule
        && left.busy == right.busy
}

impl TmuxBackend {
    fn rescue_binding(
        &self,
        selected: &EnterRescueTarget,
        allow_unset_nonce: bool,
    ) -> Result<EnterRescueTarget, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".into(), "display-message".into(), "-p".into(), "-t".into(), selected.pane_id.clone(),
            "#{socket_path}|#{pane_id}|#{pane_pid}|#{session_name}|#{window_name}|#{@team_agent_pane_binding_nonce}|#{pane_in_mode}|#{pane_height}".into(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success || output.stdout.lines().count() != 1 {
            return Err(refusal("rescue_binding_query"));
        }
        let fields: Vec<_> = output.stdout.trim_end().split('|').collect();
        if fields.len() != 8
            || !std::path::Path::new(fields[0]).is_absolute()
            || fields[1] != selected.pane_id
            || fields[3].is_empty()
            || fields[4].is_empty()
            || (fields[5].is_empty() && !allow_unset_nonce)
            || fields[6] != "0"
        {
            return Err(refusal("rescue_binding_invalid"));
        }
        let pid = fields[2]
            .parse::<u32>()
            .map_err(|_| refusal("rescue_binding_pid"))?;
        let height = fields[7]
            .parse::<u32>()
            .map_err(|_| refusal("rescue_binding_height"))?;
        if pid <= 1 || height < 10 {
            return Err(refusal("rescue_binding_invalid"));
        }
        let actual = EnterRescueTarget {
            provider: selected.provider,
            endpoint: fields[0].into(),
            pane_id: fields[1].into(),
            pane_pid: pid,
            pane_height: height,
            carrier_birth: team_agent_contract::host::process::process_birth_identity(pid)
                .map_err(|_| refusal("rescue_carrier_birth"))?,
            binding_nonce: fields[5].into(),
            session: fields[3].into(),
            window: fields[4].into(),
        };
        if !actual.same_selection(selected) {
            return Err(refusal("rescue_selection_changed"));
        }
        Ok(actual)
    }

    fn rescue_frame(
        &self,
        expected: &EnterRescueTarget,
    ) -> Result<Option<InputFrame>, TransportError> {
        if self.rescue_binding(expected, false)? != *expected {
            return Err(refusal("rescue_binding_changed"));
        }
        let frame = self.observe_native_input(
            &Target::Pane(PaneId::new(&expected.pane_id)),
            expected.provider,
        )?;
        if self.rescue_binding(expected, false)? != *expected {
            return Err(refusal("rescue_binding_changed"));
        }
        Ok(frame)
    }

    /// Same original delivery boundary only. No late snapshot is taken by fire.
    pub(super) fn snapshot_rescue_context(
        &self,
        target: &Target,
        context: &EnterRescueContext,
        rendered_payload: &str,
    ) -> Result<EnterRescueContext, TransportError> {
        let mut result = context.clone();
        if !context.valid_message() || digest(rendered_payload) != context.payload_sha256 {
            return Ok(result);
        }
        let Some(selected) = context.target.as_ref() else {
            return Ok(result);
        };
        if pane_from_target(target).as_str() != selected.pane_id {
            return Ok(result);
        }
        let Some(_lock) = acquire_required(PaneInputLockRequest {
            workspace: self.event_workspace.as_deref(),
            target_key: &selected.pane_id,
            operation: "enter_rescue_snapshot",
        }) else {
            return Ok(result);
        };
        let initial = match self.rescue_binding(selected, true) {
            Ok(value) => value,
            Err(_) => return Ok(result),
        };
        if (!selected.endpoint.is_empty() && initial.endpoint != selected.endpoint)
            || (!selected.carrier_birth.is_empty()
                && initial.carrier_birth != selected.carrier_birth)
            || (!selected.binding_nonce.is_empty()
                && initial.binding_nonce != selected.binding_nonce)
        {
            return Ok(result);
        }
        let frame = match self.observe_native_input(target, selected.provider) {
            Ok(Some(frame)) => frame,
            _ => return Ok(result),
        };
        if !current_marker_only(context, &frame)
            || marker_count(&frame.transcript, &context.marker()) != 0
        {
            return Ok(result);
        }
        if self.rescue_binding(selected, true)? != initial {
            return Ok(result);
        }
        let mut expected = initial;
        if expected.binding_nonce.is_empty() {
            let Some(workspace) = self.event_workspace.as_deref() else {
                return Ok(result);
            };
            let pane = PaneId::new(&expected.pane_id);
            let proposed = next_pane_binding_nonce(workspace, &expected.endpoint, &pane);
            expected.binding_nonce = self.set_pane_binding_nonce_if_unset(&pane, &proposed)?;
        }
        if self.rescue_binding(selected, false)? != expected {
            return Ok(result);
        }
        result.target = Some(expected);
        result.proof = Some(EnterRescueProof {
            ui_identity: frame.identity,
            footer_sha256: digest(&frame.footer),
            visible_marker: true,
            transcript_marker_absent: true,
            owned_capsule: None,
        });
        Ok(result)
    }

    pub(super) fn rescue_guarded_enter(
        &self,
        context: &EnterRescueContext,
        authorize: &dyn Fn() -> bool,
        observer: Option<&dyn SubmitObserver>,
    ) -> Result<EnterRescueOutcome, TransportError> {
        let Some(expected) = context.target.as_ref().filter(|target| target.bound()) else {
            return Ok(no_effect("binding_missing", false));
        };
        if !context.valid_message() || !context.valid_proof() {
            return Ok(no_effect("proof_missing", false));
        }
        let Some(_lock) = acquire_required(PaneInputLockRequest {
            workspace: self.event_workspace.as_deref(),
            target_key: &expected.pane_id,
            operation: "enter_rescue",
        }) else {
            return Ok(no_effect("input_lock", true));
        };
        let Some(first) = self.rescue_frame(expected)? else {
            return Ok(no_effect("surface_unknown", true));
        };
        if accepted(context, &first) {
            return Ok(if authorize() {
                EnterRescueOutcome::Consumed
            } else {
                no_effect("canonical_changed", false)
            });
        }
        if first.busy {
            return Ok(no_effect("busy", true));
        }
        if !pending(context, &first) {
            return Ok(no_effect("residue_mismatch", false));
        }
        std::thread::sleep(STABLE);
        let Some(stable) = self.rescue_frame(expected)? else {
            return Ok(no_effect("surface_unknown", true));
        };
        if !same_editor(&first, &stable) || !pending(context, &stable) {
            return Ok(no_effect("composer_changed", false));
        }
        // Last fresh current-message/busy/binding checks are under the same lock.
        let Some(last) = self.rescue_frame(expected)? else {
            return Ok(no_effect("surface_unknown", true));
        };
        if !same_editor(&stable, &last) || !pending(context, &last) {
            return Ok(no_effect("composer_changed", false));
        }
        if !authorize() {
            return Ok(no_effect("canonical_changed", false));
        }
        self.run_inject_stage(
            &tmux_send_submit_argv(&PaneId::new(&expected.pane_id), Key::Enter),
            InjectStage::Submit,
        )?;
        notify_submit_observer(observer);
        // Any failure after the key is uncertain, not a retryable no-effect.
        let start = Instant::now();
        while start.elapsed() < OBSERVE {
            match self.rescue_frame(expected) {
                Ok(Some(frame)) if accepted(context, &frame) => {
                    return Ok(EnterRescueOutcome::Entered { accepted: true })
                }
                Err(_) => break,
                _ => std::thread::sleep(POLL),
            }
        }
        Ok(EnterRescueOutcome::Entered { accepted: false })
    }
}

#[cfg(test)]
#[path = "enter_rescue_tests.rs"]
mod tests;
