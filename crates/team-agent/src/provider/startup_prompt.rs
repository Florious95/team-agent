//! codex startup-prompt recognizer — workspace-trust + update-skip screen detection.
//!
//! Golden (READ-ONLY truth `team-agent-public` v0.2.11): `provider_cli/codex.py`
//!   - `CodexAdapter.handle_startup_prompts` (:142-182)
//!   - `maybe_skip_update_prompt` (:262-268)
//!
//! recognizer-class (Gap 29 — burned 4 Mac minis): a NAIVE substring port gets the RECENCY命门 wrong.
//! A prompt is acted on ONLY when its `rfind` position is GREATER than the ready marker's `rfind`
//! position (i.e. it appears LATER / more recently in the captured scrollback). A stale prompt ABOVE an
//! already-ready marker is left alone — ready wins and polling stops. RED-first skeleton; porter-d
//! implements GREEN black-box against golden codex.py.

use std::time::Duration;

use crate::transport::{CaptureRange, Key, Target, Transport};

const TRUST_MARKERS: &[&str] = &[
    "Do you trust the contents of this directory?",
    "Do you trust the files in this folder?",
    "Do you trust this folder?",
];
const UPDATE_MARKERS: &[&str] = &["Update available!", "Update now"];
const CLAUDE_TRUST_MARKERS: &[&str] = &[
    "Quick safety check",
    "Is this a project you created or one you trust?",
    "Yes, I trust this folder",
    "No, exit",
    "Enter to confirm",
];
const CLAUDE_READY_MARKERS: &[&str] = &["Claude Code"];
pub const COPILOT_TRUST_PROMPT_MARKER: &str = include_str!("testdata/copilot-trust-prompt.txt");
pub const COPILOT_READY_MARKER: &str = include_str!("testdata/copilot-ready-marker.txt");
const COPILOT_TRUST_MARKERS: &[&str] = &[COPILOT_TRUST_PROMPT_MARKER];
const COPILOT_READY_MARKERS: &[&str] = &[COPILOT_READY_MARKER];
const COPILOT_TRUST_TITLE: &str = "Confirm folder trust";
const COPILOT_TRUST_QUESTION: &str = "Do you trust the files in this folder?";
const COPILOT_TRUST_YES_SESSION: &str = "❯ 1. Yes";
const COPILOT_TRUST_REMEMBER: &str = "2. Yes, and remember this folder for future sessions";
const COPILOT_TRUST_NO: &str = "3. No (Esc)";
const COPILOT_READY_FOOTER: &str = " / commands · ? help";
/// Plain ready markers (not the bare `›` glyph — that glyph also indicates a
/// numbered-menu selector and is handled by [`rightmost_input_prompt_glyph`] with
/// shape gating per N15 / CR-063: detect by SHAPE, not a single Unicode codepoint).
const READY_MARKERS: &[&str] = &["OpenAI Codex", "codex>"];

/// Per-poll decision for the codex startup screen. Golden order each iteration (codex.py:160-181):
/// update-skip is checked FIRST, then workspace-trust, then ready (stop), else keep polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupScreenDecision {
    /// `maybe_skip_update_prompt` matched: update_pos >= 0 && update_pos > ready_pos (codex.py:262-267).
    SkipUpdatePrompt,
    /// workspace-trust: trust_pos >= 0 && trust_pos > ready_pos (codex.py:166-174).
    AnswerWorkspaceTrust,
    /// ready_pos >= 0 with no actionable prompt above it (codex.py:178) -> stop polling.
    Ready,
    /// none of the above (codex.py:180) -> sleep + keep polling.
    KeepPolling,
}

/// One handled startup prompt — an entry of golden's `handled` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandledPrompt {
    pub prompt: String,
    pub action: String,
}

