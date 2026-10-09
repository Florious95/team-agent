#![allow(dead_code)]
// Developer-owned controlled fixtures, never a Kiro profile or native acceptance claim.
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use team_agent_contract::contract::delivery::*;
use team_agent_contract::contract::descriptor::*;
use team_agent_contract::contract::fork::*;
use team_agent_contract::contract::hooks::*;
use team_agent_contract::contract::plan::*;
use team_agent_contract::contract::probe::*;
use team_agent_contract::contract::session::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::host::clock::Clock;
use team_agent_contract::host::files::DirectoryReceipt;
use team_agent_contract::host::process::{ImageStamp, ProcessStamp};
use team_agent_contract::host::transport::*;
use team_agent_contract::host::{HostError, HostErrorKind};
use team_agent_contract::runtime::delivery::*;
use team_agent_contract::runtime::journal::*;
use team_agent_contract::runtime::probes::*;

pub const NATIVE: Digest = Digest([11; 32]);
pub const CANDIDATE: Digest = Digest([12; 32]);
pub const POLICY: Digest = Digest([13; 32]);
pub const NO: Reason = Reason {
    code: "fixture-only",
    message: "Not a native provider capability",
};
static CONFIRM: [ConfirmationStep; 1] = [ConfirmationStep {
    predicate: "confirmation-1",
    key: PhysicalKey::Enter,
    deadline: Duration::from_millis(300),
}];
static CONFIRM_TWO: [ConfirmationStep; 2] = [
    ConfirmationStep {
        predicate: "confirmation-1",
        key: PhysicalKey::Enter,
        deadline: Duration::from_millis(300),
    },
    ConfirmationStep {
        predicate: "confirmation-2",
        key: PhysicalKey::Enter,
        deadline: Duration::from_millis(300),
    },
];
static QUEUE: [QueueAction; 1] = [QueueAction {
    predicate: "current-queue",
    key: PhysicalKey::Enter,
    max_presses: 2,
    interval: Duration::from_millis(10),
}];

pub fn native(hash: Digest) -> NativeIdentity {
    NativeIdentity {
        version: "fixture-1".into(),
        harness: "fixture".into(),
        ui: "controlled-tui".into(),
        platform: if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        },
        executable_sha256: hash,
    }
}
pub fn owner() -> InstanceIdentity {
    InstanceIdentity {
        scope: ScopeId::new("scope-one").unwrap(),
        seat: SeatId::new("seat-one").unwrap(),
        instance: InstanceId::new("instance-one").unwrap(),
        generation: Generation(1),
    }
}
pub fn target() -> TargetReceipt {
    TargetReceipt {
        directory: DirectoryReceipt {
            path: "/tmp/contract-fixture".into(),
            owner: owner(),
            device: 1,
            inode: 2,
        },
        endpoint: "/tmp/contract-fixture/tmux.sock".into(),
        socket_device: 1,
        socket_inode: 3,
        session: "$1".into(),
        window: "@1".into(),
        pane: "%1".into(),
        columns: 120,
        rows: 40,
        binding: "binding-one".into(),
        process: ProcessStamp {
            identity: ProcessIdentity {
                pid: 10000,
                birth_identity: "fixture-birth".into(),
                executable: "/fixture/bin".into(),
                executable_sha256: NATIVE,
            },
            parent: 9999,
            image: ImageStamp {
                device: 1,
                inode: 10,
                length: 100,
                modified_ns: 100,
            },
        },
        provider: ProviderId::new("physical-fixture").unwrap(),
        cwd: CwdIdentity {
            path: "/tmp".into(),
            identity: NATIVE,
        },
        native: native(NATIVE),
        evidence_kind: EvidenceKind::Fixture,
        candidate_sha256: CANDIDATE,
        native_session: None,
    }
}

