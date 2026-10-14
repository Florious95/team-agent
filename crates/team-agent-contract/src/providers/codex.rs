//! ---
//! purpose: Codex CLI phase-1 contract leaf for the fifteen facets and seven hooks.
//! contract: Unregistered descriptor. Fresh and Resume validate; launch stays unverified at the model gate.
//! boundary: No registry entry, no process signals, no global config writes, no credential or rollout-body reads.
//! ---

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::contract::delivery::{CaptureFrame, InputSurface, InteractionObservation};
use crate::contract::descriptor::*;
use crate::contract::hooks::{
    InteractionHook, PlanHook, ProviderHooks, ScopedSessionEvidence, SessionHook,
};
use crate::contract::plan::{LaunchPlan, ResolvedLaunch};
use crate::contract::session::{CaptureOrigin, NativeSessionId, ResumeBinding, SessionStorage};
use crate::contract::types::*;

const MODEL: Reason = Reason {
    code: "codex-model-catalog-not-run",
    message: "Codex model catalog was not measured; a configured model is not a bounded catalog",
};
const EFFORT: Reason = Reason {
    code: "codex-effort-not-remeasured",
    message: "Effort tiers were not remeasured on this Codex CLI seat",
};
const LEADER: Reason = Reason {
    code: "codex-leader-not-run",
    message: "Codex leader operation was not run",
};
const OFFICIAL_API: Reason = Reason {
    code: "codex-official-api-not-run",
    message: "Codex official API authentication was not run",
};
const COMPATIBLE_API: Reason = Reason {
    code: "codex-compatible-api-not-run",
    message: "Codex compatible API authentication was not run",
};
const BYPASS: Reason = Reason {
    code: "codex-bypass-modal-interaction-not-run",
    message: "Codex bypass interaction with the startup modals was not run",
};
const PROMPT: Reason = Reason {
    code: "codex-prompt-bytes-not-observed",
    message: "Codex prompt bytes were not observed",
};
const MCP: Reason = Reason {
    code: "codex-mcp-scoped-mount-not-proven",
    message: "A scoped Codex MCP mount was not proven",
};
const TOOL_NAMES: Reason = Reason {
    code: "codex-tool-names-not-observed",
    message: "Codex native tool names were not observed",
};
const RESUME: Reason = Reason {
    code: "codex-live-resume-not-run",
    message: "Live Codex resume was not run",
};
const FORK: Reason = Reason {
    code: "codex-fork-not-run",
    message: "Codex fork was not run",
};
const INPUT: Reason = Reason {
    code: "codex-submit-not-run",
    message: "Codex submit was not run",
};
const CONSENT: Reason = Reason {
    code: "codex-startup-consent-not-accepted",
    message: "Codex startup consent was not accepted",
};
const SERVER: Reason = Reason {
    code: "codex-server-socket-holder-unknown",
    message: "Codex server socket holder is unknown",
};
const CLIENT_BINDING: Reason = Reason {
    code: "codex-client-binding-not-run",
    message: "Codex client binding was not run",
};
const ROUND_TRIP: Reason = Reason {
    code: "codex-round-trip-not-run",
    message: "Codex round trip was not run",
};
const SEMANTIC: Reason = Reason {
    code: "codex-semantic-not-run",
    message: "Codex semantic probe was not run",
};
const LAUNCH: Reason = Reason {
    code: "codex-launch-not-admitted",
    message: "Codex launch is not admitted",
};
const H1: Reason = Reason {
    code: "codex-h1-catalog-not-run",
    message: "Codex H1 catalog hook was not run",
};
const H3: Reason = Reason {
    code: "codex-h3-no-owned-mount",
    message: "Codex has no owned MCP mount",
};
const H5: Reason = Reason {
    code: "codex-h5-semantic-not-run",
    message: "Codex H5 semantic hook was not run",
};
const H7: Reason = Reason {
    code: "codex-h7-fork-not-run",
    message: "Codex H7 fork hook was not run",
};
const FIXTURE: Reason = Reason {
    code: "codex-fixture-session-rejected",
    message: "Fixture session evidence is rejected",
};
const PATH_NOT_ABSOLUTE: Reason = Reason {
    code: "codex-rollout-path-not-absolute",
    message: "Codex rollout path is not absolute",
};
const RECORD_UNREADABLE: Reason = Reason {
    code: "codex-session-record-unreadable",
    message: "Codex session record is not a two-field UTF-8 object",
};
const SESSION_ID: Reason = Reason {
    code: "codex-session-id-not-uuid-v7",
    message: "Codex session id is not a UUID version 7",
};
const ROLLOUT_NAME: Reason = Reason {
    code: "codex-rollout-name-mismatch",
    message: "Codex rollout file name does not match the session id",
};
const ROLLOUT_DATE: Reason = Reason {
    code: "codex-rollout-date-mismatch",
    message: "Codex rollout date does not match the date directory",
};

