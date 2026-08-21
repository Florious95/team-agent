//! ---
//! purpose: provider 会话域的命名空间——把「捕获 / resume 拒绝判定 / context-fork」三块聚合成一个出口
//! contract:
//!   provides:
//!     - name: capture
//!       what: pending id → 扫描候选 → 分配 → 写会话四元组的捕获通道(pub 子模块)
//!     - name: resume
//!       what: ResumeRefusalReason 闭合枚举与 RecoveryHint(pub 子模块)
//!     - name: ContextForkProof
//!       what: context-fork 验证通过后的证明结构，由私有 context_fork 子模块 re-export
//!   requires:
//!     - name: crate::provider::session_scan
//!       what: 磁盘候选扫描不在本命名空间内实现，由 session_scan 提供
//! boundary:
//!   - 只做子模块聚合与 re-export，本文件不含任何逻辑
//!   - 不决定何时重启/销毁席位——那是 lifecycle 的判断
//! maturity: wired
//! ---
//!
//! unit-6 (Stage 2) — provider session namespace.
//!
//! Houses session-specific provider concerns: resume preflight,
//! session capture (moved from `crate::session_capture` in unit-6),
//! session backing checks, etc.

pub mod capture;
mod context_fork;
pub mod resume;

pub use context_fork::ContextForkProof;
pub(crate) use context_fork::{
    context_fork_convergence_deadline, materialize_codex_fork, observe_context_fork,
    transition_pending_context_fork, ContextBackingSnapshot, ContextForkOutcome,
    PendingContextFork,
};

pub use resume::{RecoveryHint, ResumeRefusalReason};
