//! Owned legacy-tmux composer evidence. No keys, persistence, or provider admission.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};

use crate::model::enums::Provider;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInputRequest {
    pub message_id: String,
    pub owner_team_id: String,
    pub recipient: String,
    pub provider: Provider,
    pub pane_id: String,
    pub pane_pid: u32,
    pub session: String,
    pub window: String,
}

impl NativeInputRequest {
    pub fn marker(&self) -> String {
        format!("[team-agent-token:{}]", self.message_id)
    }

    pub fn supported(&self) -> bool {
        matches!(self.provider, Provider::Pi | Provider::CursorAgent)
            && self.message_id.starts_with("msg_")
            && self
                .message_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && !self.owner_team_id.is_empty()
            && !self.recipient.is_empty()
            && self.recipient != "leader"
            && self.pane_id.starts_with('%')
            && self.pane_pid > 1
            && !self.session.is_empty()
            && !self.window.is_empty()
    }
}

/// Carrier birth and framework pane nonce, not a claim of native executable admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInputBinding {
    pub endpoint: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub pane_height: u32,
    pub carrier_birth: String,
    pub binding_nonce: String,
    pub session: String,
    pub window: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeInputPhase {
    Ready,
    MayHavePasted,
    Pasted,
    Seen,
    MayHaveSubmitted,
    Submitted,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInputEvidence {
    pub request: NativeInputRequest,
    pub binding: NativeInputBinding,
    pub payload_sha256: String,
    pub ui_identity: String,
    pub footer_sha256: String,
    pub baseline_sha256: String,
    pub baseline_capsules: Vec<String>,
    pub phase: NativeInputPhase,
    pub capsule: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterRescueOutcome {
    Consumed,
    NoEffect {
        reason: &'static str,
        reobserve: bool,
    },
    Entered {
        accepted: bool,
    },
}

type Scope = (Option<NativeInputRequest>, Option<NativeInputEvidence>);
thread_local! {
    static INPUT_SCOPE: RefCell<Scope> = const { RefCell::new((None, None)) };
}

struct Restore(Option<Scope>);
impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            INPUT_SCOPE.with(|slot| *slot.borrow_mut() = previous);
        }
    }
}

/// Existing synchronous delivery owns this scope; no borrowed observer escapes a thread.
pub fn with_native_input<R>(
    request: Option<NativeInputRequest>,
    f: impl FnOnce() -> R,
) -> (R, Option<NativeInputEvidence>) {
    let previous = INPUT_SCOPE.with(|slot| slot.replace((request, None)));
    let restore = Restore(Some(previous));
    let result = f();
    let evidence = INPUT_SCOPE.with(|slot| slot.borrow_mut().1.take());
    drop(restore);
    (result, evidence)
}

pub(crate) fn current_request() -> Option<NativeInputRequest> {
    INPUT_SCOPE.with(|slot| slot.borrow().0.clone())
}