const PHASE1_INTERACTIVE_ARGUMENTS: &[&str] = &["--no-alt-screen", "--no-daemon"];
const FORBIDDEN_INTERACTIVE_ARGUMENTS: &[&str] = &[
    "--last",
    "npm",
    "@openai/codex",
    "--dangerously-bypass-approvals-and-sandbox",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexImageRole {
    SeatWrapper,
    SeatEngine,
    SharedDaemon,
    PathShadow,
    Unknown,
}

pub static CODEX_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet {
        id: "codex",
        display_name: "Codex CLI (contract)",
        binary: "codex",
        aliases: &[],
        leader: Support::Unverified(LEADER),
    },
    model: ModelFacet {
        selection: Support::Unverified(MODEL),
        omitted: OmittedModel::Reject,
        catalog: Support::Unverified(MODEL),
    },
    effort: EffortFacet {
        admission: [const { EffortAdmission::Reject(EFFORT) }; 6],
        carrier: Support::Unverified(EFFORT),
        inherit_team_default: false,
        model_dependent: false,
    },
    auth: AuthFacet {
        subscription: Support::Supported(AuthMechanism::NativeExistingSession),
        official_api: Support::Unverified(OFFICIAL_API),
        compatible_api: Support::Unverified(COMPATIBLE_API),
    },
    bypass: BypassFacet {
        intent: Support::Unverified(BYPASS),
    },
    prompt: PromptFacet {
        carrier: Support::Unverified(PROMPT),
    },
    mcp: McpFacet {
        carrier: Support::Unverified(MCP),
        scope: ResourceScope::RuntimeRoot,
        tools: &[],
        isolation: McpIsolation::Unverified,
    },
    tool_names: ToolNameFacet {
        naming: Support::Unverified(TOOL_NAMES),
    },
    session: SessionFacet {
        fresh: FreshSession::CaptureAfterLaunch,
        resume: Support::Unverified(RESUME),
    },
    fork: ForkFacet {
        in_window: Support::Unverified(FORK),
        full_snapshot: Support::Unverified(FORK),
        native_new_seat: Support::Unverified(FORK),
        allowed_auth: &[],
    },
    input: InputFacet {
        profiles: Support::Unverified(INPUT),
    },
    startup: StartupFacet {
        interactive: true,
        authorized_consent: Support::Unverified(CONSENT),
    },
    probe_sources: ProbeSourcesFacet {
        process: Support::Supported(ProbeSource::Host),
        pane: Support::Supported(ProbeSource::NativeSurface),
        server: Support::Unverified(SERVER),
        client_binding: Support::Unverified(CLIENT_BINDING),
        round_trip: Support::Unverified(ROUND_TRIP),
        semantic: Support::Unverified(SEMANTIC),
    },
    workspace: WorkspaceFacet {
        config_scope: ResourceScope::RuntimeRoot,
        shared_cwd: false,
        requires_materialization_lease: true,
    },
    teardown: TeardownFacet {
        resources: &[
            ResourcePolicy {
                kind: ResourceKind::SessionBacking,
                disposition: ResourceDisposition::OwnedPreserved,
            },
            ResourcePolicy {
                kind: ResourceKind::GlobalSettings,
                disposition: ResourceDisposition::Forbidden,
            },
            ResourcePolicy {
                kind: ResourceKind::NativeDatabase,
                disposition: ResourceDisposition::Forbidden,
            },
        ],
    },
};

pub struct CodexAdapter;

impl CodexAdapter {
    pub fn hooks(&self) -> ProviderHooks<'_> {
        ProviderHooks {
            catalog: HookBinding::Unverified(H1),
            plan: HookBinding::Bound(self),
            materialize: HookBinding::Unverified(H3),
            session: HookBinding::Bound(self),
            semantic: HookBinding::Unverified(H5),
            interaction: HookBinding::Bound(self),
            fork: HookBinding::Unverified(H7),
        }
    }

    pub fn plan_phase1(&self) -> Result<LaunchPlan, ContractError> {
        Err(ContractError::Unverified {
            field: "codex launch",
            reason: LAUNCH,
        })
    }
}

impl PlanHook for CodexAdapter {
    fn plan(&self, _resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        self.plan_phase1()
    }
}

impl SessionHook for CodexAdapter {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.evidence_kind != EvidenceKind::Native {
            return Err(unverified("codex session evidence", FIXTURE));
        }
        let record = session_record(&evidence.record)?;
        let path = Path::new(&record.rollout_path);
        if !path.is_absolute() {
            return Err(unverified("codex rollout path", PATH_NOT_ABSOLUTE));
        }
        let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let date_dir = date_dir_from_parent(path).unwrap_or("");
        admit_rollout_name(&record.session_id, file_name, date_dir)?;
        Ok(ResumeBinding {
            provider: ProviderId::new("codex")?,
            native_session: NativeSessionId::new(record.session_id)?,
            storage: SessionStorage::Local,
            cwd: evidence.cwd.clone(),
            native: evidence.native.clone(),
            source: evidence.scope.identity.clone(),
            origin: CaptureOrigin::ExactBacking,
            evidence_kind: evidence.evidence_kind,
            evidence_sha256: evidence.evidence_sha256,
            backing: Some(PathBuf::from(&record.rollout_path)),
        })
    }
}

