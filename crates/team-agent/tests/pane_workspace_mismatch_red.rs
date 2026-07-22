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
/// The LIVE pane-binding-nonce seam (runtime-owner-b msg_859e769113f8): the tmux
/// pane option `@team_agent_pane_binding_nonce` is projected, in the same
/// list-panes frame, into the existing `PaneInfo.leader_env` under this key. It is
/// baseline-compatible identity metadata (NOT a process env), read at delivery
/// from `list_targets`. This lets the offline fixture supply a LIVE nonce distinct
/// from the receiver's recorded nonce — closing the WLEAK false-green hole where a
/// weak impl could pass form1 on `receiver.binding_nonce` alone.
const PANE_BINDING_NONCE_KEY: &str = "TEAM_AGENT_PANE_BINDING_NONCE";

/// A mock transport whose single leader pane reports a configurable
/// `current_path`, endpoint, AND live pane-binding-nonce (via leader_env), so
/// `resolve_live_leader_channel` exercises the path/nonce branch deterministically.
struct PaneTransport {
    endpoint: String,
    pane_id: String,
    current_path: PathBuf,
    live_nonce: Option<String>,
}

impl PaneTransport {
    fn new(current_path: &Path) -> Self {
        Self {
            endpoint: SOCKET.to_string(),
            pane_id: PANE.to_string(),
            current_path: current_path.to_path_buf(),
            live_nonce: None,
        }
    }

    /// Set the LIVE pane-scoped binding nonce projected into leader_env (the
    /// value the product reads from the live pane option, not from state).
    fn with_live_nonce(mut self, nonce: &str) -> Self {
        self.live_nonce = Some(nonce.to_string());
        self
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
        let mut leader_env = BTreeMap::new();
        if let Some(nonce) = &self.live_nonce {
            leader_env.insert(PANE_BINDING_NONCE_KEY.to_string(), nonce.clone());
        }
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
            leader_env,
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
    // The LIVE pane nonce (from the pane option, projected into leader_env) MATCHES
    // the receiver's recorded nonce — this is a genuine authorized instance.
    let transport = PaneTransport::new(&sibling_cwd).with_live_nonce("nonce-match-abc");
    let receiver = claimed_receiver("explicit_claim", Some("nonce-match-abc"));

    let res = resolve_live_leader_channel(&workspace, &receiver, &transport);
    assert!(
        is_live(&res),
        "an explicitly-claimed sibling-workspace pane whose LIVE pane-binding-nonce matches the \
         receiver's recorded nonce must resolve Live (both directions deliver); baseline refuses \
         PaneWorkspaceMismatch because resolve has no explicit_claim+nonce branch (locate §8.6.1). \
         res={res:?}"
    );
}

/// FORM 2 — POSITIVE CONTROL / WLEAK tooth: a sibling pane whose LIVE
/// pane-binding-nonce is MISSING or DIFFERS from the receiver's recorded nonce
/// must remain refused. This is the tooth a weak fix that passes form1 on
/// `receiver.binding_nonce` alone would fail: the resolver must compare the LIVE
/// pane option (leader_env seam), not the receiver's self-claim. Must stay refused
/// across the fix (guards blanket-accept of siblings).
#[test]
fn form2_sibling_with_missing_or_wrong_live_nonce_stays_refused() {
    let workspace = PathBuf::from("/private/tmp/pwm-workspace-a");
    let sibling_cwd = PathBuf::from("/private/tmp/pwm-workspace-b");
    let receiver = claimed_receiver("explicit_claim", Some("nonce-match-abc"));

    // (a) LIVE nonce DIFFERS from the receiver's recorded nonce (recycled %1).
    let wrong_live = PaneTransport::new(&sibling_cwd).with_live_nonce("nonce-DIFFERENT-recycled");
    assert!(
        is_mismatch(&resolve_live_leader_channel(
            &workspace,
            &receiver,
            &wrong_live
        )),
        "positive control (WLEAK): a sibling whose LIVE pane-binding-nonce DIFFERS from the \
         receiver's recorded nonce (recycled %1) must remain PaneWorkspaceMismatch; a fix must \
         compare the live pane option, not blanket-accept on receiver.binding_nonce."
    );

    // (b) LIVE nonce MISSING (no pane option projected).
    let missing_live = PaneTransport::new(&sibling_cwd);
    assert!(
        is_mismatch(&resolve_live_leader_channel(
            &workspace,
            &receiver,
            &missing_live
        )),
        "positive control (WLEAK): a sibling with NO live binding nonce must remain \
         PaneWorkspaceMismatch; a fix must not accept a sibling lacking a matching live nonce."
    );
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
    // The live pane nonce matches the receiver's recorded nonce — a genuine,
    // delivery-usable authorized claim. The resolver must honor it.
    let transport = PaneTransport::new(&sibling_cwd).with_live_nonce("nonce-match-abc");
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
/// and the fallback must not bypass it. DOUBLE structural guard, corrected per
/// runtime-owner-b (msg_847fe346a8e0): the TRUE fallback physical-inject primitive
/// is `leader_receiver.rs::deliver_to_leader_fallback_pane` (not results.rs, which
/// is only the caller). Guarding results.rs would false-red and push duplicate
/// logic into the caller.
///   Lock A — the fallback PRIMITIVE resolves its channel through
///     `resolve_live_leader_channel` (the same typed DirectTmux channel), not a
///     state-rebuilt `leader_pane_id`+`Target::Pane` inject.
///   Lock B — the results.rs CALLER only invokes that single helper; it does not
///     build its own pane/socket inject.
/// Baseline red: `deliver_to_leader_fallback_pane` reads `leader_pane_id(state)`
/// and injects `Target::Pane` directly (leader_receiver.rs:206-300), bypassing the
/// resolver — a missing/mismatched nonce refuses primary yet slips through
/// fallback.
#[test]
fn form5_fallback_primitive_consumes_same_resolver_and_caller_only_delegates() {
    // Lock A: the fallback primitive must route through the shared resolver.
    let primitive = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/leader_receiver.rs"
    );
    let ptext = std::fs::read_to_string(primitive).expect("read leader_receiver.rs");
    let primitive_uses_resolver =
        ptext.contains("resolve_live_leader_channel") || ptext.contains("LiveLeaderChannel");
    assert!(
        primitive_uses_resolver,
        "Lock A: the fallback physical-inject primitive \
         `deliver_to_leader_fallback_pane` (leader_receiver.rs) must resolve its channel through \
         resolve_live_leader_channel (same typed DirectTmux channel), not a state-rebuilt \
         leader_pane_id + Target::Pane inject; else a missing/mismatched nonce refused on primary \
         slips through fallback (runtime-owner-b boundary; locate §8.6.5)."
    );

    // Lock B: the results.rs caller only delegates to that single helper (no
    // self-built pane/socket inject). Baseline already delegates; this guards
    // against a "fix" that duplicates inject logic into the caller.
    let caller = concat!(env!("CARGO_MANIFEST_DIR"), "/src/messaging/results.rs");
    let ctext = std::fs::read_to_string(caller).expect("read results.rs");
    let delegates = ctext.contains("deliver_to_leader_fallback_pane");
    // The caller must NOT itself build a raw pane inject target for the fallback.
    let caller_builds_own_inject = ctext.contains("Target::Pane") && ctext.contains("inject(");
    assert!(
        delegates && !caller_builds_own_inject,
        "Lock B: results.rs must delegate the fallback to the single \
         deliver_to_leader_fallback_pane primitive and must not build its own pane/socket inject \
         (no duplicated fallback logic in the caller)."
    );
}