pub(crate) fn record_evidence(evidence: &NativeInputEvidence) {
    INPUT_SCOPE.with(|slot| {
        let mut scope = slot.borrow_mut();
        if scope.0.as_ref() == Some(&evidence.request) {
            scope.1 = Some(evidence.clone());
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputFrame {
    pub composer: String,
    pub transcript: String,
    pub identity: String,
    pub footer: String,
    pub busy: bool,
    pub capsule: Option<String>,
}

impl InputFrame {
    pub fn empty(&self) -> bool {
        self.composer.trim().is_empty()
    }

    pub fn has_marker(&self, marker: &str) -> bool {
        self.composer
            .lines()
            .map(str::trim)
            .collect::<String>()
            .matches(marker)
            .count()
            == 1
    }

    pub fn pending(&self, evidence: &NativeInputEvidence) -> bool {
        !self.busy
            && self.identity == evidence.ui_identity
            && input_digest(&self.footer) == evidence.footer_sha256
            && (self.has_marker(&evidence.request.marker())
                || evidence.capsule.as_ref().is_some_and(|capsule| {
                    self.capsule.as_ref() == Some(capsule)
                        && !evidence.baseline_capsules.contains(capsule)
                }))
    }

    pub fn accepted(&self, evidence: &NativeInputEvidence) -> bool {
        if !self.empty() || self.identity != evidence.ui_identity {
            return false;
        }
        if evidence.phase < NativeInputPhase::Seen {
            return false;
        }
        let marker = evidence.request.marker();
        let joined: String = self.transcript.lines().map(str::trim).collect();
        if joined.matches(&marker).count() == 1 {
            return true;
        }
        evidence.capsule.as_ref().is_some_and(|capsule| {
            !evidence.baseline_capsules.contains(capsule)
                && self
                    .transcript
                    .lines()
                    .filter(|line| line.trim() == capsule)
                    .count()
                    == 1
        })
    }
}

fn input_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub(crate) fn inspect_input(
    provider: Provider,
    text: &str,
    pane_height: u32,
) -> Option<InputFrame> {
    match provider {
        Provider::Pi => pi_input(text, pane_height),
        Provider::CursorAgent => cursor_input(text, pane_height),
        _ => None,
    }
}

fn cursor_input(text: &str, pane_height: u32) -> Option<InputFrame> {
    use team_agent_contract::contract::delivery::InputSurface;
    if pane_height == 0 {
        return None;
    }
    let text_rows: Vec<_> = text.lines().collect();
    // Only the current viewport authorizes input; retain full history below
    // solely for same-message transcript correlation.
    let viewport_start = text_rows.len().saturating_sub(pane_height as usize);
    let viewport = text_rows[viewport_start..].join("\n");
    let (surface, rows, capsule, busy) =
        team_agent_contract::providers::cursor::interaction::inspect_legacy_composer(&viewport)?;
    let first = match *rows.first()? {
        "→" => "",
        row => row.strip_prefix("→ ")?,
    };
    let footer = rows.iter().rposition(|row| {
        row.strip_suffix("Run Everything").is_some_and(|prefix| {
            !prefix.trim().is_empty() && !prefix.trim_start().starts_with("→ ")
        })
    })?;
    if footer == 0 {
        return None;
    }
    let tail: Vec<_> = rows[footer + 1..]
        .iter()
        .filter(|row| !row.is_empty())
        .copied()
        .collect();
    if tail.is_empty() {
        return None;
    }
    // Startup history can leave Tail(200) between Ready/Pasted/Accepted. The
    // immutable carrier birth/nonce remains in NativeInputBinding; this UI
    // context is the current native cwd/footer tail, not a version attestation.
    let identity = format!(
        "cursor-current-composer-v1:{}",
        input_digest(&tail.join("\n"))
    );
    let footer_row = rows[footer]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let ready = matches!(surface, InputSurface::ComposerReady | InputSurface::Busy);
    if (ready || capsule.is_some()) && rows[1..footer].iter().any(|row| !row.trim().is_empty()) {
        return None;
    }
    let composer = if ready {
        String::new()
    } else {
        std::iter::once(first)
            .chain(rows[1..footer].iter().copied())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let arrow = text_rows
        .iter()
        .rposition(|line| line.trim() == "→" || line.trim().starts_with("→ "))?;
    Some(InputFrame {
        composer,
        transcript: text.lines().take(arrow).collect::<Vec<_>>().join("\n"),
        identity,
        footer: footer_row,
        busy,
        capsule: capsule.map(str::to_owned),
    })
}

fn border(line: &str) -> bool {
    line.chars().filter(|character| *character == '─').count() >= 8
        && line.trim_start().starts_with('─')
}

// A complete native update notice can end inside the editor search window,
// but its opening must precede every possible editor row. Never discard a
// ruler belonging to the editable region or an incomplete/unknown notice.
fn pi_closed_update_notice(rows: &[&str], closing: usize, editor_start: usize, top: usize) -> bool {
    let rule = rows[closing].trim();
    if !rule.chars().all(|character| character == '─')
        || !rows[closing + 1..top]
            .iter()
            .all(|row| row.trim().is_empty())
    {
        return false;
    }
    let Some(opening) = rows[..editor_start]
        .iter()
        .rposition(|row| row.trim() == rule)
    else {
        return false;
    };
    let body = &rows[opening + 1..closing];
    body.len() >= 4
        && body[0].trim() == "Package Updates Available"
        && body[1].trim() == "Package updates are available. Run pi update --extensions"
        && body[2].trim() == "Packages:"
        && body[3..].iter().all(|row| {
            row.trim()
                .strip_prefix("- ")
                .is_some_and(|package| !package.trim().is_empty())
        })
}

pub(crate) fn pi_paste_capsule(text: &str) -> Option<&str> {
    fn positive(value: &str) -> bool {
        !value.starts_with('0')
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u64>().is_ok()
    }
    let label = text.trim();
    let body = label.strip_prefix("[paste #")?.strip_suffix(']')?;
    let (id, size) = body.split_once(' ')?;
    let count = if let Some(lines) = size.strip_prefix('+') {
        lines.strip_suffix(" lines")?
    } else {
        size.strip_suffix(" chars")?
    };
    (positive(id) && positive(count)).then_some(label)
}

fn pi_input(text: &str, pane_height: u32) -> Option<InputFrame> {
    if pane_height < 10 {
        return None;
    }
    let rows: Vec<_> = text.lines().collect();
    let footer = rows
        .iter()
        .rposition(|row| row.contains(" · ") && row.contains("%/"))?;
    if rows[footer + 1..].iter().any(|row| !row.trim().is_empty()) {
        return None;
    }
    let bottom = (0..footer)
        .rev()
        .find(|index| !rows[*index].trim().is_empty())?;
    if !border(rows[bottom]) {
        return None;
    }
    // Current Pi renderer shows at most max(5, floor(pane_rows * .3))
    // editor lines. A task's Unicode ruler is not a native top boundary:
    // ambiguous borders inside that whole possible editor extent refuse.
    let extent = (pane_height as usize * 3 / 10).max(5) + 1;
    let editor_start = bottom.saturating_sub(extent);
    let candidates: Vec<_> = (editor_start..bottom)
        .filter(|index| border(rows[*index]))
        .collect();
    let top = *candidates.last()?;
    if candidates[..candidates.len() - 1]
        .iter()
        .any(|index| !pi_closed_update_notice(&rows, *index, editor_start, top))
    {
        return None;
    }
    let composer = rows[top + 1..bottom].join("\n");
    let transcript = rows[..top].join("\n");
    // Inspect native status adjacent to the editor, never the task body.
    let status = rows[..top]
        .iter()
        .rev()
        .filter(|row| !row.trim().is_empty())
        .take(3)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    // Pi embeds its live status/spinner after a 1–3-cell border prefix. Idle
    // (including the observed overflow/title border) has a solid leading run.
    let busy = rows[top]
        .trim_start()
        .chars()
        .take_while(|character| *character == '─')
        .count()
        < 8
        || status.to_ascii_lowercase().contains("esc to interrupt")
        || status.to_ascii_lowercase().contains("ctrl+c to stop");
    let capsule = pi_paste_capsule(&composer).map(str::to_owned);
    Some(InputFrame {
        composer,
        transcript,
        identity: rows[footer].split(" · ").next()?.trim().to_string(),
        footer: String::new(),
        busy,
        capsule,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn cursor(composer: &str, transcript: &str) -> String {
        format!("Cursor Agent\nv2026.10.01-e373342\n{transcript}\n→ {composer}\nGrok 4.7 High Run Everything\n/owned/project\n")
    }

    fn pi(composer: &str, transcript: &str) -> String {
        format!("{transcript}\n──────────────── ↑ 20 more ─ runner-luna ─\n{composer}\n────────────────────────\ngpt-6-luna · max · 0%/272k · 0% · $0.00\n")
    }

    fn inspect_input(provider: Provider, text: &str) -> Option<InputFrame> {
        super::inspect_input(provider, text, 120)
    }

    #[test]
    fn native_input_scope_restores_nested_and_panicking_calls() {
        let request = NativeInputRequest {
            message_id: "msg_one".into(),
            owner_team_id: "team".into(),
            recipient: "worker".into(),
            provider: Provider::Pi,
            pane_id: "%1".into(),
            pane_pid: 2,
            session: "team".into(),
            window: "worker".into(),
        };
        with_native_input(Some(request.clone()), || {
            assert_eq!(current_request(), Some(request.clone()));
            with_native_input(None, || assert!(current_request().is_none()));
            assert_eq!(current_request(), Some(request));
            let _ = std::panic::catch_unwind(|| with_native_input(None, || panic!("fixture")));
            assert!(current_request().is_some());
        });
        assert!(current_request().is_none());
    }

    #[test]
    fn native_input_pi_requires_editor_and_footer_not_startup_echo() {
        let marker = "[team-agent-token:msg_one]";
        let draft = pi(
            &format!("{}\n{marker}", "line\n".repeat(26)),
            "Warning: startup",
        );
        let frame = inspect_input(Provider::Pi, &draft).unwrap();
        assert!(!frame.empty());
        assert!(frame.has_marker(marker));
        assert!(!frame.busy);
        assert!(inspect_input(Provider::Pi, &format!("Warning: startup\n{marker}")).is_none());
        assert!(inspect_input(Provider::Pi, &pi("", "old history"))
            .unwrap()
            .empty());
        let busy = pi("draft", "history").replace(
            "──────────────── ↑ 20 more ─ runner-luna ─",
            "── ⠋ Working ─────────────────────",
        );
        assert!(inspect_input(Provider::Pi, &busy).unwrap().busy);
        assert!(inspect_input(
            Provider::Pi,
            &pi("foreign draft\n────────\n", "old history")
        )
        .is_none());
    }

    const PI_R6_IDLE: &str = include_str!("../../tests/fixtures/pi-r6-idle-24.txt");

    #[test]
    fn native_input_pi_r6_idle_update_notice_preserves_empty_editor() {
        assert_eq!(
            input_digest(PI_R6_IDLE),
            "40f31e7476f628a471113c89b76a1ace0246e69f53fb7238999205f62b5d20fa"
        );
        let frame = super::inspect_input(Provider::Pi, PI_R6_IDLE, 24).unwrap();
        assert!(frame.empty());
        assert!(!frame.busy);
        assert_eq!(frame.identity, "gpt-6-luna");
        assert!(frame.transcript.contains("Package Updates Available"));
        assert!(!frame.composer.contains("Package Updates Available"));
    }

    #[test]
    fn native_input_pi_r6_notice_does_not_hide_drafts_or_busy_state() {
        let top = " pi-target ─\n\n";
        let marker = "[team-agent-token:msg_one]";
        let draft = PI_R6_IDLE.replace(top, &format!(" pi-target ─\nforeign draft\n{marker}\n"));
        let frame = super::inspect_input(Provider::Pi, &draft, 24).unwrap();
        assert!(!frame.empty());
        assert!(frame.has_marker(marker));
        let busy = PI_R6_IDLE.replace(
            "──────────────────────────────────────────────────────────────────── pi-target ─",
            "── ⠋ Working ──────────────────────────────────────────────────────────────────",
        );
        let frame = super::inspect_input(Provider::Pi, &busy, 24).unwrap();
        assert!(frame.empty());
        assert!(frame.busy);
        for composer in ["foreign draft\n────────\n", "\n────────\n"]
        {
            let foreign = PI_R6_IDLE.replace(top, &format!(" pi-target ─\n{composer}\n"));
            assert!(super::inspect_input(Provider::Pi, &foreign, 24).is_none());
        }
    }

    #[test]
    fn native_input_pi_r6_incomplete_or_ambiguous_notice_refuses() {
        for frame in [
            PI_R6_IDLE.replace("Package Updates Available", "Unknown notification"),
            PI_R6_IDLE.replace(" - pi-mcp-adapter", "not a native package row"),
            PI_R6_IDLE.replace("\n\n──────────────────────────────────────────────────────────────────── pi-target", "\nforeign draft\n──────────────────────────────────────────────────────────────────── pi-target"),
        ] {
            assert!(super::inspect_input(Provider::Pi, &frame, 24).is_none());
        }
        // If the complete box itself could be editor content, geometry cannot
        // distinguish it from a pasted imitation: keep the old refusal.
        assert!(super::inspect_input(Provider::Pi, PI_R6_IDLE, 120).is_none());
    }

    const PI_R7_FOLDED: &str = include_str!("../../tests/fixtures/pi-r7-folded-24.txt");

    #[test]
    fn native_input_pi_r7_capsule_is_current_composer_not_history() {
        assert_eq!(
            input_digest(PI_R7_FOLDED),
            "b7ea6992c3303743b50714fb55ffc8b91ac5ad168e0acd015428ca5af538ed0e"
        );
        let frame = super::inspect_input(Provider::Pi, PI_R7_FOLDED, 24).unwrap();
        assert!(!frame.empty());
        assert!(!frame.busy);
        assert_eq!(frame.capsule.as_deref(), Some("[paste #1 +30 lines]"));
        assert!(!frame.has_marker("[team-agent-token:msg_one]"));
        let history = format!("[paste #1 +30 lines]\n{PI_R6_IDLE}");
        let frame = super::inspect_input(Provider::Pi, &history, 24).unwrap();
        assert!(frame.empty());
        assert!(frame.capsule.is_none());
        let extra = PI_R7_FOLDED.replace(
            "[paste #1 +30 lines]",
            "[paste #1 +30 lines]\nforeign draft",
        );
        let frame = super::inspect_input(Provider::Pi, &extra, 24).unwrap();
        assert!(!frame.empty());
        assert!(frame.capsule.is_none());
    }

    #[test]
    fn native_input_pi_capsule_grammar_is_canonical_and_provider_specific() {
        for label in ["[paste #1 +30 lines]", "[paste #2 1234 chars]"] {
            assert_eq!(pi_paste_capsule(label), Some(label));
        }
        for label in [
            "[paste #0 +30 lines]",
            "[paste #01 +30 lines]",
            "[paste #1 +0 lines]",
            "[paste #1 +030 lines]",
            "[paste #18446744073709551616 +30 lines]",
            "[paste #1 +18446744073709551616 lines]",
            "[paste #1 -30 lines]",
            "[paste #1 +30 line]",
            "[paste #1 +30 lines]suffix",
            "prefix[paste #1 +30 lines]",
            "[paste #1 +30 lines]\n[paste #2 +40 lines]",
            "[Pasted text #1 +30 lines]",
        ] {
            assert_eq!(pi_paste_capsule(label), None, "{label}");
        }
    }

    // Native Issue #328 current-composer rows; transcript removed and private
    // cwd replaced with /owned/project. Startup banner is genuinely absent
    // from the owned pane's Tail(200), not a fabricated invalid version.
    const CURSOR_FOLLOWUP: &str =
        include_str!("../../tests/fixtures/cursor-followup-headerless.txt");

    #[test]
    fn native_input_cursor_clean_followup_does_not_require_scrolled_startup_banner() {
        let ready = inspect_input(Provider::CursorAgent, CURSOR_FOLLOWUP).unwrap();
        assert!(ready.empty());
        assert!(!ready.busy);
        assert!(ready.capsule.is_none());
        let header = format!("Cursor Agent\nv2026.10.14-0f9edde\n{CURSOR_FOLLOWUP}");
        let with_header = inspect_input(Provider::CursorAgent, &header).unwrap();
        assert!(with_header.empty() && !with_header.busy);
        assert_eq!(ready.footer, with_header.footer);
        assert_eq!(
            ready.identity, with_header.identity,
            "startup history is not a new UI identity"
        );
    }

    #[test]
    fn native_input_cursor_headerless_followup_retains_draft_busy_and_footer_fences() {
        let label = "[Pasted text #2 +25 lines]";
        let folded = CURSOR_FOLLOWUP.replace("Add a follow-up", label);
        let draft = inspect_input(Provider::CursorAgent, &folded).unwrap();
        assert!(!draft.empty());
        assert_eq!(draft.capsule.as_deref(), Some(label));
        let dirty = CURSOR_FOLLOWUP.replace("Add a follow-up", "Add a follow-up draft");
        assert!(!inspect_input(Provider::CursorAgent, &dirty)
            .unwrap()
            .empty());
        for dirty in [
            CURSOR_FOLLOWUP.replace("Add a follow-up", "Add a follow-up\nforeign draft"),
            CURSOR_FOLLOWUP.replace("Add a follow-up", &format!("{label}\nforeign draft")),
        ] {
            assert!(inspect_input(Provider::CursorAgent, &dirty).is_none());
        }
        let busy = CURSOR_FOLLOWUP.replace("Add a follow-up", "Add a follow-up ctrl+c to stop");
        assert!(inspect_input(Provider::CursorAgent, &busy).unwrap().busy);
        let working = format!("⠘⠆ Working\n{CURSOR_FOLLOWUP}");
        assert!(inspect_input(Provider::CursorAgent, &working).unwrap().busy);
        for unknown in [
            "→ Add a follow-up\n".to_string(),
            CURSOR_FOLLOWUP.replace("Run Everything", "unknown footer"),
            CURSOR_FOLLOWUP.replace("  /owned/project\n", ""),
        ] {
            assert!(inspect_input(Provider::CursorAgent, &unknown).is_none());
        }
    }

    #[test]
    fn native_input_cursor_real_expanded_followup_is_nonempty_current_marker_not_capsule() {
        let expanded = include_str!("../../tests/fixtures/cursor-followup-expanded.txt");
        let marker = "[team-agent-token:msg_one]";
        for prefix in [
            "".to_string(),
            format!("Cursor Agent\nv2026.10.14-0f9edde\n{CURSOR_FOLLOWUP}"),
        ] {
            let text = format!("{prefix}{expanded}");
            let frame = inspect_input(Provider::CursorAgent, &text).unwrap();
            assert!(!frame.empty() && !frame.busy);
            assert!(frame.has_marker(marker));
            assert!(frame.capsule.is_none());
            assert!(!frame.transcript.contains(marker));
            let changed = text.replace("/owned/project", "/different/project");
            assert_ne!(
                frame.identity,
                inspect_input(Provider::CursorAgent, &changed)
                    .unwrap()
                    .identity
            );
        }
        let duplicate = expanded.replace(marker, &format!("{marker}\n{marker}"));
        assert!(!inspect_input(Provider::CursorAgent, &duplicate)
            .unwrap()
            .has_marker(marker));
        let busy = expanded.replace("  →", "⠘⠆ Working\n  →");
        assert!(inspect_input(Provider::CursorAgent, &busy).unwrap().busy);
        for unknown in [
            "→\nGrok 4.7 High Run Everything\n/owned/project\n".to_string(),
            "→\nforeign draft\n→ Add a follow-up\nGrok 4.7 High Run Everything\n/owned/project\n"
                .to_string(),
            expanded.replace("Run Everything", "unknown footer"),
            expanded.replace("  /owned/project\n", ""),
            format!("{expanded}{}", "unknown current row\n".repeat(24)),
        ] {
            assert!(super::inspect_input(Provider::CursorAgent, &unknown, 24).is_none());
        }
    }

    #[test]
    fn native_input_cursor_requires_composer_in_current_viewport_not_old_history() {
        for header in ["", "Cursor Agent\nv2026.10.14-0f9edde\n"] {
            let hidden = format!(
                "{header}{CURSOR_FOLLOWUP}{}",
                "unknown current row\n".repeat(24)
            );
            assert!(super::inspect_input(Provider::CursorAgent, &hidden, 24).is_none());
        }
        assert!(super::inspect_input(Provider::CursorAgent, CURSOR_FOLLOWUP, 0).is_none());
        let current = super::inspect_input(Provider::CursorAgent, CURSOR_FOLLOWUP, 24).unwrap();
        let changed = CURSOR_FOLLOWUP.replace("/owned/project", "/different/project");
        let changed = super::inspect_input(Provider::CursorAgent, &changed, 24).unwrap();
        assert_ne!(current.identity, changed.identity);
    }

    #[test]
    fn native_input_cursor_separates_folded_draft_from_history_and_busy() {
        let capsule = "[Pasted text #1 +25 lines]";
        let draft = inspect_input(Provider::CursorAgent, &cursor(capsule, "")).unwrap();
        assert!(!draft.empty());
        assert_eq!(draft.capsule.as_deref(), Some(capsule));
        let ready = inspect_input(
            Provider::CursorAgent,
            &cursor("Plan, search, build anything", capsule),
        )
        .unwrap();
        assert!(ready.empty());
        assert!(ready.capsule.is_none());
        let busy = inspect_input(
            Provider::CursorAgent,
            &cursor("Add a follow-up ctrl+c to stop", capsule),
        )
        .unwrap();
        assert!(busy.busy);
        assert!(
            inspect_input(Provider::CursorAgent, &cursor(capsule, "⠘⠆ Working"))
                .unwrap()
                .busy
        );
        assert!(inspect_input(Provider::CursorAgent, "→ [Pasted text #1 +25 lines]").is_none());
        assert!(inspect_input(
            Provider::CursorAgent,
            &cursor("Plan, search, build anything\nextra editable text", "")
        )
        .is_none());
        assert!(inspect_input(
            Provider::CursorAgent,
            &cursor("[Pasted text #1 +25 lines]\nsecond block", "")
        )
        .is_none());
        assert!(inspect_input(
            Provider::CursorAgent,
            "Cursor Agent\nv2026.10.01-e373342\n→ Run Everything\n"
        )
        .is_none());
        let malformed = inspect_input(
            Provider::CursorAgent,
            &cursor("[Pasted text #0 +25 lines]", ""),
        )
        .unwrap();
        assert!(malformed.capsule.is_none());
    }
}
