//! purpose: lock the phase-1 Codex descriptor admission for Issue #325
//! contract: fifteen facets and seven hooks match CODEX-DESCRIPTOR-TASKBOOK.md
//! boundary: red-track artifact; not on the crate path until the tester copies it.
//!           A missing `providers::codex` module is the expected red compile.
//!           Green means the admission table matches. It is not G1–G5 acceptance.

use std::path::PathBuf;
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
    may_signal, phase1_interactive_arguments, seat_pair_ok, CodexAdapter, CodexImageRole,
    CODEX_DESCRIPTOR,
};

const V7: &str = "018f6b6a-7c4e-7a1b-8c2d-111111111111";
const V4: &str = "018f6b6a-7c4e-4a1b-8c2d-111111111111";
const ROLL_OK: &str = "rollout-2026-10-14T05-29-00-018f6b6a-7c4e-7a1b-8c2d-111111111111.jsonl";
const WRAPPER: &str = "/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js";
const ENGINE: &str = "/opt/homebrew/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex";
const DAEMON: &str = "/Users/alauda/.codex/packages/app-server-daemon/releases/0.162.1-aarch64-apple-darwin/bin/codex";
const SHADOW: &str = "/Users/alauda/.local/bin/codex";

fn code_of(result: Result<&Reason, ContractError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected Unverified or Reject"),
        Err(ContractError::Unverified { reason, .. } | ContractError::EffortRejected(reason)) => {
            reason.code
        }
        Err(other) => panic!("unexpected {other:?}"),
    }
}

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
    CodexAdapter
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
        unverified_code(d.model.selection.require("model selection")),
        "codex-model-catalog-not-run"
    );
    assert_eq!(d.model.omitted, OmittedModel::Reject);
    assert_eq!(
        unverified_code(d.model.catalog.require("catalog")),
        "codex-model-catalog-not-run"
    );

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
        unverified_code(d.prompt.carrier.require("prompt")),
        "codex-prompt-bytes-not-observed"
    );

    assert_eq!(
        unverified_code(d.mcp.carrier.require("mcp")),
        "codex-mcp-scoped-mount-not-proven"
    );
    assert_eq!(d.mcp.scope, ResourceScope::RuntimeRoot);
    assert!(d.mcp.tools.is_empty());
    assert_eq!(d.mcp.isolation, McpIsolation::Unverified);
    assert_eq!(
        unverified_code(d.tool_names.naming.require("tool naming")),
        "codex-tool-names-not-observed"
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

    assert_eq!(
        unverified_code(d.input.profiles.require("input profiles")),
        "codex-submit-not-run"
    );
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
        kind: ResourceKind::GlobalSettings,
        disposition: ResourceDisposition::Forbidden,
    }));
    assert!(teardown.contains(&ResourcePolicy {
        kind: ResourceKind::NativeDatabase,
        disposition: ResourceDisposition::Forbidden,
    }));
    assert_eq!(teardown.len(), 3);
}

#[test]
fn seven_hooks_bind_only_plan_session_and_interaction() {
    let adapter = seated();
    let hooks = adapter.hooks();
    assert_eq!(
        unverified_code(hooks.catalog.require("H1 CatalogHook")),
        "codex-h1-catalog-not-run"
    );
    assert!(hooks.plan.require("H2 PlanHook").is_ok());
    assert_eq!(
        unverified_code(hooks.materialize.require("H3 MaterializeHook")),
        "codex-h3-no-owned-mount"
    );
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
fn fresh_descriptor_validates_and_launch_stays_closed() {
    let adapter = seated();
    let hooks = adapter.hooks();
    validate_descriptor(&CODEX_DESCRIPTOR, &hooks, Operation::Fresh).unwrap();
    validate_descriptor(&CODEX_DESCRIPTOR, &hooks, Operation::Resume).unwrap();

    let catalog = validate_descriptor(&CODEX_DESCRIPTOR, &hooks, Operation::Catalog).unwrap_err();
    assert_eq!(unverified_code::<()>(Err(catalog)), "codex-model-catalog-not-run");
    for operation in [
        Operation::InWindowBranch,
        Operation::NewSeatFullSnapshot,
        Operation::NativeNewSeat,
    ] {
        let err = validate_descriptor(&CODEX_DESCRIPTOR, &hooks, operation).unwrap_err();
        assert_eq!(unverified_code::<()>(Err(err)), "codex-fork-not-run");
    }

    for operation in [Operation::Fresh, Operation::Resume] {
        let res = resolve_launch(
            &CODEX_DESCRIPTOR,
            &hooks,
            &LaunchRequest {
                provider: "codex".into(),
                operation,
                mode: LaunchMode::LaunchOnly,
                auth: AuthMode::NativeSubscription,
                model: None,
                role_effort: None,
                team_effort: None,
                bypass: false,
                prompt: None,
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
            },
            None,
        );
        let err = match res {
            Err(err) => err,
            Ok(_) => panic!("expected Err, got Ok"),
        };
        assert_eq!(code_of(Err(err)), "codex-model-catalog-not-run");
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
    assert!(argv_mutates_global_config(&[
        "codex", "mcp", "add", "team", "--", "/usr/bin/true"
    ]));
    assert!(!argv_mutates_global_config(&["codex", "mcp", "list"]));
    assert!(!argv_mutates_global_config(phase1_interactive_arguments()));
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
