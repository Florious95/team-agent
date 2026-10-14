//! purpose: lock the phase-1 Codex descriptor admission for Issue #325
//! contract: fifteen facets and seven hooks match CODEX-DESCRIPTOR-TASKBOOK.md
//! boundary: red-track artifact; not on the crate path until the tester copies it.
//!           A missing `providers::codex` module is the expected red compile.
//!           Green means the admission table matches. It is not G1–G5 acceptance.

use std::path::{Path, PathBuf};
use std::time::Duration;

use team_agent_contract::contract::delivery::*;
use team_agent_contract::contract::descriptor::*;
use team_agent_contract::contract::hooks::*;
use team_agent_contract::contract::plan::*;
use team_agent_contract::contract::probe::EvidenceScope;
use team_agent_contract::contract::session::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::providers::codex::{
    admit_rollout_name, argv_mutates_global_config, classify_image_role, forbidden_interactive_argument,
    interactive_launch_plan, may_signal, owned_mcp_document, owned_mcp_toml, parse_bundled_catalog,
    phase1_interactive_arguments, seat_pair_ok, startup_modal_intercept, validate_catalog_request,
    CodexAdapter, CodexImageRole,
    CodexLaunchImages, CATALOG_HARNESS, CATALOG_SOURCE, CATALOG_UI, CODEX_DESCRIPTOR,
};

const V7: &str = "018f6b6a-7c4e-7a1b-8c2d-111111111111";
const V4: &str = "018f6b6a-7c4e-4a1b-8c2d-111111111111";
const ROLL_OK: &str = "rollout-2026-10-14T05-29-00-018f6b6a-7c4e-7a1b-8c2d-111111111111.jsonl";
const WRAPPER: &str = "/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js";
const ENGINE: &str = "/opt/homebrew/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex";
const DAEMON: &str = "/Users/alauda/.codex/packages/app-server-daemon/releases/0.162.1-aarch64-apple-darwin/bin/codex";
const SHADOW: &str = "/Users/alauda/.local/bin/codex";

fn unverified_code<T>(result: Result<T, ContractError>) -> &'static str {
    match result {
        Err(ContractError::Unverified { reason, .. }) => reason.code,
        Err(other) => panic!("expected Unverified, got {other:?}"),
        Ok(_) => panic!("expected Unverified"),
    }
}

fn reason_code(reason: &Reason) -> &'static str {
    reason.code
}

fn instance() -> InstanceIdentity {
    InstanceIdentity {
        scope: ScopeId::new("codex-phase1").unwrap(),
        seat: SeatId::new("red").unwrap(),
        instance: InstanceId::new("leaf").unwrap(),
        generation: Generation(1),
    }
}

fn native() -> NativeIdentity {
    NativeIdentity {
        version: "0.160.1".into(),
        harness: "codex-tui".into(),
        ui: "codex-tui".into(),
        platform: Platform::MacOs,
        executable_sha256: Digest([9; 32]),
    }
}

fn scope() -> EvidenceScope {
    EvidenceScope {
        identity: instance(),
        endpoint: "pty".into(),
        pane: "codex-red".into(),
        binding: "phase1".into(),
        session: None,
        sequence: 1,
        observed_at: Duration::from_secs(1),
        valid_for: Duration::from_secs(30),
        source: "phase1-red".into(),
    }
}

fn frame(text: &str) -> CaptureFrame {
    CaptureFrame {
        scope: scope(),
        text: text.into(),
        cwd: None,
        input: None,
        baseline: None,
        profile_id: "none".into(),
        operation: Operation::Fresh,
        message: None,
        attempt: None,
        after_step: None,
        paste_latch: PasteLatch::NeverSeen,
    }
}

fn seated() -> CodexAdapter {
    CodexAdapter::new()
}