pub fn descriptor(mode: &str, native: &NativeIdentity) -> ProviderDescriptor {
    let mut policy = SubmitPolicy {
        paste_mode: PasteMode::Bracketed,
        payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter,
        confirmation_steps: &[],
        retry_budget: RetryBudget::Never,
        queue_flush: &[],
        timing: InputTiming {
            capture_interval: Duration::from_millis(10),
            stable_window: Duration::from_millis(10),
            paste_to_submit_floor: Duration::from_millis(30),
            deadline: Duration::from_secs(2),
        },
        max_submit_keys: 1,
    };
    match mode {
        "confirm" => {
            policy.confirmation_steps = &CONFIRM;
            policy.max_submit_keys = 2;
        }
        "confirm-two" => {
            policy.confirmation_steps = &CONFIRM_TWO;
            policy.max_submit_keys = 3;
        }
        "retry" => {
            policy.retry_budget = RetryBudget::Guarded {
                predicate: "proven-unsubmitted",
                additional: 2,
                wrap_gap: 1,
            };
            policy.max_submit_keys = 4;
        }
        "queue" => {
            policy.queue_flush = &QUEUE;
            policy.max_submit_keys = 3;
        }
        _ => {}
    }
    // The immutable registry is bounded to this fixture/test. Binary hashes differ per host.
    let profiles = Box::leak(
        vec![InputProfile {
            id: "physical-fixture",
            version: "fixture-1",
            harness: "fixture",
            ui: "controlled-tui",
            platform: native.platform,
            executable_sha256: native.executable_sha256,
            policy_sha256: POLICY,
            operations: &[
                Operation::FirstBusiness,
                Operation::OrdinarySend,
                Operation::StartupBypassAck,
                Operation::SessionInspect,
                Operation::InWindowBranch,
                Operation::Stop,
                Operation::Shutdown,
            ],
            channel: Channel::Tmux,
            policy: Support::Supported(policy),
        }]
        .into_boxed_slice(),
    );
    ProviderDescriptor {
        identity: IdentityFacet {
            id: "physical-fixture",
            display_name: "Controlled physical fixture",
            binary: "fixture",
            aliases: &[],
            leader: Support::Unsupported(NO),
        },
        model: ModelFacet {
            selection: Support::Supported(ModelSelection::NativeDefaultOrOpaqueId),
            omitted: OmittedModel::NativeDefault,
            catalog: Support::Unsupported(NO),
        },
        effort: EffortFacet {
            admission: std::array::from_fn(|_| EffortAdmission::Reject(NO)),
            carrier: Support::Unsupported(NO),
            inherit_team_default: false,
            model_dependent: false,
        },
        auth: AuthFacet {
            subscription: Support::Supported(AuthMechanism::NativeExistingSession),
            official_api: Support::Unsupported(NO),
            compatible_api: Support::Unsupported(NO),
        },
        bypass: BypassFacet {
            intent: Support::Supported(BypassPolicy {
                enabled_arguments: &["--trust"],
                requires_startup_consent: true,
            }),
        },
        prompt: PromptFacet {
            carrier: Support::Supported(PromptCarrier::Argv),
        },
        mcp: McpFacet {
            carrier: Support::Supported(McpCarrier::Argv),
            scope: ResourceScope::RuntimeRoot,
            tools: &TEAM_TOOLS,
            excludes_ambient_configuration: true,
        },
        tool_names: ToolNameFacet {
            naming: Support::Supported(ToolNaming::StaticServer {
                key: "fixture-server",
            }),
        },
        session: SessionFacet {
            fresh: FreshSession::NotApplicable,
            resume: Support::Unsupported(NO),
        },
        fork: ForkFacet {
            in_window: Support::Supported(()),
            full_snapshot: Support::Unsupported(NO),
            native_new_seat: Support::Unsupported(NO),
            allowed_auth: &[AuthMode::NativeSubscription],
        },
        input: InputFacet {
            profiles: Support::Supported(profiles),
        },
        startup: StartupFacet {
            interactive: true,
            authorized_consent: Support::Supported(()),
        },
        probe_sources: ProbeSourcesFacet {
            process: Support::Supported(ProbeSource::Host),
            pane: Support::Supported(ProbeSource::NativeSurface),
            server: Support::Supported(ProbeSource::ServerLifecycle),
            client_binding: Support::Supported(ProbeSource::NativeRuntime),
            round_trip: Support::Supported(ProbeSource::NativeRuntime),
            semantic: Support::Unsupported(NO),
        },
        workspace: WorkspaceFacet {
            config_scope: ResourceScope::RuntimeRoot,
            shared_cwd: false,
            requires_materialization_lease: false,
        },
        teardown: TeardownFacet { resources: &[] },
    }
}

