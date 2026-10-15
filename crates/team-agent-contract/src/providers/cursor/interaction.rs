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
#[cfg(test)]
mod composer_observation_tests {
    use super::*;

    fn frame(composer: &str, history: &str) -> String {
        format!("Cursor Agent\nv2026.10.01-e373342\n{history}\n→ {composer}\nGrok 4.7 High Run Everything\n")
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

    #[test]
    fn legacy_composer_observes_headerless_followup_without_changing_contract_admission() {
        let ready = "→ Add a follow-up\nGrok 4.7 High Run Everything\n/owned/project\n";
        assert!(inspect_composer(ready).is_none());
        assert_eq!(surface(ready), InputSurface::ShellOrUnknown);
        assert_eq!(inspect_legacy_composer(ready).unwrap().0, InputSurface::ComposerReady);
        let busy = ready.replace("Add a follow-up", "Add a follow-up ctrl+c to stop");
        assert!(inspect_legacy_composer(&busy).unwrap().3);
        let draft = ready.replace("Add a follow-up", "foreign draft");
        assert_eq!(inspect_legacy_composer(&draft).unwrap().0, InputSurface::ComposerContainsPaste);
        assert!(inspect_legacy_composer("→ Add a follow-up\n").is_none());
    }

    #[test]
    fn legacy_composer_observes_standalone_arrow_expanded_body_not_an_empty_editor() {
        let current = "→\n  wrapped editable body\n  [team-agent-token:msg_one]\n\nGrok 4.7 High Run Everything\n/owned/project\n";
        for prefix in ["", "Cursor Agent\nv2026.10.14-0f9edde\n→ Add a follow-up\nGrok 4.7 High Run Everything\n/old/project\n"] {
            let text = format!("{prefix}{current}");
            let (observed, rows, capsule, busy) = inspect_legacy_composer(&text).unwrap();
            assert_eq!(observed, InputSurface::ComposerContainsPaste);
            assert_eq!(rows[0], "→");
            assert!(rows.contains(&"[team-agent-token:msg_one]"));
            assert!(capsule.is_none() && !busy);
        }
        assert_eq!(surface(current), InputSurface::ShellOrUnknown);
        assert!(inspect_composer(current).is_none());
        assert!(inspect_legacy_composer("→\nGrok 4.7 High Run Everything\n/owned/project\n").is_none());
        assert!(inspect_legacy_composer("→\nforeign draft\n").is_none());
        assert!(inspect_legacy_composer("→\nforeign draft\n→ Add a follow-up\nGrok 4.7 High Run Everything\n/owned/project\n").is_none());
    }
}

/// Read-only native composer view; ownership/correlation remains the caller's job.
/// Reuses the contract grammar without granting acceptance or authorizing a key.
pub fn inspect_composer(text: &str) -> Option<(InputSurface, Vec<&str>, Option<&str>, bool)> {
    let observed = surface(text);
    if matches!(observed, InputSurface::ShellOrUnknown) {
        return None;
    }
    let composer = composer_lines(text)?;
    let marker = composer.first()
        .and_then(|line| line.strip_prefix("→ "))
        .and_then(|value| folded_marker(value.trim()));
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    let index = lines.iter().rposition(|line| line.starts_with("→ "))?;
    let mut preceding = lines[..index].iter().rev().filter(|line| !line.is_empty());
    let mut status = preceding.next().copied();
    if status.is_some_and(|line| line.starts_with("Tip: ")) { status = preceding.next().copied(); }
    let busy = matches!(observed, InputSurface::Busy) || status.is_some_and(working);
    Some((observed, composer, marker, busy))
}

/// Legacy cursor_agent observation only: the startup banner can leave a
/// bounded scrollback capture after a completed turn. Require the current
/// native footer/context instead; this does not change contract H4/H6 surface
/// admission, create a paste latch, or authorize any input.
pub fn inspect_legacy_composer(text: &str) -> Option<(InputSurface, Vec<&str>, Option<&str>, bool)> {
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    // Expanded native editors put the arrow on its own row. Select the
    // current block before consulting history; strict H4/H6 grammar is unchanged.
    let index = lines.iter().rposition(|line| *line == "→" || line.starts_with("→ "))?;
    // Do not let an arrow quoted inside an expanded draft replace its real
    // opening and turn foreign editable text into a seemingly empty composer.
    if lines[..index].contains(&"→") { return None; }
    let composer = lines[index..].to_vec();
    let footer = composer.iter().rposition(|row| row.strip_suffix("Run Everything")
        .is_some_and(|prefix| !prefix.trim().is_empty() && !prefix.trim_start().starts_with("→ ")))?;
    if footer == 0 || !composer[footer + 1..].iter().any(|row| !row.is_empty()) { return None; }
    let current = if composer[0] == "→" {
        // A bare arrow is not empty-ready proof. Only expose an expanded
        // nonempty body; the caller still owns marker, busy and stability gates.
        if !composer[1..footer].iter().any(|row| !row.is_empty()) { return None; }
        ""
    } else { composer[0].strip_prefix("→ ")? };
    let mut preceding = lines[..index].iter().rev().filter(|line| !line.is_empty());
    let mut status = preceding.next().copied();
    if status.is_some_and(|line| line.starts_with("Tip: ")) { status = preceding.next().copied(); }
    let stop_hint = current.strip_suffix("ctrl+c to stop").is_some_and(|prefix| placeholder(prefix.trim()));
    let busy = stop_hint || status.is_some_and(working);
    let observed = if stop_hint || (placeholder(current) && busy) { InputSurface::Busy }
        else if placeholder(current) { InputSurface::ComposerReady }
        else { InputSurface::ComposerContainsPaste };
    let marker = folded_marker(current);
    Some((observed, composer, marker, busy))
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