#[test]
fn fifteen_facets_match_phase1_admission() {
    let d = &CODEX_DESCRIPTOR;
    assert_eq!(d.identity.id, "codex");
    assert_eq!(d.identity.display_name, "Codex CLI (contract)");
    assert_eq!(d.identity.binary, "codex");
    assert!(d.identity.aliases.is_empty());
    assert_eq!(
        unverified_code(d.identity.leader.require("leader")),
        "codex-leader-not-run"
    );

    assert_eq!(
        d.model
            .selection
            .require("model selection")
            .expect("omitted model uses the native default"),
        &ModelSelection::NativeDefaultOrOpaqueId
    );
    assert_eq!(d.model.omitted, OmittedModel::NativeDefault);
    assert_eq!(
        d.model.catalog.require("catalog").expect("catalog supported"),
        &CATALOG_SOURCE
    );
    assert_eq!(
        CATALOG_SOURCE.arguments,
        &["debug", "models", "--bundled"][..]
    );
    assert_eq!(CATALOG_SOURCE.schema, "codex-debug-models-bundled-json-v1");

    for effort in Effort::ALL {
        match &d.effort.admission[effort.index()] {
            EffortAdmission::Reject(reason) => {
                assert_eq!(reason_code(reason), "codex-effort-not-remeasured")
            }
            other => panic!("{effort:?} must be Reject, got {other:?}"),
        }
    }
    assert_eq!(
        unverified_code(d.effort.carrier.require("effort")),
        "codex-effort-not-remeasured"
    );
    assert!(!d.effort.inherit_team_default);
    assert!(!d.effort.model_dependent);

    assert!(matches!(
        d.auth.subscription,
        Support::Supported(AuthMechanism::NativeExistingSession)
    ));
    assert_eq!(
        unverified_code(d.auth.official_api.require("official api")),
        "codex-official-api-not-run"
    );
    assert_eq!(
        unverified_code(d.auth.compatible_api.require("compatible api")),
        "codex-compatible-api-not-run"
    );

    assert_eq!(
        unverified_code(d.bypass.intent.require("bypass")),
        "codex-bypass-modal-interaction-not-run"
    );
    assert_eq!(
        d.prompt.carrier.require("prompt").expect("runtime bridge"),
        &PromptCarrier::RuntimeBridge
    );

    assert_eq!(
        d.mcp.carrier.require("mcp").expect("scoped config"),
        &McpCarrier::ScopedConfig
    );
    assert_eq!(d.mcp.scope, ResourceScope::RuntimeRoot);
    assert_eq!(d.mcp.tools, &TEAM_TOOLS);
    assert!(d.mcp.tools.contains(&LogicalTool::SendMessage));
    assert!(d.mcp.tools.contains(&LogicalTool::ReportResult));
    assert!(d.mcp.tools.contains(&LogicalTool::GetTeamStatus));
    assert_eq!(d.mcp.isolation, McpIsolation::AmbientExcluded);
    assert_eq!(
        d.tool_names.naming.require("tool naming").expect("static server"),
        &ToolNaming::StaticServer {
            key: "team_orchestrator",
        }
    );

    assert_eq!(d.session.fresh, FreshSession::CaptureAfterLaunch);
    assert_eq!(
        unverified_code(d.session.resume.require("resume")),
        "codex-live-resume-not-run"
    );
    assert_eq!(
        unverified_code(d.fork.in_window.require("in-window branch")),
        "codex-fork-not-run"
    );
    assert_eq!(
        unverified_code(d.fork.full_snapshot.require("full snapshot")),
        "codex-fork-not-run"
    );
    assert_eq!(
        unverified_code(d.fork.native_new_seat.require("native new seat")),
        "codex-fork-not-run"
    );
    assert!(d.fork.allowed_auth.is_empty());

    let profiles = d.input.profiles.require("input profiles").expect("submit profile");
    assert_eq!(profiles.len(), 1);
    let profile = &profiles[0];
    assert_eq!(profile.id, "codex-tui-single-enter-v1");
    assert_eq!(profile.identity, ProfileIdentity::RuntimeCaptured);
    assert_eq!(profile.harness, CATALOG_HARNESS);
    assert_eq!(profile.ui, CATALOG_UI);
    assert_eq!(profile.platform, Platform::MacOs);
    assert_eq!(profile.channel, Channel::Tmux);
    assert_eq!(
        profile.operations,
        &[Operation::FirstBusiness, Operation::OrdinarySend]
    );
    assert_eq!(
        profile.policy_sha256,
        Digest([
            168, 118, 138, 11, 125, 142, 62, 19, 118, 180, 255, 52, 216, 236, 157, 192, 28, 208,
            80, 4, 35, 107, 173, 193, 213, 5, 148, 61, 47, 121, 240, 186,
        ])
    );
    let policy = profile.policy.require("submit").expect("single enter");
    assert_eq!(policy.paste_mode, PasteMode::Bracketed);
    assert!(matches!(policy.payload_trailer, PayloadTrailer::None));
    assert_eq!(policy.initial_submit, PhysicalKey::Enter);
    assert!(policy.confirmation_steps.is_empty());
    assert_eq!(policy.retry_budget, RetryBudget::Never);
    assert!(policy.queue_flush.is_empty());
    assert_eq!(policy.max_submit_keys, 1);
    assert_eq!(policy.timing.capture_interval, Duration::from_millis(100));
    assert_eq!(policy.timing.stable_window, Duration::from_millis(200));
    assert_eq!(policy.timing.paste_to_submit_floor, Duration::from_millis(100));
    assert_eq!(policy.timing.deadline, Duration::from_secs(30));
    assert!(d.startup.interactive);
    assert_eq!(
        unverified_code(d.startup.authorized_consent.require("startup consent")),
        "codex-startup-consent-not-accepted"
    );

    assert!(matches!(d.probe_sources.process, Support::Supported(ProbeSource::Host)));
    assert!(matches!(
        d.probe_sources.pane,
        Support::Supported(ProbeSource::NativeSurface)
    ));
    assert_eq!(
        unverified_code(d.probe_sources.server.require("server")),
        "codex-server-socket-holder-unknown"
    );
    assert_eq!(
        unverified_code(d.probe_sources.client_binding.require("client binding")),
        "codex-client-binding-not-run"
    );
    assert_eq!(
        unverified_code(d.probe_sources.round_trip.require("round trip")),
        "codex-round-trip-not-run"
    );
    assert_eq!(
        unverified_code(d.probe_sources.semantic.require("semantic")),
        "codex-semantic-not-run"
    );

    assert_eq!(d.workspace.config_scope, ResourceScope::RuntimeRoot);
    assert!(!d.workspace.shared_cwd);
    assert!(d.workspace.requires_materialization_lease);

    let teardown = &d.teardown.resources;
    assert!(teardown.contains(&ResourcePolicy {
        kind: ResourceKind::SessionBacking,
        disposition: ResourceDisposition::OwnedPreserved,
    }));
    assert!(teardown.contains(&ResourcePolicy {
        kind: ResourceKind::McpConfig,
        disposition: ResourceDisposition::OwnedRemovable,
    }));
    assert!(teardown.contains(&ResourcePolicy {
        kind: ResourceKind::Prompt,
        disposition: ResourceDisposition::OwnedRemovable,
    }));
    assert!(teardown.contains(&ResourcePolicy {
        kind: ResourceKind::GlobalSettings,
        disposition: ResourceDisposition::Forbidden,
    }));
    assert!(teardown.contains(&ResourcePolicy {
        kind: ResourceKind::NativeDatabase,
        disposition: ResourceDisposition::Forbidden,
    }));
    assert_eq!(teardown.len(), 5);
}

