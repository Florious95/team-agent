# Adaptive Display Contract

Gap 41 changes the default team display from Ghostty-first to adaptive
terminal display. These constraints define the outside behavior; implementation
details are intentionally not part of the contract.

## C1. Leader Pane Safety

Adaptive display renders the team view by appending sibling tmux windows to the
leader's current tmux session. It does not launch a GUI app. It never splits,
resizes, kills, respawns, or steals focus from the leader's own active pane.

## C2. Team-Scoped Windows

Every adaptive window uses a deterministic team-scoped name or tag:
`team-agent:<session>:overview` for the first window and
`team-agent:<session>:overview-N` for later windows. Teardown deletes only
adaptive windows carrying that team tag; unrelated windows in the same user tmux
session are never touched.

## C3. Worker Topology Is Stable

Worker tmux sessions and windows remain the source of truth. Adaptive display
creates view-layer mirror panes that attach to worker views; no worker is
reparented into the leader session as a direct worker process.

## C4. Focus Behavior

Initial open may select the first adaptive overview window so the user can see
the team. Restart and refresh must preserve the leader's current active window
and pane.

## C5. Leader Not In Tmux

If the leader is not in a tmux client, adaptive display degrades to headless,
prints one actionable hint, and emits
`display.adaptive_blocked{reason: leader_not_in_tmux}`. The team still starts or
continues running. The runtime must not auto-wrap the leader into tmux, silently
fall back to Ghostty, or hard-fail the team.

## C6. Tmux Is Not A Leader Prerequisite

The leader can run outside tmux. Missing tmux only disables the display layer;
agent lifecycle and messaging remain usable.

## C7. Default Backend Resolution

When `runtime.display_backend` is omitted, team start and restart resolve it to
`adaptive` and emit
`display.backend_resolved{requested: null, resolved: adaptive, reason: default}`.
The default flip is never silent.

## C8. No Hot-Swap For Running Teams

A running team keeps the backend recorded in state. Changing the default does
not hot-swap an already running team; the new default takes effect only on the
next quick-start or restart.

## C9. Close By Recorded Backend

Close and teardown dispatch by the backend recorded in runtime state, not by the
current default. A team opened as `ghostty_workspace` still uses Ghostty
workspace teardown even after the default becomes `adaptive`.

## C10. Shared 3/3/2 Tiling Primitive

The tiling primitive is shared by `ghostty_workspace` and `adaptive`: at most
three panes per window, `even-horizontal` layout, and window names `overview`,
`overview-2`, `overview-3`, etc. For eight workers the distribution is 3+3+2.
For N=1..8 the per-window pane distribution is identical across both backends.

## C11. Idempotent Rebuild

Adaptive open is idempotent. Restart or refresh detects dead or stale overview
windows and recreates them instead of assuming the old view is alive.

## C12. Rebuild After Leader Rebind

On restart, adaptive display rebuild runs after owner/receiver claim rebind, so
new overview windows are appended to the live leader tmux session, not a stale
previous session.

## C13. Capability Probe

Terminal capability detection goes through one probe abstraction carrying at
least `in_tmux`, `platform`, and capability fields. Display spawning branches on
the probe result, not on hardcoded platform checks such as `sys.platform ==
"darwin"`.

## C14. Windows And WSL

Windows-native and WSL are not implemented in this slice. They return
`not_implemented_this_platform` and degrade to headless with the same event and
hint shape as other adaptive display blocks.

## C15. Structured Events

Backend selection emits `display.backend_resolved`. Successful adaptive spawn
emits `display.adaptive_opened` per worker with at least `worker_id`, `window`,
`pane_id`, `target_worker_session`, and `leader_session`. Restart rebuild emits
`display.adaptive_rebuilt`. Every adaptive display failure emits
`display.adaptive_blocked`; no display degradation is silent.

## C16. Closed Failure Reasons

`display.adaptive_blocked.reason` is one of:
`leader_not_in_tmux`, `split_failed`, `window_create_failed`,
`worker_session_missing`, `not_implemented_this_platform`, or
`aggregator_rebuild_failed`.

## C17. Display Failure Does Not Block Readiness

Adaptive display failure does not block team readiness. Startup and restart
return promptly with the team running headless, preserving the existing
`tmux_headless` fallback behavior.