/// PURE recognizer (codex.py:160-181 + maybe_skip_update_prompt :262-268): captured scrollback ->
/// decision. NO IO. The RECENCY命门: a prompt is acted on ONLY when its `rfind` position is strictly
/// GREATER than the ready marker's `rfind` position. update is evaluated before trust.
///   trust strings (rfind max of):  "Do you trust the contents of this directory?" /
///       "Do you trust the files in this folder?" / "Do you trust this folder?"
///   update strings (rfind max of): "Update available!" / "Update now"
///   ready markers (rfind max of):  "OpenAI Codex" / "›" / "codex>"
pub fn classify_codex_startup_screen(output: &str) -> StartupScreenDecision {
    // CR-063 / subroot real-machine residual: actionable-shape override BEFORE recency.
    // The recency model ("prompt above ready = stale-scrolled, ignore") assumes the
    // active state is the LATEST byte on screen. Real Codex breaks that assumption:
    // while a trust modal is still active, Codex pre-renders the Update box, the
    // OpenAI Codex banner, AND a bottom `› Find and fix a bug…` input-prompt indicator
    // BELOW the trust menu — so recency would mark the screen Ready and the trust
    // menu would never be answered. When the captured text has the actionable trust
    // shape (`Do you trust …` phrase + a `› <digit>. ` numbered-menu line, N15),
    // the modal IS the live state regardless of what comes after it. Return early.
    if has_actionable_trust_shape(output) {
        return StartupScreenDecision::AnswerWorkspaceTrust;
    }
    // N15/CR-063 root-cause (recency lane): the bare `›` glyph is BOTH the Codex
    // input-prompt indicator AND the numbered-menu selector on a real trust pane
    // (`› 1. Yes, continue`). Detect by SHAPE: `›` is a ready marker only when its
    // tail is NOT a `<digit>. ` menu item.
    let ready_pos = max_two(
        max_rfind(output, READY_MARKERS),
        rightmost_input_prompt_glyph(output),
    );
    if is_more_recent(max_rfind(output, UPDATE_MARKERS), ready_pos) {
        return StartupScreenDecision::SkipUpdatePrompt;
    }
    if is_more_recent(max_rfind(output, TRUST_MARKERS), ready_pos) {
        return StartupScreenDecision::AnswerWorkspaceTrust;
    }
    if ready_pos.is_some() {
        StartupScreenDecision::Ready
    } else {
        StartupScreenDecision::KeepPolling
    }
}

pub fn classify_claude_startup_screen(output: &str) -> StartupScreenDecision {
    if has_active_claude_yes_trust_shape(output) {
        return StartupScreenDecision::AnswerWorkspaceTrust;
    }
    if has_claude_ready_shape(output) {
        StartupScreenDecision::Ready
    } else {
        StartupScreenDecision::KeepPolling
    }
}

pub fn classify_copilot_startup_screen(output: &str) -> StartupScreenDecision {
    let ready_pos = copilot_ready_pos(output);
    if has_active_copilot_trust_shape(output)
        && is_more_recent(copilot_trust_pos(output), ready_pos)
    {
        return StartupScreenDecision::AnswerWorkspaceTrust;
    }
    if ready_pos.is_some() {
        StartupScreenDecision::Ready
    } else {
        StartupScreenDecision::KeepPolling
    }
}

/// Actionable trust shape (N15): the captured text contains a trust phrase AND a
/// numbered-menu selector line `› <digit>. `. This is the modal-still-active signal
/// that survives Codex's pre-rendering of the banner/input prompt below the menu.
/// Does NOT match a single-screen "trust phrase + bare `›`" (e.g. plain Ready
/// follow-up text), so historical "trust ABOVE ready" recency tests keep passing
/// — those fixtures do not include a `› <digit>. ` menu line.
fn has_actionable_trust_shape(output: &str) -> bool {
    if !TRUST_MARKERS.iter().any(|marker| output.contains(marker)) {
        return false;
    }
    contains_numbered_menu_glyph(output)
}

/// `true` iff any `›` in the output is followed by a numbered-menu selector
/// (` <digit>. `). The shape pairs the glyph with a digit-dot line item — the
/// Codex trust/update menu printing convention.
fn contains_numbered_menu_glyph(output: &str) -> bool {
    let glyph = '›';
    let glyph_len = glyph.len_utf8();
    let mut start = 0;
    while let Some(rel) = output[start..].find(glyph) {
        let abs = start + rel;
        let tail_start = abs + glyph_len;
        if tail_start > output.len() {
            break;
        }
        if is_numbered_menu_tail(&output[tail_start..]) {
            return true;
        }
        start = tail_start;
    }
    false
}