#[test]
fn seven_hooks_bind_catalog_plan_materialize_session_and_interaction() {
    let adapter = seated();
    let hooks = adapter.hooks();
    assert!(hooks.catalog.require("H1 CatalogHook").is_ok());
    assert!(hooks.plan.require("H2 PlanHook").is_ok());
    assert!(hooks.materialize.require("H3 MaterializeHook").is_ok());
    assert!(hooks.session.require("H4 SessionHook").is_ok());
    assert_eq!(
        unverified_code(hooks.semantic.require("H5 SemanticReader")),
        "codex-h5-semantic-not-run"
    );
    assert!(hooks.interaction.require("H6 InteractionHook").is_ok());
    assert_eq!(
        unverified_code(hooks.fork.require("H7 ForkHook")),
        "codex-h7-fork-not-run"
    );
    assert_eq!(
        unverified_code(adapter.plan_phase1()),
        "codex-launch-not-admitted"
    );
}

#[test]
fn fresh_descriptor_validates() {
    let adapter = seated();
    let hooks = adapter.hooks();
    validate_descriptor(&CODEX_DESCRIPTOR, &hooks, Operation::Fresh).unwrap();
    validate_descriptor(&CODEX_DESCRIPTOR, &hooks, Operation::Resume).unwrap();

    validate_descriptor(&CODEX_DESCRIPTOR, &hooks, Operation::Catalog).unwrap();
    for operation in [
        Operation::InWindowBranch,
        Operation::NewSeatFullSnapshot,
        Operation::NativeNewSeat,
    ] {
        let err = validate_descriptor(&CODEX_DESCRIPTOR, &hooks, operation).unwrap_err();
        assert_eq!(unverified_code::<()>(Err(err)), "codex-fork-not-run");
    }
}

#[test]
fn image_roles_keep_the_shared_daemon_unsignaled() {
    assert_eq!(classify_image_role(WRAPPER), CodexImageRole::SeatWrapper);
    assert_eq!(classify_image_role(ENGINE), CodexImageRole::SeatEngine);
    assert_eq!(classify_image_role(DAEMON), CodexImageRole::SharedDaemon);
    assert_eq!(classify_image_role(SHADOW), CodexImageRole::PathShadow);
    assert_eq!(
        classify_image_role("/opt/homebrew/bin/codex"),
        CodexImageRole::Unknown
    );
    assert!(may_signal(CodexImageRole::SeatWrapper));
    assert!(may_signal(CodexImageRole::SeatEngine));
    assert!(!may_signal(CodexImageRole::SharedDaemon));
    assert!(!may_signal(CodexImageRole::PathShadow));
    assert!(!may_signal(CodexImageRole::Unknown));
    assert!(seat_pair_ok("0.160.1", "0.160.1"));
    assert!(!seat_pair_ok("0.160.1", "0.162.1"));
    assert!(!seat_pair_ok("codex-cli 0.160.1", "0.160.1"));
}

#[test]
fn interactive_argv_cannot_update_install_or_write_global_mcp() {
    assert_eq!(
        phase1_interactive_arguments(),
        &["--no-alt-screen", "--no-daemon"][..]
    );
    for arg in [
        "--last",
        "npm",
        "@openai/codex",
        "--dangerously-bypass-approvals-and-sandbox",
    ] {
        assert!(forbidden_interactive_argument(arg), "{arg}");
    }
    assert!(!forbidden_interactive_argument(WRAPPER));
    assert!(argv_mutates_global_config(&["mcp", "add"]));
    assert!(argv_mutates_global_config(&[
        "codex", "mcp", "add", "team", "--", "/usr/bin/true"
    ]));
    assert!(!argv_mutates_global_config(&["codex", "mcp", "list"]));
    assert!(!argv_mutates_global_config(phase1_interactive_arguments()));
}

fn measured_images(wrapper: &str, engine: &str) -> CodexLaunchImages {
    CodexLaunchImages {
        wrapper: PathBuf::from(wrapper),
        wrapper_sha256: Digest([1; 32]),
        engine: PathBuf::from(engine),
        engine_sha256: Digest([2; 32]),
        wrapper_version: "0.160.1".into(),
        engine_version: "0.160.1".into(),
    }
}

fn assert_launch_refused(images: &CodexLaunchImages, cwd: &str, expected: &ExpectedSession) {
    match interactive_launch_plan(images, &PathBuf::from(cwd), expected) {
        Err(error) => assert_eq!(error, ContractError::Invalid("codex interactive launch")),
        Ok(_) => panic!("expected the interactive launch to fail"),
    }
}

#[test]
fn interactive_launch_plan_keeps_the_two_measured_flags() {
    let cwd = PathBuf::from("/tmp/codex-phase1");
    let plan = interactive_launch_plan(
        &measured_images(WRAPPER, ENGINE),
        &cwd,
        &ExpectedSession::CaptureAfterLaunch,
    )
    .unwrap();
    let arguments: Vec<_> = plan
        .arguments
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(arguments, phase1_interactive_arguments());
    assert_eq!(arguments.len(), 2);
    let launcher = plan.launcher.expect("wrapper launcher");
    assert_eq!(launcher.executable, PathBuf::from(WRAPPER));
    assert_eq!(launcher.sha256, Digest([1; 32]));
    assert_eq!(plan.executable, PathBuf::from(ENGINE));
    assert!(plan.executable_inputs.is_empty());
    assert_eq!(plan.cwd, cwd);
    assert!(plan.environment.remove.is_empty());
    assert!(plan.environment.set.is_empty());
    assert!(plan.materialization.is_empty());
    assert_eq!(plan.expected_session, ExpectedSession::CaptureAfterLaunch);
    for carrier in [
        &plan.carriers.model,
        &plan.carriers.prompt,
        &plan.carriers.mcp,
        &plan.carriers.bypass,
        &plan.carriers.effort,
    ] {
        assert_eq!(*carrier, CarrierUse::NotRequested);
    }
}

