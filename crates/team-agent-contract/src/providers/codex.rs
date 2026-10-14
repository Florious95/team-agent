//! ---
//! purpose: Codex CLI contract leaf for the fifteen facets and seven hooks.
//! contract: 带映像的 LaunchOnly Fresh 仍返回两面旗且物化为空；带映像且持有自有 MCP 的 FullWorker Fresh 返回同一 argv，并把运行时根上的 `codex/mcp.toml` 与 `codex/prompt.txt` 放进物化。空适配器仍然拒绝启动。
//! boundary: No process signals, no global config writes, no credential or rollout-body reads, no static model list.
//! ---

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::contract::delivery::{
    CaptureFrame, InputProfile, InputSurface, InputTiming, InteractionObservation, PasteMode,
    PayloadTrailer, PhysicalKey, ProfileIdentity, RetryBudget, SubmitPolicy,
};
use crate::contract::descriptor::*;
use crate::contract::hooks::{
    BoundedReadHost, CatalogHook, CatalogRequest, InteractionHook, MaterializeHook, OwnedIo, PlanHook,
    ProviderHooks, ReadBounds, ReadFailure, ReadOutput, ScopedSessionEvidence, SessionHook,
};
use crate::contract::plan::{
    CarrierRef, CarrierReport, CarrierUse, CatalogObservation, EffortResolution, EnvironmentDelta,
    ExpectedSession,
    LaunchEntrypoint, LaunchMode, LaunchPlan, MaterializeReceipt, ModelRecord, OwnedResourceReceipt,
    OwnedResourceRequest, PartialFailure, ResolvedLaunch, ResourceWriteEffect,
};
use crate::contract::types::valid_id;
use crate::contract::session::{CaptureOrigin, NativeSessionId, ResumeBinding, SessionStorage};
use crate::contract::types::*;
use crate::host::command::{CommandEnd, CommandRequest, CommandRunner, OutputLimits};
use crate::host::process::{fingerprint_file, resolve_cwd};
use crate::host::{digest, HostError, HostErrorKind};

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
const RESUME: Reason = Reason {
    code: "codex-live-resume-not-run",
    message: "Live Codex resume was not run",
};
const FORK: Reason = Reason {
    code: "codex-fork-not-run",
    message: "Codex fork was not run",
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
pub const MAX_ENGINE_BYTES: u64 = 512 * 1024 * 1024;

const PHASE1_INTERACTIVE_ARGUMENTS: &[&str] = &["--no-alt-screen", "--no-daemon"];
/// UTF-8 recipe `codex-tui-v1/r1:runtime-captured+seat-wrapper+bundled-catalog;macos;paste-bracketed;enter-once;confirm-none;retry-never;queue-none;max-submit-keys-1;capture100ms;stable200ms;paste-floor100ms;deadline30s;modal-refusal-no-key`.
const CODEX_SUBMIT_POLICY_SHA256: Digest = Digest([
    168, 118, 138, 11, 125, 142, 62, 19, 118, 180, 255, 52, 216, 236, 157, 192, 28, 208, 80, 4, 35,
    107, 173, 193, 213, 5, 148, 61, 47, 121, 240, 186,
]);
const CODEX_SUBMIT: SubmitPolicy = SubmitPolicy {
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
};
pub static CODEX_SUBMIT_PROFILES: [InputProfile; 1] = [InputProfile {
    id: "codex-tui-single-enter-v1",
    identity: ProfileIdentity::RuntimeCaptured,
    harness: CATALOG_HARNESS,
    ui: CATALOG_UI,
    platform: Platform::MacOs,
    policy_sha256: CODEX_SUBMIT_POLICY_SHA256,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend],
    channel: Channel::Tmux,
    policy: Support::Supported(CODEX_SUBMIT),
}];
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
        selection: Support::Supported(ModelSelection::NativeDefaultOrOpaqueId),
        omitted: OmittedModel::NativeDefault,
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
        carrier: Support::Supported(PromptCarrier::RuntimeBridge),
    },
    mcp: McpFacet {
        carrier: Support::Supported(McpCarrier::ScopedConfig),
        scope: ResourceScope::RuntimeRoot,
        tools: &TEAM_TOOLS,
        isolation: McpIsolation::AmbientExcluded,
    },
    tool_names: ToolNameFacet {
        naming: Support::Supported(ToolNaming::StaticServer {
            key: "team_orchestrator",
        }),
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
        profiles: Support::Supported(&CODEX_SUBMIT_PROFILES),
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
                kind: ResourceKind::McpConfig,
                disposition: ResourceDisposition::OwnedRemovable,
            },
            ResourcePolicy {
                kind: ResourceKind::Prompt,
                disposition: ResourceDisposition::OwnedRemovable,
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

struct OwnedMcp {
    server_key: String,
    command: String,
    args: Vec<String>,
}

pub struct CodexAdapter {
    images: Option<CodexLaunchImages>,
    owned_mcp: Option<OwnedMcp>,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            images: None,
            owned_mcp: None,
        }
    }

    pub fn with_launch_images(mut self, images: CodexLaunchImages) -> Self {
        self.images = Some(images);
        self
    }

    /// One owned stdio server. An empty adapter still has no mount.
    pub fn with_owned_mcp(
        mut self,
        server_key: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        self.owned_mcp = Some(OwnedMcp {
            server_key: server_key.into(),
            command: command.into(),
            args: Vec::new(),
        });
        self
    }

    /// Stores args only after `owned_mcp_document` accepts them.
    pub fn with_owned_mcp_args(
        mut self,
        server_key: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
    ) -> Result<Self, ContractError> {
        let server_key = server_key.into();
        let command = command.into();
        owned_mcp_document(&server_key, &command, &args)?;
        self.owned_mcp = Some(OwnedMcp {
            server_key,
            command,
            args,
        });
        Ok(self)
    }

    pub fn owned_stdio(&self) -> Option<(&str, &str)> {
        self.owned_mcp
            .as_ref()
            .map(|owned| (owned.server_key.as_str(), owned.command.as_str()))
    }

    pub fn owned_args(&self) -> &[String] {
        self.owned_mcp
            .as_ref()
            .map(|owned| owned.args.as_slice())
            .unwrap_or(&[])
    }

    pub fn hooks(&self) -> ProviderHooks<'_> {
        ProviderHooks {
            catalog: HookBinding::Bound(self),
            plan: HookBinding::Bound(self),
            materialize: HookBinding::Bound(self),
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
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let Some(images) = &self.images else {
            return self.plan_phase1();
        };
        match resolved.request().mode {
            LaunchMode::LaunchOnly => launch_only_plan(images, resolved),
            LaunchMode::FullWorker => full_worker_plan(self, images, resolved),
        }
    }
}

