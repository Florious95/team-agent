//! Cursor TUI grammar and shared token-correlated paste acceptance. No guessed
//! /session-id, /tools panel, DirectTyping business fallback or retry loop.
use super::{native::parse_version, CursorAdapter, UNVERIFIED};
use crate::contract::{delivery::*, hooks::*, probe::*, types::*};
use crate::runtime::delivery::ControlDefinition;
use std::time::Duration;

pub const PROFILE: &str = "cursor-tui-v1";
// Recipe identity, NOT an installed release hash. Framework timing bounds are
// deliberate budgets, not a claim that one recon run proved production latency.
// SHA256 cursor-tui-v1|bracketed|enter=1|trailer=none|retry=never|capture=100ms|stable=200ms|floor=100ms|deadline=30s|correlation=baseline-seen-token-gone-transcript
pub const POLICY: Digest = Digest([
    0x2d, 0x8c, 0xe5, 0x4a, 0x54, 0x6a, 0xdd, 0x04, 0xfc, 0xc0, 0x94, 0xd5, 0x5e, 0x2a, 0xf3, 0xcf,
    0x0c, 0x8e, 0xdf, 0x3c, 0x29, 0xec, 0x20, 0xc5, 0x90, 0xf7, 0x0c, 0xf3, 0xdb, 0x25, 0x70, 0x92,
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
        correlate_business_input(frame, &mut observation, composer.as_deref());
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