fn phase1_request(
    operation: Operation,
    mode: LaunchMode,
    model: Option<&str>,
    bypass: bool,
    prompt: Option<&str>,
) -> LaunchRequest {
    LaunchRequest {
        provider: "codex".into(),
        operation,
        mode,
        auth: AuthMode::NativeSubscription,
        model: model.map(str::to_owned),
        role_effort: None,
        team_effort: None,
        bypass,
        prompt: prompt.map(str::to_owned),
        identity: instance(),
        native: native(),
        paths: LaunchPaths {
            executable: PathBuf::from(ENGINE),
            candidate: PathBuf::from(WRAPPER),
            cwd: CwdIdentity {
                path: PathBuf::from("/tmp/codex-phase1"),
                identity: Digest([4; 32]),
            },
            runtime_root: PathBuf::from("/tmp/codex-phase1-runtime"),
        },
        channel: Channel::Tmux,
        input_profile: None,
        evidence_kind: EvidenceKind::Native,
        preassigned_session: None,
        resume: None,
        fork: None,
    }
}

#[test]
fn launch_only_fresh_with_images_returns_the_two_flag_plan() {
    let adapter = seated().with_launch_images(measured_images(WRAPPER, ENGINE));
    let hooks = adapter.hooks();
    let resolved = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &phase1_request(Operation::Fresh, LaunchMode::LaunchOnly, None, false, None),
        None,
    )
    .expect("omitted model is the native default");
    let plan = adapter.plan(&resolved).expect("launch only fresh plan");
    let arguments: Vec<_> = plan
        .arguments
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(arguments, phase1_interactive_arguments());
    assert!(plan.materialization.is_empty());
    let launcher = plan.launcher.expect("wrapper");
    assert_eq!(launcher.executable, PathBuf::from(WRAPPER));
    assert_eq!(plan.executable, PathBuf::from(ENGINE));
    for carrier in [
        &plan.carriers.model,
        &plan.carriers.prompt,
        &plan.carriers.mcp,
        &plan.carriers.bypass,
        &plan.carriers.effort,
    ] {
        assert_eq!(*carrier, CarrierUse::NotRequested);
    }

    let bypass = match resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &phase1_request(Operation::Fresh, LaunchMode::LaunchOnly, None, true, None),
        None,
    ) {
        Err(error) => error,
        Ok(_) => panic!("bypass must not resolve"),
    };
    assert_eq!(
        unverified_code::<LaunchPlan>(Err(bypass)),
        "codex-bypass-modal-interaction-not-run"
    );

    let model = "opaque-seat-model";
    let resolved_model = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &phase1_request(
            Operation::Fresh,
            LaunchMode::LaunchOnly,
            Some(model),
            false,
            None,
        ),
        None,
    )
    .expect("opaque id reaches plan");
    let model_err = match adapter.plan(&resolved_model) {
        Err(error) => error,
        Ok(_) => panic!("model string must not enter argv"),
    };
    assert!(!format!("{model_err:?}").contains(model));
    assert_eq!(
        unverified_code::<LaunchPlan>(Err(model_err)),
        "codex-model-selection-not-admitted"
    );

    let prompted = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &phase1_request(
            Operation::Fresh,
            LaunchMode::LaunchOnly,
            None,
            false,
            Some("stay-out-of-argv"),
        ),
        None,
    )
    .expect("launch-only prompt reaches plan");
    let prompt_err = match adapter.plan(&prompted) {
        Err(error) => error,
        Ok(_) => panic!("launch-only prompt must stay out of argv"),
    };
    assert_eq!(
        unverified_code::<LaunchPlan>(Err(prompt_err)),
        "codex-prompt-bytes-not-observed"
    );

    let empty = seated();
    assert_eq!(
        unverified_code(empty.plan_phase1()),
        "codex-launch-not-admitted"
    );
    assert_eq!(
        unverified_code(empty.plan(&resolved)),
        "codex-launch-not-admitted"
    );
    assert_eq!(
        unverified_code(adapter.plan_phase1()),
        "codex-launch-not-admitted"
    );
    assert!(resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &phase1_request(Operation::Resume, LaunchMode::LaunchOnly, None, false, None),
        None,
    )
    .is_err());
}

#[test]
fn interactive_launch_plan_rejects_unmeasured_images() {
    let expected = ExpectedSession::CaptureAfterLaunch;
    let mut versions = measured_images(WRAPPER, ENGINE);
    versions.engine_version = "0.162.1".into();
    assert_launch_refused(&versions, "/tmp/codex-phase1", &expected);
    let mut empty = measured_images(WRAPPER, ENGINE);
    empty.wrapper_version.clear();
    empty.engine_version.clear();
    assert_launch_refused(&empty, "/tmp/codex-phase1", &expected);
    let mut zero = measured_images(WRAPPER, ENGINE);
    zero.wrapper_sha256 = Digest([0; 32]);
    assert_launch_refused(&zero, "/tmp/codex-phase1", &expected);
    let mut same = measured_images(WRAPPER, ENGINE);
    same.engine_sha256 = same.wrapper_sha256;
    assert_launch_refused(&same, "/tmp/codex-phase1", &expected);
    assert_launch_refused(
        &measured_images(
            "node_modules/@openai/codex/bin/codex.js",
            "codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex",
        ),
        "/tmp/codex-phase1",
        &expected,
    );
    assert_launch_refused(
        &measured_images(DAEMON, ENGINE),
        "/tmp/codex-phase1",
        &expected,
    );
    assert_launch_refused(
        &measured_images(WRAPPER, SHADOW),
        "/tmp/codex-phase1",
        &expected,
    );
    assert_launch_refused(
        &measured_images("/opt/homebrew/bin/codex", ENGINE),
        "/tmp/codex-phase1",
        &expected,
    );
    assert_launch_refused(
        &measured_images(WRAPPER, ENGINE),
        "relative/codex-phase1",
        &expected,
    );
    assert_launch_refused(
        &measured_images(WRAPPER, ENGINE),
        "/tmp/codex-phase1",
        &ExpectedSession::PendingAllocation,
    );
}

