//! unit-5 (Stage 2) — resume preflight + closed `ResumeRefusalReason` enum.
//!
//! Today the restart resume gate flattens every refusal into one of two
//! opaque strings:
//!
//!   * `"no_persisted_session_id"` — no `session_id` in state
//!   * `"session_unresumable"`     — `session_id` set but backing missing
//!                                   OR provider can't resume OR session
//!                                   ambiguous (all collapsed to one string)
//!
//! That string is the user-facing diagnostic, so debugging "why did restart
//! refuse?" requires reading source. This unit replaces the catch-all
//! `session_unresumable` with a closed enum carrying the structured
//! distinction.
//!
//! Migration strategy: ADDITIVE. `UnresumableWorker.reason: String` stays
//! (every CLI/JSON caller already reads it). A new optional
//! `refusal_reason: Option<ResumeRefusalReason>` carries the structured
//! value alongside. Callers that need actionable diagnostics flip to the
//! enum; legacy callers see the same string they always saw.

use std::path::PathBuf;

/// Layer 2 self-healing hint (architect probe 2026-06-22, §Recommended
/// design): operator-facing diagnostic that names the agent / cwd /
/// provider for a missing-backing refusal so the operator can manually
/// find / repair the dropped session. Carried alongside the refusal —
/// NEVER consumed for automatic resume. Auto-recovery requires multi-key
/// filtering + backing revalidation (Layer 3 follow-up, see
/// `session.recovery.candidate_promoted` event design).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryHint {
    /// Agent role label that was passed at launch as `--name` / `-n`
    /// (Claude / Copilot). Codex has no launch-time name flag, so this
    /// is `None` for Codex workers (probe verdict 2026-06-22).
    pub provider_session_name_hint: Option<String>,
    /// Spawn cwd recorded for the worker — the second key in the
    /// "provider+cwd+name+updated_at" filter Layer 3 will require.
    pub spawn_cwd: Option<PathBuf>,
    /// Wire provider name (`codex` / `claude` / `claude_code` /
    /// `copilot`). Used for picker hint string only.
    pub provider: String,
}

impl RecoveryHint {
    /// Build a human-readable picker hint (one line). Layer 2 surfaces
    /// this in the CLI refusal message and in the
    /// `session.recovery.candidate_hint` event payload.
    pub fn picker_hint(&self) -> String {
        match (&self.provider_session_name_hint, &self.spawn_cwd) {
            (Some(name), Some(cwd)) => format!(
                "{} session named '{}' under cwd {}",
                self.provider,
                name,
                cwd.display()
            ),
            (Some(name), None) => {
                format!("{} session named '{}'", self.provider, name)
            }
            (None, Some(cwd)) => format!("{} session under cwd {}", self.provider, cwd.display()),
            (None, None) => format!("{} session", self.provider),
        }
    }
}

/// Structured reason a worker is unresumable. Closed enum — future
/// reasons require a code change, which is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeRefusalReason {
    /// State has no `session_id` for this worker. Fresh start would be
    /// safe iff `--allow-fresh` is set.
    NoSessionId,
    /// State has `session_id` but the provider backing file (Codex rollout
    /// JSONL / Claude transcript) is missing on disk. Distinct from
    /// "no session" because it implies provider lost the backing.
    SessionBackingStoreMissing {
        /// Paths the runtime probed for the backing file.
        checked_paths: Vec<PathBuf>,
        /// Layer 2 self-healing diagnostic (architect probe 2026-06-22):
        /// agent / cwd / provider hint the operator can use to find or
        /// repair the dropped session via the provider's own picker
        /// (`claude --resume <name>`, `copilot --resume=<name>`).
        /// **Never auto-consumed for resume** — auto-recovery requires
        /// the Layer 3 multi-key filter + backing revalidation contract.
        recovery_hint: Option<RecoveryHint>,
    },
    /// Provider does not support resume at the protocol level (e.g.
    /// some auth modes).
    ProviderResumeUnsupported {
        /// Provider name (`codex` / `claude` / `copilot` / `fake`).
        provider: String,
    },
    /// Session capture polling did not converge before the deadline
    /// (the existing `RefusedResumeNotReady` shape).
    SessionCaptureIncomplete,
    /// State session_id differs from the provider's observed session
    /// (T6 L5.5 drift). Caller should reconcile before resuming.
    SessionDrift { expected: String, actual: String },
    /// The persisted provider backing exists, but the transcript itself
    /// declares a different Team Agent worker identity.
    SessionIdentityMismatch {
        expected_agent_id: String,
        embedded_agent_id: String,
        session_id: String,
        rollout_path: Option<PathBuf>,
    },
    /// Catch-all for refusals the structured shape hasn't taxonomized
    /// yet. Carries the legacy free-form string. This variant exists so
    /// the enum is BACKWARD-COMPATIBLE with the historical
    /// `session_unresumable` string — every value the old code emitted
    /// can be lifted into the enum without losing fidelity.
    Other { legacy_reason: String },
}

