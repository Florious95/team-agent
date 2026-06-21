//! 0.3.28 — Unified adaptive layout manager.
//!
//! Single source of truth for tmux topology decisions in the runtime:
//!   * leader session vs worker session (DISJOINT by naming)
//!   * worker placement (1 window per agent, named `agent_id`)
//!   * shutdown vs recovery semantics
//!   * display-overlay 3-pane tiling (separate from worker execution layer)
//!
//! Mirrors the Python 0.2.11 truth source. Each invariant is documented in
//! `assert_topology_invariants` so any regression surfaces as a `warn!` log
//! (during the incremental migration) and eventually as a hard error.
//!
//! Module tree per `.team/artifacts/adaptive-layout-full-architecture-locate.md`:
//!   * `sessions` — name constructors + is_leader_session / is_worker_session +
//!                  assert_topology_invariants (Step 1, this file)
//!   * `manager` — public API for leader_placement / next_worker_window /
//!                 ensure_leader_pane / recover_leader_pane (Steps 2/4/7)
//!   * `placement` — typed LeaderPlacement / WorkerSpawnTarget enums (Step 2/4)
//!   * `overlay` — 3-pane display overlay (only place split-window appears)
//!                 (Step 8)
//!   * `worker_env` — worker_spawn_env whitelist + worker_spawn_cwd (Step 3)

pub mod sessions;
pub mod manager;
pub mod worker_window_helpers;
pub mod worker_env;
pub mod placement;
pub mod recovery;
pub mod overlay;
pub mod runtime_sessions;

pub use runtime_sessions::{
    LeaderLauncherSession, LeaderLauncherSessionError, RuntimeSessionAnomaly, RuntimeSessions,
    WorkerSession, WorkerSessionError,
};
