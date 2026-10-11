//! Cursor TUI grammar, token correlation and receipt-bound folded-paste transfer.
//! No guessed /session-id, /tools panel, DirectTyping business or retry loop.
use super::{native::parse_version, CursorAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, types::*};
use crate::runtime::delivery::ControlDefinition;
use std::time::Duration;

pub const PROFILE: &str = "cursor-tui-v1";
// Recipe identity, NOT an installed release hash. Framework timing bounds are
// deliberate budgets, not a claim that one recon run proved production latency.
// SHA256 cursor-tui-v1|bracketed|enter=1|trailer=none|retry=never|capture=100ms|stable=200ms|floor=100ms|deadline=30s|correlation=baseline-seen-token-gone-transcript|folded-seen=empty-current-owned-paste-v1|folded-accepted=new-transcript-block-v1-or-visible-token
pub const POLICY: Digest = Digest([
    0x68, 0x83, 0x0f, 0x7a, 0x67, 0x1d, 0xd8, 0x0a, 0x81, 0xec, 0x92, 0xb6, 0xa2, 0xe4, 0x05, 0xfd,
    0xf3, 0x64, 0xa1, 0xfa, 0x0a, 0x70, 0xbe, 0x42, 0x7b, 0xcb, 0xff, 0xd0, 0x8d, 0x16, 0xdd, 0x73,
]);
const SUBMIT: SubmitPolicy = SubmitPolicy {
    paste_mode: PasteMode::Bracketed, payload_trailer: PayloadTrailer::None,
    initial_submit: PhysicalKey::Enter, confirmation_steps: &[], retry_budget: RetryBudget::Never,
    queue_flush: &[], max_submit_keys: 1,
    timing: InputTiming {
        capture_interval: Duration::from_millis(100), stable_window: Duration::from_millis(200),
        paste_to_submit_floor: Duration::from_millis(100), deadline: Duration::from_secs(30),
    },
};
pub static PROFILES: [InputProfile; 1] = [InputProfile {
    id: PROFILE, identity: ProfileIdentity::RuntimeCaptured,
    harness: "cursor-agent", ui: "tui", platform: Platform::MacOs,
    policy_sha256: POLICY,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend],
    channel: Channel::Tmux,
    policy: Support::Supported(SUBMIT),
}];
pub const CONTROLS: &[ControlDefinition] = &[];

