//! ---
//! purpose: coordinator tick runtime_prompts 步骤组的占位命名空间——实现仍在 tick.rs
//! contract:
//!   provides: []
//!   depends: []
//! boundary:
//!   - 本文件当前不含任何 item：startup / approval / abnormal-exit 提示步骤未来迁入处
//!   - 迁移落地前不要在此新增逻辑，否则步骤顺序会分裂成两处
//! maturity: signature_only
//! ---
//!
//! unit-11 (Stage 4) — coordinator tick `runtime_prompts` step group.
//!
//! Future home for the startup / approval / abnormal-exit prompt steps
//! from `coordinator/tick.rs`.
