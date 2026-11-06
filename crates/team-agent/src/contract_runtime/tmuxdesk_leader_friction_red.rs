//! RED: desktop-workspace leader restore/reconfigure friction (2026-11-02/03).
//!
//! Both fixtures reproduce the on-disk shape the user's workspace reached after
//! `team restart --allow-fresh` recovered a purged `/tmp` authority:
//! - siblings were re-started natively from frozen state rows (`pi_tui`), while
//!   their editable role documents still say `provider: pi`;
//! - an explicitly stopped seat was skipped by recovery and kept its old
//!   `contract_instance`, which the recovered authority does not hold.
//!
//! Real provider discovery precedes route publication and recovery preflight,
//! so these tests drive the exact production seams with fixture authority:
//! R1 = the member routes computed for a NEW seat, published into an authority
//!      that holds a native sibling (`add-agent test-runner --provider cursor --force`
//!      refused with `FenceAt peers/<sha256("grok-dev")>`).
//! R3 = team stop and status on the orphaned explicit-stop seat
//!      (`contract runtime: Invalid("unknown native seat")`, status `unknown`).
use super::{
    backend::{self, Backend, Binding},
    config::RoleConfig,
    framework,
    prompt::Prompt,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use team_agent_contract::contract::{session::CwdIdentity, types::*};
use team_agent_contract::host::{digest, digest_hex, process::resolve_cwd};
use team_agent_contract::orchestration::store::*;

const TEAM: &str = "parity-audit-team";
/// sha256("grok-dev"): the entity named by the user's FenceAt (L42093).
const GROK_DEV_PEER: &str = "5d5493906157c4cce0d075f3cbe109d4f6d6c0484f86a666b908df7e37038f76";

struct RecoveredWorkspace {
    root: PathBuf,
    cwd: CwdIdentity,
    binding: Binding,
    team_dir: PathBuf,
}

impl RecoveredWorkspace {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ta-tmuxdesk-red-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let cwd = resolve_cwd(&root).unwrap();
        let scope = backend::scope_for(&cwd, TEAM).unwrap();
        let durable = backend::root_for(&cwd.path, &scope);
        std::fs::create_dir_all(&durable).unwrap();
        let binding = Binding {
            version: 1,
            workspace: cwd.clone(),
            team: TEAM.into(),
            scope: scope.clone(),
            endpoint: format!("framework:{}", scope.as_str()),
            candidate: "/fixture/team-agent".into(),
            candidate_sha256: digest(b"candidate"),
            tmux: "/fixture/tmux".into(),
            // Recovery publishes a fresh durable authority beside the archive.
            transport_root: durable.join("authority-recovered"),
        };
        drop(ProjectionStore::create(&binding.transport_root, scope, &binding.endpoint).unwrap());
        std::fs::write(
            durable.join("framework-binding.json"),
            serde_json::to_vec(&binding).unwrap(),
        )
        .unwrap();
        let team_dir = root.join(".team").join(TEAM);
        std::fs::create_dir_all(team_dir.join("agents")).unwrap();
        std::fs::write(
            team_dir.join("TEAM.md"),
            format!("---\nname: {TEAM}\n---\nFixture team.\n"),
        )
        .unwrap();
        Self {
            root,
            cwd,
            binding,
            team_dir,
        }
    }

    fn role_document(&self, id: &str, contents: &str) {
        std::fs::write(
            self.team_dir.join("agents").join(format!("{id}.md")),
            contents,
        )
        .unwrap();
    }

    fn identity(&self, seat: &str, instance: &str) -> InstanceIdentity {
        InstanceIdentity {
            scope: self.binding.scope.clone(),
            seat: SeatId::new(seat).unwrap(),
            instance: InstanceId::new(instance).unwrap(),
            generation: Generation(1),
        }
    }

    /// The frozen per-instance role snapshot recovery/start writes beside the authority.
    fn frozen_role(&self, identity: &InstanceIdentity, provider: &str, model: &str) {
        let body = format!("{} frozen prompt", identity.seat.as_str());
        let role = RoleConfig {
            id: identity.seat.clone(),
            provider: ProviderId::new(provider).unwrap(),
            source: self
                .team_dir
                .join("agents")
                .join(format!("{}.md", identity.seat.as_str())),
            role: identity.seat.as_str().into(),
            model: model.into(),
            effort: None,
            bypass: false,
            paused: false,
            body: body.clone(),
            prompt: Prompt {
                text: body.clone(),
                sha256: digest(body.as_bytes()),
                sources: vec![],
            },
        };
        backend::write_role(&self.binding, identity, &role).unwrap();
    }

    /// A committed native seat in the recovered authority, as stopped by `team stop`.
    fn native_seat(
        &self,
        seat: &str,
        instance: &str,
        provider: &str,
        model: &str,
        status: SeatStatus,
    ) {
        let identity = self.identity(seat, instance);
        self.frozen_role(&identity, provider, model);
        let target = SeatRecord {
            identity,
            provider: ProviderId::new(provider).unwrap(),
            native: NativeIdentity {
                version: "fixture".into(),
                harness: "fixture".into(),
                ui: "tui".into(),
                platform: Platform::MacOs,
                executable_sha256: digest(b"fixture"),
            },
            cwd: self.cwd.clone(),
            endpoint: self.binding.endpoint.clone(),
            pane: "%3".into(),
            binding_key: format!("binding-{seat}"),
            profile: "fixture".into(),
            server_key: "fixture".into(),
            evidence_kind: EvidenceKind::Fixture,
            status,
            process: None,
            physical: None,
            session: None,
            resources: vec![],
            workspace_resources: vec![],
            bootstrap_used: false,
        };
        let operation = OperationRecord {
            id: OperationId::new(format!("start-{instance}")).unwrap(),
            kind: TransactionKind::Startup,
            target: target.clone(),
            parent: None,
            phase: Phase::F5Receipt,
            pending: None,
            spawn_observation: None,
            native_launch: None,
            resources: vec![],
            effect: team_agent_contract::contract::delivery::DeliveryEffect::NoEffect,
            outcome: Outcome::Committed,
            preserved: vec![],
            failure: None,
            recovery_failure: None,
            stop_receipt: None,
        };
        let mut store = ProjectionStore::open(
            &self.binding.transport_root,
            self.binding.scope.clone(),
            &self.binding.endpoint,
        )
        .unwrap();
        store.fixture_put_seat(&target).unwrap();
        store.fixture_put_operation(&operation).unwrap();
    }

    fn contract_row(
        &self,
        provider: &str,
        model: &str,
        instance: &str,
        status: &str,
        explicit_stop: bool,
    ) -> Value {
        json!({"provider":provider,"contract_provider":provider,"runtime_family":"contract",
            "status":status,"paused":false,"explicit_stop":explicit_stop,
            "contract_scope":self.binding.scope.as_str(),"contract_instance":instance,"generation":1,
            "model":model,"owner_team_id":TEAM,"team_key":TEAM})
    }

    /// Team-scoped state as status/stop receive it, persisted in the runtime
    /// state file shape (active team record + `teams` map) for selector-based paths.
    fn state(&self, agents: Value) -> Value {
        let team = json!({"team_key":TEAM,"active_team_key":TEAM,
            "team_dir":self.team_dir,"workspace":self.root,"agents":agents});
        let mut file = team.clone();
        file["teams"] = json!({ TEAM: team.clone() });
        let runtime = self.root.join(".team").join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("state.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
        team
    }

    /// Peer cell as published in the projection: present only if its last value is present.
    fn peer_present(&self, recipient: &str) -> bool {
        let cell = self
            .binding
            .transport_root
            .join("projection/cells/peers")
            .join(format!("{}.json", digest_hex(digest(recipient.as_bytes()))));
        match std::fs::read(&cell) {
            Ok(bytes) => {
                serde_json::from_slice::<Value>(&bytes).unwrap()["value"]["state"] == "present"
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("unreadable peer cell {}: {error}", cell.display()),
        }
    }
}

impl Drop for RecoveredWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn workspace_path(fixture: &RecoveredWorkspace) -> &Path {
    &fixture.cwd.path
}

/// R1 (BUG-REPORT B4): adding a new contract seat to a recovered team must not
/// fence on, or re-route, a sibling that the authority already holds natively
/// just because that sibling's editable role document uses the `pi` alias.
#[test]
fn r1_new_seat_routes_do_not_fence_on_native_sibling_with_editable_pi_alias() {
    let fixture = RecoveredWorkspace::new("r1");
    assert_eq!(
        digest_hex(digest(b"grok-dev")),
        GROK_DEV_PEER,
        "fixture hash oracle"
    );
    // Retired document left in agents/ (sorts before grok-dev; no native seat).
    fixture.role_document(
        "astra-executor",
        "---\nname: astra-executor\nprovider: pi\nmodel: openai-codex/gpt-6-astra\n---\nRetired executor.\n",
    );
    // Same front matter as the user's grok-dev.md / grok-releaser.md.
    fixture.role_document(
        "grok-dev",
        "---\nname: grok-dev\nprovider: pi\nmodel: xai/grok-4.7\ndangerously_skip_permissions: false\n---\nNative client developer.",
    );
    fixture.role_document(
        "grok-releaser",
        "---\nname: grok-releaser\nprovider: pi\nmodel: xai/grok-4.7\ndangerously_skip_permissions: false\n---\nRelease packager.",
    );
    // The new seat as `add-agent test-runner --provider cursor --model grok-4.7-high --bypass true --force` writes it.
    fixture.role_document(
        "test-runner",
        "---\nname: test-runner\nprovider: cursor\nmodel: grok-4.7-high\ndangerously_skip_permissions: true\n---\nIndependent acceptance tester.",
    );
    // Recovery re-started both siblings natively from their frozen pi_tui rows; `team stop` stopped them.
    fixture.native_seat(
        "grok-dev",
        "instance-grok-dev-recovered",
        "pi_tui",
        "xai/grok-4.7",
        SeatStatus::Stopped,
    );
    fixture.native_seat(
        "grok-releaser",
        "instance-grok-releaser-recovered",
        "pi_tui",
        "xai/grok-4.7",
        SeatStatus::Stopped,
    );
    fixture.state(json!({
        "grok-dev": fixture.contract_row("pi_tui", "xai/grok-4.7", "instance-grok-dev-recovered", "stopped", true),
        "grok-releaser": fixture.contract_row("pi_tui", "xai/grok-4.7", "instance-grok-releaser-recovered", "stopped", true),
    }));

    // Same composition as framework::start_configured for the new seat.
    let config = framework::team_config(
        workspace_path(&fixture),
        &fixture.team_dir,
        TEAM,
        Some("test-runner"),
    )
    .expect("new cursor seat compiles")
    .expect("new cursor seat is a contract role");
    assert!(
        config
            .roles
            .iter()
            .any(|role| role.id.as_str() == "test-runner"),
        "fixture precondition"
    );
    let mut authority =
        Backend::open(workspace_path(&fixture), TEAM).expect("recovered authority opens");
    let published = backend::publish_framework_routes(&mut authority, &config.members);
    drop(authority);

    assert!(
        published.is_ok(),
        "R1: publishing the new seat's routes must not fence on a natively held sibling; got: {}",
        published
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    for native in ["grok-dev", "grok-releaser"] {
        assert!(
            !fixture.peer_present(native),
            "R1: natively held {native} must not be re-published as a framework (legacy) peer"
        );
    }
    assert!(
        fixture.peer_present("leader"),
        "R1: the leader route must still be published"
    );
}

/// R3 fixture (BUG-REPORT B5): an explicitly stopped contract seat that fresh
/// recovery skipped keeps its saved instance, which the recovered authority
/// does not hold; its recovered sibling is present and stopped.
fn recovery_skipped_explicit_stop(label: &str) -> (RecoveredWorkspace, Value) {
    let fixture = RecoveredWorkspace::new(label);
    fixture.role_document(
        "grok-dev",
        "---\nname: grok-dev\nprovider: pi\nmodel: xai/grok-4.7\ndangerously_skip_permissions: false\n---\nGrok developer.",
    );
    fixture.role_document(
        "opus-architect",
        "---\nname: opus-architect\nprovider: claude\nmodel: claude-opus-5-5\neffort: xhigh\ndangerously_skip_permissions: true\n---\n# opus-architect\nArchitect.",
    );
    // Recovered sibling: present in the new authority, stopped by `team stop`.
    fixture.native_seat(
        "grok-dev",
        "instance-grok-dev-recovered",
        "pi_tui",
        "xai/grok-4.7",
        SeatStatus::Stopped,
    );
    // Skipped seat: only its pre-recovery frozen snapshot exists; no seat in the new authority.
    let old = fixture.identity("opus-architect", "instance-pre-recovery");
    fixture.frozen_role(&old, "claude_tui", "claude-opus-5-5");
    let mut orphan = fixture.contract_row(
        "claude_tui",
        "claude-opus-5-5",
        "instance-pre-recovery",
        "stopped",
        true,
    );
    orphan["effort"] = json!("xhigh");
    orphan["native_endpoint"] = json!("/private/tmp/tmux-501/ta-fixture-dead");
    orphan["native_pane"] = json!("%4");
    let state = fixture.state(json!({
        "grok-dev": fixture.contract_row("pi_tui", "xai/grok-4.7", "instance-grok-dev-recovered", "stopped", true),
        "opus-architect": orphan,
    }));
    (fixture, state)
}

/// R3a: team stop must treat the recovery-skipped explicit-stop seat as stopped.
#[test]
fn r3a_team_stop_treats_recovery_skipped_explicit_stop_seat_as_stopped() {
    let (fixture, state) = recovery_skipped_explicit_stop("r3a");
    let receipts =
        framework::stop_selected_with_budget(workspace_path(&fixture), &state, std::time::Duration::from_secs(20)).expect("team stop runs");
    let receipt = |id: &str| {
        receipts
            .iter()
            .find(|receipt| receipt["agent_id"] == id)
            .unwrap_or_else(|| panic!("no stop receipt for {id}: {receipts:?}"))
            .clone()
    };
    assert_eq!(
        receipt("grok-dev")["status"],
        "already_stopped",
        "control: recovered sibling"
    );
    let orphan_stop = receipt("opus-architect");
    assert!(
        orphan_stop["status"] == "already_stopped" || orphan_stop["status"] == "stopped",
        "R3: team stop must treat the recovery-skipped explicit-stop seat as stopped; got {orphan_stop}"
    );
    assert_ne!(
        orphan_stop["needs_recovery"], true,
        "R3: no recovery demand for a seat that was never re-started: {orphan_stop}"
    );
}

/// R3b: status must report the recovery-skipped explicit-stop seat as stopped.
#[test]
fn r3b_status_reports_recovery_skipped_explicit_stop_seat_as_stopped() {
    let (fixture, state) = recovery_skipped_explicit_stop("r3b");
    let status = crate::cli::status_port::status_brief_scoped(
        workspace_path(&fixture),
        &state,
        Some("opus-architect"),
    );
    let node = &status["nodes"][0];
    assert_eq!(
        node["runtime_status"], "stopped",
        "R3: status must report the explicit-stop seat as stopped, not unknown; got {node}"
    );
    assert_ne!(
        node["observation_reason"], "native_runtime_projection_unavailable",
        "R3: a skipped seat is not an unavailable projection: {node}"
    );
}