impl ResumeRefusalReason {
    /// Stable wire string mirroring the historical
    /// `UnresumableWorker.reason` values. Use this when emitting JSON or
    /// log fields so downstream consumers see the same strings they
    /// always have.
    pub fn wire(&self) -> &'static str {
        match self {
            ResumeRefusalReason::NoSessionId => "no_persisted_session_id",
            ResumeRefusalReason::SessionBackingStoreMissing { .. } => {
                "session_backing_store_missing"
            }
            ResumeRefusalReason::ProviderResumeUnsupported { .. } => "provider_resume_unsupported",
            ResumeRefusalReason::SessionCaptureIncomplete => "session_capture_incomplete",
            ResumeRefusalReason::SessionDrift { .. } => "session_drift",
            ResumeRefusalReason::SessionIdentityMismatch { .. } => "session_identity_mismatch",
            // For Other we still report the legacy wire so the existing
            // `session_unresumable` JSON shape is preserved end-to-end.
            ResumeRefusalReason::Other { .. } => "session_unresumable",
        }
    }

    /// Lift a legacy free-form `reason` string into the structured enum.
    /// Round-trip-safe with `wire()` for the canonical names.
    pub fn from_legacy(reason: &str) -> Self {
        match reason {
            "no_persisted_session_id" => ResumeRefusalReason::NoSessionId,
            "session_backing_store_missing" => ResumeRefusalReason::SessionBackingStoreMissing {
                checked_paths: Vec::new(),
                recovery_hint: None,
            },
            "provider_resume_unsupported" => ResumeRefusalReason::ProviderResumeUnsupported {
                provider: String::new(),
            },
            "session_capture_incomplete" => ResumeRefusalReason::SessionCaptureIncomplete,
            "session_identity_mismatch" => ResumeRefusalReason::SessionIdentityMismatch {
                expected_agent_id: String::new(),
                embedded_agent_id: String::new(),
                session_id: String::new(),
                rollout_path: None,
            },
            other => ResumeRefusalReason::Other {
                legacy_reason: other.to_string(),
            },
        }
    }
}

/// Outcome of `ResumePreflight::check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePreflightOutcome {
    /// Worker can resume from the named session.
    Resume { session_id: String },
    /// `--allow-fresh` was set; worker will start fresh.
    FreshStart,
    /// Resume refused. Caller MUST NOT proceed with teardown/spawn.
    Refuse { reason: ResumeRefusalReason },
}

/// Information about whether a provider backing file is present on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBackingCheck {
    pub paths: Vec<PathBuf>,
    pub exists: bool,
}

/// Per-decision detail emitted to events / JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeDecisionDetail {
    pub agent_id: String,
    pub session_id: Option<String>,
    pub provider: String,
    pub backing: Option<ProviderBackingCheck>,
    pub outcome: ResumePreflightOutcome,
}