#[test]
fn startup_modal_intercept_stops_before_any_key() {
    let trust = startup_modal_intercept(
        "Trust and continue\nUpdate now\nAsk Codex to do anything",
    )
    .unwrap_err();
    assert_eq!(
        trust,
        ContractError::Unverified {
            field: "codex startup modal",
            reason: Reason {
                code: "codex-folder-trust-modal",
                message: "Codex folder trust modal blocks the frame",
            },
        }
    );
    let update = startup_modal_intercept("Update available\n1. Update now").unwrap_err();
    match update {
        ContractError::Unverified { field, reason } => {
            assert_eq!(field, "codex startup modal");
            assert_eq!(reason.code, "codex-update-modal");
        }
        other => panic!("unexpected {other:?}"),
    }
    startup_modal_intercept("Ask Codex to do anything").unwrap();
    let source = include_str!("../src/providers/codex.rs");
    let body = source
        .split_once("pub fn startup_modal_intercept")
        .unwrap()
        .1
        .split_once("pub fn classify_image_role")
        .unwrap()
        .0;
    for token in ["Enter", "ESC", "Down", "0x03"] {
        assert!(!body.contains(token), "{token}");
    }
}

#[test]
fn startup_frames_keep_modals_ahead_of_the_composer() {
    let adapter = seated();
    let update = adapter.interpret(&frame(
        "OpenAI Codex (v0.160.1)\nUpdate available · 0.160.1 → 0.162.1\n1. Update now\n2. Skip\nAsk Codex to do anything",
    ));
    assert_eq!(update.surface, InputSurface::StartupRiskWarning);
    assert_eq!(update.predicate.as_deref(), Some("codex-update-modal"));

    let trust = adapter.interpret(&frame(
        "Update available\n1. Trust and continue\n2. Quit\nAsk Codex to do anything",
    ));
    assert_eq!(trust.surface, InputSurface::StartupRiskWarning);
    assert_eq!(trust.predicate.as_deref(), Some("codex-folder-trust-modal"));
    assert!(trust.control_paste.is_none());

    let ready = adapter.interpret(&frame(
        "OpenAI Codex (v0.160.1)\nAsk Codex to do anything",
    ));
    assert_eq!(ready.surface, InputSurface::ComposerReady);
    assert!(ready.predicate.is_none());

    let stall = adapter.interpret(&frame("\u{1b}[6n"));
    assert_eq!(stall.surface, InputSurface::Starting);
    assert_eq!(stall.predicate.as_deref(), Some("codex-terminal-query-stall"));
}

#[test]
fn rollout_grammar_requires_uuid_v7_and_the_exact_path() {
    admit_rollout_name(V7, ROLL_OK, "2026/10/14").unwrap();
    assert_eq!(
        unverified_code(admit_rollout_name("--last", ROLL_OK, "2026/10/14")),
        "codex-session-id-not-uuid-v7"
    );
    assert_eq!(
        unverified_code(admit_rollout_name(V4, ROLL_OK, "2026/10/14")),
        "codex-session-id-not-uuid-v7"
    );
    assert_eq!(
        unverified_code(admit_rollout_name(V7, "rollout-latest.jsonl", "2026/10/14")),
        "codex-rollout-name-mismatch"
    );
    assert_eq!(
        unverified_code(admit_rollout_name(V7, ROLL_OK, "2026/10/15")),
        "codex-rollout-date-mismatch"
    );
}

#[test]
fn session_bind_accepts_only_a_native_exact_rollout_path() {
    let adapter = seated();
    let path = format!("/tmp/codex-home/sessions/2026/10/14/{ROLL_OK}");
    let record = serde_json::json!({
        "session_id": V7,
        "rollout_path": path,
    });
    let binding = adapter
        .bind(&ScopedSessionEvidence {
            scope: scope(),
            provider: ProviderId::new("codex").unwrap(),
            native: native(),
            cwd: CwdIdentity {
                path: PathBuf::from("/tmp/codex-phase1"),
                identity: Digest([4; 32]),
            },
            origin: CaptureOrigin::ExactBacking,
            evidence_kind: EvidenceKind::Native,
            evidence_sha256: Digest([7; 32]),
            record: serde_json::to_vec(&record).unwrap(),
        })
        .unwrap();
    assert_eq!(binding.provider.as_str(), "codex");
    assert_eq!(binding.storage, SessionStorage::Local);
    assert_eq!(binding.origin, CaptureOrigin::ExactBacking);
    assert_eq!(binding.native_session.as_str(), V7);
    assert_eq!(binding.backing.unwrap().to_str().unwrap(), path);

    let fixture = adapter.bind(&ScopedSessionEvidence {
        scope: scope(),
        provider: ProviderId::new("codex").unwrap(),
        native: native(),
        cwd: CwdIdentity {
            path: PathBuf::from("/tmp/codex-phase1"),
            identity: Digest([4; 32]),
        },
        origin: CaptureOrigin::ExactBacking,
        evidence_kind: EvidenceKind::Fixture,
        evidence_sha256: Digest([7; 32]),
        record: serde_json::to_vec(&record).unwrap(),
    });
    assert_eq!(
        unverified_code(fixture),
        "codex-fixture-session-rejected"
    );

    let relative = serde_json::json!({
        "session_id": V7,
        "rollout_path": format!("sessions/2026/10/14/{ROLL_OK}"),
    });
    let relative = adapter.bind(&ScopedSessionEvidence {
        scope: scope(),
        provider: ProviderId::new("codex").unwrap(),
        native: native(),
        cwd: CwdIdentity {
            path: PathBuf::from("/tmp/codex-phase1"),
            identity: Digest([4; 32]),
        },
        origin: CaptureOrigin::ExactBacking,
        evidence_kind: EvidenceKind::Native,
        evidence_sha256: Digest([7; 32]),
        record: serde_json::to_vec(&relative).unwrap(),
    });
    assert_eq!(
        unverified_code(relative),
        "codex-rollout-path-not-absolute"
    );
}