impl InteractionHook for CodexAdapter {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let (surface, predicate) = if frame.text.contains("Trust and continue") {
            (
                InputSurface::StartupRiskWarning,
                Some("codex-folder-trust-modal".to_string()),
            )
        } else if frame.text.contains("Update available") || frame.text.contains("Update now") {
            (
                InputSurface::StartupRiskWarning,
                Some("codex-update-modal".to_string()),
            )
        } else if frame.text.contains("Ask Codex to do anything") {
            (InputSurface::ComposerReady, None)
        } else {
            (
                InputSurface::Starting,
                Some("codex-terminal-query-stall".to_string()),
            )
        };
        InteractionObservation {
            scope: frame.scope.clone(),
            surface,
            predicate,
            current_message: frame.message.clone(),
            current_attempt: frame.attempt.clone(),
            paste_latch: frame.paste_latch.clone(),
            control_paste: None,
        }
    }
}

pub fn classify_image_role(path: &str) -> CodexImageRole {
    if path.contains("/app-server-daemon/releases/") {
        CodexImageRole::SharedDaemon
    } else if path.ends_with("/.local/bin/codex") {
        CodexImageRole::PathShadow
    } else if path.contains("/node_modules/@openai/codex/") && path.ends_with("/bin/codex.js") {
        CodexImageRole::SeatWrapper
    } else if path.contains("/codex-darwin-arm64/")
        && path.contains("/vendor/")
        && path.ends_with("/bin/codex")
    {
        CodexImageRole::SeatEngine
    } else {
        CodexImageRole::Unknown
    }
}

pub fn may_signal(role: CodexImageRole) -> bool {
    matches!(role, CodexImageRole::SeatWrapper | CodexImageRole::SeatEngine)
}

pub fn seat_pair_ok(wrapper_version: &str, engine_version: &str) -> bool {
    wrapper_version.trim() == engine_version.trim()
}

pub fn phase1_interactive_arguments() -> &'static [&'static str] {
    PHASE1_INTERACTIVE_ARGUMENTS
}

pub fn forbidden_interactive_argument(arg: &str) -> bool {
    FORBIDDEN_INTERACTIVE_ARGUMENTS.contains(&arg)
}

pub fn argv_mutates_global_config(argv: &[&str]) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == "mcp" && pair[1] == "add")
}

pub fn admit_rollout_name(
    session_id: &str,
    rollout_file_name: &str,
    date_dir: &str,
) -> Result<(), ContractError> {
    if !is_uuid_v7(session_id) {
        return Err(unverified("codex session id", SESSION_ID));
    }
    let Some(file_date) = rollout_name_date(rollout_file_name, session_id) else {
        return Err(unverified("codex rollout name", ROLLOUT_NAME));
    };
    if date_dir_parts(date_dir) != Some(file_date) {
        return Err(unverified("codex rollout date", ROLLOUT_DATE));
    }
    Ok(())
}

fn unverified(field: &'static str, reason: Reason) -> ContractError {
    ContractError::Unverified { field, reason }
}

fn is_uuid_v7(session_id: &str) -> bool {
    let bytes = session_id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'7',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn rollout_name_date<'a>(name: &'a str, session_id: &str) -> Option<(&'a str, &'a str, &'a str)> {
    let rest = name
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?
        .strip_suffix(session_id)?
        .strip_suffix('-')?;
    let bytes = rest.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b'-'
        || bytes[16] != b'-'
    {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(|byte| byte.is_ascii_digit());
    if !digits(0..4)
        || !digits(5..7)
        || !digits(8..10)
        || !digits(11..13)
        || !digits(14..16)
        || !digits(17..19)
    {
        return None;
    }
    Some((&rest[0..4], &rest[5..7], &rest[8..10]))
}

fn date_dir_parts(date_dir: &str) -> Option<(&str, &str, &str)> {
    let bytes = date_dir.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'/' || bytes[7] != b'/' {
        return None;
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return None;
    }
    Some((&date_dir[0..4], &date_dir[5..7], &date_dir[8..10]))
}

fn date_dir_from_parent(path: &Path) -> Option<&str> {
    let parent = path.parent()?.as_os_str().to_str()?;
    let mut parts = parent.rsplit('/');
    let day = parts.next()?;
    let month = parts.next()?;
    let year = parts.next()?;
    if year.is_empty() || month.is_empty() || day.is_empty() {
        return None;
    }
    let suffix_len = year.len() + month.len() + day.len() + 2;
    if parent.len() < suffix_len {
        return None;
    }
    let date_dir = &parent[parent.len() - suffix_len..];
    let mut split = date_dir.split('/');
    (split.next() == Some(year)
        && split.next() == Some(month)
        && split.next() == Some(day)
        && split.next().is_none())
    .then_some(date_dir)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexSessionRecord {
    session_id: String,
    rollout_path: String,
}

fn session_record(bytes: &[u8]) -> Result<CodexSessionRecord, ContractError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| unverified("codex session record", RECORD_UNREADABLE))?;
    serde_json::from_str(text).map_err(|_| unverified("codex session record", RECORD_UNREADABLE))
}
