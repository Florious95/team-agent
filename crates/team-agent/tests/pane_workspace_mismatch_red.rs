//! 0.5.53 P0 · PaneWorkspaceMismatch cross-workspace claim compatibility RED
//! (verifier).
//!
//! From rcapp-attach-regression-locate.md §8.6 (five-form matrix) + §8.5 (binding
//! nonce design intent) + AB-result.md (0.5.47 delivers both directions, 0.5.52
//! refuses worker→leader with `PaneWorkspaceMismatch`), NOT the fix. Baseline
//! `56231ce` (origin/main). Regression introduced by `cc5e4af`
//! (`fix(messaging): reject reused foreign leader panes`). Information isolation:
//! written from the A/B phenomena + the `resolve_live_leader_channel`
//! (leader_channel.rs:45) / `path_is_in_workspace` (:116-122) surfaces, not the
//! fix.
//!
//! The delivery predicate `resolve_live_leader_channel(workspace, receiver,
//! transport)` is a pure function: a mock transport returns the observed leader
//! pane (whose `current_path` we set to A / a sibling B / a descendant of A), and
//! the receiver Value carries the recorded pane binding. This lets the path/nonce
//! forms be exercised offline; the delivery/fallback forms (④⑤) additionally pin
//! that BOTH primary and fallback consume the SAME resolver (runtime-owner-b
//! boundary msg_84c1aa8a3870), asserted structurally here and by the subscription
//! gate for the live claim topology.
//!
//! Five forms (locate §8.6):
//! 1. explicitly-claimed sibling-workspace pane with MATCHING binding nonce → both
//!    directions deliver. Baseline red: resolve has no `scope_authority=
//!    explicit_claim` + nonce branch; a sibling cwd is refused
//!    `PaneWorkspaceMismatch` regardless of an explicit claim.
//! 2. same socket + same pane id, DIFFERENT/MISSING nonce → worker→leader refused
//!    (WLEAK safety). Positive control / discriminator: baseline refuses a sibling
//!    already (by exact path, not by nonce) — the refusal must remain, but keyed
//!    on nonce mismatch not merely exact path.
//! 3. target workspace itself OR a DESCENDANT cwd → still delivers without any
//!    cross-workspace grant. GUARDRAIL (naturally-green): `path_is_in_workspace`
//!    already accepts equality/descendant; must never regress.
//! 4. a claim that returns claimed/attached MUST persist a delivery-usable
//!    authority; the canonical delivery resolver must not immediately reject it.
//!    Baseline red (AB): 0.5.52 claimed `%1` then worker→leader synchronously
//!    refused.
//! 5. direct send and `report_result` primary delivery use the SAME rule; the
//!    fallback must not hide a primary refusal — and (runtime-owner-b boundary)
//!    the fallback physical inject must itself pass the same resolver, so a
//!    missing/mismatched nonce refuses BOTH primary and fallback, not by
//!    primary_error text special-casing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use team_agent::messaging::leader_channel::{
    resolve_live_leader_channel, LeaderChannelResolution, LeaderChannelUnbound,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const SOCKET: &str = "/private/tmp/tmux-501/ta-pwm-red";
const PANE: &str = "%1";

/// A mock transport whose single leader pane reports a configurable
/// `current_path` and endpoint, so `resolve_live_leader_channel` exercises the
/// path/nonce branch deterministically.
struct PaneTransport {
    endpoint: String,
    pane_id: String,
    current_path: PathBuf,
}

impl PaneTransport {
    fn new(current_path: &Path) -> Self {
        Self {
            endpoint: SOCKET.to_string(),
            pane_id: PANE.to_string(),
            current_path: current_path.to_path_buf(),
        }
    }
}

