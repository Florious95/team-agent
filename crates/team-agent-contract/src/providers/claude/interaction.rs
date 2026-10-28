//! Claude Code TUI grammar, measured from tmux 120x40 captures of 2.1.282 and
//! 2.1.309 (bracketed `paste-buffer -r -S`, the contract host's own path).
//! Recognizing a surface never authorizes a key: every action stays in the
//! shared executor, and no startup screen is ever answered.
//!
//! Claude hides its spinner row while reply text streams, so such a frame reads
//! ready. Input submitted then joins Claude's own queue: the full turn (token
//! included) is drawn above the composer, whose placeholder becomes
//! `Press up to edit queued messages`, and Claude submits it at the turn
//! boundary with no further key. That native custody is the acceptance.
use super::{ClaudeAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, session::*, types::*};
use crate::runtime::delivery::{ControlDefinition, ControlInputMode, ControlKind};
use std::time::Duration;

pub const PROFILE: &str = "claude-tui-v1";
pub const POLICY_RECIPE: &str = "claude-tui-v1/r3:runtime-native-image-or-version-named+wrapper;--session-id-derived|--resume-exact;owned-argv-server+allowedTools-ambient-coexistence;120x40;paste-bracketed;enter-once;retry-never;queue-native:token-above-composer+placeholder-Press up to edit queued messages=busy;composer:plain-or-named-column0-top-rule+last-bottom-rule,first-row-❯+U+00A0(history-turns:❯+space);busy:spinner-row-·✢✳✶✻✽-Verb…-within-6-rows-above-composer;streaming-text-hides-spinner:reads-ready;folder-trust:no-exit-default:never-keyed;fold:any-newline:[Pasted text #N +M lines]:M=newlines|single-line:[Pasted text #N]:empty-baseline+confirmed-paste+shape+attempt->confirmed-enter+empty-composer+token-in-history+busy-or-ready;inline:token-in-composer->token-in-history;exit:direct+/exit+native-exited+dead-pane-close;capture100ms;stable200ms;paste-floor100ms;deadline30s";
pub const POLICY: Digest = Digest([178, 198, 68, 48, 51, 90, 207, 89, 169, 154, 154, 185, 201, 251, 116, 148, 231, 78, 12, 216, 74, 200, 194, 255, 254, 158, 111, 107, 50, 81, 4, 179]);
const FOLDED: &str = "claude-owned-paste-v1:";
const TRUST: &str = "claude-folder-trust-modal";
/// The composer placeholder while Claude holds queued input for the turn boundary.
const QUEUED: &str = "Press up to edit queued messages";
pub static PROFILES: &[InputProfile] = &[InputProfile {
    id: PROFILE, identity: ProfileIdentity::RuntimeCaptured, harness: "claude", ui: "tui",
    platform: Platform::MacOs, policy_sha256: POLICY,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend, Operation::Stop],
    channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed, payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter, confirmation_steps: &[], retry_budget: RetryBudget::Never,
        queue_flush: &[], max_submit_keys: 1,
        timing: InputTiming { capture_interval: Duration::from_millis(100), stable_window: Duration::from_millis(200), paste_to_submit_floor: Duration::from_millis(100), deadline: Duration::from_secs(30) },
    }),
}];
pub static CONTROLS: &[ControlDefinition] = &[ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::Stop,
    kind: ControlKind::Exit, command: "/exit", input_mode: ControlInputMode::DirectTyping,
}];