fn has_active_claude_yes_trust_shape(output: &str) -> bool {
    if !CLAUDE_TRUST_MARKERS
        .iter()
        .all(|marker| output.contains(marker))
    {
        return false;
    }
    output.lines().map(str::trim_start).any(|line| {
        line == "❯ 1. Yes, I trust this folder" || line == "> 1. Yes, I trust this folder"
    })
}

fn has_claude_ready_shape(output: &str) -> bool {
    max_rfind(output, CLAUDE_READY_MARKERS).is_some()
        && rightmost_claude_input_prompt_glyph(output).is_some()
}

fn has_active_copilot_trust_shape(output: &str) -> bool {
    if max_rfind(output, COPILOT_TRUST_MARKERS).is_some() {
        return true;
    }
    [
        COPILOT_TRUST_TITLE,
        COPILOT_TRUST_QUESTION,
        COPILOT_TRUST_YES_SESSION,
        COPILOT_TRUST_REMEMBER,
        COPILOT_TRUST_NO,
    ]
    .iter()
    .all(|marker| output.contains(marker))
}

fn copilot_trust_pos(output: &str) -> Option<usize> {
    max_two(
        max_rfind(output, COPILOT_TRUST_MARKERS),
        max_rfind(output, &[COPILOT_TRUST_TITLE, COPILOT_TRUST_QUESTION]),
    )
}

fn copilot_ready_pos(output: &str) -> Option<usize> {
    let fixture_pos = max_rfind(output, COPILOT_READY_MARKERS);
    let footer_pos = output.rfind(COPILOT_READY_FOOTER);
    let prompt_pos = rightmost_copilot_input_prompt_glyph(output);
    if footer_pos.is_some() && prompt_pos.is_some() {
        max_two(fixture_pos, max_two(footer_pos, prompt_pos))
    } else {
        fixture_pos
    }
}

fn rightmost_copilot_input_prompt_glyph(output: &str) -> Option<usize> {
    let mut best = None;
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        if line.trim() == "❯" {
            if let Some(idx) = line.find('❯') {
                best = Some(offset + idx);
            }
        }
        offset += line.len();
    }
    best
}

fn rightmost_claude_input_prompt_glyph(output: &str) -> Option<usize> {
    let mut best = rightmost_input_prompt_for_glyph(output, '❯');
    best = max_two(best, rightmost_input_prompt_for_glyph(output, '>'));
    best
}

fn rightmost_input_prompt_for_glyph(output: &str, glyph: char) -> Option<usize> {
    let glyph_len = glyph.len_utf8();
    let mut best = None;
    let mut start = 0;
    while let Some(rel) = output[start..].find(glyph) {
        let abs = start + rel;
        let tail_start = abs + glyph_len;
        if tail_start <= output.len() && !is_numbered_menu_tail(&output[tail_start..]) {
            best = Some(abs);
        }
        start = tail_start;
        if start > output.len() {
            break;
        }
    }
    best
}

/// Rightmost `›` whose tail is NOT a numbered-menu selector (` <digit>. `). A bare
/// `›` followed by free text or whitespace is the Codex main-input prompt indicator;
/// a `›` followed by `1. Yes, continue` is part of the trust/update menu and is NOT
/// a ready signal.
fn rightmost_input_prompt_glyph(output: &str) -> Option<usize> {
    let glyph = '›';
    let glyph_len = glyph.len_utf8();
    let mut best = None;
    let bytes = output.as_bytes();
    let mut start = 0;
    while let Some(rel) = output[start..].find(glyph) {
        let abs = start + rel;
        let tail_start = abs + glyph_len;
        if tail_start <= bytes.len() && !is_numbered_menu_tail(&output[tail_start..]) {
            best = Some(abs);
        }
        start = tail_start;
        if start > output.len() {
            break;
        }
    }
    best
}

fn is_numbered_menu_tail(tail: &str) -> bool {
    let trimmed = tail.trim_start_matches(' ');
    let mut chars = trimmed.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(d), Some('.')) if d.is_ascii_digit()
    )
}

