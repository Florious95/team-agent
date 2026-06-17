//! 0.3.28 layout step 4 — typed worker placement target.
//!
//! Python parity (`runtime.py:1017-1020`): one window per agent in the
//! worker session, named `agent_id`. First worker creates the session
//! (`new-session`); subsequent workers each get a new window
//! (`new-window`). No splits, no 3-pane tiling at the execution layer.
//!
//! This module provides the typed `WorkerSpawnTarget` enum returned by
//! `next_worker_window` (in `layout::manager`). Step 4 lands the API +
//! contract tests; Step 4b deletes `adaptive_layout_plan` and migrates
//! all callsites.

use crate::transport::{SessionName, WindowName};

/// Action to take when spawning a worker into the layout.
///
/// `NewSession` is used for the first agent in a fresh team (the worker
/// session does not yet exist). `NewWindow` is used for every subsequent
/// agent. There is intentionally NO `SplitWindow` variant — splits exist
/// only in the display overlay (`layout::overlay`), never in the worker
/// execution layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSpawnAction {
    /// `tmux new-session -d -s <session> -n <window> ...`
    NewSession,
    /// `tmux new-window -d -t <session> -n <window> ...`
    NewWindow,
    /// Window already exists; reuse (no spawn).
    Noop,
    /// Window already exists; caller asked to force replace —
    /// `tmux kill-window -t <session>:<window>` then recreate.
    ForceReplace,
}

/// A fully-resolved spawn target for a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpawnTarget {
    pub session: SessionName,
    pub window: WindowName,
    pub action: WorkerSpawnAction,
}

impl WorkerSpawnTarget {
    pub fn new(session: SessionName, window: WindowName, action: WorkerSpawnAction) -> Self {
        Self { session, window, action }
    }
}
