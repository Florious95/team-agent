//! Documentation fixtures only. This file intentionally does NOT invent Kiro
//! catalog/session/prompt captures or upgrade native capability evidence.
mod support;
use std::collections::BTreeMap;
use support::*;
use team_agent_contract::contract::{
    delivery::*, descriptor::*, hooks::*, plan::*, session::*, types::*,
};
use team_agent_contract::kiro::*;

fn adapter() -> KiroAdapter {
    KiroAdapter::new(McpStdio {
        executable: "/fake/candidate".into(),
        arguments: vec!["fixture-server".into(), "argument with space".into()],
        environment: BTreeMap::from([("FIXTURE_SCOPE".into(), "scope".into())]),
    })
    .unwrap()
}
fn fixture_descriptor() -> ProviderDescriptor {
    // Explicit controlled-test admission, not KIRO_DESCRIPTOR and never Native evidence.
    let mut d = descriptor();
    d.identity.id = "kiro";
    d.identity.binary = "kiro-cli";
    d.workspace = KIRO_DESCRIPTOR.workspace.clone();
    d.teardown = KIRO_DESCRIPTOR.teardown.clone();
    d.prompt = KIRO_DESCRIPTOR.prompt.clone();
    d.mcp.carrier = Support::Supported(McpCarrier::ScopedConfig);
    d.mcp.scope = ResourceScope::WorkingDirectory;
    d.effort.admission = [const { EffortAdmission::Pass }; 6];
    d.effort.carrier = Support::Supported(ValueCarrier::Flag("--effort"));
    d.bypass.intent = Support::Supported(BypassPolicy {
        enabled_arguments: &["--trust-all-tools"],
        requires_startup_consent: false,
    });
    d
}
fn fixture_request() -> LaunchRequest {
    let mut r = request(std::path::Path::new("/owned runtime"), "worker");
    r.provider = "kiro".into();
    r.native.version = BUNDLE_VERSION.into();
    r.native.harness = "v3".into();
    r.native.ui = "tui".into();
    r.paths.executable = "/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli-chat".into();
    r.paths.cwd.path = "/work/code 中 'quoted'".into();
    r.model = Some("exact-provider-model/KeepCase".into());
    r.prompt = Some("line one\nquotes \" and 中文".into());
    r.mode = LaunchMode::LaunchOnly;
    // resolve_launch validates an input profile for FullWorker. Synthetic planning
    // admission below owns its profile; no Kiro terminal profile is fabricated.
    r
}
fn fixture_resolved(mut r: LaunchRequest) -> ResolvedLaunch {
    let mut d = fixture_descriptor();
    // Reuse the existing generic fake profile, explicitly scoped to this synthetic
    // identity, rather than pretend it is a captured Kiro terminal fixture.
    let mut profile = d.input.profiles.require("fake profiles").unwrap()[0].clone();
    profile.version = Box::leak(r.native.version.clone().into_boxed_str());
    profile.harness = Box::leak(r.native.harness.clone().into_boxed_str());
    profile.ui = Box::leak(r.native.ui.clone().into_boxed_str());
    let profiles = Box::leak(vec![profile].into_boxed_slice());
    d.input.profiles = Support::Supported(profiles);
    r.mode = LaunchMode::FullWorker;
    resolve_launch(&d, &hooks(), &r, None).unwrap()
}

#[test]
fn kiro_has_exact_identity_and_no_native_admission_without_r0() {
    let adapter = adapter();
    let hooks = adapter.hooks();
    assert_eq!(KIRO_DESCRIPTOR.identity.id, "kiro");
    assert_eq!(KIRO_DESCRIPTOR.identity.binary, "kiro-cli");
    assert!(KIRO_DESCRIPTOR.identity.aliases.is_empty());
    for operation in [
        Operation::Catalog,
        Operation::Fresh,
        Operation::Resume,
        Operation::InWindowBranch,
        Operation::NewSeatFullSnapshot,
        Operation::NativeNewSeat,
    ] {
        assert!(
            validate_descriptor(&KIRO_DESCRIPTOR, &hooks, operation).is_err(),
            "{operation:?}"
        );
    }
    assert!(matches!(hooks.catalog, HookBinding::Unverified(_)));
    assert!(matches!(hooks.session, HookBinding::Unverified(_)));
    assert!(matches!(hooks.semantic, HookBinding::Unverified(_)));
    assert!(matches!(hooks.interaction, HookBinding::Unverified(_)));
    assert!(matches!(hooks.fork, HookBinding::Unverified(_)));
}

