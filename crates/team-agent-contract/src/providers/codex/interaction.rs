//! Codex inline TUI grammar (`--no-alt-screen`), measured from the tagged
//! openai/codex 0.160.1 TUI snapshots and the recon receipts. Recognizing a
//! surface never authorizes a key: every action stays in the shared executor.
use super::{CodexAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, session::*, types::*};
use crate::runtime::delivery::{ControlDefinition, ControlInputMode, ControlKind};
use std::{path::Path, time::Duration};

pub const PROFILE: &str = "codex-tui-v1";
pub const POLICY_RECIPE: &str = "codex-tui-v1/r1:runtime-native-image-or-env-node+wrapper;--no-alt-screen;--no-daemon;check_for_update_on_startup=false;private-non-git-cwd:no-trust-screen;owned-argv-server-ambient-coexistence;120x40;paste-bracketed;enter-once;retry-never;queue-preserve;composer:last-row-›|»;empty:Ask Codex to do anything;busy:row-ends-to-interrupt);starting:header-loading|resuming|forking;selection-row:startup-modal-or-unknown:no-key;fold:>1000-chars:[Pasted Content N chars]:empty-baseline+confirmed-paste+N<=bytes+attempt->confirmed-enter+empty-composer+token-in-history+busy-or-ready;inline:token-in-composer->token-in-history;status:direct+/status+new-card+Session-uuid+Directory-cwd+v-version;exit:direct+/exit+native-exited+dead-pane-close;capture100ms;stable200ms;paste-floor100ms;deadline30s";
pub const POLICY: Digest = Digest([218, 195, 166, 140, 161, 0, 254, 248, 74, 243, 8, 64, 110, 5, 163, 80, 232, 124, 204, 160, 240, 165, 202, 188, 103, 5, 70, 18, 200, 70, 214, 242]);
const PLACEHOLDER: &str = "Ask Codex to do anything";
/// Codex folds a paste longer than this many characters into a placeholder.
const FOLD_CHARS: usize = 1000;
const FOLDED: &str = "codex-owned-paste-v1:";
pub static PROFILES: &[InputProfile] = &[InputProfile {
    id: PROFILE, identity: ProfileIdentity::RuntimeCaptured, harness: "codex", ui: "tui",
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
pub static CONTROLS: &[ControlDefinition] = &[ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::SessionInspect,
    kind: ControlKind::InspectSession, command: "/status", input_mode: ControlInputMode::DirectTyping,
}, ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::Stop,
    kind: ControlKind::Exit, command: "/exit", input_mode: ControlInputMode::DirectTyping,
}];

