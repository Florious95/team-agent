//! ---
//! purpose: 把 restart resume 门的「为什么拒绝」从两个不透明字符串升级成闭合枚举，并携带操作者可用的恢复线索
//! contract:
//!   provides:
//!     - name: ResumeRefusalReason
//!       what: resume 拒绝原因闭合枚举(7 变体)，新原因必须改代码才能加
//!     - name: wire
//!       what: 枚举 → 历史 UnresumableWorker.reason 稳定串，供 JSON/日志沿用旧形状
//!     - name: from_legacy
//!       what: 历史字符串 → 枚举的逆映射，未识别串一律落进 Other{legacy_reason}
//!     - name: RecoveryHint
//!       what: 缺 backing 时给操作者的 provider/name/cwd 三元线索
//!     - name: picker_hint
//!       what: 把 RecoveryHint 渲染成一行人读文本
//!   requires:
//!     - name: std::path::PathBuf
//!       what: checked_paths 与 spawn_cwd 的载体
//! boundary:
//!   - 只做「原因的分类与措辞」：不探测 backing 是否存在，不读磁盘，不发事件
//!   - RecoveryHint 只呈现给人，绝不被自动 resume 消费(自动恢复需 Layer 3 多键过滤+backing 复验)
//!   - 判定 resume 是否可行的调用点在 lifecycle/restart，不在本文件
//! maturity: wired
//! ---
//!
//! unit-5 (Stage 2) — closed `ResumeRefusalReason` enum and recovery hints.
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
    /// ---
    /// purpose: 把 provider/name/cwd 三元线索拼成一行人读文本，供 CLI 拒绝信息与事件载荷使用
    /// returns: 四种措辞之一，按 name/cwd 各自是否存在退化;两者都缺时退到 "<provider> session"
    /// contract:
    ///   provides:
    ///     - name: picker_hint
    ///       what: 纯格式化，不查磁盘、不校验 cwd 是否还在
    /// boundary:
    ///   - 输出只给人看，不做机器解析的契约，调用方不得据此自动 resume
    /// ---
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
    /// ---
    /// purpose: 把结构化拒绝原因压回历史 UnresumableWorker.reason 稳定串
    /// returns: 六个 canonical 串之一;Other 一律折回 "session_unresumable"——未 taxonomize 的新失败类型对外与历史大杂烩不可区分
    /// contract:
    ///   provides:
    ///     - name: wire
    ///       what: 枚举 → 稳定串的全函数映射，不丢字段但丢细节
    /// boundary:
    ///   - 不携带 checked_paths / recovery_hint / drift 的 expected-actual 等负载，只给分类名
    ///   - 与 from_legacy 只在五个 canonical 串上互逆;SessionDrift 有 wire 串但 from_legacy 无对应臂，"session_drift" 会落进 Other，该变体不可往返
    /// ---
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
    /// ---
    /// purpose: 把历史自由串抬升成结构化枚举，保证旧持久化数据不丢分类
    /// params:
    ///   reason: 历史 UnresumableWorker.reason 串;不在识别表内的任意值都合法
    /// returns: 匹配到的变体(负载字段一律填空，因为串里没有这些事实);未匹配则 Other{legacy_reason=原串}
    /// contract:
    ///   provides:
    ///     - name: from_legacy
    ///       what: 全函数、不失败、不 panic 的逆映射
    /// boundary:
    ///   - 不还原 checked_paths / recovery_hint / provider 名等负载——它们在串里本就不存在
    ///   - 识别表缺 "session_drift" 臂，该串会落到 Other 而非 SessionDrift
    /// ---
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
}
