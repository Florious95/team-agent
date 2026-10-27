//! Cursor TUI grammar, token correlation and receipt-bound folded-paste transfer.
//! No guessed /session-id, /tools panel, DirectTyping business or retry loop.
use super::{native::parse_version, CursorAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, types::*};
use crate::runtime::delivery::ControlDefinition;
use std::time::Duration;

pub const PROFILE: &str = "cursor-tui-v1";
// Recipe identity, NOT an installed release hash. Framework timing bounds are
// deliberate budgets, not a claim that one recon run proved production latency.
// SHA256 cursor-tui-v1|bracketed|enter=1|trailer=none|retry=never|capture=100ms|stable=200ms|floor=100ms|deadline=30s|correlation=baseline-seen-token-gone-transcript|folded-seen=empty-current-owned-paste-v1|folded-accepted=new-transcript-block-v1-or-visible-token|composer=footer-bounded-inline-or-standalone-arrow-v2|visible-seen=attempt-scoped-current-region-v2|visible-accepted=empty-current-new-transcript-v2
pub const POLICY: Digest = Digest([
    0x08, 0x90, 0x5f, 0x19, 0x3b, 0xf8, 0x3a, 0x4d, 0xb5, 0xe9, 0x96, 0xc9, 0x70, 0xd9, 0x64, 0xa0,
    0x63, 0x63, 0x8c, 0xf4, 0x2b, 0x99, 0xea, 0x3c, 0xe4, 0x59, 0x17, 0xdf, 0x4d, 0x79, 0xf2, 0x16,
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
fn native_footer(line: &str) -> bool {
    line.trim().strip_suffix("Run Everything")
        .is_some_and(|prefix| !prefix.trim().is_empty() && !prefix.trim_start().starts_with('→'))
}
struct ComposerView<'a> {
    start: usize,
    rows: Vec<&'a str>,
    input: String,
    footer: usize,
    busy: bool,
    empty: bool,
}
// Physical captures show inline text and a standalone `  →` followed by
// indented editable rows. Retain the whole region, not its last quoted arrow.
// Footer/mode rows are native chrome, never input or transcript-token evidence.
fn composer_view(text: &str) -> Option<ComposerView<'_>> {
    let lines: Vec<_> = text.lines().collect();
    let footer = lines.iter().rposition(|line| native_footer(line))?;
    let context: Vec<_> = lines[footer + 1..].iter().map(|line| line.trim()).filter(|line| !line.is_empty()).collect();
    if !context.first().is_some_and(|line| line.starts_with('/'))
        || context.iter().any(|line| *line == "→" || line.starts_with("→ "))
    { return None; }
    let start = lines[..footer].iter().rposition(|line| {
        let trimmed = line.trim_start();
        line.len() - trimmed.len() <= 2 && (trimmed.trim_end() == "→" || trimmed.starts_with("→ "))
    })?;
    let previous_footer = lines[..start].iter().rposition(|line| native_footer(line)).map_or(0, |index| index + 1);
    if lines[previous_footer..start].iter().any(|line| line.trim() == "→") { return None; }
    let rows: Vec<_> = lines[start..].iter().map(|line| line.trim()).collect();
    let footer = footer - start;
    let mut end = footer;
    while end > 1 && rows[end - 1].is_empty() { end -= 1; }
    if end > 1 && rows[end - 1] == "Ask (shift+tab to cycle)"
        && lines[start + end - 1].len() - lines[start + end - 1].trim_start().len() <= 2
    {
        end -= 1;
        while end > 1 && rows[end - 1].is_empty() { end -= 1; }
    }
    let first = rows[0].strip_prefix("→ ").unwrap_or("");
    let continuation = &rows[1..end];
    let single = continuation.iter().all(|line| line.is_empty());
    // A bare arrow does not prove an empty editor. Only expose its nonempty
    // expanded body; a literal placeholder plus another row is also a draft.
    if rows[0] == "→" && single { return None; }
    let stop = single && first.strip_suffix("ctrl+c to stop").is_some_and(|text| placeholder(text.trim()));
    let empty = single && (placeholder(first) || stop);
    let mut preceding = lines[..start].iter().rev().map(|line| line.trim()).filter(|line| !line.is_empty());
    let mut status = preceding.next();
    if status.is_some_and(|line| line.starts_with("Tip: ")) { status = preceding.next(); }
    let busy = stop || status.is_some_and(working);
    let input = std::iter::once(first).chain(continuation.iter().copied()).collect::<Vec<_>>().join("\n");
    Some(ComposerView { start, rows, input, footer, busy, empty })
}
fn surface(text: &str) -> InputSurface {
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    if !lines.windows(2).any(|pair| pair[0] == "Cursor Agent"
        && pair[1].strip_prefix('v').is_some_and(|version| parse_version(version.as_bytes()).is_ok()))
    { return InputSurface::ShellOrUnknown; }
    let Some(composer) = composer_view(text) else { return InputSurface::ShellOrUnknown; };
    if composer.busy { InputSurface::Busy }
    else if composer.empty { InputSurface::ComposerReady }
    else { InputSurface::ComposerContainsPaste }
}
// A folded block is native UI identity, NOT a claim that its hidden token was
// read. Keep the latch bound to this attempt/subject; never reuse a screen label
// across generations, sessions, panes, messages or attempts.
const FOLDED_RECEIPT: &str = "cursor-owned-folded-paste-v1:";
const VISIBLE_RECEIPT: &str = "cursor-owned-visible-paste-v2:";
fn receipt_prefix(frame: &CaptureFrame, kind: &str) -> Option<String> {
    if !matches!(frame.operation, Operation::FirstBusiness | Operation::OrdinarySend) {
        return None;
    }
    let context = serde_json::to_string(&(
        &frame.scope.identity, &frame.scope.endpoint, &frame.scope.pane,
        &frame.scope.binding, frame.scope.session.as_ref()?,
        frame.message.as_ref()?, frame.attempt.as_ref()?,
        frame.operation == Operation::FirstBusiness,
    )).ok()?;
    Some(format!("{kind}{context}:"))
}
fn folded_prefix(frame: &CaptureFrame) -> Option<String> { receipt_prefix(frame, FOLDED_RECEIPT) }
fn composer_lines(text: &str) -> Option<Vec<&str>> { Some(composer_view(text)?.rows) }
fn current_baseline(frame: &CaptureFrame, baseline: &CaptureBaseline) -> bool {
    baseline.scope.identity == frame.scope.identity
        && baseline.scope.endpoint == frame.scope.endpoint && baseline.scope.pane == frame.scope.pane
        && baseline.scope.binding == frame.scope.binding && baseline.scope.session == frame.scope.session
        && baseline.scope.sequence < frame.scope.sequence && baseline.scope.observed_at <= frame.scope.observed_at
}
fn same_footer(before: &ComposerView<'_>, current: &ComposerView<'_>) -> bool {
    before.rows[before.footer + 1..].iter().filter(|line| !line.is_empty())
        .eq(current.rows[current.footer + 1..].iter().filter(|line| !line.is_empty()))
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
/// Read-only native composer view; ownership/correlation remains the caller's job.
/// Reuses the contract grammar without granting acceptance or authorizing a key.
pub fn inspect_composer(text: &str) -> Option<(InputSurface, Vec<&str>, Option<&str>, bool)> {
    let observed = surface(text);
    if matches!(observed, InputSurface::ShellOrUnknown) {
        return None;
    }
    let composer = composer_view(text)?;
    let marker = composer.rows.first()
        .and_then(|line| line.strip_prefix("→ "))
        .and_then(|value| folded_marker(value.trim()));
    Some((observed, composer.rows, marker, composer.busy))
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
    if !current_baseline(frame, baseline) || surface(&baseline.text) != InputSurface::ComposerReady
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
            let transcript_has_token = composer_view(&frame.text).is_some_and(|view| {
                frame.text.lines().take(view.start).map(|line| line.matches(&expected).count()).sum::<usize>() == 1
            });
            if !current_has_token && empty_composer && (transcript_has_token || folded_transferred)
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
fn correlate_visible_input(frame: &CaptureFrame, observation: &mut InteractionObservation) {
    let Some(prefix) = receipt_prefix(frame, VISIBLE_RECEIPT) else { return; };
    let Some(baseline) = &frame.baseline else { return; };
    let expected = format!("[team-agent-token:{}]", frame.message.as_ref().unwrap().as_str());
    if !current_baseline(frame, baseline) || baseline.text.contains(&expected)
        || surface(&baseline.text) != InputSurface::ComposerReady
    { return; }
    let (Some(before), Some(current)) = (composer_view(&baseline.text), composer_view(&frame.text)) else { return; };
    if !same_footer(&before, &current) { return; }
    let identity = format!("{prefix}{expected}");
    let pending = current.input.matches(&expected).count() == 1;
    match (&frame.paste_latch, frame.after_step) {
        (PasteLatch::NeverSeen, Some(StepKind::Paste))
        | (PasteLatch::Seen { .. }, Some(StepKind::Paste))
            if pending && observation.surface == InputSurface::ComposerContainsPaste
                && (matches!(frame.paste_latch, PasteLatch::NeverSeen)
                    || matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &identity)) =>
        {
            observation.paste_latch = PasteLatch::Seen { native_identity: identity };
        }
        (PasteLatch::Seen { native_identity }, Some(StepKind::InitialSubmit))
            if native_identity == &identity && current.empty
                && matches!(observation.surface, InputSurface::Busy | InputSurface::ComposerReady)
                && frame.text.lines().take(current.start).collect::<Vec<_>>().join("\n").matches(&expected).count() == 1 =>
        {
            observation.surface = InputSurface::NativeAccepted;
            observation.paste_latch = PasteLatch::Gone { native_identity: identity };
        }
        _ => return,
    }
    observation.current_message = frame.message.clone();
    observation.current_attempt = frame.attempt.clone();
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
        if !correlate_folded_input(frame, &mut observation) {
            correlate_visible_input(frame, &mut observation);
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

#[cfg(test)]
mod composer_observation_tests {
    use super::*;

    fn frame(composer: &str, history: &str) -> String {
        format!("Cursor Agent\nv2026.10.01-e373342\n{history}\n→ {composer}\nGrok 4.7 High Run Everything\n/owned/project\n")
    }

    #[test]
    fn inspect_composer_observes_only_current_label_without_granting_seen() {
        let label = "[Pasted text #1 +25 lines]";
        let draft = frame(label, "old history");
        let (surface, _, current, _) = inspect_composer(&draft).unwrap();
        assert_eq!(surface, InputSurface::ComposerContainsPaste);
        assert_eq!(current, Some(label));
        let ready = frame("Plan, search, build anything", label);
        let (surface, _, current, _) = inspect_composer(&ready).unwrap();
        assert_eq!(surface, InputSurface::ComposerReady);
        assert_eq!(current, None);
        let busy = frame("Add a follow-up ctrl+c to stop", label);
        assert_eq!(inspect_composer(&busy).unwrap().0, InputSurface::Busy);
        assert!(inspect_composer(&frame(label, "⠘⠆ Working")).unwrap().3);
        assert!(inspect_composer("→ [Pasted text #1 +25 lines]").is_none());
        for marker in ["[Pasted text #0 +25 lines]", "[Pasted text #01 +25 lines]", "[Pasted text #1 +0 lines]", "[Pasted text #1 +25 lines] suffix"] {
            assert_eq!(inspect_composer(&frame(marker, "")).unwrap().2, None);
        }
        // The port deliberately returns these rows: the root must reject extra
        // editable content, not treat first-line ComposerReady as whole-editor empty.
        let extra = frame("Plan, search, build anything\nother editable line", "");
        assert!(inspect_composer(&extra).unwrap().1.contains(&"other editable line"));
    }
}
