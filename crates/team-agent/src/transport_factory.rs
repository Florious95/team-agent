//! 0.5.x Phase 1d Batch 0: backend assembly factory.
//!
//! Truth sources (READ-ONLY):
//! - Design:   `.team/artifacts/0.5.x-backend-assembly-factory-design.md`
//! - CR verdict: `.team/artifacts/0.5.x-backend-factory-cr-verdict.md`
//!               (6 constraints; every one anchors into this module)
//!
//! ## Scope of Batch 0 (this commit)
//!
//! - Add the factory type + resolver ONLY. Zero caller migration.
//! - Ship the 5 independent tests the design lists (§Batch 0
//!   Verification) + the C-1 grep guard (resolver has no
//!   `if conpty|pty { .. tmux .. }` fallback branch) + the C-5 grep
//!   guard (this file must never call `kill_server`, emit stale
//!   events, or start a shim).
//! - C-6 gate: Batch 0 tests + guards must be GREEN before Batch 1
//!   moves callers. This commit is the Batch 0 half; the very next
//!   commit is Batch 1.
//!
//! ## CR anchors
//!
//! - **C-1** fail-closed × 2:
//!     ① explicit `conpty` on an unsupported host / missing shim
//!        surfaces `MuxUnavailable` — NEVER silent-downgrade to tmux.
//!     ② `spec.runtime.backend = "pty"` returns
//!        `UnsupportedSpecBackendLiteral` — NEVER auto-map ConPTY.
//! - **C-2** CLI override vs `state.transport.kind`: if both are set
//!   and disagree, the factory REFUSES with `BackendConflict` (unless
//!   fresh_switch is explicitly true — the caller path threads this
//!   through when the user passes `--fresh`).
//! - **C-3** two-source conflict N38 events: when the resolver chooses
//!   a backend and there's a shadow source that was ignored, the
//!   `ResolvedTransport.notices` list carries structured entries the
//!   caller emits into `events.jsonl`. Emission happens at the caller
//!   boundary (not here) because the factory itself must have zero
//!   side effects (C-5).
//! - **C-4** compact-status JSON stability is a separate grep-guard
//!   test (`transport_factory_compact_status_guard.rs`); this module
//!   just declares which fields are `--detail`-only.
//! - **C-5** factory purity: this file NEVER calls
//!   `transport.kill_server(...)`, NEVER writes to `event_log`, NEVER
//!   opens a pipe, NEVER starts a shim, NEVER rotates a token. Guard
//!   test greps this file for those calls.
//! - **C-6** Batch 0 gate: the 5 core resolver tests + both C-1 grep
//!   guards + the C-5 grep guard must pass before Batch 1's caller
//!   migration begins. Enforced by CI + local test order.

use std::path::Path;

use crate::conpty::ConPtyBackend;
use crate::tmux_backend::TmuxBackend;
use crate::transport::{BackendKind, Transport};

/// User-visible backend selector on the CLI (`--backend tmux|conpty`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedTransportBackend {
    Tmux,
    ConPty,
}

impl RequestedTransportBackend {
    pub fn parse_literal(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tmux" => Some(Self::Tmux),
            "conpty" => Some(Self::ConPty),
            _ => None,
        }
    }

    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::ConPty => "conpty",
        }
    }
}

/// Which lifecycle site is asking the factory to resolve a transport.
/// Passed through so events + coordinator boot metadata can attribute
/// which caller made the choice. This has no effect on resolution
/// itself; it exists only for observability + C-3 event payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPurpose {
    Launch,
    LifecycleWorker,
    Coordinator,
    Status,
    Diagnose,
    Shutdown,
    MessageDelivery,
}

/// Factory input. Every field except `workspace` and `purpose` is
/// optional; the resolution order (§Factory Design:141-149) picks the
/// first source that fires.
///
/// The lifetimes tie each borrowed value to the caller so the factory
/// is a pure function of its inputs.
#[derive(Debug, Clone, Copy)]
pub struct TransportFactoryInput<'a> {
    pub workspace: &'a Path,
    pub team_key: Option<&'a str>,
    pub state: Option<&'a serde_json::Value>,
    pub spec_backend_literal: Option<&'a str>,
    pub explicit_backend: Option<RequestedTransportBackend>,
    pub purpose: TransportPurpose,
    /// True only when the caller has confirmed the switch (via
    /// `--fresh` or a similar explicit gate). Used to allow C-2's
    /// otherwise-refuse path.
    pub allow_backend_switch: bool,
}