/// The live composer is the bottom-most prompt row: `›`, or `»` at ultra
/// effort. History turns, the slash popup's selection and modal options above
/// it also start with `›`; the prompt marks the first visible composer row.
fn composer(lines: &[&str]) -> Option<(usize, String)> {
    let prompt = |line: &str| ["›", "»"].into_iter().find(|glyph| line == *glyph || line.strip_prefix(glyph).is_some_and(|rest| rest.starts_with(' ')));
    let index = lines.iter().rposition(|line| prompt(*line).is_some())?;
    let glyph = prompt(lines[index])?;
    Some((index, lines[index][glyph.len()..].trim().to_owned()))
}
fn empty(first: &str) -> bool { first.is_empty() || first == PLACEHOLDER }
fn numbered(first: &str) -> bool {
    first.split_once(". ").is_some_and(|(number, _)| (1..=2).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit()))
}
/// A numbered prompt row is a selection list. Known startup modals are named
/// for diagnostics; any other list (approval, migration) stays unknown.
fn modal(lines: &[&str], index: usize) -> Option<&'static str> {
    let above = &lines[..index];
    let starts = |prefix: &str| above.iter().any(|line| line.trim_start().starts_with(prefix));
    if starts("Update available") && lines[index..].iter().any(|line| line.contains("Skip until next version")) { Some("codex-update-modal") }
    else if starts("Trust this folder?") { Some("codex-folder-trust-modal") }
    else if above.iter().any(|line| line.trim() == "Hooks need review") { Some("codex-hooks-review-modal") }
    else { None }
}
fn starting(lines: &[&str]) -> bool {
    lines.windows(2).any(|pair| pair[0].contains(">_ OpenAI Codex (v") && pair[1].trim() == "loading")
        || lines.iter().any(|line| matches!(line.trim(), "Resuming session…" | "Forking session…"))
}
/// The status indicator sits just above the composer: `Working (3s • esc to interrupt)`.
fn busy(lines: &[&str], index: usize) -> bool {
    lines[index.saturating_sub(8)..index].iter().any(|line| line.ends_with(" to interrupt)") || line.contains(" to interrupt) · "))
}
fn rows(text: &str) -> Vec<&str> { text.lines().map(str::trim_end).collect() }
pub fn surface(text: &str) -> InputSurface {
    let lines = rows(text);
    let Some((index, first)) = composer(&lines) else { return InputSurface::ShellOrUnknown; };
    if numbered(&first) {
        return if modal(&lines, index).is_some() { InputSurface::StartupRiskWarning } else { InputSurface::ShellOrUnknown };
    }
    if starting(&lines) { return InputSurface::Starting; }
    if !empty(&first) { return InputSurface::ComposerContainsPaste; }
    if busy(&lines, index) { InputSurface::Busy } else { InputSurface::ComposerReady }
}
fn uuid(text: &str) -> bool {
    text.len() == 36 && text.bytes().enumerate().all(|(i, byte)| if [8, 13, 18, 23].contains(&i) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}
/// The last `/status` card: rows after its `>_ OpenAI Codex (vX)` header, up to the composer.
fn status_card<'a>(lines: &[&'a str]) -> Option<(&'a str, &[&'a str])> {
    let end = composer(lines).map_or(lines.len(), |(index, _)| index);
    let header = lines[..end].iter().rposition(|line| line.contains(">_ OpenAI Codex (v"))?;
    let version = lines[header].trim().strip_prefix(">_ OpenAI Codex (v")?.strip_suffix(')')?;
    Some((version, &lines[header + 1..end]))
}
fn field<'a>(rows: &[&'a str], label: &str) -> Result<&'a str, ContractError> {
    let mut values = rows.iter().filter_map(|row| row.trim().strip_prefix(label)?.strip_prefix(':').map(str::trim));
    let value = values.next().ok_or(ContractError::Invalid("Codex status card field"))?;
    if values.next().is_some() { return Err(ContractError::Invalid("Codex status card field")); }
    Ok(value)
}
/// Only a `/status` card for this exact owned cwd binds a native ID. A UUID in
/// chat output or a requested `resume` argument cannot. Returns (id, version).
pub fn session_id(text: &str, cwd: &Path) -> Result<(NativeSessionId, String), ContractError> {
    let lines = rows(text);
    let (version, card) = status_card(&lines).ok_or(ContractError::Invalid("Codex status card"))?;
    let id = field(card, "Session")?;
    if !uuid(id) || Some(field(card, "Directory")?) != cwd.to_str() { return Err(ContractError::Invalid("Codex status card")); }
    Ok((NativeSessionId::new(id)?, version.into()))
}
fn status_rows(text: &str) -> usize { rows(text).iter().filter(|row| row.trim().strip_prefix("Session:").is_some_and(|value| uuid(value.trim()))).count() }
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
fn placeholder_chars(first: &str) -> Option<usize> {
    let count = first.strip_prefix("[Pasted Content ")?.strip_suffix(" chars]")?;
    if count.is_empty() || (count.len() > 1 && count.starts_with('0')) || !count.bytes().all(|b| b.is_ascii_digit()) { return None; }
    count.parse().ok()
}
/// A folded paste shows only `[Pasted Content N chars]`. Seen requires this
/// attempt's confirmed paste into an empty composer and a count no larger than
/// the immutable payload; acceptance requires the full token in history.
fn folded(frame: &CaptureFrame, observation: &mut InteractionObservation) -> bool {
    let owned = matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } | PasteLatch::Gone { native_identity } if native_identity.starts_with(FOLDED));
    let (Some(identity), Some(input), Some(before), Some(message)) = (folded_identity(frame), &frame.input, &frame.baseline, &frame.message) else { return owned; };
    let token = format!("[team-agent-token:{}]", message.as_str());
    let lines = rows(&frame.text);
    let Some((index, first)) = composer(&lines) else { return owned; };
    let correlate = |observation: &mut InteractionObservation| {
        observation.current_message = frame.message.clone(); observation.current_attempt = frame.attempt.clone();
    };
    if frame.after_step == Some(StepKind::Paste) && current_subject(frame) && !before.text.contains(&token)
        && placeholder_chars(&first).is_some_and(|count| count > FOLD_CHARS && count <= input.bytes)
        && !before.text.contains(&first)
        && (matches!(frame.paste_latch, PasteLatch::NeverSeen)
            || matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity))
    {
        observation.paste_latch = PasteLatch::Seen { native_identity: identity };
        observation.surface = InputSurface::ComposerContainsPaste; correlate(observation); return true;
    }
    if !matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity) { return owned; }
    if frame.after_step == Some(StepKind::InitialSubmit) && empty(&first) && !before.text.contains(&token)
        && lines[..index].iter().any(|line| line.contains(&token))
        && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
    {
        observation.surface = InputSurface::NativeAccepted;
        observation.paste_latch = PasteLatch::Gone { native_identity: identity }; correlate(observation);
    }
    owned
}
impl InteractionHook for CodexAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let mut observation = InteractionObservation { scope: frame.scope.clone(), surface: InputSurface::ShellOrUnknown,
            predicate: None, current_message: None, current_attempt: None, paste_latch: frame.paste_latch.clone(), control_paste: None };
        if frame.profile_id != PROFILE { return observation; }
        observation.surface = surface(&frame.text);
        let lines = rows(&frame.text);
        let current = composer(&lines);
        if let Some((index, first)) = &current {
            if numbered(first) { observation.predicate = modal(&lines, *index).map(Into::into); }
        }
        if matches!(frame.operation, Operation::FirstBusiness | Operation::OrdinarySend) {
            if !folded(frame, &mut observation) {
                let tail = current.as_ref().map(|(index, _)| lines[*index..].join("\n"));
                correlate_business_input(frame, &mut observation, tail.as_deref());
            }
            return observation;
        }
        if !matches!(frame.operation, Operation::SessionInspect | Operation::Stop) || frame.message.is_some() { return observation; }
        let command = if frame.operation == Operation::SessionInspect { "/status" } else { "/exit" };
        let typed = current.as_ref().is_some_and(|(_, first)| first == command);
        if frame.after_step == Some(StepKind::Paste) && current_subject(frame) && typed {
            observation.paste_latch = PasteLatch::Seen { native_identity: command.into() };
            observation.current_attempt = frame.attempt.clone();
        }
        if frame.operation == Operation::SessionInspect && frame.after_step == Some(StepKind::InitialSubmit) && !typed
            && matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == command)
            && frame.cwd.as_ref().is_some_and(|cwd| session_id(&frame.text, cwd).is_ok())
            && frame.baseline.as_ref().is_some_and(|before| status_rows(&frame.text) > status_rows(&before.text))
        {
            observation.surface = InputSurface::NativeAccepted;
            observation.paste_latch = PasteLatch::Gone { native_identity: command.into() };
            observation.current_attempt = frame.attempt.clone();
        }
        observation
    }
}
impl SessionHook for CodexAdapter {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.provider.as_str() != super::ID || !PROFILES[0].matches_native(&evidence.native)
            || evidence.origin != CaptureOrigin::CurrentNativeSession || evidence.evidence_sha256 != crate::host::digest(&evidence.record)
        { return Err(ContractError::Mismatch("Codex session provenance")); }
        let text = std::str::from_utf8(&evidence.record).map_err(|_| ContractError::Invalid("Codex session UTF-8"))?;
        let (native_session, version) = session_id(text, &evidence.cwd.path)?;
        if version != evidence.native.version { return Err(ContractError::Mismatch("Codex status card version")); }
        Ok(ResumeBinding { provider: evidence.provider.clone(), native_session,
            storage: SessionStorage::Local, cwd: evidence.cwd.clone(), native: evidence.native.clone(),
            source: evidence.scope.identity.clone(), origin: evidence.origin, evidence_kind: evidence.evidence_kind,
            evidence_sha256: evidence.evidence_sha256, backing: None })
    }
}
impl SemanticReader for CodexAdapter {
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
