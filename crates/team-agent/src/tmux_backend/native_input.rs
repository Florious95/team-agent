//! Guarded legacy native-input effects. Shared contract parsers remain observation-only.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::*;
use crate::lifecycle::pane_input_lock::{acquire_required, PaneInputLockRequest};
use crate::transport::native_input::{
    inspect_input, record_evidence, EnterRescueOutcome, InputFrame, NativeInputBinding,
    NativeInputEvidence, NativeInputPhase, NativeInputRequest,
};

#[cfg(test)]
#[path = "native_input_tests.rs"]
mod tests;

const STABLE: Duration = Duration::from_millis(200);
const OBSERVE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);

fn refused(reason: &str) -> TransportError {
    TransportError::NativeInputNotReady {
        reason: reason.to_string(),
    }
}

pub(crate) fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

impl TmuxBackend {
    fn native_binding(&self, target: &Target) -> Result<NativeInputBinding, TransportError> {
        self.query_native_binding(target, false)
    }

    fn query_native_binding(
        &self,
        target: &Target,
        allow_unset_nonce: bool,
    ) -> Result<NativeInputBinding, TransportError> {
        let pane = pane_from_target(target);
        let argv = self.tmux_argv(&[
            "tmux".into(), "display-message".into(), "-p".into(), "-t".into(), pane.as_str().into(),
            "#{socket_path}|#{pane_id}|#{pane_pid}|#{session_name}|#{window_name}|#{@team_agent_pane_binding_nonce}|#{pane_in_mode}|#{pane_height}".into(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success || output.stdout.lines().count() != 1 {
            return Err(refused("binding_query"));
        }
        let fields: Vec<_> = output.stdout.trim_end().split('|').collect();
        if fields.len() != 8 {
            return Err(refused("binding_invalid:field_count"));
        }
        for (invalid, field) in [
            (!std::path::Path::new(fields[0]).is_absolute(), "endpoint"),
            (fields[1] != pane.as_str(), "pane"),
            (fields[3].is_empty(), "session_empty"),
            (fields[4].is_empty(), "window_empty"),
            (fields[5].is_empty() && !allow_unset_nonce, "nonce_empty"),
            (fields[6] != "0", "pane_in_mode"),
        ] {
            if invalid {
                // Field class only: no argv, environment, business text or raw
                // native metadata is copied into the existing refusal event.
                return Err(refused(&format!("binding_invalid:{field}")));
            }
        }
        let pane_height = fields[7]
            .parse::<u32>()
            .map_err(|_| refused("binding_height"))?;
        if pane_height < 10 {
            return Err(refused("binding_height"));
        }
        let pid = fields[2]
            .parse::<u32>()
            .map_err(|_| refused("binding_pid"))?;
        let birth = team_agent_contract::host::process::process_birth_identity(pid)
            .map_err(|_| refused("carrier_birth"))?;
        Ok(NativeInputBinding {
            endpoint: fields[0].into(),
            pane_id: fields[1].into(),
            pane_pid: pid,
            pane_height,
            carrier_birth: birth,
            session: fields[3].into(),
            window: fields[4].into(),
            binding_nonce: fields[5].into(),
        })
    }

    // Called only under the required input lock. A newly launched legacy worker
    // has no Leader lease nonce producer: initialize its exact current carrier,
    // never overwrite an existing nonce or waive the subsequent strict reads.
    fn prepare_native_binding(
        &self,
        target: &Target,
        request: &NativeInputRequest,
    ) -> Result<NativeInputBinding, TransportError> {
        let initial = self.query_native_binding(target, true)?;
        if initial.pane_id != request.pane_id
            || initial.pane_pid != request.pane_pid
            || initial.session != request.session
            || initial.window != request.window
        {
            return Err(refused("selected_binding_changed"));
        }
        if !initial.binding_nonce.is_empty() {
            return Ok(initial);
        }
        let nonce = self.set_pane_binding_nonce_if_unset(
            &PaneId::new(&initial.pane_id),
            &uuid::Uuid::new_v4().to_string(),
        )?;
        let expected = NativeInputBinding {
            binding_nonce: nonce,
            ..initial
        };
        let observed = self.native_binding(target)?;
        if observed != expected {
            return Err(refused("binding_changed_after_nonce"));
        }
        Ok(observed)
    }

    fn native_frame(&self, evidence: &NativeInputEvidence) -> Result<InputFrame, TransportError> {
        let target = Target::Pane(PaneId::new(&evidence.binding.pane_id));
        if self.native_binding(&target)? != evidence.binding {
            return Err(refused("binding_changed"));
        }
        let capture = self.capture(&target, CaptureRange::Tail(200))?;
        let frame = inspect_input(
            evidence.request.provider,
            &capture.text,
            evidence.binding.pane_height,
        )
        .ok_or_else(|| refused("native_surface_unknown"))?;
        if self.native_binding(&target)? != evidence.binding {
            return Err(refused("binding_changed"));
        }
        Ok(frame)
    }

    fn native_record(&self, evidence: &NativeInputEvidence) -> Result<(), TransportError> {
        record_evidence(evidence);
        let workspace = self
            .event_workspace
            .as_deref()
            .ok_or_else(|| refused("workspace_missing"))?;
        crate::event_log::EventLog::new(workspace)
            .write(
                "native_input.evidence",
                serde_json::to_value(evidence).map_err(std::io::Error::other)?,
            )
            .map(|_| ())
            .map_err(|error| TransportError::Io(std::io::Error::other(error)))
    }

    fn stable_frame(
        &self,
        evidence: &NativeInputEvidence,
        empty: bool,
    ) -> Result<InputFrame, TransportError> {
        let start = Instant::now();
        let stable_window = if empty {
            STABLE
        } else {
            STABLE.max(current_paste_to_submit_floor())
        };
        let mut eligible: Option<(InputFrame, Instant)> = None;
        while start.elapsed() < OBSERVE {
            let frame = self.native_frame(evidence)?;
            let valid = if empty {
                frame.empty() && !frame.busy
            } else {
                frame.pending(evidence)
            };
            if valid {
                if let Some((previous, since)) = &eligible {
                    if previous.composer == frame.composer
                        && previous.identity == frame.identity
                        && previous.capsule == frame.capsule
                        && since.elapsed() >= stable_window
                    {
                        return Ok(frame);
                    }
                    if previous.composer != frame.composer
                        || previous.identity != frame.identity
                        || previous.capsule != frame.capsule
                    {
                        eligible = Some((frame, Instant::now()));
                    }
                } else {
                    eligible = Some((frame, Instant::now()));
                }
            } else {
                eligible = None;
            }
            std::thread::sleep(POLL);
        }
        Err(refused(if empty {
            "composer_not_ready"
        } else {
            "owned_paste_not_stable"
        }))
    }

    fn observe_native_acceptance(
        &self,
        evidence: &NativeInputEvidence,
    ) -> Result<bool, TransportError> {
        let start = Instant::now();
        while start.elapsed() < OBSERVE {
            if self.native_frame(evidence)?.accepted(evidence) {
                return Ok(true);
            }
            std::thread::sleep(POLL);
        }
        Ok(false)
    }

    pub(super) fn inject_native_input(
        &self,
        target: &Target,
        text: &str,
        bracketed: bool,
        request: NativeInputRequest,
        observer: Option<&dyn SubmitObserver>,
    ) -> Result<InjectReport, TransportError> {
        let pane = pane_from_target(target);
        let _lock = acquire_required(PaneInputLockRequest {
            workspace: self.event_workspace.as_deref(),
            target_key: pane.as_str(),
            operation: "native_inject",
        })
        .ok_or_else(|| refused("input_lock"))?;
        if !request.supported() {
            return Err(refused("unsupported_profile"));
        }
        let binding = self.prepare_native_binding(target, &request)?;
        let mut evidence = NativeInputEvidence {
            request,
            binding,
            payload_sha256: digest(text),
            ui_identity: String::new(),
            footer_sha256: String::new(),
            baseline_sha256: String::new(),
            baseline_capsules: Vec::new(),
            phase: NativeInputPhase::Ready,
            capsule: None,
        };
        if evidence.binding.pane_id != evidence.request.pane_id
            || evidence.binding.pane_pid != evidence.request.pane_pid
            || evidence.binding.session != evidence.request.session
            || evidence.binding.window != evidence.request.window
        {
            return Err(refused("selected_binding_changed"));
        }
        let baseline = self.stable_frame(&evidence, true)?;
        if baseline.transcript.contains(&evidence.request.marker()) {
            return Err(refused("marker_in_baseline"));
        }
        evidence.ui_identity = baseline.identity;
        evidence.footer_sha256 = digest(&baseline.footer);
        evidence.baseline_sha256 = digest(&baseline.transcript);
        evidence.baseline_capsules = capsule_labels(&baseline.transcript);
        self.native_record(&evidence)?;
        let buffer = owned_buffer(&evidence);
        for argv in tmux_inject_text_argv(&pane, &buffer, text, bracketed) {
            let stage = inject_stage_for_argv(&argv);
            if stage == InjectStage::PasteBuffer {
                let frame = self.native_frame(&evidence)?;
                if !frame.empty()
                    || frame.busy
                    || frame.identity != evidence.ui_identity
                    || digest(&frame.footer) != evidence.footer_sha256
                {
                    return Err(refused("composer_changed_before_paste"));
                }
                evidence.phase = NativeInputPhase::MayHavePasted;
                self.native_record(&evidence)?;
            }
            if stage == InjectStage::LoadBuffer {
                self.run_inject_stage_with_stdin(&argv, stage, text)?;
            } else {
                self.run_inject_stage(&argv, stage)?;
            }
            if stage == InjectStage::PasteBuffer {
                evidence.phase = NativeInputPhase::Pasted;
                self.native_record(&evidence)?;
            }
        }
        // A capsule becomes owned only after this empty-baseline, single-paste transition.
        let start = Instant::now();
        while start.elapsed() < OBSERVE {
            let frame = self.native_frame(&evidence)?;
            if frame.identity == evidence.ui_identity && !frame.busy {
                if let Some(capsule) = &frame.capsule {
                    if !evidence.baseline_capsules.contains(capsule) {
                        evidence.capsule = Some(capsule.clone());
                    }
                }
                if frame.pending(&evidence) {
                    break;
                }
            }
            std::thread::sleep(POLL);
        }
        self.stable_frame(&evidence, false)?;
        evidence.phase = NativeInputPhase::Seen;
        self.native_record(&evidence)?;
        self.native_submit(&mut evidence, observer)?;
        let accepted = self.observe_native_acceptance(&evidence)?;
        if accepted {
            evidence.phase = NativeInputPhase::Accepted;
            self.native_record(&evidence)?;
        }
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: if evidence.capsule.is_some() {
                InjectVerification::CaptureContainsNewPastedContentPrompt
            } else {
                InjectVerification::CaptureContainsToken
            },
            submit_verification: if accepted {
                SubmitVerification::EnterSentWithoutPlaceholderCheck
            } else {
                SubmitVerification::SubmitConsumptionUnverified
            },
            turn_verification: TurnVerification::NotRequired,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn native_submit(
        &self,
        evidence: &mut NativeInputEvidence,
        observer: Option<&dyn SubmitObserver>,
    ) -> Result<(), TransportError> {
        let frame = self.native_frame(evidence)?;
        if !frame.pending(evidence) {
            return Err(refused("composer_changed_before_enter"));
        }
        evidence.phase = NativeInputPhase::MayHaveSubmitted;
        self.native_record(evidence)?;
        self.run_inject_stage(
            &tmux_send_submit_argv(&PaneId::new(&evidence.binding.pane_id), Key::Enter),
            InjectStage::Submit,
        )?;
        notify_submit_observer(observer);
        evidence.phase = NativeInputPhase::Submitted;
        self.native_record(evidence)
    }

    pub(super) fn rescue_native_input(
        &self,
        evidence: &NativeInputEvidence,
        authorize: &dyn Fn() -> bool,
        observer: Option<&dyn SubmitObserver>,
    ) -> Result<EnterRescueOutcome, TransportError> {
        let _lock = match acquire_required(PaneInputLockRequest {
            workspace: self.event_workspace.as_deref(),
            target_key: &evidence.binding.pane_id,
            operation: "enter_rescue",
        }) {
            Some(lock) => lock,
            None => {
                return Ok(EnterRescueOutcome::NoEffect {
                    reason: "input_lock",
                    reobserve: true,
                })
            }
        };
        if !evidence.request.supported() || evidence.phase < NativeInputPhase::Pasted {
            return Ok(EnterRescueOutcome::NoEffect {
                reason: "proof_missing",
                reobserve: false,
            });
        }
        let frame = self.native_frame(evidence)?;
        if frame.accepted(evidence) {
            return Ok(EnterRescueOutcome::Consumed);
        }
        if frame.busy {
            return Ok(EnterRescueOutcome::NoEffect {
                reason: "busy",
                reobserve: true,
            });
        }
        if !frame.pending(evidence) {
            return Ok(EnterRescueOutcome::NoEffect {
                reason: "residue_mismatch",
                reobserve: false,
            });
        }
        self.stable_frame(evidence, false)?;
        if !authorize() {
            return Ok(EnterRescueOutcome::NoEffect {
                reason: "canonical_changed",
                reobserve: false,
            });
        }
        let mut current = evidence.clone();
        current.phase = NativeInputPhase::Seen;
        self.native_record(&current)?;
        self.native_submit(&mut current, observer)?;
        let accepted = self.observe_native_acceptance(&current)?;
        if accepted {
            current.phase = NativeInputPhase::Accepted;
            self.native_record(&current)?;
        }
        Ok(EnterRescueOutcome::Entered { accepted })
    }
}

fn owned_buffer(evidence: &NativeInputEvidence) -> String {
    format!(
        "ta-owned-{}-{}",
        evidence.request.message_id,
        digest(&format!(
            "{}:{}:{}",
            evidence.binding.binding_nonce, evidence.binding.pane_id, evidence.payload_sha256
        ))
    )
}

fn capsule_labels(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let start = line.find("[Pasted text #")?;
            let end = line[start..].find(']')? + start;
            let label = &line[start..=end];
            (label.len() <= 80).then(|| label.to_string())
        })
        .collect()
}