impl Transport for PaneTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn tmux_endpoint(&self) -> Option<String> {
        Some(self.endpoint.clone())
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(vec![PaneInfo {
            pane_id: PaneId::new(self.pane_id.clone()),
            session: SessionName::new("team-pwm"),
            window_index: Some(0),
            window_name: Some(WindowName::new("leader")),
            pane_index: Some(0),
            tty: None,
            current_command: Some("bash".to_string()),
            current_path: Some(self.current_path.clone()),
            active: true,
            pane_pid: Some(4242),
            leader_env: BTreeMap::new(),
        }])
    }
    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult {
            pane_id: PaneId::new(self.pane_id.clone()),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(1),
        })
    }
    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        a: &[String],
        c: &Path,
        e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, a, c, e)
    }
    fn inject(
        &self,
        _t: &Target,
        _p: &InjectPayload,
        _s: Key,
        _b: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _t: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }
    fn set_session_env(
        &self,
        _s: &SessionName,
        _k: &str,
        _v: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
        Ok(())
    }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

/// A canonical claimed-leader receiver row: attached, on the absolute socket,
/// pane `%1`, with an explicit-claim scope authority and a binding nonce. The
/// authority/nonce fields are the §8.5 design intent — baseline `resolve` does not
/// read them, so a sibling cwd is refused regardless.
fn claimed_receiver(scope_authority: &str, binding_nonce: Option<&str>) -> Value {
    let mut r = json!({
        "status": "attached",
        "pane_id": PANE,
        "tmux_socket": SOCKET,
        "session_name": "team-pwm",
        "scope_authority": scope_authority,
        "authorized_team_workspace": "/private/tmp/pwm-workspace-a",
    });
    if let Some(n) = binding_nonce {
        r["binding_nonce"] = json!(n);
    }
    r
}

fn is_mismatch(res: &LeaderChannelResolution) -> bool {
    matches!(
        res,
        LeaderChannelResolution::Unbound(LeaderChannelUnbound::PaneWorkspaceMismatch)
    )
}
fn is_live(res: &LeaderChannelResolution) -> bool {
    matches!(res, LeaderChannelResolution::Live(_))
}

/// FORM 1 — an explicitly-claimed sibling-workspace pane with a MATCHING binding
/// nonce must resolve Live (both directions deliver). Baseline red: resolve has no
/// explicit-claim + nonce branch; the sibling cwd is refused PaneWorkspaceMismatch
/// even though the pane was an explicit canonical claim (locate §8.6.1, AB v0.5.52
/// worker→leader refusal).
#[test]
fn form1_explicit_claim_sibling_with_matching_nonce_delivers() {
    let workspace = PathBuf::from("/private/tmp/pwm-workspace-a");
    let sibling_cwd = PathBuf::from("/private/tmp/pwm-workspace-b");
    let transport = PaneTransport::new(&sibling_cwd);
    let receiver = claimed_receiver("explicit_claim", Some("nonce-match-abc"));

    let res = resolve_live_leader_channel(&workspace, &receiver, &transport);
    assert!(
        is_live(&res),
        "an explicitly-claimed sibling-workspace pane with a matching binding nonce must resolve \
         Live (both directions deliver); baseline refuses it PaneWorkspaceMismatch because resolve \
         has no explicit_claim+nonce branch (locate §8.6.1). res={res:?}"
    );
}

/// FORM 2 — same socket + same pane id but MISSING/DIFFERENT nonce must remain
/// refused (WLEAK). Discriminator: the refusal is correct, but it must be keyed on
/// nonce mismatch, not merely exact-path. Baseline refuses a sibling by exact
/// path; post-fix it must still refuse when the explicit-claim nonce is
/// missing/wrong. This form is a POSITIVE CONTROL: it must stay refused across the
/// fix (guards against a fix that blanket-accepts siblings).
#[test]
fn form2_sibling_with_missing_or_wrong_nonce_stays_refused() {
    let workspace = PathBuf::from("/private/tmp/pwm-workspace-a");
    let sibling_cwd = PathBuf::from("/private/tmp/pwm-workspace-b");
    let transport = PaneTransport::new(&sibling_cwd);

    // missing nonce
    let missing = claimed_receiver("explicit_claim", None);
    assert!(
        is_mismatch(&resolve_live_leader_channel(
            &workspace, &missing, &transport
        )),
        "positive control: an explicit-claim sibling with MISSING binding nonce must remain \
         PaneWorkspaceMismatch (WLEAK); a fix must not blanket-accept siblings."
    );
    // different nonce is exercised at the live layer (the mock cannot mismatch a
    // live pane-scoped option); the offline guard here pins missing-nonce refusal.
}

