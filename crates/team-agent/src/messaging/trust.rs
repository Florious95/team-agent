//! leader_panes.py — trust 自动应答 (step 10 主拥,step 11 借;card §22)。§11 canonical-token
//! trust matcher:**只对自己工作目录 realpath 全等**才应答,禁 basename/startswith/子串/反推 cwd。

use std::path::Path;

use crate::event_log::EventLog;
use crate::transport::{InjectPayload, Key, PaneId, Target, Transport};

use super::{MessagingError, PaneWidthQuery};

/// trust 自动应答结果 (`leader_panes.py:383` `attempt_trust_auto_answer` 返回 dict 的 typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAnswerOutcome {
    pub ok: bool,
    pub answered: bool,
    /// `not_opted_in`/`pane_id_missing`/`already_answered`/`workspace_dir_mismatch`/
    /// `trust_auto_answered`/`tmux_send_keys_failed`。
    pub reason: String,
    pub action: Option<String>,
}

/// `attempt_trust_auto_answer` (`leader_panes.py:383`):**只对自己工作目录 realpath 全等**才
/// 自动应答 Codex trust (card §124:禁 basename/startswith/子串/反推 cwd)。pane_width 来自
/// [`PaneWidthQuery`] 注入 (`state["pane_width"]`),失败时 matcher 退回精确相等。
pub fn attempt_trust_auto_answer(
    workspace: &Path,
    transport: &dyn Transport,
    pane_id: Option<&PaneId>,
    pane_capture_tail: &str,
    pane_width: &PaneWidthQuery,
    event_log: &EventLog,
) -> Result<TrustAnswerOutcome, MessagingError> {
    let Some(pane_id) = pane_id else {
        return Ok(TrustAnswerOutcome {
            ok: false,
            answered: false,
            reason: "pane_id_missing".to_string(),
            action: None,
        });
    };
    let canonical = match std::fs::canonicalize(workspace) {
        Ok(path) => path,
        Err(err) => {
            return Ok(TrustAnswerOutcome {
                ok: false,
                answered: false,
                reason: format!("workspace_realpath_failed:{err}"),
                action: Some("prompt_leader".to_string()),
            });
        }
    };
    if workspace_matches_prompt_tail(&canonical, pane_capture_tail, pane_width) {
        let target = Target::Pane(pane_id.clone());
        match submit_trust_answer(transport, &target) {
            Ok(true) => {
                event_log.write(
                    "leader_panes.trust_auto_answered",
                    serde_json::json!({"pane_id": pane_id.as_str(), "workspace": canonical.to_string_lossy()}),
                )?;
            }
            Ok(false) => {
                event_log.write(
                    "leader_panes.trust_auto_answer_transport_panic",
                    serde_json::json!({"pane_id": pane_id.as_str()}),
                )?;
            }
            Err(error) => {
                event_log.write(
                    "leader_panes.trust_auto_answer_failed",
                    serde_json::json!({"pane_id": pane_id.as_str(), "error": error.to_string()}),
                )?;
                return Ok(TrustAnswerOutcome {
                    ok: false,
                    answered: false,
                    reason: "tmux_send_keys_failed".to_string(),
                    action: Some("prompt_leader".to_string()),
                });
            }
        }
        return Ok(TrustAnswerOutcome {
            ok: true,
            answered: true,
            reason: "trust_auto_answered".to_string(),
            action: None,
        });
    }
    Ok(TrustAnswerOutcome {
        ok: false,
        answered: false,
        reason: "workspace_dir_mismatch".to_string(),
        action: Some("prompt_leader".to_string()),
    })
}

fn submit_trust_answer(
    transport: &dyn Transport,
    target: &Target,
) -> Result<bool, crate::transport::TransportError> {
    let submitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.inject(target, &InjectPayload::Empty, Key::Enter, false)
    }));
    match submitted {
        Ok(result) => result.map(|_| true),
        Err(_) => Ok(false),
    }
}

fn workspace_matches_prompt_tail(
    canonical_workspace: &Path,
    pane_capture_tail: &str,
    pane_width: &PaneWidthQuery,
) -> bool {
    let workspace_text = canonical_workspace.to_string_lossy();
    for token in path_tokens(pane_capture_tail) {
        if token_matches_workspace(&token, &workspace_text, pane_capture_tail, pane_width) {
            return true;
        }
    }
    false
}

fn path_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|part| {
            let token = part.trim_matches(token_trim_char);
            if token.contains('/') {
                Some(token.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn token_trim_char(c: char) -> bool {
    matches!(
        c,
        '?' | '!'
            | ','
            | ';'
            | ':'
            | '"'
            | '\''
            | '`'
            | '['
            | ']'
            | '('
            | ')'
            | '{'
            | '}'
            | '<'
            | '>'
            | '│'
            | '┃'
            | '┆'
            | '┊'
            | '┌'
            | '┐'
            | '└'
            | '┘'
            | '─'
            | '═'
    )
}

fn token_matches_workspace(
    token: &str,
    workspace_text: &str,
    pane_capture_tail: &str,
    pane_width: &PaneWidthQuery,
) -> bool {
    if let Ok(path) = std::fs::canonicalize(token) {
        return path.to_string_lossy() == workspace_text;
    }
    if token.contains('…') || token.contains("...") {
        return ellipsis_matches_workspace(token, workspace_text);
    }
    match pane_width {
        PaneWidthQuery::Ok { pane_width } => {
            workspace_text.starts_with(token)
                && token.len() < workspace_text.len()
                && token_reaches_right_edge(token, pane_capture_tail, *pane_width)
        }
        PaneWidthQuery::Failed { .. } => false,
    }
}

fn ellipsis_matches_workspace(token: &str, workspace_text: &str) -> bool {
    let parts: Vec<&str> = if token.contains('…') {
        token.split('…').collect()
    } else {
        token.split("...").collect()
    };
    if parts.len() != 2 {
        return false;
    }
    let head = parts[0];
    let tail = parts[1];
    !head.is_empty()
        && !tail.is_empty()
        && workspace_text.starts_with(head)
        && workspace_text.ends_with(tail)
}

fn token_reaches_right_edge(token: &str, text: &str, pane_width: u32) -> bool {
    let width = match usize::try_from(pane_width) {
        Ok(width) if width > 0 => width,
        _ => return false,
    };
    text.lines().any(|line| {
        line.find(token)
            .map(|idx| idx.saturating_add(token.len()) >= width)
            .unwrap_or(false)
    })
}