impl<'a> TransportFactoryInput<'a> {
    pub fn new(workspace: &'a Path, purpose: TransportPurpose) -> Self {
        Self {
            workspace,
            team_key: None,
            state: None,
            spec_backend_literal: None,
            explicit_backend: None,
            purpose,
            allow_backend_switch: false,
        }
    }

    pub fn with_team_key(mut self, team_key: Option<&'a str>) -> Self {
        self.team_key = team_key;
        self
    }

    pub fn with_state(mut self, state: Option<&'a serde_json::Value>) -> Self {
        self.state = state;
        self
    }

    pub fn with_spec_backend_literal(mut self, literal: Option<&'a str>) -> Self {
        self.spec_backend_literal = literal;
        self
    }

    pub fn with_explicit_backend(mut self, override_: Option<RequestedTransportBackend>) -> Self {
        self.explicit_backend = override_;
        self
    }

    pub fn allow_backend_switch(mut self, allow: bool) -> Self {
        self.allow_backend_switch = allow;
        self
    }
}

/// One-line trace of what the factory resolved and why. Emitted by the
/// caller (never by the factory itself — C-5) as
/// `transport.backend_selected`.
///
/// The `source` field is a stable wire string so telemetry can pivot
/// on it. Values: `"cli"`, `"state.transport.kind"`,
/// `"legacy_tmux_endpoint"`, `"spec.runtime.backend"`, `"default"`.
///
/// No `Clone` — the boxed `Transport` trait object is not cloneable.
pub struct ResolvedTransport {
    pub backend: Box<dyn Transport>,
    pub kind: BackendKind,
    pub source: &'static str,
    pub tmux_endpoint_used: Option<String>,
    pub team_key: Option<String>,
    /// N38 交底: extra events the caller should emit next to
    /// `transport.backend_selected` so users can explain WHY a shadow
    /// source was ignored (C-3).
    pub notices: Vec<TransportNotice>,
}

impl std::fmt::Debug for ResolvedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedTransport")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .field("tmux_endpoint_used", &self.tmux_endpoint_used)
            .field("team_key", &self.team_key)
            .field("notices", &self.notices)
            .field("backend", &"<dyn Transport>")
            .finish()
    }
}

/// A single N38-style notice the caller should emit into events.jsonl
/// after choosing a backend. The factory returns them; it never emits
/// (C-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportNotice {
    pub event: &'static str,
    pub payload: serde_json::Value,
}

/// Every way the factory can refuse honestly (MUST-NOT-13 + C-1/C-2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportFactoryError {
    /// C-1 ①: explicit `conpty` was requested but a required precondition
    /// isn't met (missing `team_key`, non-Windows host with no shim
    /// wired, etc.). Callers MUST surface this as
    /// `TransportError::MuxUnavailable` — NEVER silent-downgrade to
    /// tmux.
    #[error("conpty backend requires {reason}")]
    ConPtyPreconditionUnmet { reason: String },

    /// C-1 ②: `spec.runtime.backend = "pty"` was seen. The MVP does not
    /// auto-map `pty` to ConPTY (design §Non-Goals:370). Callers must
    /// treat this as a hard error.
    #[error("unsupported spec.runtime.backend literal {literal:?}; only tmux|conpty are supported in Phase 1d")]
    UnsupportedSpecBackendLiteral { literal: String },

    /// C-2: CLI `--backend X` and `state.transport.kind=Y` disagree and
    /// the caller did not pass `allow_backend_switch=true` (i.e. no
    /// `--fresh`). Refuse rather than orphan existing workers whose
    /// `pane_id` prefix matches the OTHER backend.
    #[error(
        "backend conflict: CLI requested {cli:?} but state.transport.kind={state:?}; \
         switching would abandon existing {state:?} workers — pass --fresh to switch"
    )]
    BackendConflict {
        cli: RequestedTransportBackend,
        state: BackendKind,
    },
}