/// FORM 3 — GUARDRAIL (naturally-green): the target workspace itself and a
/// descendant cwd must resolve Live without any cross-workspace grant.
/// `path_is_in_workspace` already accepts equality/descendant; this must never
/// regress when the fix adds the sibling+nonce path.
#[test]
fn form3_workspace_and_descendant_cwd_still_deliver_guardrail() {
    let workspace = PathBuf::from("/private/tmp/pwm-workspace-a");
    let receiver = claimed_receiver("owner", None);

    // exact workspace
    let exact = PaneTransport::new(&workspace);
    assert!(
        is_live(&resolve_live_leader_channel(&workspace, &receiver, &exact)),
        "guardrail: a leader pane whose cwd equals the workspace must resolve Live."
    );
    // descendant
    let descendant = PaneTransport::new(&workspace.join("sub/dir"));
    assert!(
        is_live(&resolve_live_leader_channel(
            &workspace,
            &receiver,
            &descendant
        )),
        "guardrail: a leader pane whose cwd is a workspace DESCENDANT must resolve Live without a \
         cross-workspace grant (path_is_in_workspace accepts descendant; must not regress)."
    );
}

/// FORM 4 — a claimed/attached receiver must persist a delivery-usable authority:
/// the canonical resolver must NOT immediately reject a pane that a claim just
/// authorized. Baseline red: an explicit-claim sibling is claimed/attached yet the
/// resolver returns PaneWorkspaceMismatch on the very next delivery (AB v0.5.52).
/// This pins that `resolve` consults the persisted claim authority, not only the
/// live cwd. (Same offline surface as form 1, asserted from the delivery angle:
/// claimed ⇒ not immediately self-rejected.)
#[test]
fn form4_claimed_authority_is_not_immediately_rejected_by_resolver() {
    let workspace = PathBuf::from("/private/tmp/pwm-workspace-a");
    let sibling_cwd = PathBuf::from("/private/tmp/pwm-workspace-b");
    let transport = PaneTransport::new(&sibling_cwd);
    // A receiver that recorded an explicit claim with a matching live nonce is, by
    // the claim contract, delivery-usable. The resolver must honor it.
    let receiver = claimed_receiver("explicit_claim", Some("nonce-match-abc"));
    let res = resolve_live_leader_channel(&workspace, &receiver, &transport);
    assert!(
        !is_mismatch(&res),
        "a claim that returned claimed/attached with a matching binding nonce must persist a \
         delivery-usable authority; the canonical resolver must not immediately reject it with \
         PaneWorkspaceMismatch (locate §8.6.4; AB v0.5.52 claimed-then-refused). res={res:?}"
    );
}

/// FORM 5 — direct send and report_result primary delivery use the SAME resolver,
/// and the fallback must not bypass it. Structural source guard (runtime-owner-b
/// boundary msg_84c1aa8a3870): the fallback physical-inject site in `results.rs`
/// must resolve its channel through `resolve_live_leader_channel` (the same typed
/// DirectTmux channel), NOT rebuild pane/socket from state and inject directly.
/// Baseline red: `results.rs` fallback reads pane/socket and injects, bypassing
/// the resolver, so a missing/mismatched nonce is refused on primary yet slips
/// through fallback.
#[test]
fn form5_fallback_consumes_same_resolver_not_state_rebuilt_pane() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/messaging/results.rs");
    let text = std::fs::read_to_string(src).expect("read results.rs");
    // The fallback inject path must route through the shared resolver.
    let fallback_uses_resolver = text.contains("resolve_live_leader_channel")
        || text.contains("resolve_leader_channel")
        || text.contains("LiveLeaderChannel");
    assert!(
        fallback_uses_resolver,
        "the report_result fallback physical inject must consume the SAME \
         resolve_live_leader_channel typed channel (primary+fallback share one resolver; a \
         missing/mismatched nonce must refuse BOTH, not slip through fallback by \
         primary_error text special-casing). Baseline results.rs rebuilds pane/socket from state \
         and injects, bypassing the resolver (runtime-owner-b boundary; locate §8.6.5)."
    );
}