fn launch_only_plan(
    images: &CodexLaunchImages,
    resolved: &ResolvedLaunch,
) -> Result<LaunchPlan, ContractError> {
    let request = resolved.request();
    if request.operation != Operation::Fresh
        || *resolved.expected_session() != ExpectedSession::CaptureAfterLaunch
    {
        return Err(launch_not_admitted());
    }
    if request.bypass {
        return match CODEX_DESCRIPTOR.bypass.intent.require("bypass") {
            Err(error) => Err(error),
            Ok(_) => Err(unverified("bypass", BYPASS)),
        };
    }
    if resolved.model().is_some() {
        return Err(unverified("model selection", SELECTION));
    }
    if !matches!(resolved.effort(), EffortResolution::Unspecified) {
        return Err(ContractError::EffortRejected(EFFORT));
    }
    if request.prompt.is_some() {
        return Err(unverified("prompt", PROMPT));
    }
    interactive_launch_plan(images, &request.paths.cwd.path, resolved.expected_session())
}

fn full_worker_plan(
    adapter: &CodexAdapter,
    images: &CodexLaunchImages,
    resolved: &ResolvedLaunch,
) -> Result<LaunchPlan, ContractError> {
    let request = resolved.request();
    if request.operation != Operation::Fresh
        || *resolved.expected_session() != ExpectedSession::CaptureAfterLaunch
        || request.channel != Channel::Tmux
        || request.auth != AuthMode::NativeSubscription
    {
        return Err(launch_not_admitted());
    }
    if request.bypass {
        return match CODEX_DESCRIPTOR.bypass.intent.require("bypass") {
            Err(error) => Err(error),
            Ok(_) => Err(unverified("bypass", BYPASS)),
        };
    }
    if resolved.model().is_some() {
        return Err(unverified("model selection", SELECTION));
    }
    if !matches!(resolved.effort(), EffortResolution::Unspecified) {
        return Err(ContractError::EffortRejected(EFFORT));
    }
    let Some(prompt) = request.prompt.as_deref().filter(|text| nonblank(text)) else {
        return Err(ContractError::Invalid("worker prompt required"));
    };
    let Some((server_key, command)) = adapter.owned_stdio() else {
        return Err(no_owned_mount());
    };
    if server_key != "team_orchestrator" {
        return Err(no_owned_mount());
    }
    let document = owned_mcp_document(server_key, command, adapter.owned_args())?;
    let root = &request.paths.runtime_root;
    if !root.is_absolute() || codex_home_root(root) {
        return Err(no_owned_mount());
    }
    let profile = &CODEX_SUBMIT_PROFILES[0];
    if request.input_profile.as_deref() != Some(profile.id)
        || !profile.matches_native(&request.native)
    {
        return Err(ContractError::ProfileUnavailable);
    }
    if images.engine != request.paths.executable {
        return Err(ContractError::Invalid("codex interactive launch"));
    }
    let mut plan =
        interactive_launch_plan(images, &request.paths.cwd.path, resolved.expected_session())?;
    let owner = request.identity.clone();
    let mcp_path = OwnedPath::new(root.clone(), PathBuf::from("codex/mcp.toml"))?;
    let prompt_path = OwnedPath::new(root.clone(), PathBuf::from("codex/prompt.txt"))?;
    plan.materialization = vec![
        OwnedResourceRequest {
            path: mcp_path.clone(),
            owner: owner.clone(),
            kind: ResourceKind::McpConfig,
            contents: document,
        },
        OwnedResourceRequest {
            path: prompt_path.clone(),
            owner,
            kind: ResourceKind::Prompt,
            contents: prompt.as_bytes().to_vec(),
        },
    ];
    plan.carriers = CarrierReport {
        model: CarrierUse::NotRequested,
        prompt: CarrierUse::Specified(CarrierRef::Resource(prompt_path)),
        mcp: CarrierUse::Specified(CarrierRef::Resource(mcp_path)),
        bypass: CarrierUse::NotRequested,
        effort: CarrierUse::NotRequested,
    };
    Ok(plan)
}