/// Resolve a transport per the 5-layer order:
///
/// 1. Explicit CLI override.
/// 2. `state.transport.kind`.
/// 3. Legacy `state.tmux_endpoint` / `state.tmux_socket` → tmux.
/// 4. Spec `runtime.backend` literal (`tmux` / `conpty`; `pty`
///    explicitly unsupported).
/// 5. Default → tmux workspace socket.
///
/// The function returns `ResolvedTransport` for the success path or
/// `TransportFactoryError` for any refusal. **Never silently downgrades
/// conpty → tmux, never auto-maps pty → conpty, never runs
/// side-effects (open pipe, kill server, rotate token, emit event —
/// see C-5 guard test).**
pub fn resolve_transport(
    input: TransportFactoryInput<'_>,
) -> Result<ResolvedTransport, TransportFactoryError> {
    let state_kind = input
        .state
        .and_then(|s| s.pointer("/transport/kind"))
        .and_then(|v| v.as_str())
        .and_then(RequestedTransportBackend::parse_literal);
    // ── C-2 ────────────────────────────────────────────────────────
    // If CLI override disagrees with state.transport.kind, refuse
    // unless the caller opted-in via allow_backend_switch (--fresh).
    // This is fail-closed BEFORE we build any backend so no side effect
    // leaks.
    if let (Some(cli), Some(state)) = (input.explicit_backend, state_kind) {
        if cli != state && !input.allow_backend_switch {
            return Err(TransportFactoryError::BackendConflict {
                cli,
                state: requested_to_kind(state),
            });
        }
    }
    // ── Layer 1: CLI override ─────────────────────────────────────
    if let Some(requested) = input.explicit_backend {
        return build_selected(&input, requested, "cli");
    }
    // ── Layer 2: state.transport.kind ─────────────────────────────
    if let Some(requested) = state_kind {
        return build_selected(&input, requested, "state.transport.kind");
    }
    // ── Layer 3: legacy tmux endpoint / socket ────────────────────
    if let Some((_, _)) = crate::tmux_backend::runtime_tmux_endpoint_from_state_pub(input.state) {
        return build_selected(&input, RequestedTransportBackend::Tmux, "legacy_tmux_endpoint");
    }
    // ── Layer 4: spec.runtime.backend literal ─────────────────────
    if let Some(literal) = input.spec_backend_literal {
        match literal.trim().to_ascii_lowercase().as_str() {
            "tmux" => {
                return build_selected(&input, RequestedTransportBackend::Tmux, "spec.runtime.backend");
            }
            "conpty" => {
                return build_selected(&input, RequestedTransportBackend::ConPty, "spec.runtime.backend");
            }
            other => {
                // C-1 ②: `pty` (and everything else) is NOT auto-mapped
                // to ConPTY. Fail closed with a typed error.
                return Err(TransportFactoryError::UnsupportedSpecBackendLiteral {
                    literal: other.to_string(),
                });
            }
        }
    }
    // ── Layer 5: default → tmux workspace ─────────────────────────
    build_selected(&input, RequestedTransportBackend::Tmux, "default")
}

fn requested_to_kind(r: RequestedTransportBackend) -> BackendKind {
    match r {
        RequestedTransportBackend::Tmux => BackendKind::Tmux,
        RequestedTransportBackend::ConPty => BackendKind::ConPty,
    }
}

fn build_selected(
    input: &TransportFactoryInput<'_>,
    requested: RequestedTransportBackend,
    source: &'static str,
) -> Result<ResolvedTransport, TransportFactoryError> {
    match requested {
        RequestedTransportBackend::Tmux => Ok(build_tmux(input, source)),
        RequestedTransportBackend::ConPty => build_conpty(input, source),
    }
}

fn build_tmux(input: &TransportFactoryInput<'_>, source: &'static str) -> ResolvedTransport {
    // Preserve the existing runtime endpoint / workspace fallback shape
    // so tmux callers see byte-equivalent behavior.
    let selection = crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(
        input.workspace,
        input.state,
    );
    // C-3: if we chose tmux because of `state.transport.kind = tmux`
    // OR `spec.runtime.backend = tmux` OR `default`, but a legacy tmux
    // endpoint is *also* present, that is not a conflict (they
    // agree). The conflict scenario in design §Two-Source Conflict
    // Scene C is `legacy tmux + spec conpty` — order 3 wins, spec is
    // ignored. Callers of the `spec.runtime.backend` path with a
    // legacy tmux endpoint present take a different code path (they
    // hit layer 3 first). So there is no notice to emit for tmux.
    ResolvedTransport {
        kind: BackendKind::Tmux,
        source,
        tmux_endpoint_used: selection.tmux_endpoint_used,
        team_key: input.team_key.map(str::to_string),
        backend: Box::new(selection.backend),
        notices: Vec::new(),
    }
}

