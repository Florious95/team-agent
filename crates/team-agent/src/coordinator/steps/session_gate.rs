//! ---
//! purpose: coordinator tick session_gate 步骤组的占位命名空间——实现仍在 tick.rs
//! contract:
//!   provides: []
//!   depends: []
//! boundary:
//!   - 本文件当前不含任何 item：provider session 捕获与就绪门步骤未来迁入处
//!   - 迁移落地前不要在此新增逻辑，否则步骤顺序会分裂成两处
//! maturity: signature_only
//! ---
//!
//! unit-11 (Stage 4) — coordinator tick `session_gate` step group.
//!
//! Future home for the provider session-capture + readiness-gate steps
//! currently inlined in `coordinator/tick.rs`. Boundary established by
//! unit-11; actual relocation lands in follow-up commits.