pub struct FixtureHooks;
pub static HOOK: FixtureHooks = FixtureHooks;
impl InteractionHook for FixtureHooks {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        let line = frame
            .text
            .lines()
            .rfind(|line| !line.trim().is_empty())
            .unwrap_or("");
        let fields: Vec<_> = line.trim().split('|').collect();
        let tag = fields.first().copied().unwrap_or("");
        let id = fields.get(1).copied().unwrap_or("-");
        let message = if id == "-" {
            None
        } else {
            MessageId::new(id).ok()
        };
        let current = message.as_ref() == frame.message.as_ref();
        let (surface, predicate) = match tag {
            "READY" => (InputSurface::ComposerReady, None),
            "WARNING" => (
                InputSurface::StartupRiskWarning,
                Some("consent".to_string()),
            ),
            "PASTED" => (InputSurface::ComposerContainsPaste, None),
            "CONFIRM1" => (
                InputSurface::ComposerContainsPaste,
                Some("confirmation-1".to_string()),
            ),
            "CONFIRM2" => (
                InputSurface::ComposerContainsPaste,
                Some("confirmation-2".to_string()),
            ),
            "RETRY" => (
                InputSurface::ComposerContainsPaste,
                Some("proven-unsubmitted".to_string()),
            ),
            "QUEUE" => (InputSurface::Queued, Some("current-queue".to_string())),
            "BUSY" => (InputSurface::Busy, None),
            "ACCEPTED" => (InputSurface::NativeAccepted, None),
            _ => (InputSurface::ShellOrUnknown, None),
        };
        let latch = if current && matches!(tag, "PASTED" | "RETRY" | "CONFIRM1" | "CONFIRM2") {
            PasteLatch::Seen {
                native_identity: format!("fixture-input-{id}"),
            }
        } else if current
            && matches!(tag, "ACCEPTED" | "QUEUE")
            && matches!(
                frame.paste_latch,
                PasteLatch::Seen { .. } | PasteLatch::Gone { .. }
            )
        {
            PasteLatch::Gone {
                native_identity: format!("fixture-input-{id}"),
            }
        } else {
            frame.paste_latch.clone()
        };
        InteractionObservation {
            scope: frame.scope.clone(),
            surface,
            predicate,
            current_message: message,
            current_attempt: if current { frame.attempt.clone() } else { None },
            paste_latch: latch,
        }
    }
}
impl PlanHook for FixtureHooks {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake_terminal.py");
        let arguments: Vec<OsString> = vec![
            "-u".into(),
            script.into_os_string(),
            "--mode".into(),
            resolved.model().unwrap_or("single").into(),
            "--events".into(),
            r.paths
                .runtime_root
                .join("native-events.jsonl")
                .into_os_string(),
            "--prompt".into(),
            r.prompt.clone().unwrap_or_default().into(),
            "--server".into(),
            "fixture-server".into(),
        ];
        Ok(LaunchPlan {
            executable: r.paths.executable.clone(),
            cwd: r.paths.cwd.path.clone(),
            arguments,
            environment: EnvironmentDelta {
                remove: BTreeSet::from(["CONTRACT_OLD_SCOPE".into()]),
                set: BTreeMap::from([(
                    "CONTRACT_FIXTURE_ENV".into(),
                    "quotes ' 中文\nnew line".into(),
                )]),
            },
            expected_session: resolved.expected_session().clone(),
            materialization: Vec::new(),
            carriers: CarrierReport {
                model: if resolved.model().is_some() {
                    CarrierUse::Specified(CarrierRef::Arguments(vec![2, 3]))
                } else {
                    CarrierUse::NotRequested
                },
                prompt: CarrierUse::Specified(CarrierRef::Arguments(vec![6, 7])),
                mcp: CarrierUse::Specified(CarrierRef::Arguments(vec![8, 9])),
                bypass: CarrierUse::NotRequested,
                effort: CarrierUse::NotRequested,
            },
        })
    }
}
impl SessionHook for FixtureHooks {
    fn bind(&self, _: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        Err(ContractError::Unverified {
            field: "fixture session binding",
            reason: NO,
        })
    }
}
impl ForkHook for FixtureHooks {
    fn stage(
        &self,
        request: &ResolvedFork,
        _: &mut dyn OwnedIo,
    ) -> Result<NativeForkPlan, PartialFailure> {
        if request.request().mode == ForkMode::InWindowBranch {
            Ok(NativeForkPlan::InWindow {
                control: request
                    .request()
                    .selected_turn
                    .map(NativeControl::BranchToTurn)
                    .unwrap_or(NativeControl::BranchCurrent),
            })
        } else {
            Err(PartialFailure {
                error: ContractError::Unsupported {
                    field: "fixture fork",
                    reason: NO,
                },
                receipt: MaterializeReceipt { resources: vec![] },
            })
        }
    }
}
pub fn hooks() -> ProviderHooks<'static> {
    ProviderHooks {
        catalog: HookBinding::NotRequired(NO),
        plan: HookBinding::Bound(&HOOK),
        materialize: HookBinding::NotRequired(NO),
        session: HookBinding::Bound(&HOOK),
        semantic: HookBinding::NotRequired(NO),
        interaction: HookBinding::Bound(&HOOK),
        fork: HookBinding::Bound(&HOOK),
    }
}
pub fn capability(target: &TargetReceipt, operation: Operation) -> CapabilityEvidence {
    CapabilityEvidence {
        provider: target.provider.clone(),
        native: target.native.clone(),
        profile_id: "physical-fixture".into(),
        policy_sha256: POLICY,
        candidate_sha256: target.candidate_sha256,
        evidence_sha256: POLICY,
        kind: target.evidence_kind,
        operation,
        channel: Channel::Tmux,
    }
}
pub fn policy<'a>(
    descriptor: &'a ProviderDescriptor,
    hooks: &'a ProviderHooks<'a>,
    target: &TargetReceipt,
    operation: Operation,
) -> ResolvedSubmitPolicy<'a> {
    resolve_submit_policy(
        descriptor,
        hooks,
        &PolicyRequest {
            provider: target.provider.as_str(),
            native: &target.native,
            profile_id: "physical-fixture",
            operation,
            channel: Channel::Tmux,
            evidence: &capability(target, operation),
            required_evidence_kind: target.evidence_kind,
            candidate_sha256: target.candidate_sha256,
        },
    )
    .unwrap()
}
pub fn protocol(target: &TargetReceipt, now: Duration) -> ProtocolSnapshot {
    let scope = target.scope(1, now, Duration::from_secs(60), "fixture-protocol");
    ProtocolSnapshot {
        server: Probe {
            scope: scope.clone(),
            outcome: ProbeOutcome::Observed(ServerHandshakeEvidence {
                server_instance: InstanceId::new("server-one").unwrap(),
                initialize_response_written: true,
                tools_list_response_written: true,
            }),
        },
        binding: Probe {
            scope,
            outcome: ProbeOutcome::Observed(ClientBindingEvidence {
                server_key: "fixture-server".into(),
                tools: TEAM_TOOLS.to_vec(),
                source: ClientBindingSource::NativeRegistry,
            }),
        },
        calls: vec![],
    }
}
pub fn envelope(message: &str, content: &str) -> PreparedEnvelope {
    // Fixed golden protocol fixture, not a second production renderer.
    let marker = format!("[team-agent-token:{message}]");
    let rendered = format!("Team Agent message from fixture-sender:\n\n{content}\n\n{marker}");
    let end = rendered.len();
    PreparedEnvelope::from_rendered(
        MessageId::new(message).unwrap(),
        rendered,
        end - marker.len()..end,
    )
    .unwrap()
}

