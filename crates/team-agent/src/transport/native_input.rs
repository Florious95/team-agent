//! Owned legacy-tmux composer evidence. No keys, persistence, or provider admission.

use serde::{Deserialize, Serialize};

use crate::model::enums::Provider;



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






#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFrame {
    pub composer: String,
    pub transcript: String,
    pub identity: String,
    pub footer: String,
    pub busy: bool,
    pub capsule: Option<String>,
}


pub(crate) fn supports_input_observation(provider: Provider) -> bool {
    provider == Provider::Pi
}

pub(crate) fn inspect_input(
    provider: Provider,
    text: &str,
    pane_height: u32,
) -> Option<InputFrame> {
    match provider {
        Provider::Pi => pi_input(text, pane_height),
        _ => None,
    }
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

    fn pi(composer: &str, transcript: &str) -> String {
        format!("{transcript}\n──────────────── ↑ 20 more ─ runner-luna ─\n{composer}\n────────────────────────\ngpt-6-luna · max · 0%/272k · 0% · $0.00\n")
    }

    fn inspect_input(provider: Provider, text: &str) -> Option<InputFrame> {
        super::inspect_input(provider, text, 120)
    }


    #[test]
    fn native_input_pi_requires_editor_and_footer_not_startup_echo() {
        let marker = "[team-agent-token:msg_one]";
        let draft = pi(
            &format!("{}\n{marker}", "line\n".repeat(26)),
            "Warning: startup",
        );
        let frame = inspect_input(Provider::Pi, &draft).unwrap();
        assert!(!frame.composer.trim().is_empty());
        assert!((frame.composer.lines().map(str::trim).collect::<String>().matches(marker).count() == 1));
        assert!(!frame.busy);
        assert!(inspect_input(Provider::Pi, &format!("Warning: startup\n{marker}")).is_none());
        assert!(inspect_input(Provider::Pi, &pi("", "old history"))
            .unwrap()
            .composer.trim().is_empty());
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
            format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(PI_R6_IDLE.as_bytes())),
            "40f31e7476f628a471113c89b76a1ace0246e69f53fb7238999205f62b5d20fa"
        );
        let frame = super::inspect_input(Provider::Pi, PI_R6_IDLE, 24).unwrap();
        assert!(frame.composer.trim().is_empty());
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
        assert!(!frame.composer.trim().is_empty());
        assert!((frame.composer.lines().map(str::trim).collect::<String>().matches(marker).count() == 1));
        let busy = PI_R6_IDLE.replace(
            "──────────────────────────────────────────────────────────────────── pi-target ─",
            "── ⠋ Working ──────────────────────────────────────────────────────────────────",
        );
        let frame = super::inspect_input(Provider::Pi, &busy, 24).unwrap();
        assert!(frame.composer.trim().is_empty());
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
            format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(PI_R7_FOLDED.as_bytes())),
            "b7ea6992c3303743b50714fb55ffc8b91ac5ad168e0acd015428ca5af538ed0e"
        );
        let frame = super::inspect_input(Provider::Pi, PI_R7_FOLDED, 24).unwrap();
        assert!(!frame.composer.trim().is_empty());
        assert!(!frame.busy);
        assert_eq!(frame.capsule.as_deref(), Some("[paste #1 +30 lines]"));
        assert!(!(frame.composer.lines().map(str::trim).collect::<String>().matches("[team-agent-token:msg_one]").count() == 1));
        let history = format!("[paste #1 +30 lines]\n{PI_R6_IDLE}");
        let frame = super::inspect_input(Provider::Pi, &history, 24).unwrap();
        assert!(frame.composer.trim().is_empty());
        assert!(frame.capsule.is_none());
        let extra = PI_R7_FOLDED.replace(
            "[paste #1 +30 lines]",
            "[paste #1 +30 lines]\nforeign draft",
        );
        let frame = super::inspect_input(Provider::Pi, &extra, 24).unwrap();
        assert!(!frame.composer.trim().is_empty());
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
}
