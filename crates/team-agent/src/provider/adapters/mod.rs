//!
//! Provider-local adapter implementations. Split from the monolithic
//! `provider/adapter.rs` as 0.4.x decoupling step 2. Each file owns its
//! provider's command builders and auth helpers,
//! anything else that varies by provider but doesn't need shared
//! capture/scan utilities. The `ProviderAdapter` trait and the
//! `BasicProviderAdapter` registry stay in `adapter.rs`; the trait impl
//! dispatches into these helpers exactly as the inline forms did.
//!
//! Per-file scope:
//!   * `codex`  — Codex argv, profile flags, MCP `-c` injection with 600s
//!     tool_timeout, developer-instructions escaping.
//!   * `copilot` — Copilot argv (no-color/no-remote/disable-builtin-mcps),
//!     MCP `type→transport` translation, resume base, weak auth hint.
//!   * `fake`   — Built-in scripted worker exec path.
//!
//! Claude Code and Pi workers run only through their contract leaves
//! (`claude_tui`, `pi_tui`); their legacy worker builders were excised. The Pi
//! leader's fresh launch lives in `lifecycle/launch/pi_mcp.rs`.
//!
//! Behavior is byte-identical to pre-split. Future steps may move
//! `pre_spawn_adjust_plan`, `profile_env`, `session_backing_probe`, and
//! `session_candidates` into provider-owned hooks following the same
//! per-file layout.

pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod copilot_fork;
pub(crate) mod fake;
pub(crate) mod grok;