/// Assemble the measured keyboard recipe using explicit caller-owned bounds.
pub fn submit_policy(timing: InputTiming) -> Result<SubmitPolicy, ContractError> {
    let policy = SubmitPolicy { timing, ..SUBMIT };
    policy.validate()?;
    Ok(policy)
}
const SPINNERS: [&str; 8] = ["⠀⠞", "⠘⠆", "⠘⠣", "⠘⠤", "⠠⠛", "⠠⠜", "⠰⠰", "⠰⠳"];
fn working(line: &str) -> bool {
    let line = line.trim();
    SPINNERS.iter().any(|spinner| {
        let Some(rest) = line.strip_prefix(spinner).and_then(|text| text.strip_prefix(" Working")) else { return false; };
        if rest.is_empty() { return true; }
        let words: Vec<_> = rest.split_whitespace().collect();
        rest.starts_with("  ") && words.len() == 2
            && !words[0].is_empty() && words[0].bytes().all(|byte| byte.is_ascii_digit())
            && matches!(words[1], "token" | "tokens")
    })
}
fn placeholder(text: &str) -> bool {
    matches!(text, "Plan, search, build anything" | "Add a follow-up")
}
fn surface(text: &str) -> InputSurface {
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    if !lines.windows(2).any(|pair| pair[0] == "Cursor Agent"
        && pair[1].strip_prefix('v').is_some_and(|version| parse_version(version.as_bytes()).is_ok()))
    { return InputSurface::ShellOrUnknown; }
    let Some(index) = lines.iter().rposition(|line| line.starts_with("→ ")) else { return InputSurface::ShellOrUnknown; };
    let composer = lines[index].strip_prefix("→ ").unwrap().trim();
    // Stop hint is attached to the *current* composer, not a full-screen search.
    if composer.strip_suffix("ctrl+c to stop").is_some_and(|prefix| placeholder(prefix.trim())) {
        return InputSurface::Busy;
    }
    if !placeholder(composer) { return InputSurface::ComposerContainsPaste; }
    // The first observed Working frame precedes the cancel hint. Inspect only
    // the nearest status (and one optional Tip), never old transcript matches.
    let mut preceding = lines[..index].iter().rev().filter(|line| !line.is_empty());
    let mut status = preceding.next().copied();
    if status.is_some_and(|line| line.starts_with("Tip: ")) { status = preceding.next().copied(); }
    if status.is_some_and(working) { InputSurface::Busy } else { InputSurface::ComposerReady }
}
// A folded block is native UI identity, NOT a claim that its hidden token was
// read. Keep the latch bound to this attempt/subject; never reuse a screen label
// across generations, sessions, panes, messages or attempts.
const FOLDED_RECEIPT: &str = "cursor-owned-folded-paste-v1:";
fn folded_prefix(frame: &CaptureFrame) -> Option<String> {
    if !matches!(frame.operation, Operation::FirstBusiness | Operation::OrdinarySend) {
        return None;
    }
    let context = serde_json::to_string(&(
        &frame.scope.identity, &frame.scope.endpoint, &frame.scope.pane,
        &frame.scope.binding, frame.scope.session.as_ref()?,
        frame.message.as_ref()?, frame.attempt.as_ref()?,
        frame.operation == Operation::FirstBusiness,
    )).ok()?;
    Some(format!("{FOLDED_RECEIPT}{context}:"))
}
fn composer_lines(text: &str) -> Option<Vec<&str>> {
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    let index = lines.iter().rposition(|line| line.starts_with("→ "))?;
    Some(lines[index..].to_vec())
}
fn folded_marker(value: &str) -> Option<&str> {
    // R6 capture includes the visible `^[` prefix before the native block.
    // Normalizing this UI spelling never changes the actual pasted bytes.
    let marker = value.strip_prefix("^[").unwrap_or(value);
    let body = marker.strip_prefix("[Pasted text #")?.strip_suffix(" lines]")?;
    let (number, lines) = body.split_once(" +")?;
    if [number, lines].iter().any(|value| {
        value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit())
            || !value.parse::<u64>().is_ok_and(|number| number > 0)
    }) { return None; }
    Some(marker)
}
fn correlate_folded_input(frame: &CaptureFrame, observation: &mut InteractionObservation) -> bool {
    let latched = match &frame.paste_latch {
        PasteLatch::Seen { native_identity } | PasteLatch::Gone { native_identity }
            if native_identity.starts_with(FOLDED_RECEIPT) => Some(native_identity),
        _ => None,
    };
    // An invalid/stale folded receipt must not fall through to another strategy.
    let refuse = latched.is_some();
    let Some(prefix) = folded_prefix(frame) else { return refuse; };
    let Some(baseline) = &frame.baseline else { return refuse; };
    let expected = format!("[team-agent-token:{}]", frame.message.as_ref().unwrap().as_str());
    let same_subject = baseline.scope.identity == frame.scope.identity
        && baseline.scope.endpoint == frame.scope.endpoint && baseline.scope.pane == frame.scope.pane
        && baseline.scope.binding == frame.scope.binding && baseline.scope.session == frame.scope.session;
    if !same_subject || baseline.scope.sequence >= frame.scope.sequence
        || baseline.scope.observed_at > frame.scope.observed_at
        || surface(&baseline.text) != InputSurface::ComposerReady
        || baseline.text.contains(&expected)
    { return refuse; }
    let Some(current) = composer_lines(&frame.text) else { return refuse; };
    let Some(before) = composer_lines(&baseline.text) else { return refuse; };
    // The single arrow line may change into one folded block. Extra editable
    // lines or a different footer are not evidence of that conversion.
    let same_tail = before.iter().skip(1).any(|line| !line.is_empty())
        && current.iter().skip(1).filter(|line| !line.is_empty())
            .eq(before.iter().skip(1).filter(|line| !line.is_empty()));
    let marker = current[0].strip_prefix("→ ").and_then(folded_marker);
    let identity = if let Some(identity) = latched {
        let Some(original) = identity.strip_prefix(&prefix).and_then(folded_marker) else { return true; };
        if !matches!(frame.paste_latch, PasteLatch::Seen { .. }) { return true; }
        if frame.after_step == Some(StepKind::InitialSubmit) {
            // The executor only captures here after Confirmed InitialSubmit.
            // R7 keeps the token hidden but moves the owned capsule to history.
            // Its exact row must be new, unique and outside the empty composer;
            // Busy or disappearance alone still cannot prove acceptance.
            let current_footer: Vec<_> = current.iter().skip(1).filter(|line| !line.is_empty()).collect();
            let before_footer: Vec<_> = before.iter().skip(1).filter(|line| !line.is_empty()).collect();
            let empty_composer = current[0].strip_prefix("→ ")
                .map(|text| text.strip_suffix("ctrl+c to stop").unwrap_or(text).trim())
                .is_some_and(placeholder)
                && current.get(1) == Some(&"")
                // The leading native footer row gains usage statistics. Keep
                // its remaining rows (owned cwd) fixed; reject extra input.
                && before_footer.len() > 1 && current_footer.len() == before_footer.len()
                && current_footer[1..] == before_footer[1..]
                && current.iter().skip(1).all(|line| folded_marker(line).is_none());
            let transcript_end = frame.text.lines().count() - current.len();
            let mut new_blocks = frame.text.lines().take(transcript_end)
                .map(str::trim).filter_map(folded_marker)
                .filter(|marker| !baseline.text.contains(*marker));
            let folded_transferred = empty_composer
                && new_blocks.next() == Some(original) && new_blocks.next().is_none();
            let current_has_token = current.iter().any(|line| line.contains(&expected));
            if !current_has_token && (frame.text.contains(&expected) || folded_transferred)
                && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
            {
                observation.surface = InputSurface::NativeAccepted;
                observation.paste_latch = PasteLatch::Gone { native_identity: identity.clone() };
                observation.current_message = frame.message.clone();
                observation.current_attempt = frame.attempt.clone();
            }
            return true;
        }
        if marker != Some(original) { return true; }
        identity.clone()
    } else {
        if !matches!(frame.paste_latch, PasteLatch::NeverSeen) { return false; }
        let Some(marker) = marker else { return false; };
        format!("{prefix}{marker}")
    };
    // The shared executor reaches this capture only after Confirmed Paste (an
    // unconfirmed physical action terminates before wait_guard). No hook issues
    // keys or substitutes a marker for the host's owned-buffer/hash receipt.
    if frame.after_step == Some(StepKind::Paste) && same_tail
        && observation.surface == InputSurface::ComposerContainsPaste
    {
        observation.paste_latch = PasteLatch::Seen { native_identity: identity };
        observation.current_message = frame.message.clone();
        observation.current_attempt = frame.attempt.clone();
    }
    true
}
impl InteractionHook for CursorAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let mut observation = InteractionObservation {
            scope: frame.scope.clone(), surface: InputSurface::ShellOrUnknown,
            predicate: None, current_message: None, current_attempt: None,
            paste_latch: frame.paste_latch.clone(), control_paste: None,
        };
        if frame.profile_id != PROFILE { return observation; }
        observation.surface = surface(&frame.text);
        let lines: Vec<_> = frame.text.lines().collect();
        let composer = lines.iter().rposition(|line| line.trim().starts_with("→ "))
            .map(|index| lines[index..].join("\n"));
        if !correlate_folded_input(frame, &mut observation) {
            correlate_business_input(frame, &mut observation, composer.as_deref());
        }
        observation
    }
}
impl SemanticReader for CursorAdapter {
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
