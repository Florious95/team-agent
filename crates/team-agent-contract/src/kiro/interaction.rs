//! Kiro 2.28.0 V2/TUI grammar from the scoped R0 captures. The single Enter
//! budget is fixed; neither a slow reply nor a busy screen permits resubmission.
//! Bracketed multi-line delivery is a candidate behavior, not an R0 PASS claim.
use super::KiroAdapter;
use crate::contract::{delivery::*, hooks::*, probe::*, session::*, types::*};
use crate::host::digest;
use crate::runtime::delivery::{ControlDefinition, ControlKind};
use std::time::Duration;

pub const PROFILE: &str = "kiro-v2-tui-macos";
pub const POLICY: Digest = Digest([
    164, 212, 44, 211, 110, 104, 19, 79, 151, 125, 118, 45, 26, 228, 173, 129, 13, 207, 154, 246,
    237, 237, 197, 25, 27, 160, 229, 245, 129, 12, 178, 213,
]);
pub static PROFILES: &[InputProfile] = &[InputProfile {
    id: PROFILE,
    identity: ProfileIdentity::RuntimeCaptured,
    harness: "v2",
    ui: "tui",
    platform: Platform::MacOs,
    policy_sha256: POLICY,
    operations: &[
        Operation::FirstBusiness,
        Operation::OrdinarySend,
        Operation::SessionInspect,
        Operation::ToolInspect,
    ],
    channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed,
        payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter,
        confirmation_steps: &[],
        retry_budget: RetryBudget::Never,
        queue_flush: &[],
        max_submit_keys: 1,
        timing: InputTiming {
            capture_interval: Duration::from_millis(100),
            stable_window: Duration::from_millis(200),
            paste_to_submit_floor: Duration::from_millis(100),
            deadline: Duration::from_secs(30),
        },
    }),
}];
pub static CONTROLS: &[ControlDefinition] = &[
    ControlDefinition {
        profile_id: PROFILE,
        policy_sha256: POLICY,
        operation: Operation::SessionInspect,
        kind: ControlKind::InspectSession,
        command: "/session-id",
    },
    ControlDefinition {
        profile_id: PROFILE,
        policy_sha256: POLICY,
        operation: Operation::ToolInspect,
        kind: ControlKind::InspectTools,
        command: "/tools",
    },
];
pub static TOOL_PANEL: crate::runtime::native_panel::NativePanelPolicy =
    crate::runtime::native_panel::NativePanelPolicy {
        profile_id: PROFILE,
        policy_sha256: POLICY,
        panel_predicates: &["kiro-tools-panel", "kiro-team-tools-bound"],
        bound_predicate: "kiro-team-tools-bound",
        server_key: "team",
    };

/// The R0 native table has Name / Source / Status / Description columns. Only
/// tools in the exact bound server column count; descriptions and builtin names
/// cannot establish registry membership. Missing/scrolled-off rows stay unknown.
pub fn registry_tools(text: &str, server: &str) -> Option<Vec<LogicalTool>> {
    if composer(text).is_some()
        || !text.lines().any(|line| {
            line.split_whitespace().collect::<Vec<_>>()
                == ["Name", "Source", "Status", "Description"]
        })
        || text.lines().map(str::trim).rfind(|line| !line.is_empty())
            != Some("esc to close · ↑↓ to scroll")
    {
        return None;
    }
    let source = format!("mcp:{server}");
    let mut tools = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let name = fields.next();
        if fields.next() != Some(source.as_str()) {
            continue;
        }
        let tool = match name {
            Some("send_message") => LogicalTool::SendMessage,
            Some("report_result") => LogicalTool::ReportResult,
            Some("get_team_status") => LogicalTool::GetTeamStatus,
            _ => continue,
        };
        if !tools.contains(&tool) {
            tools.push(tool);
        }
    }
    Some(tools)
}

fn composer(text: &str) -> Option<(usize, &str)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            line.trim_start()
                .strip_prefix('›')
                .map(|value| (index, value.trim()))
        })
        .last()
}
fn empty(value: &str) -> bool {
    value.is_empty() || value == "ask a question or describe a task ↵"
}
fn surface(text: &str) -> InputSurface {
    let input = composer(text);
    let busy = text
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim();
            line.starts_with("Thinking") || line.starts_with("Kiro is working")
        })
        .map(|(index, _)| index)
        .last();
    if busy.is_some_and(|line| input.is_none_or(|(index, _)| line > index)) {
        return InputSurface::Busy;
    }
    match input {
        Some((_, value)) if empty(value) => InputSurface::ComposerReady,
        Some(_) => InputSurface::ComposerContainsPaste,
        None => InputSurface::ShellOrUnknown,
    }
}