fn max_two(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Capture-poll loop (codex.py:142-182) over the `transport.capture()` seam (NOT a raw subprocess, so
/// it stays unit-testable). On `AnswerWorkspaceTrust` -> send `Enter` + push
/// {prompt:"codex_workspace_trust", action:"sent_enter"}; on `SkipUpdatePrompt` -> send `Down`,`Enter`
/// + push {prompt:"codex_update_available", action:"sent_skip"}; on `Ready` -> stop. Loops up to
///   `checks` (golden default 30), `sleep_s` (golden 0.5) between iterations. Returns the ordered
///   `handled` list. Capture is full scrollback (golden `tmux capture-pane -p -S - -t <target>`).
/// swallow batch 2 ② (A1): the structured startup-prompt outcome — `handled` keeps the
/// Python golden list shape; `capture_error` carries the FIRST capture failure so the
/// caller can surface it (an unobservable pane must never be silently treated as
/// "no prompts to handle").
#[derive(Debug, Clone, Default)]
pub struct StartupPromptOutcome {
    pub handled: Vec<HandledPrompt>,
    pub capture_error: Option<String>,
}

pub fn codex_handle_startup_prompts(
    transport: &dyn Transport,
    target: &Target,
    checks: usize,
    sleep_s: f64,
) -> StartupPromptOutcome {
    let mut handled = Vec::new();
    let mut capture_error: Option<String> = None;
    for _ in 0..checks {
        let screen = match transport.capture(target, CaptureRange::Full) {
            Ok(captured) => captured.text,
            Err(error) => {
                capture_error.get_or_insert_with(|| error.to_string());
                String::new()
            }
        };
        match classify_codex_startup_screen(&screen) {
            StartupScreenDecision::SkipUpdatePrompt => {
                let _ = transport.send_keys(target, &[Key::Down, Key::Enter]);
                handled.push(HandledPrompt {
                    prompt: "codex_update_available".to_string(),
                    action: "sent_skip".to_string(),
                });
                sleep_between_polls(sleep_s);
            }
            StartupScreenDecision::AnswerWorkspaceTrust => {
                let _ = transport.send_keys(target, &[Key::Enter]);
                handled.push(HandledPrompt {
                    prompt: "codex_workspace_trust".to_string(),
                    action: "sent_enter".to_string(),
                });
                sleep_between_polls(sleep_s);
            }
            StartupScreenDecision::Ready => break,
            StartupScreenDecision::KeepPolling => sleep_between_polls(sleep_s),
        }
    }
    StartupPromptOutcome {
        handled,
        capture_error,
    }
}

pub fn claude_handle_startup_prompts(
    transport: &dyn Transport,
    target: &Target,
    checks: usize,
    sleep_s: f64,
) -> StartupPromptOutcome {
    let mut handled = Vec::new();
    let mut capture_error: Option<String> = None;
    for _ in 0..checks {
        let screen = match transport.capture(target, CaptureRange::Full) {
            Ok(captured) => captured.text,
            Err(error) => {
                capture_error.get_or_insert_with(|| error.to_string());
                String::new()
            }
        };
        match classify_claude_startup_screen(&screen) {
            StartupScreenDecision::AnswerWorkspaceTrust => {
                let _ = transport.send_keys(target, &[Key::Enter]);
                handled.push(HandledPrompt {
                    prompt: "claude_workspace_trust".to_string(),
                    action: "sent_enter".to_string(),
                });
                sleep_between_polls(sleep_s);
            }
            StartupScreenDecision::Ready => break,
            StartupScreenDecision::SkipUpdatePrompt | StartupScreenDecision::KeepPolling => {
                sleep_between_polls(sleep_s);
            }
        }
    }
    StartupPromptOutcome {
        handled,
        capture_error,
    }
}

pub fn copilot_handle_startup_prompts(
    transport: &dyn Transport,
    target: &Target,
    checks: usize,
    sleep_s: f64,
) -> StartupPromptOutcome {
    let mut handled = Vec::new();
    let mut capture_error: Option<String> = None;
    for _ in 0..checks {
        let screen = match transport.capture(target, CaptureRange::Full) {
            Ok(captured) => captured.text,
            Err(error) => {
                capture_error.get_or_insert_with(|| error.to_string());
                String::new()
            }
        };
        match classify_copilot_startup_screen(&screen) {
            StartupScreenDecision::AnswerWorkspaceTrust => {
                let _ = transport.send_keys(target, &[Key::Enter]);
                handled.push(HandledPrompt {
                    prompt: "copilot_workspace_trust".to_string(),
                    action: "sent_enter_yes_session".to_string(),
                });
                sleep_between_polls(sleep_s);
            }
            StartupScreenDecision::Ready => break,
            StartupScreenDecision::SkipUpdatePrompt | StartupScreenDecision::KeepPolling => {
                sleep_between_polls(sleep_s);
            }
        }
    }
    StartupPromptOutcome {
        handled,
        capture_error,
    }
}

fn max_rfind(output: &str, needles: &[&str]) -> Option<usize> {
    needles
        .iter()
        .filter_map(|needle| output.rfind(needle))
        .max()
}

fn is_more_recent(prompt_pos: Option<usize>, ready_pos: Option<usize>) -> bool {
    match (prompt_pos, ready_pos) {
        (Some(prompt), Some(ready)) => prompt > ready,
        (Some(_), None) => true,
        _ => false,
    }
}

fn sleep_between_polls(sleep_s: f64) {
    let millis = (sleep_s * 1000.0).round();
    if millis.is_finite() && millis > 0.0 && millis <= u64::MAX as f64 {
        std::thread::sleep(Duration::from_millis(millis as u64));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::model::enums::PaneLiveness;
    use crate::transport::{
        AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
        PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome, SpawnResult, Target,
        TransportError, WindowName,
    };
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;

    // ── EXACT golden strings (provider_cli/codex.py). Do not paraphrase — recognizer-class. ──────────
    const TRUST_DIR: &str = "Do you trust the contents of this directory?";
    const TRUST_FILES: &str = "Do you trust the files in this folder?";
    const TRUST_FOLDER: &str = "Do you trust this folder?";
    const UPDATE_AVAIL: &str = "Update available!";
    const UPDATE_NOW: &str = "Update now";
    const READY_BANNER: &str = "OpenAI Codex";
    const READY_PROMPT: &str = "›"; // U+203A
    const READY_BARE: &str = "codex>";

    // ── ① + ② RED核心 — workspace-trust MORE RECENT than ready -> answer it ──────────────────────────
    #[test]
    fn trust_more_recent_than_ready_answers_workspace_trust() {
        // ready banner appears early; the trust prompt appears LATER; no ready marker after it
        // => trust_pos > ready_pos => answer.
        let screen = format!("{READY_BANNER} v1.2\nwelcome\n\n{TRUST_DIR}\n  hit enter ");
        assert_eq!(
            classify_codex_startup_screen(&screen),
            StartupScreenDecision::AnswerWorkspaceTrust
        );
    }

    // ── ② 命门 CORE — a STALE trust prompt ABOVE the ready marker is NOT answered (ready wins) ────────
    #[test]
    fn stale_trust_above_ready_is_not_answered_ready_wins() {
        // trust prompt FIRST, then a ready marker LATER => trust_pos < ready_pos => do NOT answer.
        // This is the positional-recency命门 a naive substring port gets wrong (would re-send Enter).
        let screen =
            format!("{TRUST_DIR}\n[trusted earlier]\n{READY_BANNER} ready\n{READY_PROMPT} ");
        assert_eq!(
            classify_codex_startup_screen(&screen),
            StartupScreenDecision::Ready,
            "RECENCY命门: a trust prompt ABOVE the ready marker is stale; ready wins, NO Enter sent"
        );
    }

    #[test]
    fn each_trust_string_recognized_when_more_recent() {
        for s in [TRUST_DIR, TRUST_FILES, TRUST_FOLDER] {
            let screen = format!("{READY_BANNER}\n...banner...\n{s}\n");
            assert_eq!(
                classify_codex_startup_screen(&screen),
                StartupScreenDecision::AnswerWorkspaceTrust,
                "trust string {s:?} after ready must answer"
            );
        }
    }

    // ── ③ sibling — update-skip recognizer (maybe_skip_update_prompt), same recency命门 ──────────────
    #[test]
    fn update_more_recent_than_ready_skips_update() {
        for s in [UPDATE_AVAIL, UPDATE_NOW] {
            let screen = format!("{READY_BANNER}\nblah\n{s}\n");
            assert_eq!(
                classify_codex_startup_screen(&screen),
                StartupScreenDecision::SkipUpdatePrompt,
                "update string {s:?} after ready must skip"
            );
        }
    }

    #[test]
    fn stale_update_above_ready_is_not_skipped_ready_wins() {
        let screen = format!("{UPDATE_AVAIL}\n{READY_BANNER}\n{READY_PROMPT} ");
        assert_eq!(
            classify_codex_startup_screen(&screen),
            StartupScreenDecision::Ready
        );
    }

    // ── golden ORDER — update is checked BEFORE trust (both more recent) -> SkipUpdatePrompt wins ─────
    #[test]
    fn update_checked_before_trust() {
        // both update + trust appear after the ready marker; golden runs maybe_skip_update_prompt
        // FIRST each iteration -> the screen resolves to SkipUpdatePrompt, not AnswerWorkspaceTrust.
        let screen = format!("{READY_BANNER}\n{TRUST_DIR}\n{UPDATE_AVAIL}\n");
        assert_eq!(
            classify_codex_startup_screen(&screen),
            StartupScreenDecision::SkipUpdatePrompt
        );
    }

    // ── ready-only / neither ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn each_ready_marker_alone_is_ready() {
        for m in [READY_BANNER, READY_PROMPT, READY_BARE] {
            let screen = format!("booting...\n{m} ");
            assert_eq!(
                classify_codex_startup_screen(&screen),
                StartupScreenDecision::Ready,
                "ready marker {m:?} alone must be Ready"
            );
        }
    }

    #[test]
    fn no_prompt_no_ready_keeps_polling() {
        assert_eq!(
            classify_codex_startup_screen("loading spinner...\nstill starting\n"),
            StartupScreenDecision::KeepPolling
        );
    }

    #[test]
    fn copilot_trust_fixture_answers_yes_for_this_session() {
        assert_eq!(
            classify_copilot_startup_screen(COPILOT_TRUST_PROMPT_MARKER),
            StartupScreenDecision::AnswerWorkspaceTrust
        );
        assert!(
            COPILOT_TRUST_PROMPT_MARKER.contains(COPILOT_TRUST_YES_SESSION),
            "copilot trust default must remain option 1, not persistent option 2"
        );
    }

    #[test]
    fn copilot_trust_prompt_tolerates_path_variation() {
        let dir_b = COPILOT_TRUST_PROMPT_MARKER.replace("/dir-a", "/dir-b");
        assert_eq!(
            classify_copilot_startup_screen(&dir_b),
            StartupScreenDecision::AnswerWorkspaceTrust
        );
    }

    #[test]
    fn copilot_ready_fixture_is_ready_without_trust_prompt() {
        assert_eq!(
            classify_copilot_startup_screen(COPILOT_READY_MARKER),
            StartupScreenDecision::Ready
        );
    }

    #[test]
    fn copilot_ready_after_stale_trust_wins() {
        let screen = format!("{COPILOT_TRUST_PROMPT_MARKER}\n{COPILOT_READY_MARKER}");
        assert_eq!(
            classify_copilot_startup_screen(&screen),
            StartupScreenDecision::Ready
        );
    }

    // ── ④ transport.capture() SEAM — the loop answers trust then breaks on ready, via the seam ───────
    /// Scripted transport: `capture` pops the next canned screen; `send_keys` records the keys. All
    /// other methods are unreachable by the startup-prompt loop.
    struct ScriptedTransport {
        screens: Mutex<Vec<String>>,
        sent: Mutex<Vec<Vec<Key>>>,
    }
    impl Transport for ScriptedTransport {
        fn kind(&self) -> BackendKind {
            BackendKind::Tmux
        }
        fn spawn_first(
            &self,
            _s: &SessionName,
            _w: &WindowName,
            _a: &[String],
            _c: &Path,
            _e: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            unimplemented!("not reached by startup-prompt loop")
        }
        fn spawn_into(
            &self,
            _s: &SessionName,
            _w: &WindowName,
            _a: &[String],
            _c: &Path,
            _e: &BTreeMap<String, String>,
        ) -> Result<SpawnResult, TransportError> {
            unimplemented!("not reached by startup-prompt loop")
        }
        fn inject(
            &self,
            _t: &Target,
            _p: &InjectPayload,
            _submit: Key,
            _b: bool,
        ) -> Result<InjectReport, TransportError> {
            unimplemented!("not reached")
        }
        fn send_keys(&self, _t: &Target, keys: &[Key]) -> Result<(), TransportError> {
            self.sent.lock().unwrap().push(keys.to_vec());
            Ok(())
        }
        fn capture(
            &self,
            _t: &Target,
            range: CaptureRange,
        ) -> Result<CapturedText, TransportError> {
            let mut q = self.screens.lock().unwrap();
            let text = if q.is_empty() {
                String::new()
            } else {
                q.remove(0)
            };
            Ok(CapturedText { text, range })
        }
        fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
            Ok(None)
        }
        fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
            unimplemented!("not reached")
        }
        fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
            unimplemented!("not reached")
        }
        fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
            Ok(true)
        }
        fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
            unimplemented!("not reached")
        }
        fn set_session_env(
            &self,
            _s: &SessionName,
            _k: &str,
            _v: &str,
        ) -> Result<SetEnvOutcome, TransportError> {
            unimplemented!("not reached")
        }
        fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
            unimplemented!("not reached")
        }
        fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
            unimplemented!("not reached")
        }
        fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> {
            unimplemented!("not reached")
        }
    }

    #[test]
    fn loop_answers_trust_then_breaks_on_ready_via_capture_seam() {
        let t = ScriptedTransport {
            screens: Mutex::new(vec![
                // iter 1: trust prompt more recent than ready -> answer (send Enter) + continue.
                format!("{READY_BANNER}\n{TRUST_DIR}\n"),
                // iter 2: ready marker, no actionable prompt above it -> break.
                format!("{READY_BANNER} ready\n{READY_PROMPT} "),
            ]),
            sent: Mutex::new(Vec::new()),
        };
        let target = Target::Pane(PaneId::new("%1"));

        let handled = codex_handle_startup_prompts(&t, &target, 5, 0.0).handled;

        assert_eq!(
            handled,
            vec![HandledPrompt {
                prompt: "codex_workspace_trust".to_string(),
                action: "sent_enter".to_string(),
            }],
            "the loop must answer the workspace-trust prompt exactly once, then break on ready"
        );
        let sent = t.sent.lock().unwrap();
        assert!(
            sent.iter().any(|keys| keys.as_slice() == [Key::Enter]),
            "on workspace-trust the loop must send Enter via the transport.capture() seam; got {sent:?}"
        );
    }

    #[test]
    fn copilot_loop_answers_trust_then_breaks_on_ready_via_capture_seam() {
        let t = ScriptedTransport {
            screens: Mutex::new(vec![
                COPILOT_TRUST_PROMPT_MARKER.to_string(),
                COPILOT_READY_MARKER.to_string(),
            ]),
            sent: Mutex::new(Vec::new()),
        };
        let target = Target::Pane(PaneId::new("%1"));

        let handled = copilot_handle_startup_prompts(&t, &target, 5, 0.0).handled;

        assert_eq!(
            handled,
            vec![HandledPrompt {
                prompt: "copilot_workspace_trust".to_string(),
                action: "sent_enter_yes_session".to_string(),
            }]
        );
        let sent = t.sent.lock().unwrap();
        assert_eq!(
            sent.iter()
                .filter(|keys| keys.as_slice() == [Key::Enter])
                .count(),
            1,
            "copilot trust prompt must choose option 1 with one Enter; got {sent:?}"
        );
    }
}