fn rows(text: &str) -> Vec<&str> { text.lines().map(str::trim_end).collect() }
/// A full-width rule drawn from column 0; transcript text is indented.
fn rule(line: &str) -> bool { line.chars().count() >= 40 && line.chars().all(|ch| ch == '─') }
/// Native `--name` decorates only the top composer border.
fn top_rule(line: &str) -> bool {
    rule(line) || (line.starts_with("─── ") && line.ends_with("───")
        && !line.trim_matches('─').trim().is_empty())
}
/// The live composer is the region between the last two rules; its first row
/// is `❯` + U+00A0 (2.1.282 and 2.1.309 alike). History user turns start with
/// `❯` + an ordinary space and sit above it. Returns (top rule index, composer
/// text with continuation indents removed); the queued placeholder is drawn there too.
fn composer(lines: &[&str]) -> Option<(usize, String)> {
    let bottom = lines.iter().rposition(|line| rule(line))?;
    let top = lines[..bottom].iter().rposition(|line| top_rule(line))?;
    let region = &lines[top + 1..bottom];
    let first = region.first()?;
    let first = if *first == "❯" { "" } else { first.strip_prefix("❯\u{a0}")? };
    let mut text = vec![first.trim()];
    text.extend(region[1..].iter().map(|line| line.trim()));
    Some((top, text.join("\n").trim().to_owned()))
}
/// `✻ Sprouting…`, `✽ Beboppin'…` or `✢ Boondoggling… (3s · thinking)` just above the composer.
/// A finished turn's past-tense summary carries no ellipsis.
fn spinner(line: &str) -> bool {
    let mut chars = line.chars();
    if !chars.next().is_some_and(|glyph| "·✢✳✶✻✽".contains(glyph)) || chars.next() != Some(' ') { return false; }
    let Some((verb, rest)) = chars.as_str().split_once('…') else { return false; };
    verb.chars().next().is_some_and(|ch| ch.is_ascii_uppercase()) && verb.chars().all(|ch| ch.is_ascii_alphabetic() || ch == '\'' || ch == '-')
        && (rest.is_empty() || (rest.starts_with(" (") && rest.ends_with(')')))
}
fn busy(lines: &[&str], top: usize) -> bool { lines[top.saturating_sub(6)..top].iter().any(|line| spinner(line)) }
/// A live spinner row, for legacy observers that locate it themselves.
pub fn spinner_row(line: &str) -> bool { spinner(line.trim_end()) }
/// Legacy `claude`/`claude_code` observation only: those captures normalize
/// U+00A0 to a space, so the live composer is the region between the last two
/// column-0 rules alone. Busy when the queued placeholder is shown or the last
/// column-0 row above the composer is a live spinner (its tip/todo rows and the
/// effort hint are indented). Returns the surface and the text a submit would
/// send. Does not change contract surface admission, create a paste latch, or
/// authorize any input.
pub fn inspect_legacy_composer(text: &str) -> Option<(InputSurface, String)> {
    let lines = rows(text);
    let bottom = lines.iter().rposition(|line| rule(line))?;
    let top = lines[..bottom].iter().rposition(|line| rule(line))?;
    let region = &lines[top + 1..bottom];
    let first = *region.first()?;
    let first = if first == "❯" { "" } else { first.strip_prefix("❯\u{a0}").or_else(|| first.strip_prefix("❯ "))? };
    let current = std::iter::once(first).chain(region[1..].iter().copied()).map(str::trim).collect::<Vec<_>>().join("\n");
    let current = current.trim();
    let status = lines[..top].iter().rev().find(|line| !line.is_empty() && !line.starts_with(' '));
    let observed = if current == QUEUED || status.is_some_and(|line| spinner(line)) { InputSurface::Busy }
        else if current.is_empty() { InputSurface::ComposerReady } else { InputSurface::ComposerContainsPaste };
    Some((observed, typed(current).to_owned()))
}
/// The folder-trust screen (no composer). Its focus defaults to `No, exit`.
fn trust(lines: &[&str]) -> bool {
    let trimmed: Vec<_> = lines.iter().map(|line| line.trim()).collect();
    let once = |text: &str| trimmed.iter().filter(|line| **line == text).count() == 1;
    once("Accessing workspace:") && once("Enter to confirm · Esc to cancel")
        && trimmed.iter().any(|line| line.starts_with("Quick safety check: Is this a project you created or one you trust?"))
        && trimmed.iter().any(|line| line.trim_start_matches('❯').trim() == "Yes, I trust this folder")
        && trimmed.iter().any(|line| line.trim_start_matches('❯').trim() == "No, exit")
}
pub fn surface(text: &str) -> InputSurface {
    let lines = rows(text);
    if trust(&lines) { return InputSurface::StartupRiskWarning; }
    let Some((top, current)) = composer(&lines) else { return InputSurface::ShellOrUnknown; };
    if current == QUEUED { return InputSurface::Busy; }
    if !current.is_empty() { return InputSurface::ComposerContainsPaste; }
    if busy(&lines, top) { InputSurface::Busy } else { InputSurface::ComposerReady }
}
/// The text the user would submit: the queued placeholder is not input.
fn typed(current: &str) -> &str { if current == QUEUED { "" } else { current } }
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
fn number(text: &str) -> Option<usize> {
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) || !text.bytes().all(|b| b.is_ascii_digit()) { return None; }
    text.parse().ok()
}
/// `[Pasted text #N +M lines]` (M newlines) or `[Pasted text #N]` (one line):
/// the placeholder must fit this payload's own shape.
fn placeholder_fits(first: &str, input: &InputShape) -> bool {
    let Some(inner) = first.strip_prefix("[Pasted text #").and_then(|rest| rest.strip_suffix(']')) else { return false; };
    match inner.split_once(" +") {
        None => number(inner).is_some() && input.lines == 1,
        Some((paste, lines)) => number(paste).is_some()
            && lines.strip_suffix(" lines").and_then(number).is_some_and(|newlines| newlines >= 1 && (newlines + 1 == input.lines || newlines == input.lines)),
    }
}
/// A folded paste hides the token. Seen requires this attempt's confirmed paste
/// into an empty composer with a placeholder fitting the immutable payload;
/// acceptance requires the full token in the expanded history turn.
fn folded(frame: &CaptureFrame, observation: &mut InteractionObservation) -> bool {
    let owned = matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } | PasteLatch::Gone { native_identity } if native_identity.starts_with(FOLDED));
    let (Some(identity), Some(shape), Some(before), Some(message)) = (folded_identity(frame), &frame.input, &frame.baseline, &frame.message) else { return owned; };
    let token = format!("[team-agent-token:{}]", message.as_str());
    let lines = rows(&frame.text);
    let Some((top, current)) = composer(&lines) else { return owned; };
    let correlate = |observation: &mut InteractionObservation| {
        observation.current_message = frame.message.clone(); observation.current_attempt = frame.attempt.clone();
    };
    if frame.after_step == Some(StepKind::Paste) && current_subject(frame) && !before.text.contains(&token)
        && placeholder_fits(&current, shape) && !before.text.contains(&current)
        && (matches!(frame.paste_latch, PasteLatch::NeverSeen)
            || matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity))
    {
        observation.paste_latch = PasteLatch::Seen { native_identity: identity };
        observation.surface = InputSurface::ComposerContainsPaste; correlate(observation); return true;
    }
    if !matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity) { return owned; }
    if frame.after_step == Some(StepKind::InitialSubmit) && typed(&current).is_empty() && !before.text.contains(&token)
        && lines[..top].iter().any(|line| line.contains(&token))
        && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
    {
        observation.surface = InputSurface::NativeAccepted;
        observation.paste_latch = PasteLatch::Gone { native_identity: identity }; correlate(observation);
    }
    owned
}
impl InteractionHook for ClaudeAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let mut observation = InteractionObservation { scope: frame.scope.clone(), surface: InputSurface::ShellOrUnknown,
            predicate: None, current_message: None, current_attempt: None, paste_latch: frame.paste_latch.clone(), control_paste: None };
        if frame.profile_id != PROFILE { return observation; }
        observation.surface = surface(&frame.text);
        let lines = rows(&frame.text);
        if observation.surface == InputSurface::StartupRiskWarning { observation.predicate = Some(TRUST.into()); return observation; }
        let current = composer(&lines);
        if matches!(frame.operation, Operation::FirstBusiness | Operation::OrdinarySend) {
            if !folded(frame, &mut observation) {
                correlate_business_input(frame, &mut observation, current.as_ref().map(|(_, text)| typed(text)));
            }
            return observation;
        }
        if frame.operation != Operation::Stop || frame.message.is_some() { return observation; }
        if frame.after_step == Some(StepKind::Paste) && current_subject(frame) && current.as_ref().is_some_and(|(_, text)| text == "/exit") {
            observation.paste_latch = PasteLatch::Seen { native_identity: "/exit".into() };
            observation.current_attempt = frame.attempt.clone();
        }
        observation
    }
}
impl SessionHook for ClaudeAdapter {
    /// The fresh ID is the one this launch passed to `--session-id` (or the
    /// exact `--resume` ID), paired by the root with a live image-fenced ready
    /// pane; it is never read back from chat output or a newest-session scan.
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.provider.as_str() != super::ID || !PROFILES[0].matches_native(&evidence.native)
            || evidence.origin != CaptureOrigin::ExplicitResumeLaunch || evidence.evidence_sha256 != crate::host::digest(&evidence.record)
        { return Err(ContractError::Mismatch("Claude explicit session provenance")); }
        let id = std::str::from_utf8(&evidence.record).ok().filter(|id| super::session_uuid(id)).ok_or(ContractError::Invalid("Claude session UUID"))?;
        Ok(ResumeBinding { provider: evidence.provider.clone(), native_session: NativeSessionId::new(id)?,
            storage: SessionStorage::Local, cwd: evidence.cwd.clone(), native: evidence.native.clone(),
            source: evidence.scope.identity.clone(), origin: evidence.origin, evidence_kind: evidence.evidence_kind,
            evidence_sha256: evidence.evidence_sha256, backing: None })
    }
}
impl SemanticReader for ClaudeAdapter {
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