impl InteractionHook for KiroAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let mut observation = InteractionObservation {
            scope: frame.scope.clone(),
            surface: InputSurface::ShellOrUnknown,
            predicate: None,
            current_message: None,
            current_attempt: None,
            paste_latch: frame.paste_latch.clone(),
        };
        if frame.profile_id != PROFILE {
            return observation;
        }
        observation.surface = surface(&frame.text);
        let tools = registry_tools(&frame.text, "team");
        if let Some(tools) = &tools {
            observation.predicate = Some(
                if TEAM_TOOLS.iter().all(|tool| tools.contains(tool)) {
                    "kiro-team-tools-bound"
                } else {
                    "kiro-tools-panel"
                }
                .into(),
            );
        }
        let expected = match (&frame.message, frame.operation) {
            (Some(message), Operation::FirstBusiness | Operation::OrdinarySend) => {
                format!("[team-agent-token:{}]", message.as_str())
            }
            (None, Operation::SessionInspect) => "/session-id".into(),
            (None, Operation::ToolInspect) => "/tools".into(),
            _ => return observation,
        };
        let current = composer(&frame.text).map(|(index, _)| {
            frame
                .text
                .lines()
                .skip(index)
                .collect::<Vec<_>>()
                .join("\n")
        });
        let new_token = frame
            .baseline
            .as_ref()
            .is_some_and(|baseline| !baseline.text.contains(&expected));
        if frame.after_step == Some(StepKind::Paste)
            && new_token
            && current
                .as_ref()
                .is_some_and(|text| text.contains(&expected))
        {
            observation.surface = InputSurface::ComposerContainsPaste;
            observation.paste_latch = PasteLatch::Seen {
                native_identity: expected.clone(),
            };
            observation.current_message = frame.message.clone();
            observation.current_attempt = frame.attempt.clone();
        }
        if frame.after_step == Some(StepKind::InitialSubmit)
            && matches!(&frame.paste_latch, PasteLatch::Seen { native_identity } if native_identity == &expected)
            && !current
                .as_ref()
                .is_some_and(|text| text.contains(&expected))
        {
            let accepted = if frame.operation == Operation::SessionInspect {
                session_id(&frame.text).is_ok()
            } else if frame.operation == Operation::ToolInspect {
                tools.is_some()
            } else {
                // A disappeared paste alone is NOT acceptance. Require the exact
                // current token in the native transcript and a busy/ready surface.
                frame.text.contains(&expected)
                    && matches!(
                        observation.surface,
                        InputSurface::Busy | InputSurface::ComposerReady
                    )
            };
            if accepted {
                observation.surface = InputSurface::NativeAccepted;
                observation.paste_latch = PasteLatch::Gone {
                    native_identity: expected,
                };
                observation.current_message = frame.message.clone();
                observation.current_attempt = frame.attempt.clone();
            }
        }
        observation
    }
}

/// Only a command-labelled ID with its matching native resume hint is accepted.
/// Neither an arbitrary UUID in model output nor newest/listed sessions bind a seat.
pub fn session_id(text: &str) -> Result<NativeSessionId, ContractError> {
    let mut ids = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("• Session ID: "));
    let id = ids
        .next()
        .ok_or(ContractError::Invalid("Kiro session command output"))?;
    if ids.next().is_some() {
        return Err(ContractError::Invalid("ambiguous Kiro session ID"));
    }
    let uuid = id.strip_prefix("sess_").unwrap_or(id);
    if uuid.len() != 36
        || !uuid.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        || !text
            .lines()
            .any(|line| line.trim() == format!("Resume with: kiro-cli --resume-id {id}"))
    {
        return Err(ContractError::Invalid("Kiro session ID grammar"));
    }
    NativeSessionId::new(id)
}
impl SessionHook for KiroAdapter {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.provider.as_str() != "kiro"
            || !super::native::is_release_version(&evidence.native.version)
            || !PROFILES[0].matches_native(&evidence.native)
            || evidence.origin != CaptureOrigin::CurrentNativeSession
            || evidence.evidence_sha256 != digest(&evidence.record)
        {
            return Err(ContractError::Mismatch("Kiro session capture provenance"));
        }
        let text = std::str::from_utf8(&evidence.record)
            .map_err(|_| ContractError::Invalid("Kiro session UTF-8"))?;
        Ok(ResumeBinding {
            provider: evidence.provider.clone(),
            native_session: session_id(text)?,
            storage: SessionStorage::Local,
            cwd: evidence.cwd.clone(),
            native: evidence.native.clone(),
            source: evidence.scope.identity.clone(),
            origin: evidence.origin,
            evidence_kind: evidence.evidence_kind,
            evidence_sha256: evidence.evidence_sha256,
            backing: None,
        })
    }
}
impl SemanticReader for KiroAdapter {
    fn read(&self, record: &NativeRecord) -> Probe<SemanticEvidence> {
        let outcome = if record.kind != NativeRecordKind::Surface {
            ProbeOutcome::Unknown(super::NATIVE_UNVERIFIED)
        } else {
            match std::str::from_utf8(&record.bytes).ok().map(surface) {
                Some(InputSurface::Busy) => ProbeOutcome::Observed(SemanticEvidence::Busy),
                Some(InputSurface::ComposerReady) => ProbeOutcome::Observed(SemanticEvidence::Idle),
                _ => ProbeOutcome::Unknown(super::NATIVE_UNVERIFIED),
            }
        };
        Probe {
            scope: record.scope.clone(),
            outcome,
        }
    }
}
