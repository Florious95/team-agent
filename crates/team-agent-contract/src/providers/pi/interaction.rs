//! Pi interactive TUI grammar, read from the pi-tui editor, footer and
//! interactive-mode sources of pi 1.4 and the legacy measured Pi composer
//! rules. Recognizing a surface never authorizes a key: every action stays in
//! the shared executor.
use super::{session_uuid, PiAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, session::*, types::*};
use crate::runtime::delivery::{ControlDefinition, ControlInputMode, ControlKind};
use std::time::Duration;

pub const PROFILE: &str = "pi-tui-v1";
pub const POLICY_RECIPE: &str = "pi-tui-v1/r2:runtime-native-image-or-env-node+wrapper;--no-approve:no-trust-screen;owned-cwd-extension-server-ambient-coexistence;120x40;paste-bracketed;enter-once;retry-never;queue-preserve;editor:last-two-rulers-within-13-rows+nonblank-footer-below;busy:top-ruler-run<8|braille-or-interrupt/cancel-status-above;fold:>10-lines:[paste #N +L lines]:L==lines|>1000-chars:[paste #N C chars]:C<=bytes:empty-baseline+confirmed-paste+attempt->confirmed-enter+empty-composer+token-above-editor+busy-or-ready;inline:token-in-composer->token-in-history;session:profile-paste+/session+new-Session-Info-block+one-ID-uuid+File-label-or-own-wrapped-rows-ends-_uuid.jsonl;exit:profile-paste+/quit+native-exited+dead-pane-close;capture100ms;stable200ms;paste-floor100ms;deadline30s";
pub const POLICY: Digest = Digest([74, 181, 235, 173, 72, 194, 23, 24, 96, 115, 36, 82, 244, 234, 116, 29, 21, 244, 139, 146, 137, 116, 155, 227, 134, 112, 14, 28, 80, 241, 38, 206]);
const SESSION: &str = "/session";
const QUIT: &str = "/quit";
const FOLDED: &str = "pi-owned-paste-v1:";
/// The editor shows at most max(5, rows * 0.3) lines: 12 in the 40-row pane.
const EDITOR_ROWS: usize = 12;
pub static PROFILES: &[InputProfile] = &[InputProfile {
    id: PROFILE, identity: ProfileIdentity::RuntimeCaptured, harness: "pi", ui: "tui",
    platform: Platform::MacOs, policy_sha256: POLICY,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend, Operation::SessionInspect, Operation::Stop],
    channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed, payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter, confirmation_steps: &[], retry_budget: RetryBudget::Never,
        queue_flush: &[], max_submit_keys: 1,
        timing: InputTiming { capture_interval: Duration::from_millis(100), stable_window: Duration::from_millis(200), paste_to_submit_floor: Duration::from_millis(100), deadline: Duration::from_secs(30) },
    }),
}];
/// Typed `/` opens Pi's command autocomplete, whose Enter applies the selected
/// row; a bracketed paste inserts the command without it.
pub static CONTROLS: &[ControlDefinition] = &[ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::SessionInspect,
    kind: ControlKind::InspectSession, command: SESSION, input_mode: ControlInputMode::ProfilePaste,
}, ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::Stop,
    kind: ControlKind::Exit, command: QUIT, input_mode: ControlInputMode::ProfilePaste,
}];

