//! 0.5.x Windows portability Batch 0: platform abstraction layer.
//!
//! Truth sources (READ-ONLY):
//! - Design:  `.team/artifacts/0.5.x-windows-portability-survey-design.md`
//!            (§Target Design + §Ordered Migration Plan)
//! - CR verdict: `.team/artifacts/0.5.x-windows-portability-cr-verdict.md`
//!            (6 constraints; C-1 through C-6 anchor into this module)
//!
//! ## Scope of Batch 0 (this commit)
//!
//! - Add the `platform` module scaffold + Unix-side implementations
//!   populated from the current inline `libc`/`std::os::unix` code
//!   (byte-equivalent behavior on macOS/Linux).
//! - No caller migration yet — the three legacy fallback sites
//!   (`mcp_server/wire.rs`, `lifecycle/restart/agent.rs`,
//!   `state/persist.rs`) stay in place with `FIXME(portability)`
//!   comments pointing at this design. Batch 2/3 replaces them.
//! - Windows side is empty stubs today; every fn is `unimplemented!()`
//!   with a `# Safety:` / `# Windows:` doc note explaining what
//!   Batch 2/3/4 will supply.
//! - CI adds a Windows compile-only gate (`cargo check --target
//!   x86_64-pc-windows-msvc`), reporting RED as the burn-down baseline
//!   for Batches 1-4. The Windows job uses `continue-on-error: true`
//!   so it does not block the Ubuntu/macOS/Windows-binaries jobs.
//!
//! ## CR anchors
//!
//! - **C-1**: `packaging/types.rs::WindowsX8664 → PlatformSupport::Native`
//!   claim is FALSE while `cargo check --target x86_64-pc-windows-msvc`
//!   is RED. Downgraded to `PreviewCompileOnly` in this batch; will be
//!   promoted back to `Native` only after Batch 6/7 real-machine gates.
//! - **C-2**: three legacy non-Unix fallbacks
//!   (`wire.rs::parent_pid=0`, `restart/agent.rs::pid_is_alive=true`,
//!   `persist.rs::runtime_lock=not_implemented`) get
//!   `FIXME(portability)` comments referencing this design. Batch 2
//!   removes the persist.rs one; Batch 3 removes the wire.rs +
//!   restart/agent.rs ones. Grep guards verify no reintroduction.
//! - **C-4**: `platform::process` is NOT importable from
//!   `messaging/` (team-worker liveness must go through the backend
//!   `Transport`, not a shell-out process snapshot). Grep guard test
//!   walks the source tree.
//! - **C-6**: hard-terminate on Windows lacks Unix grace semantics —
//!   the `terminate_pid`/`terminate_group` signatures include a
//!   `SignalKind` enum + a caller-visible `TerminationOutcome` so
//!   Windows callers can emit a `platform.terminate_force_only` event
//!   when they downgrade from `TerminateGraceful` to `TerminateForce`.
//!
//! ## Module layout
//!
//! - `process`  — process lifecycle + snapshot + termination
//! - `file_lock` — cross-platform exclusive file lock
//! - `errors`  — retryable error classifier + os_error_name
//! - `argv`    — process argv/environ query (Batch 4)
//!
//! Every submodule follows the same pattern: a `unix.rs` file with the
//! current libc code, a `windows.rs` file with `unimplemented!()`
//! stubs + Windows API sketch, and a `mod.rs` that re-exports the
//! platform-appropriate impl via `cfg`.
//!
//! Callers see a single `crate::platform::process::pid_liveness(pid)`
//! (etc.) with no `cfg` at the callsite.

pub mod argv;
pub mod errors;
pub mod file_lock;
pub mod process;
