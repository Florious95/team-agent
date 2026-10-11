//! Finite Cursor TUI grammar, not a Team business-acceptance detector. No guessed
//! /session-id, /tools panel, DirectTyping business fallback or retry loop.
use super::{native::parse_version, CursorAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, types::*};
use crate::runtime::delivery::ControlDefinition;

pub const PROFILE: &str = "cursor-tui-recon-v1";
// SHA256 of cursor-tui-recon-v1|bracketed|enter=1|no-trailer|no-retry|no-controls|acceptance=unverified
const POLICY: Digest = Digest([
    0xaa, 0x51, 0x45, 0x39, 0x1c, 0xff, 0x15, 0x0c, 0xbf, 0x7b, 0x6f, 0xa1, 0x0a, 0x26, 0xf0, 0xf2,
    0xa4, 0x43, 0x5b, 0xc9, 0x82, 0x4e, 0xcd, 0xa8, 0x97, 0xb5, 0x0f, 0xbe, 0x61, 0x3a, 0xd9, 0x38,
]);
pub static PROFILES: [InputProfile; 1] = [InputProfile {
    id: PROFILE, identity: ProfileIdentity::RuntimeCaptured,
    harness: "cursor-agent", ui: "tui", platform: Platform::MacOs,
    policy_sha256: POLICY,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend],
    channel: Channel::Tmux,
    policy: Support::Unverified(UNVERIFIED),
}];
pub const CONTROLS: &[ControlDefinition] = &[];

/// Assemble only the measured keyboard recipe using caller-owned timing limits.
/// This does not admit PROFILES or prove token freshness/physical acceptance.
pub fn submit_policy(timing: InputTiming) -> Result<SubmitPolicy, ContractError> {
    let policy = SubmitPolicy {
        paste_mode: PasteMode::Bracketed,
        payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter,
        confirmation_steps: &[], retry_budget: RetryBudget::Never, queue_flush: &[],
        timing, max_submit_keys: 1,
    };
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
impl InteractionHook for CursorAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        InteractionObservation {
            scope: frame.scope.clone(),
            surface: if frame.profile_id == PROFILE { surface(&frame.text) } else { InputSurface::ShellOrUnknown },
            predicate: None, current_message: None, current_attempt: None,
            // No genuine Team-token capture exists: do not fabricate Seen/Gone,
            // NativeAccepted or a ToolInspect registry witness from echo traffic.
            paste_latch: frame.paste_latch.clone(), control_paste: None,
        }
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
