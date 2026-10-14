//! ---
//! purpose: Codex CLI contract leaf for the fifteen facets and seven hooks.
//! contract: H1 catalog reads `debug models --bundled` from the seat wrapper. PlanHook stays refused. H2 is a pure argv plan plus a modal refusal.
//! boundary: No process signals, no global config writes, no credential or rollout-body reads, no static model list.
//! ---

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;

use crate::contract::delivery::{CaptureFrame, InputSurface, InteractionObservation};
use crate::contract::descriptor::*;
use crate::contract::hooks::{
    BoundedReadHost, CatalogHook, CatalogRequest, InteractionHook, PlanHook, ProviderHooks,
    ReadBounds, ReadFailure, ReadOutput, ScopedSessionEvidence, SessionHook,
};
use crate::contract::plan::{
    CarrierReport, CarrierUse, CatalogObservation, EnvironmentDelta, ExpectedSession, LaunchEntrypoint,
    LaunchPlan, ModelRecord, ResolvedLaunch,
};
use crate::contract::session::{CaptureOrigin, NativeSessionId, ResumeBinding, SessionStorage};
use crate::contract::types::*;
use crate::host::command::{CommandEnd, CommandRequest, CommandRunner, OutputLimits};
use crate::host::process::{fingerprint_file, resolve_cwd};
use crate::host::{HostError, HostErrorKind};

const SELECTION: Reason = Reason {
    code: "codex-model-selection-not-admitted",
    message: "Catalog IDs were measured, but none is admitted as a launch model",
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
const FOLDER_TRUST_MODAL: Reason = Reason {
    code: "codex-folder-trust-modal",
    message: "Codex folder trust modal blocks the frame",
};
const UPDATE_MODAL: Reason = Reason {
    code: "codex-update-modal",
    message: "Codex update modal blocks the frame",
};
const TRUST_AND_CONTINUE: &str = "Trust and continue";
const UPDATE_AVAILABLE: &str = "Update available";
const UPDATE_NOW: &str = "Update now";
const COMPOSER_PLACEHOLDER: &str = "Ask Codex to do anything";
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

pub const CATALOG_SOURCE: CatalogSource = CatalogSource {
    arguments: &["debug", "models", "--bundled"],
    schema: "codex-debug-models-bundled-json-v1",
};
pub const CATALOG_HARNESS: &str = "codex-seat-wrapper";
pub const CATALOG_UI: &str = "bundled-catalog";
/// Symlink fact only. Catalog execution uses the canonical seat wrapper.
pub const SEAT_WRAPPER_LINK: &str = "/opt/homebrew/bin/codex";
const MAX_WRAPPER_BYTES: u64 = 1024 * 1024;

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
        selection: Support::Unverified(SELECTION),
        omitted: OmittedModel::Reject,
        catalog: Support::Supported(CATALOG_SOURCE),
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
            catalog: HookBinding::Bound(self),
            plan: HookBinding::Bound(self),
            materialize: HookBinding::Unverified(H3),
            session: HookBinding::Bound(self),
            semantic: HookBinding::Unverified(H5),
            interaction: HookBinding::Bound(self),
            fork: HookBinding::Unverified(H7),
        }
    }

    pub fn plan_phase1(&self) -> Result<LaunchPlan, ContractError> {
        Err(launch_not_admitted())
    }
}

pub fn launch_not_admitted() -> ContractError {
    ContractError::Unverified {
        field: "codex launch",
        reason: LAUNCH,
    }
}

impl PlanHook for CodexAdapter {
    fn plan(&self, _resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        self.plan_phase1()
    }
}