#[derive(Default)]
pub struct FakeClock(pub Cell<Duration>);
impl Clock for FakeClock {
    fn now(&self) -> Duration {
        self.0.get()
    }
    fn sleep(&self, duration: Duration) {
        self.0.set(self.0.get().saturating_add(duration));
    }
}

#[derive(Default)]
pub struct MemoryJournal {
    pub seen: BTreeSet<String>,
    pub records: Vec<JournalRecord>,
    pub metadata: Vec<AttemptMetadata>,
    pub fail_kind: Option<JournalKind>,
    pub fail_submitted: bool,
    pub fail_intent: Option<StepKind>,
    pub fail_result: Option<StepKind>,
}
impl AttemptJournal for MemoryJournal {
    fn begin(&mut self, metadata: &AttemptMetadata) -> Result<(), HostError> {
        if !self.seen.insert(journal_name(metadata)) {
            return Err(HostError::new("attempt exists", HostErrorKind::Conflict));
        }
        self.metadata.push(metadata.clone());
        Ok(())
    }
    fn append(&mut self, record: &JournalRecord) -> Result<(), HostError> {
        if self.fail_kind == Some(record.kind)
            || (self.fail_submitted && record.effect == DeliveryEffect::Submitted)
            || (record.kind == JournalKind::ActionIntent
                && self.fail_intent.is_some()
                && record.step == self.fail_intent)
            || (record.kind == JournalKind::ActionResult
                && self.fail_result.is_some()
                && record.step == self.fail_result)
        {
            return Err(HostError::new(
                "fixture journal failure",
                HostErrorKind::Io(std::io::ErrorKind::Other),
            ));
        }
        self.records.push(record.clone());
        Ok(())
    }
}