fn build_conpty(
    input: &TransportFactoryInput<'_>,
    source: &'static str,
) -> Result<ResolvedTransport, TransportFactoryError> {
    // C-1 fail-closed: ConPTY needs a team_key. Missing → refuse.
    // NEVER silent-downgrade to tmux; that would violate MUST-NOT-13.
    let Some(team_key) = input.team_key else {
        return Err(TransportFactoryError::ConPtyPreconditionUnmet {
            reason: "team_key required for conpty (design §Factory Design:159-163)"
                .to_string(),
        });
    };
    // Build a workspace hash from the canonical workspace path using
    // the same source of truth the tmux short-socket derivation uses,
    // so the shim + tmux socket see identical workspace identity.
    let workspace_hash = crate::tmux_backend::workspace_short_hash_pub(input.workspace);
    // No pipe client is wired here (that's Batch 2/3 scope). Without
    // a live client, `ConPtyBackend` degrades to `MuxUnavailable`
    // honestly — which is exactly what C-1 requires when the caller
    // asked for conpty on a host with no shim.
    let backend = ConPtyBackend::new(workspace_hash, team_key);
    let mut notices = Vec::new();
    // C-3: if state has BOTH `transport.kind=conpty` and a legacy
    // `tmux_endpoint`, tell the caller so they emit
    // `transport.legacy_tmux_endpoint_ignored`. Same shape for the
    // Scene C case (legacy tmux + spec conpty): if we're on the spec
    // path we would have hit the legacy branch first, so this only
    // fires when `state.transport.kind=conpty` explicitly overrode
    // the legacy fields.
    if let Some(state) = input.state {
        if source == "state.transport.kind" {
            if let Some(legacy) = state.get("tmux_endpoint").and_then(|v| v.as_str()) {
                notices.push(TransportNotice {
                    event: "transport.legacy_tmux_endpoint_ignored",
                    payload: serde_json::json!({
                        "chosen_backend": "conpty",
                        "legacy_tmux_endpoint": legacy,
                        "reason": "state.transport.kind=conpty explicitly overrides legacy tmux endpoint",
                    }),
                });
            }
        }
    }
    Ok(ResolvedTransport {
        kind: BackendKind::ConPty,
        source,
        tmux_endpoint_used: None,
        team_key: Some(team_key.to_string()),
        backend: Box::new(backend),
        notices,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Tmux channel helpers.
//
// These are intentional tmux-specific entrypoints for callers that
// target a KNOWN tmux socket (external leader panes, orphan diagnosis,
// managed leader launcher). They are NOT part of the team backend
// selection surface. Callers grep for these names to signal intent.
// ─────────────────────────────────────────────────────────────────────────

/// 0.5.x Phase 1d Batch 3 read-path convenience: build a
/// `ResolvedTransport` for a plain workspace + optional state pair.
///
/// This is the single entrypoint the coordinator boot / status /
/// diagnose read-only paths use. It follows the same 5-layer resolution
/// order as `resolve_transport`; it does NOT accept a CLI backend
/// override (read paths don't have one) — that avoids accidentally
/// widening the CLI surface at every reader.
///
/// On `Err`, callers should fall back to a tmux workspace transport
/// (byte-equivalent to today's shape) and record the refusal so
/// telemetry stays honest.
pub fn resolve_read_only_transport(
    workspace: &Path,
    state: Option<&serde_json::Value>,
    purpose: TransportPurpose,
) -> Result<ResolvedTransport, TransportFactoryError> {
    let input = TransportFactoryInput::new(workspace, purpose)
        .with_state(state)
        // Read paths never pass a team_key — they use whatever state
        // says. If `state.transport.kind = conpty` but there is no
        // team_key, factory refuses (C-1 ①), which is the same honest
        // signal we want to surface to the coordinator/status reader.
        .with_team_key(
            state
                .and_then(|s| s.get("teams"))
                .and_then(|t| t.as_object())
                .and_then(|obj| obj.keys().next().map(String::as_str)),
        );
    resolve_transport(input)
}

/// Tmux transport for a caller-supplied tmux endpoint literal. Used by
/// leader launcher, diagnose/orphans, and external leader pane
/// discovery — NOT for worker/team selection.
pub fn tmux_endpoint_transport(endpoint: &str) -> TmuxBackend {
    TmuxBackend::for_tmux_endpoint(endpoint)
}

/// Tmux transport for the default (shared) socket. Same intent as
/// above — used by leader-side channel operations, never worker/team
/// selection.
pub fn tmux_default_transport() -> TmuxBackend {
    TmuxBackend::new()
}

/// Tmux transport for the per-workspace socket. This is what the tmux
/// launch path uses when there is no persisted endpoint. Not for
/// generic worker/team selection — that goes through
/// `resolve_transport`.
pub fn tmux_workspace_transport(workspace: &Path) -> TmuxBackend {
    TmuxBackend::for_workspace(workspace)
}

/// Tmux transport for a specific named tmux socket (e.g.
/// `ta-abcdef012345`). Used by orphan-scanner and diagnose paths that
/// intentionally enumerate the tmux socket roots — NOT for team
/// backend selection.
pub fn tmux_socket_name_transport(socket: &str) -> TmuxBackend {
    TmuxBackend::for_socket_name(socket)
}

// ─────────────────────────────────────────────────────────────────────────
// Tests (§Batch 0 Verification, design.md:220-224).
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ws() -> PathBuf {
        std::env::temp_dir().join("ta-phase1d-factory-tests")
    }

    // Test #1 — no state/spec/override → tmux workspace backend.
    #[test]
    fn no_input_resolves_to_default_tmux_workspace_backend() {
        let workspace = ws();
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::Launch);
        let resolved = resolve_transport(input).expect("default path must succeed");
        assert_eq!(resolved.kind, BackendKind::Tmux);
        assert_eq!(resolved.source, "default");
        assert!(resolved.notices.is_empty());
    }

    // Test #2 — legacy `state.tmux_endpoint` → tmux endpoint (order 3).
    #[test]
    fn legacy_tmux_endpoint_resolves_to_tmux_endpoint_backend() {
        let workspace = ws();
        let state = serde_json::json!({ "tmux_endpoint": "default" });
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_state(Some(&state));
        let resolved = resolve_transport(input).expect("legacy path must succeed");
        assert_eq!(resolved.kind, BackendKind::Tmux);
        assert_eq!(resolved.source, "legacy_tmux_endpoint");
    }

    // Test #3 — explicit conpty (via CLI) with team_key → ConPty.
    #[test]
    fn explicit_conpty_with_team_key_resolves_to_conpty() {
        let workspace = ws();
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_team_key(Some("team-a"))
            .with_explicit_backend(Some(RequestedTransportBackend::ConPty));
        let resolved = resolve_transport(input).expect("explicit conpty must succeed with team_key");
        assert_eq!(resolved.kind, BackendKind::ConPty);
        assert_eq!(resolved.source, "cli");
        assert_eq!(resolved.team_key.as_deref(), Some("team-a"));
    }

    // Test #4 — explicit conpty WITHOUT team_key → refusal (C-1 ①).
    #[test]
    fn explicit_conpty_without_team_key_refuses_no_silent_fallback() {
        let workspace = ws();
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_explicit_backend(Some(RequestedTransportBackend::ConPty));
        let err = resolve_transport(input).expect_err("conpty without team_key must refuse");
        match err {
            TransportFactoryError::ConPtyPreconditionUnmet { reason } => {
                assert!(reason.contains("team_key"), "reason must mention team_key: {reason}");
            }
            other => panic!("expected ConPtyPreconditionUnmet, got {other:?}"),
        }
    }

    // Test #5 — `pty` literal is NOT silently mapped to ConPTY (C-1 ②).
    #[test]
    fn pty_literal_is_not_silently_mapped_to_conpty() {
        let workspace = ws();
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::Launch)
            .with_team_key(Some("team-a"))
            .with_spec_backend_literal(Some("pty"));
        let err = resolve_transport(input)
            .expect_err("pty literal must be refused, not auto-mapped");
        match err {
            TransportFactoryError::UnsupportedSpecBackendLiteral { literal } => {
                assert_eq!(literal, "pty");
            }
            other => panic!("expected UnsupportedSpecBackendLiteral, got {other:?}"),
        }
    }

    // C-2 supplementary — CLI override vs state disagreement refuses.
    #[test]
    fn cli_override_disagreeing_with_state_kind_refuses_unless_fresh() {
        let workspace = ws();
        let state = serde_json::json!({ "transport": { "kind": "conpty" } });
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_team_key(Some("team-a"))
            .with_state(Some(&state))
            .with_explicit_backend(Some(RequestedTransportBackend::Tmux));
        let err = resolve_transport(input).expect_err("disagreement must refuse");
        match err {
            TransportFactoryError::BackendConflict { cli, state } => {
                assert_eq!(cli, RequestedTransportBackend::Tmux);
                assert_eq!(state, BackendKind::ConPty);
            }
            other => panic!("expected BackendConflict, got {other:?}"),
        }
        // With allow_backend_switch=true (== --fresh), the same input succeeds.
        let input_fresh = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_team_key(Some("team-a"))
            .with_state(Some(&state))
            .with_explicit_backend(Some(RequestedTransportBackend::Tmux))
            .allow_backend_switch(true);
        let ok = resolve_transport(input_fresh).expect("--fresh must allow switch");
        assert_eq!(ok.kind, BackendKind::Tmux);
        assert_eq!(ok.source, "cli");
    }

    // C-3 supplementary — state kind=conpty + legacy tmux endpoint emits
    // a `legacy_tmux_endpoint_ignored` notice.
    #[test]
    fn state_conpty_with_legacy_tmux_endpoint_emits_ignored_notice() {
        let workspace = ws();
        let state = serde_json::json!({
            "transport": { "kind": "conpty" },
            "tmux_endpoint": "default",
        });
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_team_key(Some("team-a"))
            .with_state(Some(&state));
        let resolved = resolve_transport(input).expect("must succeed");
        assert_eq!(resolved.kind, BackendKind::ConPty);
        assert_eq!(resolved.source, "state.transport.kind");
        // N38: one notice emitted.
        assert_eq!(resolved.notices.len(), 1);
        assert_eq!(resolved.notices[0].event, "transport.legacy_tmux_endpoint_ignored");
        assert_eq!(
            resolved.notices[0].payload["legacy_tmux_endpoint"],
            serde_json::json!("default")
        );
        assert_eq!(
            resolved.notices[0].payload["chosen_backend"],
            serde_json::json!("conpty")
        );
    }

    // C-3 also — pure conpty state (no legacy) emits zero notices.
    #[test]
    fn state_conpty_without_legacy_emits_no_notice() {
        let workspace = ws();
        let state = serde_json::json!({ "transport": { "kind": "conpty" } });
        let input = TransportFactoryInput::new(&workspace, TransportPurpose::LifecycleWorker)
            .with_team_key(Some("team-a"))
            .with_state(Some(&state));
        let resolved = resolve_transport(input).expect("must succeed");
        assert_eq!(resolved.kind, BackendKind::ConPty);
        assert!(resolved.notices.is_empty());
    }

    // Wire-string invariants for RequestedTransportBackend so a rename
    // in Rust cannot silently drift the CLI accept-list.
    #[test]
    fn requested_backend_wire_strings_are_pinned() {
        assert_eq!(RequestedTransportBackend::Tmux.as_wire(), "tmux");
        assert_eq!(RequestedTransportBackend::ConPty.as_wire(), "conpty");
        assert_eq!(
            RequestedTransportBackend::parse_literal("tmux"),
            Some(RequestedTransportBackend::Tmux)
        );
        assert_eq!(
            RequestedTransportBackend::parse_literal("conpty"),
            Some(RequestedTransportBackend::ConPty)
        );
        // pty is deliberately NOT parsed as ConPty at the wire level
        // either. The spec.runtime.backend layer handles the typed
        // refusal for the spec case; here we just prove the CLI
        // literal parser rejects it.
        assert_eq!(RequestedTransportBackend::parse_literal("pty"), None);
        assert_eq!(RequestedTransportBackend::parse_literal("nonsense"), None);
    }
}