#[test]
fn documentation_plan_keeps_argument_boundaries_and_encodes_owned_agent_json() {
    let adapter = adapter();
    let request = fixture_request();
    let resolved = fixture_resolved(request.clone());
    let plan = adapter.plan(&resolved).unwrap();
    validate_launch_plan(&fixture_descriptor(), &resolved, &plan).unwrap();
    assert_eq!(plan.executable, request.paths.executable);
    assert_eq!(plan.cwd, request.paths.cwd.path);
    let args: Vec<_> = plan.arguments.iter().map(|v| v.to_str().unwrap()).collect();
    assert_eq!(&args[..3], &["chat", "--v3", "--agent"]);
    assert!(args.contains(&"exact-provider-model/KeepCase"));
    assert!(!args.contains(&"--trust-all-tools"));
    assert!(!args.contains(&"--format")); // list-only, not an interactive response format
    assert!(!args.contains(&"--list-models"));
    assert!(!args
        .iter()
        .any(|v| v.contains("line one") || *v == "--resume" || *v == "--resume-picker"));
    let resource = &plan.materialization[0];
    assert_eq!(resource.kind, ResourceKind::AgentConfig);
    assert_eq!(resource.path.root(), request.paths.cwd.path);
    assert_eq!(resource.owner, request.identity);
    let json: serde_json::Value = serde_json::from_slice(&resource.contents).unwrap();
    assert_eq!(json["prompt"], request.prompt.unwrap());
    assert_eq!(json["includeMcpJson"], false);
    assert_eq!(json["includePowers"], false);
    assert_eq!(json["mcpServers"]["team"]["args"][1], "argument with space");
    assert!(json.get("hooks").is_none());
    assert_eq!(plan.environment.set.len(), 1);
    assert_eq!(plan.environment.set["KIRO_CHAT_UI"], "tui");
    assert!(!plan.environment.set.contains_key("HOME"));
    assert!(!plan.environment.set.contains_key("XDG_CONFIG_HOME"));
}

#[test]
fn confirmed_help_syntax_does_not_imply_authenticated_model_or_terminal_admission() {
    for effort in [Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max] {
        assert_eq!(KIRO_DESCRIPTOR.effort.admission[effort.index()], EffortAdmission::Pass);
        let mut request = fixture_request();
        request.role_effort = Some(effort.as_str().into());
        let plan = adapter().plan(&fixture_resolved(request)).unwrap();
        assert!(plan.arguments.windows(2).any(|v| v[0] == "--effort" && v[1] == effort.as_str()));
    }
    assert!(matches!(KIRO_DESCRIPTOR.effort.admission[Effort::Ultra.index()], EffortAdmission::Reject(_)));
    assert!(matches!(KIRO_DESCRIPTOR.model.catalog, Support::Unverified(_)));
    assert!(matches!(KIRO_DESCRIPTOR.auth.subscription, Support::Unverified(_)));
    assert!(matches!(KIRO_DESCRIPTOR.input.profiles, Support::Unverified(_)));
    let mut request = fixture_request();
    request.paths.executable = "/opt/homebrew/bin/kiro-cli".into();
    assert!(adapter().plan(&fixture_resolved(request)).is_err());
}

#[test]
fn effort_and_bypass_are_explicit_and_not_inferred_from_native_banner() {
    let adapter = adapter();
    let mut request = fixture_request();
    request.role_effort = Some("max".into());
    request.bypass = true;
    let resolved = fixture_resolved(request);
    let plan = adapter.plan(&resolved).unwrap();
    validate_launch_plan(&fixture_descriptor(), &resolved, &plan).unwrap();
    assert!(plan
        .arguments
        .windows(2)
        .any(|v| v[0] == "--effort" && v[1] == "max"));
    assert_eq!(
        plan.arguments
            .iter()
            .filter(|v| *v == "--trust-all-tools")
            .count(),
        1
    );
    assert!(!plan.arguments.iter().any(|v| v == "--require-mcp-startup")); // TUI behavior not yet verified
}

#[test]
fn ultra_effort_is_not_guessed_as_a_native_flag_or_silently_downgraded() {
    let mut request = fixture_request();
    request.role_effort = Some("ultra".into());
    assert!(matches!(
        adapter().plan(&fixture_resolved(request)),
        Err(ContractError::Unverified { .. })
    ));
}

#[test]
fn documented_exact_resume_never_becomes_latest_or_cloud_discovery() {
    let mut request = fixture_request();
    let source = request.identity.clone();
    request.operation = Operation::Resume;
    request.identity.generation = Generation(2);
    request.identity.instance = InstanceId::new("worker-2").unwrap();
    request.resume = Some(ResumeRequest {
        expected_source: source.clone(),
        binding: ResumeBinding {
            provider: ProviderId::new("kiro").unwrap(),
            native_session: NativeSessionId::new("Exact SID not a shell fragment").unwrap(),
            storage: SessionStorage::Local,
            cwd: request.paths.cwd.clone(),
            native: request.native.clone(),
            source,
            origin: CaptureOrigin::CurrentNativeSession,
            evidence_kind: EvidenceKind::Fixture,
            evidence_sha256: HASH,
            backing: None,
        },
    });
    let plan = adapter().plan(&fixture_resolved(request)).unwrap();
    assert!(plan
        .arguments
        .windows(2)
        .any(|v| v[0] == "--resume-id" && v[1] == "Exact SID not a shell fragment"));
    assert!(!plan
        .arguments
        .iter()
        .any(|v| v == "--resume" || v == "--list-sessions"));
}