pub struct Lease {
    directory: DirectoryReceipt,
    locked: Arc<AtomicBool>,
}
impl InputLease for Lease {
    fn directory(&self) -> &DirectoryReceipt {
        &self.directory
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.locked.store(false, Ordering::SeqCst);
    }
}

pub struct FakeTransport {
    pub target: TargetReceipt,
    pub mode: String,
    pub state: String,
    pub message: String,
    pub sequence: u64,
    pub buffer: Option<Vec<u8>>,
    pub pasted: Vec<Vec<u8>>,
    pub keys: Vec<PhysicalKey>,
    pub key_times: Vec<Duration>,
    pub paste_time: Option<Duration>,
    pub preparations: u32,
    pub locked: Arc<AtomicBool>,
    pub bad_scope: bool,
    pub capture_error: bool,
    pub fail_capture_after_key: bool,
    pub uncertain_paste: bool,
    pub uncertain_key: bool,
    pub wrong_message_after_key: bool,
    pub in_copy_mode: bool,
    pub fail_prepare: bool,
    pub never_accept: bool,
}
impl FakeTransport {
    pub fn new(target: TargetReceipt, mode: &str, message: &str) -> Self {
        Self {
            target,
            mode: mode.into(),
            state: if mode == "startup" {
                "WARNING".into()
            } else {
                "READY".into()
            },
            message: message.into(),
            sequence: 0,
            buffer: None,
            pasted: Vec::new(),
            keys: Vec::new(),
            key_times: Vec::new(),
            paste_time: None,
            preparations: 0,
            locked: Arc::new(AtomicBool::new(false)),
            bad_scope: false,
            capture_error: false,
            fail_capture_after_key: false,
            uncertain_paste: false,
            uncertain_key: false,
            wrong_message_after_key: false,
            in_copy_mode: false,
            fail_prepare: false,
            never_accept: false,
        }
    }
}
impl PhysicalTransport for FakeTransport {
    fn acquire_lane(&mut self, target: &TargetReceipt) -> Result<Box<dyn InputLease>, HostError> {
        if self.locked.swap(true, Ordering::SeqCst) {
            return Err(HostError::new("busy lane", HostErrorKind::Conflict));
        }
        Ok(Box::new(Lease {
            directory: target.directory.clone(),
            locked: self.locked.clone(),
        }))
    }
    fn validate_target(
        &mut self,
        target: &TargetReceipt,
        _: &dyn Clock,
        _: Duration,
    ) -> Result<ProcessAliveEvidence, HostError> {
        if target != &self.target || self.bad_scope {
            return Err(HostError::new(
                "changed fixture target",
                HostErrorKind::Ownership,
            ));
        }
        Ok(ProcessAliveEvidence {
            process: target.process.identity.clone(),
        })
    }
    fn capture(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        _: Duration,
        freshness: Duration,
    ) -> Result<HostCapture, HostError> {
        if self.capture_error || (self.fail_capture_after_key && !self.keys.is_empty()) {
            return Err(HostError::new("capture failed", HostErrorKind::Unknown));
        }
        self.sequence += 1;
        let mut scope = target.scope(
            self.sequence,
            clock.now(),
            freshness,
            "controlled-fake-pane",
        );
        if self.bad_scope {
            scope.identity.generation = Generation(99);
        }
        let id = if matches!(self.state.as_str(), "READY" | "WARNING") {
            "-"
        } else if self.wrong_message_after_key && !self.keys.is_empty() {
            "other-message"
        } else {
            &self.message
        };
        Ok(HostCapture {
            scope,
            text: format!("{}|{}", self.state, id),
            mode: if self.in_copy_mode {
                PaneMode::Copy
            } else {
                PaneMode::Normal
            },
        })
    }
    fn prepare_host_mode(
        &mut self,
        _: &TargetReceipt,
        _: &dyn Clock,
        _: Duration,
    ) -> HostPreparation {
        self.preparations += 1;
        if self.fail_prepare {
            return HostPreparation {
                mode_before: PaneMode::Unknown("bad".into()),
                control_commands_confirmed: 0,
                control_commands_uncertain: 0,
                error: Some(HostError::new("unknown host mode", HostErrorKind::Unknown)),
            };
        }
        let copied = self.in_copy_mode;
        self.in_copy_mode = false;
        HostPreparation {
            mode_before: if copied {
                PaneMode::Copy
            } else {
                PaneMode::Normal
            },
            control_commands_confirmed: u16::from(copied),
            control_commands_uncertain: 0,
            error: None,
        }
    }
    fn stage_buffer(
        &mut self,
        _: &TargetReceipt,
        _: &str,
        bytes: &[u8],
        _: PasteMode,
        _: &dyn Clock,
        _: Duration,
    ) -> ActionResult {
        self.buffer = Some(bytes.to_vec());
        ActionResult::confirmed()
    }
    fn paste_buffer(
        &mut self,
        _: &TargetReceipt,
        _: &str,
        _: PasteMode,
        clock: &dyn Clock,
        _: Duration,
    ) -> ActionResult {
        self.pasted.push(self.buffer.clone().unwrap());
        self.state = "PASTED".into();
        self.paste_time = Some(clock.now());
        if self.uncertain_paste {
            ActionResult::uncertain(HostError::new("paste unknown", HostErrorKind::Deadline))
        } else {
            ActionResult::confirmed()
        }
    }
    fn release_buffer(
        &mut self,
        _: &TargetReceipt,
        _: &str,
        _: &dyn Clock,
        _: Duration,
    ) -> ActionResult {
        self.buffer = None;
        ActionResult::confirmed()
    }
    fn key(
        &mut self,
        _: &TargetReceipt,
        key: PhysicalKey,
        clock: &dyn Clock,
        _: Duration,
    ) -> ActionResult {
        self.keys.push(key);
        self.key_times.push(clock.now());
        self.state = if self.never_accept {
            "PASTED"
        } else {
            match (self.mode.as_str(), self.keys.len()) {
                ("confirm", 1) | ("confirm-two", 1) => "CONFIRM1",
                ("confirm-two", 2) => "CONFIRM2",
                ("retry", 1..=3) => "RETRY",
                ("queue", 1..=2) => "QUEUE",
                ("busy", _) => "BUSY",
                ("startup", _) => "READY",
                _ => "ACCEPTED",
            }
        }
        .into();
        if self.uncertain_key {
            ActionResult::uncertain(HostError::new("key unknown", HostErrorKind::Deadline))
        } else {
            ActionResult::confirmed()
        }
    }
}