/// Pure resume preflight evaluator. No I/O — callers pass facts in.
pub struct ResumePreflight;

impl ResumePreflight {
    /// Decide a single worker's resume outcome from the facts the
    /// restart code already collected. Mirrors the existing
    /// `classify_restart_plan_with_resume_validation` decision tree:
    ///
    ///   * session_id present + provider can resume + backing exists -> Resume
    ///   * session_id present + (no resume OR no backing) + allow_fresh -> FreshStart
    ///   * session_id present + (no resume OR no backing) + !allow_fresh -> Refuse
    ///   * session_id absent + allow_fresh -> FreshStart
    ///   * session_id absent + !allow_fresh -> Refuse(NoSessionId)
    pub fn check(
        session_id: Option<&str>,
        provider_can_resume: bool,
        backing: Option<&ProviderBackingCheck>,
        provider_name: &str,
        allow_fresh: bool,
    ) -> ResumePreflightOutcome {
        Self::check_with_hint(
            session_id,
            provider_can_resume,
            backing,
            provider_name,
            allow_fresh,
            None,
        )
    }

    /// Layer 2 self-healing variant: same decision tree as
    /// [`Self::check`], but propagates an optional `RecoveryHint` into
    /// the `SessionBackingStoreMissing` refusal so the CLI / event log
    /// can surface a "look for session named X under cwd Y" pointer to
    /// the operator. The hint is for HUMAN consumption only — no
    /// auto-resume happens off it (Layer 3 follow-up).
    pub fn check_with_hint(
        session_id: Option<&str>,
        provider_can_resume: bool,
        backing: Option<&ProviderBackingCheck>,
        provider_name: &str,
        allow_fresh: bool,
        recovery_hint: Option<RecoveryHint>,
    ) -> ResumePreflightOutcome {
        match session_id {
            Some(sid) if provider_can_resume => {
                let backing_present = backing.map(|b| b.exists).unwrap_or(true);
                if backing_present {
                    ResumePreflightOutcome::Resume {
                        session_id: sid.to_string(),
                    }
                } else if allow_fresh {
                    ResumePreflightOutcome::FreshStart
                } else {
                    ResumePreflightOutcome::Refuse {
                        reason: ResumeRefusalReason::SessionBackingStoreMissing {
                            checked_paths: backing.map(|b| b.paths.clone()).unwrap_or_default(),
                            recovery_hint,
                        },
                    }
                }
            }
            Some(_) if allow_fresh => ResumePreflightOutcome::FreshStart,
            Some(_) => ResumePreflightOutcome::Refuse {
                reason: ResumeRefusalReason::ProviderResumeUnsupported {
                    provider: provider_name.to_string(),
                },
            },
            None if allow_fresh => ResumePreflightOutcome::FreshStart,
            None => ResumePreflightOutcome::Refuse {
                reason: ResumeRefusalReason::NoSessionId,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip_canonical_strings() {
        for wire in [
            "no_persisted_session_id",
            "session_backing_store_missing",
            "provider_resume_unsupported",
            "session_capture_incomplete",
            "session_identity_mismatch",
        ] {
            assert_eq!(ResumeRefusalReason::from_legacy(wire).wire(), wire);
        }
    }

    #[test]
    fn other_preserves_legacy_session_unresumable_wire() {
        let r = ResumeRefusalReason::from_legacy("session_unresumable");
        assert_eq!(r.wire(), "session_unresumable");
        assert!(matches!(r, ResumeRefusalReason::Other { .. }));
    }

    #[test]
    fn preflight_resume_when_session_and_backing_present() {
        let backing = ProviderBackingCheck {
            paths: vec![PathBuf::from("/tmp/x.jsonl")],
            exists: true,
        };
        let out = ResumePreflight::check(Some("sess-1"), true, Some(&backing), "codex", false);
        assert!(matches!(out, ResumePreflightOutcome::Resume { .. }));
    }

    #[test]
    fn preflight_refuses_when_session_id_missing_without_allow_fresh() {
        let out = ResumePreflight::check(None, true, None, "codex", false);
        assert_eq!(
            out,
            ResumePreflightOutcome::Refuse {
                reason: ResumeRefusalReason::NoSessionId
            }
        );
    }

    #[test]
    fn preflight_distinguishes_backing_missing_from_no_session_id() {
        let backing = ProviderBackingCheck {
            paths: vec![PathBuf::from("/missing.jsonl")],
            exists: false,
        };
        let out = ResumePreflight::check(Some("sess-x"), true, Some(&backing), "codex", false);
        match out {
            ResumePreflightOutcome::Refuse { reason } => {
                assert_eq!(reason.wire(), "session_backing_store_missing");
            }
            other => panic!("expected backing-missing refusal; got {other:?}"),
        }
    }

    #[test]
    fn preflight_allow_fresh_converts_refusals_to_fresh() {
        let backing = ProviderBackingCheck {
            paths: vec![PathBuf::from("/missing.jsonl")],
            exists: false,
        };
        assert_eq!(
            ResumePreflight::check(None, true, None, "codex", true),
            ResumePreflightOutcome::FreshStart
        );
        assert_eq!(
            ResumePreflight::check(Some("sess-x"), true, Some(&backing), "codex", true),
            ResumePreflightOutcome::FreshStart
        );
    }

    // Layer 2 self-healing tests ────────────────────────────────────────────

    #[test]
    fn recovery_hint_picker_string_with_all_fields() {
        let hint = RecoveryHint {
            provider_session_name_hint: Some("coder".to_string()),
            spawn_cwd: Some(PathBuf::from("/repo/team-a")),
            provider: "claude".to_string(),
        };
        assert_eq!(
            hint.picker_hint(),
            "claude session named 'coder' under cwd /repo/team-a"
        );
    }

    #[test]
    fn recovery_hint_picker_string_no_name_no_cwd() {
        // Codex worker — no launch-time name, spawn_cwd missing.
        let hint = RecoveryHint {
            provider_session_name_hint: None,
            spawn_cwd: None,
            provider: "codex".to_string(),
        };
        assert_eq!(hint.picker_hint(), "codex session");
    }

    #[test]
    fn preflight_with_hint_attaches_to_backing_missing_refusal() {
        let backing = ProviderBackingCheck {
            paths: vec![PathBuf::from("/missing.jsonl")],
            exists: false,
        };
        let hint = RecoveryHint {
            provider_session_name_hint: Some("coder".to_string()),
            spawn_cwd: Some(PathBuf::from("/repo")),
            provider: "claude".to_string(),
        };
        let out = ResumePreflight::check_with_hint(
            Some("sess-1"),
            true,
            Some(&backing),
            "claude",
            false,
            Some(hint.clone()),
        );
        match out {
            ResumePreflightOutcome::Refuse {
                reason:
                    ResumeRefusalReason::SessionBackingStoreMissing {
                        recovery_hint: Some(h),
                        ..
                    },
            } => {
                assert_eq!(h, hint);
            }
            other => panic!("expected SessionBackingStoreMissing with hint; got {other:?}"),
        }
    }

    #[test]
    fn preflight_no_hint_legacy_behavior_unchanged() {
        // Default check() (no hint) still produces None recovery_hint —
        // wire-string-compatible with pre-Layer-2 callers.
        let backing = ProviderBackingCheck {
            paths: vec![PathBuf::from("/missing.jsonl")],
            exists: false,
        };
        let out = ResumePreflight::check(Some("sess-x"), true, Some(&backing), "codex", false);
        match out {
            ResumePreflightOutcome::Refuse {
                reason: ResumeRefusalReason::SessionBackingStoreMissing { recovery_hint, .. },
            } => {
                assert!(recovery_hint.is_none());
            }
            other => panic!("expected backing-missing refusal; got {other:?}"),
        }
    }
}