impl MaterializeHook for CodexAdapter {
    fn materialize(
        &self,
        requests: &[OwnedResourceRequest],
        io: &mut dyn OwnedIo,
    ) -> Result<MaterializeReceipt, PartialFailure> {
        let Some(owned) = &self.owned_mcp else {
            return Err(no_owned_mount_failure());
        };
        let contents = match owned_mcp_document(&owned.server_key, &owned.command, &owned.args) {
            Ok(contents) => contents,
            Err(error) => {
                return Err(PartialFailure {
                    error,
                    receipt: MaterializeReceipt { resources: Vec::new() },
                })
            }
        };
        if let Some((mcp, prompt)) = exact_owned_prompt_pair(requests, &contents) {
            let mut resources = Vec::new();
            for request in [mcp, prompt] {
                let receipt = match io.create_exclusive(request) {
                    Ok(receipt) => receipt,
                    Err(mut failure) => {
                        resources.append(&mut failure.receipt.resources);
                        failure.receipt.resources = resources;
                        return Err(failure);
                    }
                };
                let matched = owned_receipt_matches(request, &receipt);
                resources.push(receipt);
                if !matched {
                    return Err(PartialFailure {
                        error: no_owned_mount(),
                        receipt: MaterializeReceipt { resources },
                    });
                }
            }
            return Ok(MaterializeReceipt { resources });
        }
        let Some(request) = exact_owned_mcp_request(requests, &contents) else {
            return Err(no_owned_mount_failure());
        };
        let receipt = io.create_exclusive(request)?;
        if owned_receipt_matches(request, &receipt) {
            Ok(MaterializeReceipt {
                resources: vec![receipt],
            })
        } else {
            Err(PartialFailure {
                error: no_owned_mount(),
                receipt: MaterializeReceipt {
                    resources: vec![receipt],
                },
            })
        }
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

#[derive(Clone)]
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

/// Body of the owned Codex MCP file. Only the measured `[mcp_servers.<key>]` command table.
pub fn owned_mcp_toml(server_key: &str, command: &str) -> Result<Vec<u8>, ContractError> {
    owned_mcp_document(server_key, command, &[])
}

/// Owned stdio table. Empty `args` matches `owned_mcp_toml`. A non-empty list
/// must start with `contract-mcp` and is quoted into the same file, never argv.
pub fn owned_mcp_document(
    server_key: &str,
    command: &str,
    args: &[String],
) -> Result<Vec<u8>, ContractError> {
    if !valid_id(server_key) || !absolute_stdio_command(command) {
        return Err(no_owned_mount());
    }
    if args.is_empty() {
        return Ok(format!("[mcp_servers.{server_key}]\ncommand = \"{command}\"\n").into_bytes());
    }
    if args.first().map(String::as_str) != Some("contract-mcp")
        || args.iter().any(|arg| !owned_mcp_arg(arg))
    {
        return Err(no_owned_mount());
    }
    let mut quoted = Vec::with_capacity(args.len());
    for arg in args {
        quoted.push(serde_json::to_string(arg).map_err(|_| no_owned_mount())?);
    }
    Ok(format!(
        "[mcp_servers.{server_key}]\ncommand = \"{command}\"\nargs = [{}]\n",
        quoted.join(", ")
    )
    .into_bytes())
}

fn owned_mcp_arg(arg: &str) -> bool {
    !arg.is_empty()
        && !arg.chars().any(|ch| ch == '"' || ch == '\\' || ch == '\n' || ch.is_control())
}

fn absolute_stdio_command(command: &str) -> bool {
    command.starts_with('/')
        && !command.is_empty()
        && !command
            .chars()
            .any(|ch| ch == '"' || ch == '\\' || ch == '\n' || ch.is_whitespace())
}

fn no_owned_mount() -> ContractError {
    unverified("codex mcp", H3)
}

fn no_owned_mount_failure() -> PartialFailure {
    PartialFailure {
        error: no_owned_mount(),
        receipt: MaterializeReceipt { resources: Vec::new() },
    }
}

fn exact_owned_prompt_pair<'a>(
    requests: &'a [OwnedResourceRequest],
    contents: &[u8],
) -> Option<(&'a OwnedResourceRequest, &'a OwnedResourceRequest)> {
    let [mcp, prompt] = requests else {
        return None;
    };
    if !owned_mcp_request_matches(mcp, contents) {
        return None;
    }
    let prompt_text = std::str::from_utf8(&prompt.contents).ok()?;
    if prompt.kind != ResourceKind::Prompt
        || prompt.owner != mcp.owner
        || prompt.path.root() != mcp.path.root()
        || prompt.path.relative() != Path::new("codex/prompt.txt")
        || prompt_text.is_empty()
    {
        return None;
    }
    Some((mcp, prompt))
}

fn owned_mcp_request_matches(request: &OwnedResourceRequest, contents: &[u8]) -> bool {
    request.kind == ResourceKind::McpConfig
        && request.path.relative() == Path::new("codex/mcp.toml")
        && request.contents == contents
        && !codex_home_root(request.path.root())
}

fn exact_owned_mcp_request<'a>(
    requests: &'a [OwnedResourceRequest],
    contents: &[u8],
) -> Option<&'a OwnedResourceRequest> {
    let [request] = requests else {
        return None;
    };
    owned_mcp_request_matches(request, contents).then_some(request)
}