fn bundled_model(slug: &str, visibility: &str, api: serde_json::Value, efforts: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "slug": slug,
        "display_name": format!("{slug} display"),
        "visibility": visibility,
        "supported_in_api": api,
        "supported_reasoning_levels": efforts.iter().map(|effort| serde_json::json!({
            "effort": effort,
            "description": "not a model alias",
        })).collect::<Vec<_>>(),
        "base_instructions": "ignored",
    })
}

#[test]
fn bundled_catalog_keeps_hidden_rows_and_ignores_unknown_ids() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "models": [
            bundled_model("seat-hide", "hide", serde_json::Value::Bool(true), &["xhigh", "max"]),
            bundled_model("seat-list", "list", serde_json::Value::Bool(true), &["low", "medium"]),
        ]
    })).unwrap();
    let models = parse_bundled_catalog(&bytes).unwrap();
    assert_eq!(models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["seat-hide", "seat-list"]);
    assert!(models.iter().all(|model| model.id != "o3"));
    assert_eq!(models[0].efforts, Support::Supported(vec![Effort::Xhigh, Effort::Max]));
    assert_eq!(models[1].efforts, Support::Supported(vec![Effort::Low, Effort::Medium]));
}

#[test]
fn bundled_catalog_rejects_malformed_documents() {
    let valid = bundled_model("seat-list", "list", serde_json::Value::Bool(true), &["high"]);
    let cases: Vec<(&str, serde_json::Value, bool)> = vec![
        ("extra key", serde_json::json!({"models": [valid.clone()], "extra": true}), false),
        ("empty", serde_json::json!({"models": []}), false),
        ("duplicate", serde_json::json!({"models": [valid.clone(), valid.clone()]}), true),
        ("visibility", serde_json::json!({"models": [bundled_model("seat-list", "none", serde_json::Value::Bool(true), &["high"])]}), false),
        ("blank slug", serde_json::json!({"models": [{
            "slug": " seat-list ",
            "display_name": "Seat",
            "visibility": "list",
            "supported_in_api": true,
            "supported_reasoning_levels": [{"effort": "low"}],
        }]}), false),
        ("unknown effort", serde_json::json!({"models": [bundled_model("seat-list", "list", serde_json::Value::Bool(true), &["turbo"])]}), false),
        ("api flag", serde_json::json!({"models": [bundled_model("seat-list", "list", serde_json::Value::String("true".into()), &["low"])]}), false),
    ];
    for (name, value, ambiguous) in cases {
        let error = parse_bundled_catalog(&serde_json::to_vec(&value).unwrap()).expect_err(name);
        if ambiguous {
            assert!(matches!(error, ContractError::AmbiguousModel), "{name}: {error:?}");
        } else {
            assert!(matches!(error, ContractError::Invalid("codex model catalog")), "{name}: {error:?}");
        }
    }
}

struct ScriptedHost {
    result: Result<ReadOutput, ReadFailure>,
}

impl BoundedReadHost for ScriptedHost {
    fn read_catalog(&mut self, _request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        std::mem::replace(&mut self.result, Err(ReadFailure::Exit { code: -1 }))
    }
}