fn rows(text: &str) -> Vec<&str> { text.lines().map(str::trim_end).collect() }
/// Pi draws full-width editor borders; the top one embeds the working status
/// after a short `── ` prefix and still carries a long ruler run after it.
fn ruler(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('─') && line.chars().filter(|ch| *ch == '─').count() >= 8
}
struct Editor { top: usize, input: String }
/// The editor lies between the last two rulers on screen, at most its own
/// height apart, with the footer (built-in or an extension's) below it.
fn editor(lines: &[&str]) -> Option<Editor> {
    let bottom = lines.iter().rposition(|line| ruler(line))?;
    if lines[bottom + 1..].iter().all(|line| line.trim().is_empty()) { return None; }
    let top = (bottom.saturating_sub(EDITOR_ROWS + 1)..bottom).rev().find(|index| ruler(lines[*index]))?;
    let input = lines[top + 1..bottom].iter().map(|line| line.trim()).collect::<Vec<_>>().join("\n").trim().to_owned();
    Some(Editor { top, input })
}
/// A working status sits in the top border or, for an editor that cannot
/// embed it, in a spinner row just above. Idle has a solid leading ruler run.
fn busy(lines: &[&str], top: usize) -> bool {
    let status = |line: &&str| {
        let line = line.trim();
        line.chars().next().is_some_and(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch))
            || line.ends_with(" to interrupt)") || line.ends_with(" to cancel)")
    };
    lines[top].trim_start().chars().take_while(|ch| *ch == '─').count() < 8
        || lines[..top].iter().rev().filter(|line| !line.trim().is_empty()).take(3).any(status)
}
pub fn surface(text: &str) -> InputSurface {
    let lines = rows(text);
    let Some(editor) = editor(&lines) else { return InputSurface::ShellOrUnknown; };
    if !editor.input.is_empty() { return InputSurface::ComposerContainsPaste; }
    if busy(&lines, editor.top) { InputSurface::Busy } else { InputSurface::ComposerReady }
}
fn positive(value: &str) -> Option<usize> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) { return None; }
    value.parse().ok()
}
/// Pi folds a paste of more than ten lines into `[paste #N +L lines]`, else one
/// of more than 1000 characters into `[paste #N C chars]`. C counts UTF-16
/// units, never more than the payload's UTF-8 bytes.
fn fold_matches(capsule: &str, input: &InputShape) -> bool {
    let Some((id, size)) = capsule.strip_prefix("[paste #").and_then(|rest| rest.strip_suffix(']')).and_then(|body| body.split_once(' ')) else { return false; };
    if positive(id).is_none() { return false; }
    if let Some(lines) = size.strip_prefix('+').and_then(|size| size.strip_suffix(" lines")) {
        return positive(lines).is_some_and(|lines| lines > 10 && lines == input.lines);
    }
    size.strip_suffix(" chars").and_then(positive).is_some_and(|chars| input.lines <= 10 && chars > 1000 && chars <= input.bytes)
}
fn current_subject(frame: &CaptureFrame) -> bool {
    frame.baseline.as_ref().is_some_and(|before| before.scope.identity == frame.scope.identity
        && before.scope.endpoint == frame.scope.endpoint && before.scope.pane == frame.scope.pane
        && before.scope.binding == frame.scope.binding && before.scope.session == frame.scope.session
        && before.scope.sequence < frame.scope.sequence && before.scope.observed_at <= frame.scope.observed_at
        && surface(&before.text) == InputSurface::ComposerReady)
}
fn folded_identity(frame: &CaptureFrame) -> Option<String> {
    let input = frame.input.as_ref()?;
    let context = serde_json::to_string(&(&frame.scope.identity, &frame.scope.endpoint, &frame.scope.pane, &frame.scope.binding,
        frame.scope.session.as_ref().map(NativeSessionId::as_str), frame.message.as_ref()?, frame.attempt.as_ref()?,
        frame.operation == Operation::FirstBusiness, input.sha256, input.bytes, input.lines, &input.first_line)).ok()?;
    Some(format!("{FOLDED}{context}"))
}
/// Seen requires this attempt's confirmed paste into an empty composer and a
/// capsule matching the immutable payload shape; acceptance requires the full
/// token in the history above the editor once Pi expands and submits it.
fn folded(frame: &CaptureFrame, observation: &mut InteractionObservation, lines: &[&str], editor: Option<&Editor>) -> bool {
    let owned = matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } | PasteLatch::Gone { native_identity } if native_identity.starts_with(FOLDED));
    let (Some(identity), Some(input), Some(before), Some(message), Some(editor)) = (folded_identity(frame), &frame.input, &frame.baseline, &frame.message, editor) else { return owned; };
    let token = format!("[team-agent-token:{}]", message.as_str());
    let correlate = |observation: &mut InteractionObservation| {
        observation.current_message = frame.message.clone(); observation.current_attempt = frame.attempt.clone();
    };
    if frame.after_step == Some(StepKind::Paste) && current_subject(frame) && !before.text.contains(&token)
        && fold_matches(&editor.input, input) && !before.text.contains(&editor.input)
        && (matches!(frame.paste_latch, PasteLatch::NeverSeen)
            || matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity))
    {
        observation.paste_latch = PasteLatch::Seen { native_identity: identity };
        observation.surface = InputSurface::ComposerContainsPaste; correlate(observation); return true;
    }
    if !matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity) { return owned; }
    if frame.after_step == Some(StepKind::InitialSubmit) && editor.input.is_empty() && !before.text.contains(&token)
        && lines[..editor.top].iter().any(|line| line.contains(&token))
        && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
    {
        observation.surface = InputSurface::NativeAccepted;
        observation.paste_latch = PasteLatch::Gone { native_identity: identity }; correlate(observation);
    }
    owned
}
fn info_blocks(text: &str) -> usize { rows(text).iter().filter(|line| line.trim() == "Session Info").count() }
/// The newest `/session` block above the editor: exactly one `ID: <uuid>` and
/// a `File:` path naming that session's `<timestamp>_<uuid>.jsonl`. A path
/// wider than the pane moves to its own rows after a bare `File:` label and is
/// hard-wrapped there. A UUID in chat output or a requested `--session`
/// argument cannot bind.
pub fn session_id(text: &str) -> Result<NativeSessionId, ContractError> {
    let invalid = || ContractError::Invalid("Pi session info");
    let lines = rows(text);
    let end = editor(&lines).ok_or_else(invalid)?.top;
    let header = lines[..end].iter().rposition(|line| line.trim() == "Session Info").ok_or_else(invalid)?;
    let block: Vec<&str> = lines[header + 1..end].iter().map(|line| line.trim()).take_while(|line| *line != "Messages").collect();
    // A block clipped by the editor has no `Messages` section: never bind it.
    if header + 1 + block.len() == end { return Err(invalid()); }
    let ids: Vec<usize> = block.iter().enumerate().filter(|(_, line)| line.starts_with("ID: ")).map(|(index, _)| index).collect();
    let [row] = ids.as_slice() else { return Err(invalid()); };
    let id = block[*row]["ID: ".len()..].trim();
    let file = block[..*row].iter().position(|line| *line == "File:" || line.starts_with("File: ")).ok_or_else(invalid)?;
    let path = block[file..*row].concat();
    let path = path.strip_prefix("File:").ok_or_else(invalid)?.trim_start();
    if !session_uuid(id) || !path.starts_with('/') || !path.ends_with(&format!("_{id}.jsonl")) { return Err(invalid()); }
    NativeSessionId::new(id)
}
impl InteractionHook for PiAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let mut observation = InteractionObservation { scope: frame.scope.clone(), surface: InputSurface::ShellOrUnknown,
            predicate: None, current_message: None, current_attempt: None, paste_latch: frame.paste_latch.clone(), control_paste: None };
        if frame.profile_id != PROFILE { return observation; }
        observation.surface = surface(&frame.text);
        let lines = rows(&frame.text);
        let editor = editor(&lines);
        if matches!(frame.operation, Operation::FirstBusiness | Operation::OrdinarySend) {
            if !folded(frame, &mut observation, &lines, editor.as_ref()) {
                correlate_business_input(frame, &mut observation, editor.as_ref().map(|editor| editor.input.as_str()));
            }
            return observation;
        }
        if !matches!(frame.operation, Operation::SessionInspect | Operation::Stop) || frame.message.is_some() { return observation; }
        let command = if frame.operation == Operation::SessionInspect { SESSION } else { QUIT };
        let typed = editor.as_ref().is_some_and(|editor| editor.input == command);
        if frame.after_step == Some(StepKind::Paste) && current_subject(frame) && typed {
            observation.paste_latch = PasteLatch::Seen { native_identity: command.into() };
            observation.current_attempt = frame.attempt.clone();
        }
        if frame.operation == Operation::SessionInspect && frame.after_step == Some(StepKind::InitialSubmit) && !typed
            && matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == command)
            && session_id(&frame.text).is_ok()
            && frame.baseline.as_ref().is_some_and(|before| info_blocks(&frame.text) > info_blocks(&before.text))
        {
            observation.surface = InputSurface::NativeAccepted;
            observation.paste_latch = PasteLatch::Gone { native_identity: command.into() };
            observation.current_attempt = frame.attempt.clone();
        }
        observation
    }
}
impl SessionHook for PiAdapter {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.provider.as_str() != super::ID || !PROFILES[0].matches_native(&evidence.native)
            || evidence.origin != CaptureOrigin::CurrentNativeSession || evidence.evidence_sha256 != crate::host::digest(&evidence.record)
        { return Err(ContractError::Mismatch("Pi session provenance")); }
        let text = std::str::from_utf8(&evidence.record).map_err(|_| ContractError::Invalid("Pi session UTF-8"))?;
        Ok(ResumeBinding { provider: evidence.provider.clone(), native_session: session_id(text)?,
            storage: SessionStorage::Local, cwd: evidence.cwd.clone(), native: evidence.native.clone(),
            source: evidence.scope.identity.clone(), origin: evidence.origin, evidence_kind: evidence.evidence_kind,
            evidence_sha256: evidence.evidence_sha256, backing: None })
    }
}
impl SemanticReader for PiAdapter {
    fn read(&self, record: &NativeRecord) -> Probe<SemanticEvidence> {
        let outcome = if record.kind == NativeRecordKind::Surface {
            match std::str::from_utf8(&record.bytes).ok().map(surface) {
                Some(InputSurface::Busy) => ProbeOutcome::Observed(SemanticEvidence::Busy),
                Some(InputSurface::ComposerReady) => ProbeOutcome::Observed(SemanticEvidence::Idle),
                _ => ProbeOutcome::Unknown(UNVERIFIED),
            }
        } else { ProbeOutcome::Unknown(UNVERIFIED) };
        Probe { scope: record.scope.clone(), outcome }
    }
}
