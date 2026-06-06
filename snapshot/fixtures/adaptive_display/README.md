# Adaptive Display Fixtures

These fixtures are read-only layout metadata for Gap 41 adaptive display work.
They contain tmux/session/window/pane metadata only, not provider transcripts,
profiles, environment files, or secrets.

## Current ghostty_workspace layout

- `raw_tmux/current_tmux_sessions.tsv`: live tmux sessions visible during capture.
- `raw_tmux/current_tmux_windows_all.tsv`: live tmux windows across sessions.
- `raw_tmux/current_tmux_panes_all.tsv`: live tmux panes across sessions.
- `raw_tmux/team_refactor_windows.tsv`: filtered windows for the active
  `team-refactor-maintainability` team and display sessions.
- `raw_tmux/team_refactor_panes.tsv`: filtered panes for the same team and
  display sessions.
- `raw_tmux/team_refactor_display_sessions.txt`: display-linked sessions present
  at capture time.

Layout point: the live `ghostty_workspace` backend has six workers in the base
team session, one worker per tmux window. It also has one linked display session
per worker. Each linked session exposes the same six worker windows and marks one
worker window active.

## State relationship sample

- `state/leader_worker_relation.selected.json`: selected public-safe state fields
  showing the leader pane and each worker's display metadata.
- `state/display_state_selected.json`: selected display metadata per worker.

Layout point: the leader pane is separate from worker display panes. The state
records `display_backend: ghostty_workspace`, a workspace aggregator session,
and two intended workspace windows: `overview` for the first three workers and
`overview-2` for the next three.

## Aggregator liveness probe

- `raw_tmux/missing_workspace_aggregator_probe.*`: `tmux has-session` result for
  the state-recorded workspace aggregator session.

Layout point: state records
`team-refactor-maintainability__display__workspace__8897d78f`, but the live tmux
server did not have that aggregator session during capture. This is evidence for
CR review of stale display metadata versus live tmux layout.

## Plain tmux 3+3+2 comparison

- `plain_tmux_332/windows.tsv`: disposable plain tmux session windows.
- `plain_tmux_332/panes.tsv`: disposable plain tmux session panes.
- `plain_tmux_332/README.meta.txt`: capture metadata and source commands.

Layout point: the adaptive display target for eight workers can be represented
as three ordinary tmux windows with pane counts 3, 3, and 2. This fixture is a
plain-tmux comparison sample, not a product acceptance test.
