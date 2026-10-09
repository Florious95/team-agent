#![allow(dead_code)]
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use team_agent_contract::contract::{
    delivery::*, descriptor::*, fork::*, hooks::*, plan::*, probe::*, session::*, types::*,
};
use team_agent_contract::orchestration::{lifecycle::*, store::*, supervisor::*, Error};

pub const HASH: Digest = Digest([1; 32]);
pub const NO: Reason = Reason {
    code: "fixture",
    message: "controlled fake only",
};
static PROFILES: [InputProfile; 1] = [InputProfile {
    id: "fake",
    version: "1",
    harness: "fake",
    ui: "tui",
    platform: Platform::Linux,
    executable_sha256: HASH,
    policy_sha256: HASH,
    operations: &[
        Operation::FirstBusiness,
        Operation::OrdinarySend,
        Operation::InWindowBranch,
    ],
    channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed,
        payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter,
        confirmation_steps: &[],
        retry_budget: RetryBudget::Never,
        queue_flush: &[],
        timing: InputTiming {
            capture_interval: Duration::from_millis(1),
            stable_window: Duration::ZERO,
            paste_to_submit_floor: Duration::ZERO,
            deadline: Duration::from_secs(1),
        },
        max_submit_keys: 1,
    }),
}];
pub fn descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        identity: IdentityFacet {
            id: "fake",
            display_name: "Controlled fake",
            binary: "fake",
            aliases: &[],
            leader: Support::Unsupported(NO),
        },
        model: ModelFacet {
            selection: Support::Supported(ModelSelection::NativeDefaultOrOpaqueId),
            omitted: OmittedModel::NativeDefault,
            catalog: Support::Unsupported(NO),
        },
        effort: EffortFacet {
            admission: [const { EffortAdmission::Reject(NO) }; 6],
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
            intent: Support::Unsupported(NO),
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
            naming: Support::Supported(ToolNaming::StaticServer { key: "team" }),
        },
        session: SessionFacet {
            fresh: FreshSession::CaptureAfterLaunch,
            resume: Support::Supported(ResumeMode::ExactId),
        },
        fork: ForkFacet {
            in_window: Support::Supported(()),
            full_snapshot: Support::Supported(()),
            native_new_seat: Support::Supported(()),
            allowed_auth: &[AuthMode::NativeSubscription],
        },
        input: InputFacet {
            profiles: Support::Supported(&PROFILES),
        },
        startup: StartupFacet {
            interactive: true,
            authorized_consent: Support::Unsupported(NO),
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
            requires_materialization_lease: true,
        },
        teardown: TeardownFacet {
            resources: &[
                ResourcePolicy {
                    kind: ResourceKind::Prompt,
                    disposition: ResourceDisposition::OwnedRemovable,
                },
                ResourcePolicy {
                    kind: ResourceKind::SessionBacking,
                    disposition: ResourceDisposition::OwnedPreserved,
                },
            ],
        },
    }
}
pub struct Fake;
pub fn hooks() -> ProviderHooks<'static> {
    ProviderHooks {
        catalog: HookBinding::NotRequired(NO),
        plan: HookBinding::Bound(&Fake),
        materialize: HookBinding::Bound(&Fake),
        session: HookBinding::Bound(&Fake),
        semantic: HookBinding::NotRequired(NO),
        interaction: HookBinding::Bound(&Fake),
        fork: HookBinding::Bound(&Fake),
    }
}
impl PlanHook for Fake {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        Ok(LaunchPlan {
            executable: r.paths.executable.clone(),
            arguments: vec![
                "chat".into(),
                r.prompt.clone().unwrap_or_default().into(),
                "--mcp".into(),
                "team".into(),
            ],
            cwd: r.paths.cwd.path.clone(),
            environment: EnvironmentDelta {
                remove: Default::default(),
                set: Default::default(),
            },
            expected_session: resolved.expected_session().clone(),
            materialization: vec![OwnedResourceRequest {
                path: OwnedPath::new(
                    r.paths.runtime_root.clone(),
                    format!("{}-prompt", r.identity.instance.as_str()).into(),
                )?,
                owner: r.identity.clone(),
                kind: ResourceKind::Prompt,
                contents: b"fake prompt".to_vec(),
            }],
            carriers: CarrierReport {
                model: CarrierUse::NotRequested,
                prompt: CarrierUse::Specified(CarrierRef::Arguments(vec![1])),
                mcp: CarrierUse::Specified(CarrierRef::Arguments(vec![2, 3])),
                bypass: CarrierUse::NotRequested,
                effort: CarrierUse::NotRequested,
            },
        })
    }
}
impl MaterializeHook for Fake {
    fn materialize(
        &self,
        requests: &[OwnedResourceRequest],
        io: &mut dyn OwnedIo,
    ) -> Result<MaterializeReceipt, PartialFailure> {
        let mut resources = vec![];
        for request in requests {
            resources.push(io.create_exclusive(request)?);
        }
        Ok(MaterializeReceipt { resources })
    }
}
impl SessionHook for Fake {
    fn bind(&self, e: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        serde_json::from_slice(&e.record).map_err(|_| ContractError::Invalid("fake capture"))
    }
}
impl InteractionHook for Fake {
    fn interpret(&self, f: &CaptureFrame) -> InteractionObservation {
        InteractionObservation {
            scope: f.scope.clone(),
            surface: InputSurface::ComposerReady,
            predicate: None,
            current_message: f.message.clone(),
            current_attempt: f.attempt.clone(),
            paste_latch: f.paste_latch.clone(),
        }
    }
}
impl ForkHook for Fake {
    fn stage(
        &self,
        r: &ResolvedFork,
        io: &mut dyn OwnedIo,
    ) -> Result<NativeForkPlan, PartialFailure> {
        let r = r.request();
        Ok(match r.mode {
            ForkMode::InWindowBranch => NativeForkPlan::InWindow {
                control: r
                    .selected_turn
                    .map(NativeControl::BranchToTurn)
                    .unwrap_or(NativeControl::BranchCurrent),
            },
            ForkMode::NativeNewSeat => NativeForkPlan::NativeNewSeat {
                source: Box::new(r.source.clone()),
                target: r.target.clone(),
            },
            ForkMode::NewSeatFullSnapshot => {
                let path = r.target_backing.clone().unwrap();
                let receipt = io.create_exclusive(&OwnedResourceRequest {
                    path: path.clone(),
                    owner: r.target.clone(),
                    kind: ResourceKind::SessionBacking,
                    contents: b"full native fake snapshot".to_vec(),
                })?;
                let mut target = r.source.clone();
                target.native_session = r.target_native_session.clone().unwrap();
                target.source = r.target.clone();
                target.backing = Some(path.path());
                target.origin = CaptureOrigin::ExactBacking;
                target.evidence_sha256 = HASH;
                NativeForkPlan::FullSnapshot {
                    source: Box::new(r.source.clone()),
                    target: Box::new(target),
                    staging: MaterializeReceipt {
                        resources: vec![receipt],
                    },
                }
            }
        })
    }
}