fn codex_home_root(root: &Path) -> bool {
    let Some(text) = root.to_str() else {
        return true;
    };
    let trimmed = text.trim_end_matches('/');
    trimmed == "/Users/alauda/.codex" || trimmed.ends_with("/.codex")
}

fn owned_receipt_matches(request: &OwnedResourceRequest, receipt: &OwnedResourceReceipt) -> bool {
    receipt.path == request.path
        && receipt.owner == request.owner
        && receipt.kind == request.kind
        && receipt.exclusive
        && receipt.disposition == ResourceDisposition::OwnedRemovable
        && matches!(
            receipt.write_effect,
            ResourceWriteEffect::Written { bytes_sha256 }
                if bytes_sha256 == digest(&request.contents)
        )
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

pub struct PreparedSeatEngine {
    pub engine: PathBuf,
    pub version: String,
}

/// The package root is the parent of `bin/codex.js`. Exactly one canonical
/// SeatEngine may exist there. Zero or several fail before any engine process.
pub fn resolve_unique_seat_engine(wrapper: &Path) -> Result<PathBuf, ContractError> {
    let wrapper = resolve_seat_wrapper(wrapper)?;
    let bin = wrapper
        .parent()
        .ok_or(ContractError::Invalid("codex seat engine"))?;
    if bin.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return Err(ContractError::Invalid("codex seat engine"));
    }
    let package = bin
        .parent()
        .ok_or(ContractError::Invalid("codex seat engine"))?
        .canonicalize()
        .map_err(|_| ContractError::Invalid("codex seat engine"))?;
    let engines = collect_seat_engines(&package)?;
    if engines.len() != 1 {
        return Err(ContractError::Invalid("codex seat engine"));
    }
    Ok(engines.into_iter().next().expect("one seat engine"))
}