#[test]
fn promoting_a_descriptor_does_not_open_native_plan_gate() {
    let mut request = fixture_request();
    request.evidence_kind = EvidenceKind::Native;
    assert!(matches!(
        adapter().plan(&fixture_resolved(request)),
        Err(ContractError::Unverified { .. })
    ));
}

#[test]
fn unknown_version_harness_or_ui_is_not_a_default_fallback() {
    for field in ["version", "harness", "ui"] {
        let mut resolved = fixture_request();
        match field {
            "version" => resolved.native.version = "2.29.0".into(),
            "harness" => resolved.native.harness = "v2".into(),
            _ => resolved.native.ui = "classic".into(),
        }
        // Synthetic admission follows the test identity; Kiro itself must reject it.
        assert!(adapter().plan(&fixture_resolved(resolved)).is_err());
    }
}

#[test]
fn no_prompt_matcher_or_enter_timing_is_claimed_from_documentation() {
    let adapter = adapter();
    let hooks = adapter.hooks();
    assert!(matches!(
        KIRO_DESCRIPTOR.input.profiles,
        Support::Unverified(_)
    ));
    assert!(hooks.interaction.require("H6").is_err());
    // Generic '>', startup warnings, pasted tokens and native queue text cannot
    // reach an executable Kiro policy. There are no synthetic native timing constants.
    assert!(KIRO_DESCRIPTOR
        .input
        .resolve(
            &fixture_request().native,
            "fake",
            Operation::OrdinarySend,
            Channel::Tmux
        )
        .is_err());
}

#[test]
fn materialization_retains_written_receipts_when_native_validation_is_unknown() {
    struct ValidationFailure(FakeIo);
    impl OwnedIo for ValidationFailure {
        fn create_exclusive(
            &mut self,
            r: &OwnedResourceRequest,
        ) -> Result<OwnedResourceReceipt, PartialFailure> {
            self.0.create_exclusive(r)
        }
        fn read_bound_session(
            &mut self,
            b: &ResumeBinding,
            t: ReadBounds,
        ) -> Result<ReadOutput, ReadFailure> {
            self.0.read_bound_session(b, t)
        }
        fn validate_configuration(
            &mut self,
            _: &OwnedValidationRequest,
        ) -> Result<ReadOutput, ReadFailure> {
            Err(ReadFailure::TimedOut {
                elapsed: std::time::Duration::from_secs(15),
            })
        }
    }
    let adapter = adapter();
    let plan = adapter.plan(&fixture_resolved(fixture_request())).unwrap();
    let mut io = ValidationFailure(FakeIo::new("materialize"));
    let failure = adapter
        .materialize(&plan.materialization, &mut io)
        .unwrap_err();
    assert_eq!(failure.receipt.resources.len(), 1);
    assert_eq!(io.0.writes, 1);
}

#[test]
fn materialization_writes_only_the_planned_config_after_owned_validation() {
    let adapter = adapter();
    let plan = adapter.plan(&fixture_resolved(fixture_request())).unwrap();
    let mut io = FakeIo::new("materialize");
    let receipt = adapter.materialize(&plan.materialization, &mut io).unwrap();
    assert_eq!(io.writes, 1);
    assert_eq!(receipt.resources.len(), 1);
    assert_eq!(receipt.resources[0].path, plan.materialization[0].path);
}

#[test]
fn materialization_rejects_ambient_mcp_or_wrong_owned_name_before_io() {
    let adapter = adapter();
    let mut plan = adapter.plan(&fixture_resolved(fixture_request())).unwrap();
    let mut config: serde_json::Value =
        serde_json::from_slice(&plan.materialization[0].contents).unwrap();
    config["includeMcpJson"] = serde_json::json!(true);
    plan.materialization[0].contents = serde_json::to_vec(&config).unwrap();
    let mut io = FakeIo::new("materialize");
    assert!(adapter.materialize(&plan.materialization, &mut io).is_err());
    assert_eq!(io.writes, 0);
}

#[test]
fn exit_codes_never_authorize_replay_or_erase_possible_effects() {
    for floor in [
        DeliveryEffect::NoEffect,
        DeliveryEffect::MayHavePasted,
        DeliveryEffect::MayHaveSubmitted,
        DeliveryEffect::Submitted,
    ] {
        for code in [
            None,
            Some(0),
            Some(1),
            Some(3),
            Some(4),
            Some(124),
            Some(99),
        ] {
            let observation = classify_exit(code, floor);
            assert_eq!(observation.delivery_floor, floor);
            assert!(observation.native_effects_may_have_occurred);
        }
    }
    assert_eq!(
        classify_exit(Some(4), DeliveryEffect::MayHaveSubmitted).class,
        ExitClass::RequestedAgentNotFound
    );
    assert_eq!(
        classify_exit(Some(3), DeliveryEffect::NoEffect).class,
        ExitClass::McpStartupFailure
    );
    assert_eq!(
        classify_exit(Some(124), DeliveryEffect::NoEffect).class,
        ExitClass::Unknown
    ); // harness timeout is not a native exit receipt
}