impl CatalogHook for CodexAdapter {
    fn discover(
        &self,
        request: &CatalogRequest,
        host: &mut dyn BoundedReadHost,
    ) -> Result<CatalogObservation, ReadFailure> {
        validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline {
            return Err(ReadFailure::TimedOut {
                elapsed: output.elapsed,
            });
        }
        if output.stdout.len() > request.bounds.max_output_bytes {
            return Err(ReadFailure::OutputLimit {
                limit: request.bounds.max_output_bytes,
            });
        }
        if output.exit_code != 0 {
            return Err(ReadFailure::Exit {
                code: output.exit_code,
            });
        }
        Ok(CatalogObservation {
            provider: request.provider.clone(),
            native: request.native.clone(),
            schema: request.source.schema.into(),
            models: parse_bundled_catalog(&output.stdout).map_err(ReadFailure::Error)?,
        })
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
        let (surface, predicate) = if frame.text.contains(TRUST_AND_CONTINUE) {
            (
                InputSurface::StartupRiskWarning,
                Some(FOLDER_TRUST_MODAL.code.to_string()),
            )
        } else if frame.text.contains(UPDATE_AVAILABLE) || frame.text.contains(UPDATE_NOW) {
            (
                InputSurface::StartupRiskWarning,
                Some(UPDATE_MODAL.code.to_string()),
            )
        } else if frame.text.contains(COMPOSER_PLACEHOLDER) {
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

pub struct CodexLaunchImages {
    pub wrapper: PathBuf,
    pub wrapper_sha256: Digest,
    pub engine: PathBuf,
    pub engine_sha256: Digest,
    pub wrapper_version: String,
    pub engine_version: String,
}

pub fn interactive_launch_plan(
    images: &CodexLaunchImages,
    cwd: &Path,
    expected: &ExpectedSession,
) -> Result<LaunchPlan, ContractError> {
    let invalid = || ContractError::Invalid("codex interactive launch");
    require_absolute(&images.wrapper, "codex interactive launch")?;
    require_absolute(&images.engine, "codex interactive launch")?;
    require_absolute(cwd, "codex interactive launch")?;
    let wrapper = images.wrapper.to_str().ok_or_else(invalid)?;
    let engine = images.engine.to_str().ok_or_else(invalid)?;
    if classify_image_role(wrapper) != CodexImageRole::SeatWrapper
        || classify_image_role(engine) != CodexImageRole::SeatEngine
    {
        return Err(invalid());
    }
    if images.wrapper_version.trim().is_empty()
        || images.engine_version.trim().is_empty()
        || !seat_pair_ok(&images.wrapper_version, &images.engine_version)
    {
        return Err(invalid());
    }
    let zero = Digest([0; 32]);
    if images.wrapper_sha256 == zero
        || images.engine_sha256 == zero
        || images.wrapper_sha256 == images.engine_sha256
    {
        return Err(invalid());
    }
    if *expected != ExpectedSession::CaptureAfterLaunch {
        return Err(invalid());
    }
    if PHASE1_INTERACTIVE_ARGUMENTS.len() != 2
        || PHASE1_INTERACTIVE_ARGUMENTS
            .iter()
            .any(|arg| forbidden_interactive_argument(arg))
        || argv_mutates_global_config(PHASE1_INTERACTIVE_ARGUMENTS)
    {
        return Err(invalid());
    }
    Ok(LaunchPlan {
        executable: images.engine.clone(),
        launcher: Some(LaunchEntrypoint {
            executable: images.wrapper.clone(),
            sha256: images.wrapper_sha256,
        }),
        arguments: PHASE1_INTERACTIVE_ARGUMENTS
            .iter()
            .copied()
            .map(OsString::from)
            .collect(),
        executable_inputs: Vec::new(),
        cwd: cwd.to_path_buf(),
        environment: EnvironmentDelta {
            remove: Default::default(),
            set: Default::default(),
        },
        expected_session: ExpectedSession::CaptureAfterLaunch,
        materialization: Vec::new(),
        carriers: CarrierReport {
            model: CarrierUse::NotRequested,
            prompt: CarrierUse::NotRequested,
            mcp: CarrierUse::NotRequested,
            bypass: CarrierUse::NotRequested,
            effort: CarrierUse::NotRequested,
        },
    })
}

/// Refusal only. A clear frame does not authorize a key.
pub fn startup_modal_intercept(text: &str) -> Result<(), ContractError> {
    if text.contains(TRUST_AND_CONTINUE) {
        Err(unverified("codex startup modal", FOLDER_TRUST_MODAL))
    } else if text.contains(UPDATE_AVAILABLE) || text.contains(UPDATE_NOW) {
        Err(unverified("codex startup modal", UPDATE_MODAL))
    } else {
        Ok(())
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

pub fn resolve_seat_wrapper(path: &Path) -> Result<PathBuf, ContractError> {
    require_absolute(path, "codex catalog executable")?;
    let canonical = path
        .canonicalize()
        .map_err(|_| ContractError::Invalid("codex catalog executable"))?;
    let text = canonical
        .to_str()
        .ok_or(ContractError::Invalid("codex catalog executable"))?;
    if classify_image_role(text) != CodexImageRole::SeatWrapper {
        return Err(ContractError::Invalid("codex catalog executable"));
    }
    Ok(canonical)
}

pub fn validate_catalog_request(request: &CatalogRequest) -> Result<PathBuf, ContractError> {
    request.bounds.validate()?;
    request.native.validate()?;
    require_absolute(&request.cwd.path, "codex catalog cwd")?;
    if request.provider.as_str() != "codex"
        || request.source != CATALOG_SOURCE
        || request.native.harness != CATALOG_HARNESS
        || request.native.ui != CATALOG_UI
    {
        return Err(ContractError::Mismatch("codex catalog grant"));
    }
    resolve_seat_wrapper(&request.executable)
}

pub fn probe_seat_version<R: CommandRunner>(
    wrapper: &Path,
    runner: &mut R,
    bounds: ReadBounds,
) -> Result<NativeIdentity, HostError> {
    bounds
        .validate()
        .map_err(|_| HostError::new("Codex probe bounds", HostErrorKind::Invalid))?;
    let wrapper = resolve_seat_wrapper(wrapper)
        .map_err(|_| HostError::new("Codex seat wrapper", HostErrorKind::Invalid))?;
    let started = Instant::now();
    let hash = fingerprint_file(&wrapper, MAX_WRAPPER_BYTES, bounds.deadline)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(HostError::new("Codex version deadline", HostErrorKind::Deadline));
    }
    let output = runner.run(&catalog_command(
        &wrapper,
        &["--version"],
        None,
        ReadBounds {
            deadline: remaining,
            ..bounds
        },
    ));
    if output.end == CommandEnd::TimedOut || output.elapsed > remaining {
        return Err(HostError::new("Codex version deadline", HostErrorKind::Deadline));
    }
    if output.end == CommandEnd::OutputLimit || output.stdout.len() > bounds.max_output_bytes {
        return Err(HostError::new("Codex version output", HostErrorKind::Invalid));
    }
    if !output.success() {
        return Err(HostError::new("Codex version command", HostErrorKind::Command));
    }
    let version = std::str::from_utf8(&output.stdout)
        .map_err(|_| HostError::new("Codex version", HostErrorKind::Invalid))?
        .trim();
    if version.is_empty() {
        return Err(HostError::new("Codex version", HostErrorKind::Invalid));
    }
    if fingerprint_file(
        &wrapper,
        MAX_WRAPPER_BYTES,
        bounds.deadline.saturating_sub(started.elapsed()),
    )? != hash
    {
        return Err(HostError::new(
            "Codex seat wrapper changed during probe",
            HostErrorKind::Conflict,
        ));
    }
    Ok(NativeIdentity {
        version: version.to_string(),
        harness: CATALOG_HARNESS.into(),
        ui: CATALOG_UI.into(),
        platform: if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Windows
        },
        executable_sha256: hash,
    })
}

fn catalog_command(
    executable: &Path,
    arguments: &[&str],
    cwd: Option<PathBuf>,
    bounds: ReadBounds,
) -> CommandRequest {
    CommandRequest {
        executable: executable.into(),
        arguments: arguments.iter().map(|arg| (*arg).into()).collect(),
        cwd,
        environment: EnvironmentDelta {
            remove: Default::default(),
            set: Default::default(),
        },
        stdin: None,
        budget: bounds.deadline,
        reject_stdout: None,
        limits: OutputLimits {
            stdout: bounds.max_output_bytes,
            stderr: bounds.max_output_bytes,
            stdin: 0,
        },
    }
}

/// One captured catalog grant. The child argv is only `debug models --bundled`.
pub struct NativeCatalog<R> {
    grant: CatalogRequest,
    executable: PathBuf,
    runner: R,
}

impl<R: CommandRunner> NativeCatalog<R> {
    pub fn new(grant: CatalogRequest, runner: R) -> Result<Self, ContractError> {
        let executable = validate_catalog_request(&grant)?;
        Ok(Self {
            grant,
            executable,
            runner,
        })
    }
}

impl<R: CommandRunner> BoundedReadHost for NativeCatalog<R> {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        if *request != self.grant {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "captured codex catalog request",
            )));
        }
        let started = Instant::now();
        let verify = |executable: &Path| -> Result<(), ReadFailure> {
            if resolve_cwd(&request.cwd.path).map_err(|_| {
                ReadFailure::Error(ContractError::Mismatch("codex catalog cwd"))
            })? != request.cwd
            {
                return Err(ReadFailure::Error(ContractError::Mismatch(
                    "codex catalog cwd",
                )));
            }
            let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(ReadFailure::TimedOut {
                    elapsed: started.elapsed(),
                });
            }
            if fingerprint_file(executable, MAX_WRAPPER_BYTES, remaining).map_err(|_| {
                ReadFailure::Error(ContractError::Mismatch("codex catalog executable"))
            })? != request.native.executable_sha256
            {
                return Err(ReadFailure::Error(ContractError::Mismatch(
                    "codex catalog executable",
                )));
            }
            Ok(())
        };
        verify(&self.executable)?;
        let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(ReadFailure::TimedOut {
                elapsed: started.elapsed(),
            });
        }
        let output = self.runner.run(&catalog_command(
            &self.executable,
            CATALOG_SOURCE.arguments,
            Some(request.cwd.path.clone()),
            ReadBounds {
                deadline: remaining,
                ..request.bounds
            },
        ));
        if output.end == CommandEnd::TimedOut || output.elapsed > remaining {
            return Err(ReadFailure::TimedOut {
                elapsed: started.elapsed().max(output.elapsed),
            });
        }
        if output.end == CommandEnd::OutputLimit
            || output.stdout.len() > request.bounds.max_output_bytes
        {
            return Err(ReadFailure::OutputLimit {
                limit: request.bounds.max_output_bytes,
            });
        }
        if !output.success() {
            return match (output.end, output.exit_code) {
                (CommandEnd::Exited, Some(code)) => Err(ReadFailure::Exit { code }),
                _ => Err(ReadFailure::Error(ContractError::Invalid(
                    "codex model catalog",
                ))),
            };
        }
        verify(&self.executable)?;
        Ok(ReadOutput {
            stdout: output.stdout,
            elapsed: started.elapsed(),
            exit_code: 0,
        })
    }
}