pub fn prepare_seat_engine<R: CommandRunner>(
    wrapper: &Path,
    runner: &mut R,
    bounds: ReadBounds,
) -> Result<PreparedSeatEngine, HostError> {
    let engine = resolve_unique_seat_engine(wrapper)
        .map_err(|_| HostError::new("Codex seat engine", HostErrorKind::Invalid))?;
    let version = probe_engine_version(&engine, runner, bounds)?;
    Ok(PreparedSeatEngine { engine, version })
}

pub fn probe_engine_version<R: CommandRunner>(
    engine: &Path,
    runner: &mut R,
    bounds: ReadBounds,
) -> Result<String, HostError> {
    bounds
        .validate()
        .map_err(|_| HostError::new("Codex probe bounds", HostErrorKind::Invalid))?;
    require_absolute(engine, "codex seat engine")
        .map_err(|_| HostError::new("Codex seat engine", HostErrorKind::Invalid))?;
    let text = engine
        .to_str()
        .ok_or_else(|| HostError::new("Codex seat engine", HostErrorKind::Invalid))?;
    if classify_image_role(text) != CodexImageRole::SeatEngine {
        return Err(HostError::new("Codex seat engine", HostErrorKind::Invalid));
    }
    let output = runner.run(&catalog_command(engine, &["--version"], None, bounds));
    if output.end == CommandEnd::TimedOut || output.elapsed > bounds.deadline {
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
    Ok(version.to_string())
}

fn collect_seat_engines(package: &Path) -> Result<Vec<PathBuf>, ContractError> {
    let invalid = || ContractError::Invalid("codex seat engine");
    let mut engines = Vec::new();
    let mut stack = vec![package.to_path_buf()];
    let mut seen_dirs = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        let dir = dir.canonicalize().map_err(|_| invalid())?;
        if !dir.starts_with(package) || !seen_dirs.insert(dir.clone()) {
            continue;
        }
        let entries = std::fs::read_dir(&dir).map_err(|_| invalid())?;
        for entry in entries {
            let path = entry.map_err(|_| invalid())?.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|_| invalid())?;
            if meta.file_type().is_symlink() {
                let Ok(canonical) = path.canonicalize() else {
                    continue;
                };
                let Ok(canon_meta) = std::fs::metadata(&canonical) else {
                    continue;
                };
                if canon_meta.is_file() {
                    record_seat_engine(&canonical, &mut engines)?;
                }
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if meta.is_file() {
                let canonical = path.canonicalize().map_err(|_| invalid())?;
                if canonical.starts_with(package) {
                    record_seat_engine(&canonical, &mut engines)?;
                }
            }
        }
    }
    Ok(engines)
}

fn record_seat_engine(canonical: &Path, engines: &mut Vec<PathBuf>) -> Result<(), ContractError> {
    let text = canonical
        .to_str()
        .ok_or(ContractError::Invalid("codex seat engine"))?;
    if classify_image_role(text) == CodexImageRole::SeatEngine {
        engines.push(canonical.to_path_buf());
    }
    Ok(())
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
