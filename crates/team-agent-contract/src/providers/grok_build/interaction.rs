//! Grok Build native TUI grammar. Folded paste is an owned input transition, not
//! a claim to have read hidden tokens. All actions remain in the shared executor.
use super::{GrokAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, session::*, types::*};
use crate::runtime::delivery::{ControlDefinition, ControlInputMode, ControlKind, StartupDefinition};
use std::{path::Path, time::Duration};
use unicode_width::UnicodeWidthStr;

pub const PROFILE: &str = "grok-build-tui-v1";
pub const STARTUP_PROFILE: &str = "grok-build-trust-v1";
pub const POLICY_RECIPE: &str = "grok-build-tui-v1/r5:runtime-native-image+wrapper;owned-server-ambient-coexistence;120x40;paste-bracketed;enter-once;retry-never;queue-owned-visible-token+Queued · Enter to send now|Enter:send now:Enter-once:150ms;folded-capsule-still-in-composer:same-Enter-once;capture100ms;stable200ms;paste-floor100ms;deadline30s;trust-v1:exact-cwd+bypass:y-once;session-info:direct+unique-closed-modal-own-width+current-uuid+cwd+version:Escape-once;exit:direct+native-exited+dead-pane-close;folded-v3:empty-baseline+confirmed-paste+payload-shape-bytes+lines-or-kib-rounded-0-to-3-decimals+attempt+closed-preview-unicode-width-0.2.2-header-and-token->confirmed-enter+new-unboxed-transcript-prefix+empty-composer+busy-or-ready";
pub const POLICY: Digest = Digest([165, 117, 153, 51, 155, 109, 234, 245, 92, 96, 176, 156, 210, 3, 205, 181, 202, 35, 40, 73, 148, 167, 220, 151, 249, 103, 123, 119, 61, 175, 204, 123]);
const QUEUE: &str = "grok-owned-queue-send-now";
pub static PROFILES: &[InputProfile] = &[InputProfile {
    id: PROFILE, identity: ProfileIdentity::RuntimeCaptured, harness: "grok-build", ui: "tui",
    platform: Platform::MacOs, policy_sha256: POLICY,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend, Operation::SessionInspect, Operation::Stop],
    channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed, payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter, confirmation_steps: &[], retry_budget: RetryBudget::Never,
        queue_flush: &[QueueAction { predicate: QUEUE, key: PhysicalKey::Enter, max_presses: 1, interval: Duration::from_millis(150) }],
        max_submit_keys: 2,
        timing: InputTiming { capture_interval: Duration::from_millis(100), stable_window: Duration::from_millis(200), paste_to_submit_floor: Duration::from_millis(100), deadline: Duration::from_secs(30) },
    }),
}, InputProfile {
    id: STARTUP_PROFILE, identity: ProfileIdentity::RuntimeCaptured, harness: "grok-build", ui: "tui",
    platform: Platform::MacOs, policy_sha256: POLICY, operations: &[Operation::StartupBypassAck], channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed, payload_trailer: PayloadTrailer::None, initial_submit: PhysicalKey::Enter,
        confirmation_steps: &[], retry_budget: RetryBudget::Never, queue_flush: &[], max_submit_keys: 1,
        timing: InputTiming { capture_interval: Duration::from_millis(100), stable_window: Duration::from_millis(200), paste_to_submit_floor: Duration::ZERO, deadline: Duration::from_secs(30) },
    }),
}];
pub static CONTROLS: &[ControlDefinition] = &[ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::SessionInspect,
    kind: ControlKind::InspectSession, command: "/session-info", input_mode: ControlInputMode::DirectTyping,
}, ControlDefinition {
    profile_id: PROFILE, policy_sha256: POLICY, operation: Operation::Stop,
    kind: ControlKind::Exit, command: "/exit", input_mode: ControlInputMode::DirectTyping,
}];
pub static STARTUP: StartupDefinition = StartupDefinition {
    profile_id: STARTUP_PROFILE, policy_sha256: POLICY, predicate: "grok-owned-directory-trust", key: PhysicalKey::ConfirmYes,
};
pub static SESSION_PANEL: crate::runtime::native_panel::SessionPanelPolicy = crate::runtime::native_panel::SessionPanelPolicy {
    profile_id: PROFILE, policy_sha256: POLICY, predicate: "grok-session-info-panel",
};