pub struct FakeIo {
    pub operation: OperationId,
    pub writes: usize,
    pub fail_after_write: bool,
}
impl FakeIo {
    pub fn new(id: &str) -> Self {
        Self {
            operation: OperationId::new(id).unwrap(),
            writes: 0,
            fail_after_write: false,
        }
    }
}
impl OwnedIo for FakeIo {
    fn create_exclusive(
        &mut self,
        r: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure> {
        self.writes += 1;
        if self.fail_after_write {
            return Err(PartialFailure {
                error: ContractError::Invalid("fake postwrite failure"),
                receipt: MaterializeReceipt { resources: vec![] },
            });
        }
        Ok(OwnedResourceReceipt {
            path: r.path.clone(),
            owner: r.owner.clone(),
            operation: self.operation.clone(),
            kind: r.kind,
            disposition: if r.kind == ResourceKind::SessionBacking {
                ResourceDisposition::OwnedPreserved
            } else {
                ResourceDisposition::OwnedRemovable
            },
            write_effect: ResourceWriteEffect::Written { bytes_sha256: HASH },
            exclusive: true,
            creation_identity: None,
        })
    }
    fn read_bound_session(
        &mut self,
        _: &ResumeBinding,
        _: ReadBounds,
    ) -> Result<ReadOutput, ReadFailure> {
        Err(ReadFailure::Unknown(NO))
    }
    fn validate_configuration(
        &mut self,
        _: &OwnedValidationRequest,
    ) -> Result<ReadOutput, ReadFailure> {
        Ok(ReadOutput {
            stdout: vec![],
            elapsed: Duration::ZERO,
            exit_code: 0,
        })
    }
}
#[derive(Default)]
pub struct FakeHost {
    pub spawned: usize,
    pub stopped: usize,
    pub controls: usize,
    pub removed: usize,
    pub deliveries: usize,
    pub fail_spawn: bool,
    pub fail_capture: bool,
    pub fail_stop: bool,
    pub uncertain_delivery: bool,
    pub route_after_spawn: Option<(String, String, String)>,
    pub sessions: BTreeMap<String, ResumeBinding>,
}
impl LifecycleHost for FakeHost {
    fn spawn(&mut self, _: &ProviderDescriptor, _: &ResolvedLaunch, seat: &SeatRecord, plan: &LaunchPlan) -> Result<SpawnedProcess, Error> {
        self.spawned += 1;
        if self.fail_spawn {
            return Err(Error::Host("spawn response lost"));
        }
        let (session, backing) = match &plan.expected_session {
            ExpectedSession::Resume(b) => (b.native_session.clone(), b.backing.clone()),
            ExpectedSession::SnapshotTarget {
                session, backing, ..
            } => (session.clone(), Some(backing.path())),
            _ => (
                NativeSessionId::new(format!("sid-{}", seat.identity.instance.as_str()))?,
                Some(seat.cwd.path.join("native-session")),
            ),
        };
        self.sessions.insert(
            seat.identity.seat.as_str().into(),
            ResumeBinding {
                provider: seat.provider.clone(),
                native_session: session,
                storage: SessionStorage::Local,
                cwd: seat.cwd.clone(),
                native: seat.native.clone(),
                source: seat.identity.clone(),
                origin: CaptureOrigin::CurrentNativeSession,
                evidence_kind: EvidenceKind::Fixture,
                evidence_sha256: HASH,
                backing,
            },
        );
        let (endpoint, pane, binding_key) = self.route_after_spawn.clone().unwrap_or_else(|| {
            (seat.endpoint.clone(), seat.pane.clone(), seat.binding_key.clone())
        });
        Ok(SpawnedProcess {
            process: ProcessIdentity {
                pid: 1000 + self.spawned as u32,
                birth_identity: format!("birth-{}", self.spawned),
                executable: plan.executable.clone(),
                executable_sha256: HASH,
            },
            endpoint, pane, binding_key, physical: None,
        })
    }
    fn stop(&mut self, _: &SeatRecord) -> Result<(), Error> {
        self.stopped += 1;
        if self.fail_stop {
            Err(Error::Host("stop unknown"))
        } else {
            Ok(())
        }
    }
    fn control(&mut self, seat: &SeatRecord, _: &NativeControl) -> Result<DeliveryEffect, Error> {
        self.controls += 1;
        self.sessions
            .get_mut(seat.identity.seat.as_str())
            .unwrap()
            .native_session = NativeSessionId::new(format!("branch-{}", self.controls))?;
        Ok(DeliveryEffect::Submitted)
    }
    fn session_evidence(&mut self, seat: &SeatRecord) -> Result<ScopedSessionEvidence, Error> {
        if self.fail_capture {
            return Err(Error::Host("capture failed"));
        }
        let binding = self.sessions[seat.identity.seat.as_str()].clone();
        Ok(ScopedSessionEvidence {
            scope: evidence_scope(seat),
            provider: seat.provider.clone(),
            native: seat.native.clone(),
            cwd: seat.cwd.clone(),
            origin: binding.origin,
            evidence_kind: EvidenceKind::Fixture,
            evidence_sha256: HASH,
            record: serde_json::to_vec(&binding)?,
        })
    }
    fn remove_owned(&mut self, _: &OwnedResourceReceipt) -> Result<bool, Error> {
        self.removed += 1;
        Ok(true)
    }
}
impl DeliveryHost for FakeHost {
    fn deliver(&mut self, job: &DeliveryJob) -> Result<DeliveryReceipt, Error> {
        self.deliveries += 1;
        Ok(DeliveryReceipt {
            identity: job.target.identity.clone(),
            message: job.envelope.message.clone(),
            attempt: job.attempt.clone(),
            operation: job.operation,
            channel: job.channel,
            policy_sha256: job.policy_sha256,
            events: vec![],
            effect_floor: if self.uncertain_delivery {
                DeliveryEffect::MayHaveSubmitted
            } else {
                DeliveryEffect::Submitted
            },
            failure: None,
            persistence: PersistenceState::Durable,
        })
    }
}

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub struct Sandbox {
    pub parent: PathBuf,
    pub root: PathBuf,
}
impl Sandbox {
    pub fn new() -> Self {
        let parent = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "contract-k3-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&parent).unwrap();
        Self {
            root: parent.join("owned"),
            parent,
        }
    }
    pub fn store(&self) -> ContractStore {
        ContractStore::create(
            &self.root,
            ScopeId::new("scope").unwrap(),
            "isolated-endpoint",
        )
        .unwrap()
    }
    pub fn reopen(&self) -> ContractStore {
        ContractStore::open(
            &self.root,
            ScopeId::new("scope").unwrap(),
            "isolated-endpoint",
        )
        .unwrap()
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}
pub fn request(root: &std::path::Path, seat: &str) -> LaunchRequest {
    LaunchRequest {
        provider: "fake".into(),
        operation: Operation::Fresh,
        mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription,
        model: None,
        role_effort: None,
        team_effort: None,
        bypass: false,
        prompt: Some("fake worker".into()),
        identity: InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(),
            seat: SeatId::new(seat).unwrap(),
            instance: InstanceId::new(format!("{seat}-1")).unwrap(),
            generation: Generation(1),
        },
        native: NativeIdentity {
            version: "1".into(),
            harness: "fake".into(),
            ui: "tui".into(),
            platform: Platform::Linux,
            executable_sha256: HASH,
        },
        paths: LaunchPaths {
            executable: "/fake/bin".into(),
            candidate: "/fake/candidate".into(),
            cwd: CwdIdentity {
                path: root.into(),
                identity: HASH,
            },
            runtime_root: root.into(),
        },
        channel: Channel::Tmux,
        input_profile: Some("fake".into()),
        evidence_kind: EvidenceKind::Fixture,
        preassigned_session: None,
        resume: None,
        fork: None,
    }
}
pub fn routing(seat: &str) -> Routing {
    Routing {
        pane: format!("pane-{seat}"),
        binding_key: format!("binding-{seat}"),
        server_key: "team".into(),
    }
}
pub fn start(
    store: &mut ContractStore,
    host: &mut FakeHost,
    io: &mut FakeIo,
    seat: &str,
) -> OperationRecord {
    let req = request(store.root(), seat);
    let d = descriptor();
    let h = hooks();
    let op = OperationId::new(format!("start-{seat}")).unwrap();
    io.operation = op.clone();
    Lifecycle { store, host, io }
        .startup(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None,
            },
            &req,
            routing(seat),
            op,
        )
        .unwrap()
}
pub fn evidence_scope(seat: &SeatRecord) -> EvidenceScope {
    EvidenceScope {
        identity: seat.identity.clone(),
        endpoint: seat.endpoint.clone(),
        pane: seat.pane.clone(),
        binding: seat.binding_key.clone(),
        session: seat.session.as_ref().map(|b| b.native_session.clone()),
        sequence: 1,
        observed_at: Duration::from_secs(1),
        valid_for: Duration::from_secs(10),
        source: "fake-native".into(),
    }
}
pub fn readiness(seat: &SeatRecord, bound: bool) -> ReadinessSample {
    let scope = evidence_scope(seat);
    ReadinessSample {
        process: Probe {
            scope: scope.clone(),
            outcome: ProbeOutcome::Observed(ProcessAliveEvidence {
                process: seat.process.clone().unwrap(),
            }),
        },
        pane: Probe {
            scope: scope.clone(),
            outcome: ProbeOutcome::Observed(PaneReadyEvidence {
                profile_id: seat.profile.clone(),
                surface: InputSurface::ComposerReady,
            }),
        },
        binding: Probe {
            scope: scope.clone(),
            outcome: if bound {
                ProbeOutcome::Observed(ClientBindingEvidence {
                    server_key: "team".into(),
                    tools: TEAM_TOOLS.to_vec(),
                    source: ClientBindingSource::NativeRegistry,
                })
            } else {
                ProbeOutcome::Pending {
                    deadline: Duration::from_secs(10),
                }
            },
        },
        server: Probe {
            scope,
            outcome: ProbeOutcome::Observed(ServerHandshakeEvidence {
                server_instance: InstanceId::new("mcp").unwrap(),
                initialize_response_written: true,
                tools_list_response_written: true,
            }),
        },
        minimum_sequence: 1,
        now: Duration::from_secs(2),
    }
}
pub fn authorization() -> DeliveryAuthorization {
    DeliveryAuthorization {
        allow_first_business_bootstrap: true,
        require_server_handshake: true,
        channel: Channel::Tmux,
        policy_sha256: HASH,
    }
}