pub fn parse_bundled_catalog(bytes: &[u8]) -> Result<Vec<ModelRecord>, ContractError> {
    let invalid = || ContractError::Invalid("codex model catalog");
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() != 1 || !object.contains_key("models") {
        return Err(invalid());
    }
    let models = object["models"].as_array().ok_or_else(invalid)?;
    if models.is_empty() {
        return Err(invalid());
    }
    let mut records = Vec::with_capacity(models.len());
    let mut seen = Vec::<String>::with_capacity(models.len());
    for model in models {
        let model = model.as_object().ok_or_else(invalid)?;
        let slug = exact_text(model.get("slug")).ok_or_else(invalid)?;
        if seen.iter().any(|id| id == &slug) {
            return Err(ContractError::AmbiguousModel);
        }
        seen.push(slug.clone());
        if exact_text(model.get("display_name")).is_none() {
            return Err(invalid());
        }
        let visibility = model.get("visibility").and_then(serde_json::Value::as_str);
        if visibility != Some("list") && visibility != Some("hide") {
            return Err(invalid());
        }
        if model.get("supported_in_api") != Some(&serde_json::Value::Bool(true)) {
            return Err(invalid());
        }
        let levels = model
            .get("supported_reasoning_levels")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(invalid)?;
        if levels.is_empty() {
            return Err(invalid());
        }
        let mut efforts = Vec::with_capacity(levels.len());
        for level in levels {
            let raw = level.get("effort").and_then(serde_json::Value::as_str);
            let effort = match raw {
                Some("low") => Effort::Low,
                Some("medium") => Effort::Medium,
                Some("high") => Effort::High,
                Some("xhigh") => Effort::Xhigh,
                Some("max") => Effort::Max,
                Some("ultra") => Effort::Ultra,
                _ => return Err(invalid()),
            };
            if efforts.contains(&effort) {
                return Err(invalid());
            }
            efforts.push(effort);
        }
        records.push(ModelRecord {
            id: slug,
            efforts: Support::Supported(efforts),
        });
    }
    Ok(records)
}

fn exact_text(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?;
    if text.is_empty() || text != text.trim() {
        None
    } else {
        Some(text.to_string())
    }
}