fn catalog_request(max_output_bytes: usize) -> CatalogRequest {
    let root = std::env::temp_dir().join(format!("codex-h1-{}", std::process::id()));
    let bin = root.join("node_modules/@openai/codex/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("codex.js");
    std::fs::write(&executable, b"fixture\n").unwrap();
    CatalogRequest {
        provider: ProviderId::new("codex").unwrap(),
        native: NativeIdentity {
            version: "fixture".into(),
            harness: CATALOG_HARNESS.into(),
            ui: CATALOG_UI.into(),
            platform: Platform::MacOs,
            executable_sha256: Digest([3; 32]),
        },
        executable,
        cwd: CwdIdentity {
            path: PathBuf::from("/tmp/codex-h1"),
            identity: Digest([4; 32]),
        },
        source: CATALOG_SOURCE,
        bounds: ReadBounds {
            deadline: Duration::from_secs(30),
            max_output_bytes,
        },
    }
}

#[test]
fn catalog_read_failures_do_not_yield_an_observation() {
    let adapter = seated();
    let request = catalog_request(4);
    validate_catalog_request(&request).unwrap();
    let cases = [
        ReadOutput { stdout: Vec::new(), elapsed: Duration::from_secs(31), exit_code: 0 },
        ReadOutput { stdout: b"12345".to_vec(), elapsed: Duration::from_secs(1), exit_code: 0 },
        ReadOutput { stdout: b"{}".to_vec(), elapsed: Duration::from_secs(1), exit_code: 2 },
    ];
    let mut saw_timeout = false;
    let mut saw_limit = false;
    let mut saw_exit = false;
    for output in cases {
        let result = adapter.discover(&request, &mut ScriptedHost { result: Ok(output) });
        match result {
            Err(ReadFailure::TimedOut { .. }) => saw_timeout = true,
            Err(ReadFailure::OutputLimit { limit: 4 }) => saw_limit = true,
            Err(ReadFailure::Exit { code: 2 }) => saw_exit = true,
            Err(other) => panic!("unexpected catalog failure: {other:?}"),
            Ok(_) => panic!("catalog observation"),
        }
    }
    assert!(saw_timeout && saw_limit && saw_exit);
}

struct CountingIo {
    writes: usize,
}

impl CountingIo {
    fn new() -> Self {
        Self { writes: 0 }
    }
}

impl OwnedIo for CountingIo {
    fn create_exclusive(
        &mut self,
        request: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure> {
        self.writes += 1;
        Ok(OwnedResourceReceipt {
            path: request.path.clone(),
            owner: request.owner.clone(),
            operation: OperationId::new("h3").unwrap(),
            kind: request.kind,
            disposition: ResourceDisposition::OwnedRemovable,
            write_effect: ResourceWriteEffect::Written {
                bytes_sha256: team_agent_contract::host::digest(&request.contents),
            },
            exclusive: true,
            creation_identity: None,
        })
    }

    fn read_bound_session(
        &mut self,
        _: &ResumeBinding,
        _: ReadBounds,
    ) -> Result<ReadOutput, ReadFailure> {
        Err(ReadFailure::Unknown(Reason {
            code: "unused",
            message: "unused",
        }))
    }

    fn validate_configuration(
        &mut self,
        _: &OwnedValidationRequest,
    ) -> Result<ReadOutput, ReadFailure> {
        Err(ReadFailure::Unknown(Reason {
            code: "unused",
            message: "unused",
        }))
    }
}

fn prompt_request(root: &str, contents: &[u8]) -> OwnedResourceRequest {
    OwnedResourceRequest {
        path: OwnedPath::new(PathBuf::from(root), PathBuf::from("codex/prompt.txt")).unwrap(),
        owner: instance(),
        kind: ResourceKind::Prompt,
        contents: contents.to_vec(),
    }
}

fn mcp_request(root: &str, contents: Vec<u8>) -> OwnedResourceRequest {
    OwnedResourceRequest {
        path: OwnedPath::new(PathBuf::from(root), PathBuf::from("codex/mcp.toml")).unwrap(),
        owner: instance(),
        kind: ResourceKind::McpConfig,
        contents,
    }
}

fn assert_h3_unwritten(adapter: &CodexAdapter, requests: &[OwnedResourceRequest]) {
    let mut io = CountingIo::new();
    let failure = adapter.materialize(requests, &mut io).unwrap_err();
    assert_eq!(
        unverified_code::<()>(Err(failure.error)),
        "codex-h3-no-owned-mount"
    );
    assert!(failure.receipt.resources.is_empty());
    assert_eq!(io.writes, 0);
}

#[test]
fn owned_mcp_file_is_one_exclusive_stdio_table() {
    assert_eq!(
        owned_mcp_toml("team_a", "/usr/bin/true").unwrap(),
        b"[mcp_servers.team_a]\ncommand = \"/usr/bin/true\"\n"
    );
    assert_eq!(
        owned_mcp_toml("team_orchestrator", "/usr/bin/true").unwrap(),
        b"[mcp_servers.team_orchestrator]\ncommand = \"/usr/bin/true\"\n"
    );
    for (key, command) in [
        ("", "/usr/bin/true"),
        ("team.a", "/usr/bin/true"),
        ("team_a", ""),
        ("team_a", "usr/bin/true"),
        ("team_a", "/usr/bin/tru\"e"),
        ("team_a", "/usr/bin/true\n"),
        ("team_a", "/usr/bin/true --flag"),
        ("team_a", "/usr/bin/true\t--flag"),
    ] {
        assert_eq!(
            unverified_code(owned_mcp_toml(key, command)),
            "codex-h3-no-owned-mount"
        );
    }

    assert_h3_unwritten(&CodexAdapter::new(), &[]);
    assert_h3_unwritten(&CodexAdapter::new().with_owned_mcp("team_a", "usr/bin/true"), &[]);

    let adapter = CodexAdapter::new().with_owned_mcp("team_orchestrator", "/usr/bin/true");
    let contents = owned_mcp_toml("team_orchestrator", "/usr/bin/true").unwrap();
    let mut with_args = contents.clone();
    with_args.extend(b"args = []\n");
    assert_h3_unwritten(
        &adapter,
        &[mcp_request("/tmp/codex-runtime", with_args)],
    );
    let mut with_env = contents.clone();
    with_env.extend(b"env = {}\n");
    assert_h3_unwritten(&adapter, &[mcp_request("/tmp/codex-runtime", with_env)]);
    assert_h3_unwritten(
        &adapter,
        &[
            mcp_request("/tmp/codex-runtime", contents.clone()),
            mcp_request("/tmp/codex-runtime", contents.clone()),
        ],
    );
    assert_h3_unwritten(
        &adapter,
        &[mcp_request("/Users/alauda/.codex", contents.clone())],
    );
    assert_h3_unwritten(
        &adapter,
        &[mcp_request("/tmp/home/.codex", contents.clone())],
    );

    let mut io = CountingIo::new();
    let receipt = adapter
        .materialize(
            &[mcp_request("/tmp/codex-runtime", contents.clone())],
            &mut io,
        )
        .unwrap();
    assert_eq!(io.writes, 1);
    assert_eq!(receipt.resources.len(), 1);
    let written = &receipt.resources[0];
    assert!(written.exclusive);
    assert_eq!(written.disposition, ResourceDisposition::OwnedRemovable);
    assert_eq!(written.kind, ResourceKind::McpConfig);
    assert_eq!(written.path.relative(), Path::new("codex/mcp.toml"));
    assert_eq!(written.path.root(), Path::new("/tmp/codex-runtime"));
    assert!(matches!(
        written.write_effect,
        ResourceWriteEffect::Written { bytes_sha256 }
            if bytes_sha256 == team_agent_contract::host::digest(&contents)
    ));

    let mounted = adapter.with_launch_images(measured_images(WRAPPER, ENGINE));
    let hooks = mounted.hooks();
    let resolved = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &phase1_request(Operation::Fresh, LaunchMode::LaunchOnly, None, false, None),
        None,
    )
    .expect("owned file does not change launch-only admission");
    let plan = mounted.plan(&resolved).expect("launch only argv stays closed");
    let arguments: Vec<_> = plan.arguments.iter().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(arguments, phase1_interactive_arguments());
    assert!(plan.materialization.is_empty());
    assert_eq!(plan.carriers.mcp, CarrierUse::NotRequested);

    let workspace_args = vec!["contract-mcp".to_string(), "/tmp/codex-phase1".to_string()];
    let with_args = owned_mcp_document("team_orchestrator", "/usr/bin/true", &workspace_args).unwrap();
    let rendered = std::str::from_utf8(&with_args).unwrap();
    assert!(rendered.contains("args = "));
    assert_eq!(
        owned_mcp_document("team_a", "/usr/bin/true", &[]).unwrap(),
        owned_mcp_toml("team_a", "/usr/bin/true").unwrap()
    );
    let arg_adapter = seated()
        .with_launch_images(measured_images(WRAPPER, ENGINE))
        .with_owned_mcp_args("team_orchestrator", "/usr/bin/true", workspace_args)
        .expect("contract-mcp args");
    let arg_plan = arg_adapter.plan(&resolved).expect("args stay out of argv");
    let arg_argv: Vec<_> = arg_plan.arguments.iter().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(arg_argv, phase1_interactive_arguments());
    assert_eq!(arg_argv.len(), 2);

    assert_h3_unwritten(&adapter, &[prompt_request("/tmp/codex-runtime", b"worker")]);
    assert_h3_unwritten(
        &adapter,
        &[
            prompt_request("/tmp/codex-runtime", b"worker"),
            mcp_request("/tmp/codex-runtime", contents.clone()),
        ],
    );
    assert_h3_unwritten(
        &adapter,
        &[
            mcp_request("/Users/alauda/.codex", contents.clone()),
            prompt_request("/Users/alauda/.codex", b"worker"),
        ],
    );
    let mut pair_io = CountingIo::new();
    let pair = adapter
        .materialize(
            &[
                mcp_request("/tmp/codex-runtime", contents.clone()),
                prompt_request("/tmp/codex-runtime", b"worker"),
            ],
            &mut pair_io,
        )
        .unwrap();
    assert_eq!(pair_io.writes, 2);
    assert_eq!(pair.resources.len(), 2);
    assert_eq!(pair.resources[1].kind, ResourceKind::Prompt);
    assert_eq!(pair.resources[1].path.relative(), Path::new("codex/prompt.txt"));
}

fn full_worker_request(model: Option<&str>, bypass: bool, prompt: Option<&str>) -> LaunchRequest {
    let mut request = phase1_request(Operation::Fresh, LaunchMode::FullWorker, model, bypass, prompt);
    request.input_profile = Some("codex-tui-single-enter-v1".into());
    request.native.harness = CATALOG_HARNESS.into();
    request.native.ui = CATALOG_UI.into();
    request
}

fn full_worker_adapter() -> CodexAdapter {
    seated()
        .with_launch_images(measured_images(WRAPPER, ENGINE))
        .with_owned_mcp("team_orchestrator", "/usr/bin/true")
}

#[test]
fn full_worker_fresh_plans_two_owned_files_and_the_same_argv() {
    let prompt = "worker-bytes";
    let adapter = full_worker_adapter();
    let hooks = adapter.hooks();
    let resolved = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &full_worker_request(None, false, Some(prompt)),
        None,
    )
    .expect("full worker fresh resolves");
    let plan = adapter.plan(&resolved).expect("full worker fresh plan");
    validate_launch_plan(&CODEX_DESCRIPTOR, &resolved, &plan).expect("plan shape");
    let arguments: Vec<_> = plan.arguments.iter().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(arguments, phase1_interactive_arguments());
    assert!(plan.executable_inputs.is_empty());
    assert!(plan.environment.remove.is_empty());
    assert!(plan.environment.set.is_empty());
    assert_eq!(plan.expected_session, ExpectedSession::CaptureAfterLaunch);
    assert_eq!(plan.executable, PathBuf::from(ENGINE));
    assert_eq!(
        plan.launcher.expect("wrapper").executable,
        PathBuf::from(WRAPPER)
    );
    assert_eq!(plan.materialization.len(), 2);
    assert_eq!(plan.materialization[0].path.relative(), Path::new("codex/mcp.toml"));
    assert_eq!(plan.materialization[1].path.relative(), Path::new("codex/prompt.txt"));
    assert_eq!(plan.materialization[0].path.root(), Path::new("/tmp/codex-phase1-runtime"));
    assert_eq!(plan.materialization[1].path.root(), plan.materialization[0].path.root());
    assert_eq!(plan.materialization[1].contents, prompt.as_bytes());
    assert_eq!(
        plan.materialization[0].contents,
        owned_mcp_toml("team_orchestrator", "/usr/bin/true").unwrap()
    );
    assert!(matches!(plan.carriers.model, CarrierUse::NotRequested));
    assert!(matches!(plan.carriers.bypass, CarrierUse::NotRequested));
    assert!(matches!(plan.carriers.effort, CarrierUse::NotRequested));
    assert!(matches!(
        &plan.carriers.mcp,
        CarrierUse::Specified(CarrierRef::Resource(path)) if path == &plan.materialization[0].path
    ));
    assert!(matches!(
        &plan.carriers.prompt,
        CarrierUse::Specified(CarrierRef::Resource(path)) if path == &plan.materialization[1].path
    ));

    let model = "opaque-seat-model";
    let resolved_model = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &full_worker_request(Some(model), false, Some(prompt)),
        None,
    )
    .expect("opaque id reaches plan");
    let model_err = adapter.plan(&resolved_model).expect_err("model stays off argv");
    assert!(!format!("{model_err:?}").contains(model));
    assert_eq!(
        unverified_code::<LaunchPlan>(Err(model_err)),
        "codex-model-selection-not-admitted"
    );

    let bypass = resolve_launch(
        &CODEX_DESCRIPTOR,
        &hooks,
        &full_worker_request(None, true, Some(prompt)),
        None,
    )
    .expect_err("bypass stays closed");
    assert_eq!(
        unverified_code::<LaunchPlan>(Err(bypass)),
        "codex-bypass-modal-interaction-not-run"
    );
}

#[test]
fn bundled_slugs_are_not_hardcoded() {
    let source = include_str!("../src/providers/codex.rs");
    for slug in [
        "gpt-6-astra",
        "gpt-6.1-sol",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-daybreak-blue-latest",
        "gpt-daybreak-red-latest",
        "codex-auto-review",
    ] {
        assert!(!source.contains(slug), "{slug}");
    }
    assert!(!source.contains("/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js"));
    assert!(!source.contains(ENGINE));
    assert!(!source.contains("recon_echo"));
    assert!(!source.contains("config.toml"));
}