fn boxed(line: &str) -> Option<&str> { line.trim().strip_prefix('│')?.strip_suffix('│').map(str::trim) }
fn composer(text: &str) -> Option<(usize, String)> {
    let lines: Vec<_> = text.lines().collect();
    let index = lines.iter().rposition(|line| boxed(line).is_some_and(|line| line.starts_with('❯')))?;
    if index == 0 || !lines[index - 1].trim().starts_with('╭') { return None; }
    let mut input = vec![boxed(lines[index])?.strip_prefix('❯')?.trim()];
    for line in &lines[index + 1..] {
        if line.trim().starts_with('╰') && line.contains(" Grok ") {
            return Some((index, input.join("\n")));
        }
        input.push(boxed(line)?);
    }
    None
}
fn queue_visible(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line == "Queued · Enter to send now" || line.contains("Enter:send now")
    })
}
fn folded_capsule_still_owned(frame: &CaptureFrame) -> bool {
    let Some(input) = frame.input.as_ref() else { return false; };
    let Some((_, current)) = composer(&frame.text) else { return false; };
    !current.is_empty()
        && folded_marker_matches(&current, input)
        && matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity.starts_with("grok-owned-folded-v3:"))
}
pub fn surface(text: &str) -> InputSurface {
    let Some((index, input)) = composer(text) else { return InputSurface::ShellOrUnknown; };
    if !input.is_empty() { return InputSurface::ComposerContainsPaste; }
    if queue_visible(text) { return InputSurface::Queued; }
    let status = text.lines().take(index - 1).map(str::trim).filter(|line| !line.is_empty()).last();
    if status.is_some_and(|line| {
        line.split_once(' ').is_some_and(|(spinner, rest)| spinner.chars().count() == 1
            && spinner.chars().all(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch))
            && rest.starts_with("Waiting for response… "))
            || (line.ends_with("[stop]") && line.contains('⇣'))
    }) { InputSurface::Busy } else { InputSurface::ComposerReady }
}
fn trust(frame: &CaptureFrame) -> bool {
    let Some(cwd) = frame.cwd.as_ref().and_then(|path| path.to_str()) else { return false; };
    let lines: Vec<_> = frame.text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    composer(&frame.text).is_none()
        && lines.iter().filter(|line| **line == "Do you trust the contents of this directory?").count() == 1
        && lines.iter().filter(|line| **line == cwd).count() == 1
        && lines.iter().any(|line| line.split_whitespace().collect::<Vec<_>>() == ["Yes,", "proceed", "y"])
        && lines.iter().any(|line| line.split_whitespace().collect::<Vec<_>>() == ["No,", "quit", "n"])
        && lines.contains(&"Grok Build may run or modify contents in this directory,")
}
#[derive(Clone, Copy)]
enum NativeRectangle { Session, PastePreview }
// Native overlays float above restored transcript text and the scrollbar. Their
// byte/character offsets are NOT terminal columns (background may be wide).
// Match each measured single-cell rectangle's own width; never change `boxed`
// for the composer or broaden the unboxed post-submit transcript matcher.
fn rectangle_rows<'a>(lines: &[&'a str], kind: NativeRectangle) -> Result<Vec<&'a str>, ContractError> {
    let invalid = || ContractError::Invalid("Grok native rectangle");
    let horizontal = |text: &str| !text.is_empty() && text.chars().all(|ch| ch == '─');
    let captioned = |text: &str, caption: &str| text.split_once(caption)
        .is_some_and(|(before, after)| horizontal(before) && horizontal(after));
    let (top_left, top_right, bottom_left, bottom_right) = match kind {
        NativeRectangle::Session => ('┌', '┐', '└', '┘'),
        NativeRectangle::PastePreview => ('╭', '╮', '╰', '╯'),
    };
    let mut tops = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        for (start, _) in line.match_indices(top_left) {
            let rest = &line[start + top_left.len_utf8()..];
            let Some((inside, _)) = rest.split_once(top_right) else { continue; };
            if horizontal(inside) || (matches!(kind, NativeRectangle::Session) && captioned(inside, " [✗] ")) {
                tops.push((row, inside.chars().count()));
            }
        }
    }
    let [(top, width)] = tops.as_slice() else { return Err(invalid()); };
    let mut rows = Vec::new();
    for line in &lines[*top + 1..] {
        let bottoms = line.match_indices(bottom_left).filter(|(start, _)| {
            line[*start + bottom_left.len_utf8()..].split_once(bottom_right).is_some_and(|(inside, _)| {
                inside.chars().count() == *width && match kind {
                    NativeRectangle::Session => horizontal(inside),
                    NativeRectangle::PastePreview => captioned(inside, " paste again or double-click to expand "),
                }
            })
        }).count();
        if bottoms != 0 {
            return if bottoms == 1 { Ok(rows) } else { Err(invalid()) };
        }
        let mut candidates = line.match_indices('│').filter_map(|(start, _)| {
            let rest = &line[start + '│'.len_utf8()..];
            let mut edges = rest.char_indices().filter(|(end, edge)|
                *edge == '│' && UnicodeWidthStr::width(&rest[..*end]) == *width);
            let (end, _) = edges.next()?;
            if edges.next().is_some() { return None; }
            Some(rest[..end].trim())
        });
        let row = candidates.next().ok_or_else(invalid)?;
        if candidates.next().is_some() { return Err(invalid()); }
        if !row.is_empty() { rows.push(row); }
    }
    Err(invalid()) // No closed bottom edge: never bind a clipped/partial panel.
}
fn session_rows(text: &str) -> Result<Vec<&str>, ContractError> {
    rectangle_rows(&text.lines().collect::<Vec<_>>(), NativeRectangle::Session)
}
fn uuid(text: &str) -> bool {
    text.len() == 36 && text.bytes().enumerate().all(|(i, byte)| if [8,13,18,23].contains(&i) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}
/// Only the measured modal panel, with the exact owned working directory, binds
/// a native ID. A UUID in chat output or a requested --resume argument cannot.
pub fn session_id(text: &str, cwd: &Path) -> Result<NativeSessionId, ContractError> {
    let invalid = || ContractError::Invalid("Grok session info panel");
    let lines = session_rows(text)?;
    if !lines.iter().any(|line| line.starts_with("Session info   click or drag to copy"))
        || !lines.contains(&"Esc close") || !lines.iter().any(|line| line.starts_with("Tab switch  |"))
        || !lines.iter().any(|line| line.starts_with("Model: "))
    { return Err(invalid()); }
    let value = |label| {
        let mut values = lines.windows(2).filter(|pair| pair[0] == label).map(|pair| pair[1]);
        let value = values.next().ok_or_else(invalid)?;
        if values.next().is_some() { return Err(invalid()); }
        Ok(value)
    };
    let id = value("Session ID:")?;
    if !uuid(id) || Some(value("Working directory:")?) != cwd.to_str() { return Err(invalid()); }
    NativeSessionId::new(id)
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
    let context = serde_json::to_string(&(&frame.scope.identity, &frame.scope.endpoint, &frame.scope.pane,
        &frame.scope.binding, frame.scope.session.as_ref()?, frame.message.as_ref()?, frame.attempt.as_ref()?,
        frame.operation == Operation::FirstBusiness, input.sha256, input.bytes, input.lines, &input.first_line)).ok()?;
    Some(format!("grok-owned-folded-v3:{context}"))
}
fn visible_prefix(line: &str, first: &str) -> bool {
    let line = boxed(line).unwrap_or(line.trim());
    let visible = line.split("  ").next().unwrap_or(line).trim_end_matches('…').trim();
    // A timestamp/wrapped/truncated prefix must still carry a nontrivial exact
    // fragment of the immutable payload, never any arbitrary transcript row.
    visible.len() >= 24 && (visible.starts_with(first) || first.starts_with(visible))
}
fn folded_marker_matches(current: &str, input: &InputShape) -> bool {
    if current == format!("[Pasted: {} lines]", input.lines) { return true; }
    let Some(size) = current.strip_prefix("[Pasted: ").and_then(|s| s.strip_suffix(" KB]")) else { return false; };
    let (whole, fraction) = size.split_once('.').map_or((size, ""), |(whole, fraction)| (whole, fraction));
    if whole.is_empty() || whole.len() > 12 || (whole.len() > 1 && whole.starts_with('0'))
        || !whole.bytes().all(|b| b.is_ascii_digit()) || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit()) || (size.contains('.') && fraction.is_empty())
        || input.bytes == 0
    { return false; }
    let Ok(whole) = whole.parse::<u128>() else { return false; };
    let decimals = fraction.len() as u32;
    let scale = 10u128.pow(decimals);
    let fraction = if fraction.is_empty() { 0 } else {
        let Ok(value) = fraction.parse::<u128>() else { return false; }; value
    };
    // A rounded native size label is metadata, never the payload ownership proof.
    // Compare its displayed precision with executor-owned bytes using integer math.
    let shown = whole * scale + fraction;
    shown > 0 && (input.bytes as u128 * scale + 512) / 1024 == shown
}
fn folded(frame: &CaptureFrame, observation: &mut InteractionObservation) -> bool {
    let owned = matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } | PasteLatch::Gone { native_identity } if native_identity.starts_with("grok-owned-folded-v3:"));
    let Some(identity) = folded_identity(frame) else { return owned; };
    let Some(input) = &frame.input else { return owned; };
    let Some(before) = &frame.baseline else { return owned; };
    let token = format!("[team-agent-token:{}]", frame.message.as_ref().unwrap().as_str());
    if !current_subject(frame) || before.text.contains(&token) || input.first_line.len() < 24 { return owned; }
    let Some((index, current)) = composer(&frame.text) else { return owned; };
    let correlate = |observation: &mut InteractionObservation| {
        observation.current_message = frame.message.clone(); observation.current_attempt = frame.attempt.clone();
    };
    if frame.after_step == Some(StepKind::Paste) && folded_marker_matches(&current, input)
        && input.lines > 1 && !before.text.contains(&current)
        && rectangle_rows(&frame.text.lines().take(index - 1).collect::<Vec<_>>(), NativeRectangle::PastePreview)
            .is_ok_and(|rows| rows.iter().any(|row| visible_prefix(row, &input.first_line))
                && rows.iter().filter(|row| **row == token.as_str()).count() == 1)
        && (matches!(frame.paste_latch, PasteLatch::NeverSeen)
            || matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity))
    {
        observation.paste_latch = PasteLatch::Seen { native_identity: identity };
        observation.surface = InputSurface::ComposerContainsPaste; correlate(observation); return true;
    }
    if !matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity) { return owned; }
    if matches!(frame.after_step, Some(StepKind::InitialSubmit | StepKind::QueueFlush)) && current.is_empty() {
        let count = |text: &str| text.lines().filter(|line| boxed(line).is_none() && !line.trim().starts_with('#') && visible_prefix(line, &input.first_line)).count();
        if count(&frame.text) == count(&before.text) + 1
            && !queue_visible(&frame.text) && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
        {
            observation.surface = InputSurface::NativeAccepted;
            observation.paste_latch = PasteLatch::Gone { native_identity: identity }; correlate(observation);
        }
    }
    owned
}
impl InteractionHook for GrokAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let mut observation = InteractionObservation { scope: frame.scope.clone(), surface: InputSurface::ShellOrUnknown,
            predicate: None, current_message: None, current_attempt: None, paste_latch: frame.paste_latch.clone(), control_paste: None };
        if frame.profile_id != PROFILE && !(frame.profile_id == STARTUP_PROFILE && frame.operation == Operation::StartupBypassAck) { return observation; }
        observation.surface = surface(&frame.text);
        if frame.operation == Operation::StartupBypassAck && trust(frame) {
            observation.surface = InputSurface::StartupRiskWarning; observation.predicate = Some(STARTUP.predicate.into());
            observation.current_attempt = frame.attempt.clone(); return observation;
        }
        let panel = frame.cwd.as_ref().is_some_and(|cwd| session_id(&frame.text, cwd).is_ok());
        if panel { observation.surface = InputSurface::Starting; observation.predicate = Some(SESSION_PANEL.predicate.into()); }
        if matches!(frame.operation, Operation::FirstBusiness | Operation::OrdinarySend) {
            if !folded(frame, &mut observation) {
                let current = composer(&frame.text).map(|(_, input)| input);
                correlate_business_input(frame, &mut observation, current.as_deref());
            }
            let token = frame.message.as_ref().map(|id| format!("[team-agent-token:{}]", id.as_str()));
            let owned_visible = token.as_ref().is_some_and(|token| matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == token))
                && frame.attempt.is_some() && current_subject(frame)
                && token.as_ref().is_some_and(|token| frame.baseline.as_ref().is_some_and(|before| !before.text.contains(token)));
            let queued_owned = frame.after_step == Some(StepKind::InitialSubmit) && owned_visible && queue_visible(&frame.text)
                && token.as_ref().is_some_and(|token| frame.text.contains(token));
            let swallowed_fold = frame.after_step == Some(StepKind::InitialSubmit)
                && observation.surface != InputSurface::NativeAccepted
                && folded_capsule_still_owned(frame);
            if queued_owned || swallowed_fold {
                observation.surface = InputSurface::Queued; observation.predicate = Some(QUEUE.into());
                observation.current_message = frame.message.clone(); observation.current_attempt = frame.attempt.clone();
            }
            if frame.after_step == Some(StepKind::QueueFlush) && owned_visible && !queue_visible(&frame.text)
                && composer(&frame.text).is_some_and(|(_, input)| input.is_empty())
                && token.as_ref().is_some_and(|token| frame.text.contains(token))
                && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
            {
                observation.surface = InputSurface::NativeAccepted;
                observation.paste_latch = PasteLatch::Gone { native_identity: token.unwrap() };
                observation.current_message = frame.message.clone(); observation.current_attempt = frame.attempt.clone();
            }
            return observation;
        }
        if !matches!(frame.operation, Operation::SessionInspect | Operation::Stop) || frame.message.is_some() { return observation; }
        let command = if frame.operation == Operation::SessionInspect { "/session-info" } else { "/exit" };
        if frame.after_step == Some(StepKind::Paste) && current_subject(frame)
            && composer(&frame.text).is_some_and(|(_, input)| input == command)
        {
            observation.paste_latch = PasteLatch::Seen { native_identity: command.into() };
            observation.current_attempt = frame.attempt.clone();
        }
        if frame.operation == Operation::SessionInspect && frame.after_step == Some(StepKind::InitialSubmit) && panel
            && matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == command)
        {
            observation.surface = InputSurface::NativeAccepted;
            observation.paste_latch = PasteLatch::Gone { native_identity: command.into() };
            observation.current_attempt = frame.attempt.clone();
        }
        observation
    }
}
impl SessionHook for GrokAdapter {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.provider.as_str() != "grok_build" || !PROFILES[0].matches_native(&evidence.native)
            || evidence.origin != CaptureOrigin::CurrentNativeSession || evidence.evidence_sha256 != crate::host::digest(&evidence.record)
        { return Err(ContractError::Mismatch("Grok session provenance")); }
        let text = std::str::from_utf8(&evidence.record).map_err(|_| ContractError::Invalid("Grok session UTF-8"))?;
        let rows = session_rows(text)?;
        let expected = evidence.native.version.split_once(" (").map(|(version, _)| version);
        let versions: Vec<_> = rows.windows(2).filter(|pair| pair[0] == "Shell version:").map(|pair| pair[1]).collect();
        if versions.len() != 1 || versions.first().copied() != expected {
            return Err(ContractError::Mismatch("Grok session panel version"));
        }
        Ok(ResumeBinding { provider: evidence.provider.clone(), native_session: session_id(text, &evidence.cwd.path)?,
            storage: SessionStorage::Local, cwd: evidence.cwd.clone(), native: evidence.native.clone(),
            source: evidence.scope.identity.clone(), origin: evidence.origin, evidence_kind: evidence.evidence_kind,
            evidence_sha256: evidence.evidence_sha256, backing: None })
    }
}
impl SemanticReader for GrokAdapter {
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
