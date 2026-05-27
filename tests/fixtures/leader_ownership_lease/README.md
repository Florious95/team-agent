# Leader Ownership Lease Fixtures

Captured for Slice 0.2.4-1 S0 from the real workspace:

`/Users/alauda/Documents/code/agent前沿探索/多agent协作`

No acceptance tests are included in this fixture-only slice.

## Raw Command Fixtures

Each command fixture has four files:

- `*.meta.txt`: command, cwd, capture time, and relevant env.
- `*.stdout`: verbatim stdout.
- `*.stderr`: verbatim stderr.
- `*.exitcode`: process exit code.

Fixtures:

- `raw_commands/01_identity_team_refactor.*`
  - Surface: identity output for the broken `team-refactor-maintainability` binding.
  - Command: `team-agent identity --workspace <workspace> --team team-refactor-maintainability --json`

- `raw_commands/02_status_workspace.*`
  - Surface: status JSON with stale leader receiver and coordinator/tmux state.
  - Command: `team-agent status --workspace <workspace> --json`

- `raw_commands/03_takeover_team_refactor_claimed.*`
  - Surface: takeover claims ownership but does not repair the stale leader receiver.
  - Command: `TEAM_AGENT_LEADER_PANE_ID=%1827 TEAM_AGENT_LEADER_PROVIDER=claude team-agent takeover --workspace <workspace> --team team-refactor-maintainability --confirm --json`

- `raw_commands/04_attach_leader_workspace.*`
  - Surface: attach-leader refusal with `leader_uuid_missing`.
  - Command: `team-agent attach-leader --workspace <workspace> --json`

- `raw_commands/05_claim_leader_team_refactor_no_ambiguous.*`
  - Surface: claim-leader refusal with `no_ambiguous_candidates`.
  - Command: `TEAM_AGENT_LEADER_PANE_ID=%1827 TEAM_AGENT_LEADER_PROVIDER=claude team-agent claim-leader --workspace <workspace> --team team-refactor-maintainability --confirm --json`

Additional diagnostics:

- `raw_commands/03a_takeover_team_refactor_no_caller_identity.*`
  - Surface: takeover run without leader identity env returns `no_caller_identity`.

- `raw_commands/05a_claim_leader_team_refactor_no_caller_pane.*`
  - Surface: claim-leader run without leader pane env returns `no_caller_pane`.

## State Snapshots

Only `team_owner`, `leader_receiver`, and `coordinator` were extracted.
No profile env files or secrets were read.

- `state_snapshots/runtime_state.selected-fields.json`
  - Source: `<workspace>/.team/runtime/state.json`
  - Surface: top-level `team_owner` points at `%1827`, while `leader_receiver.pane_id` still points at dead `%648`.

- `state_snapshots/team_refactor_state.selected-fields.json`
  - Source: `<workspace>/.team/runtime/teams/team-refactor-maintainability/state.json`
  - Surface: team-specific snapshot still has `team_owner: null`, while `leader_receiver.pane_id` still points at dead `%648`.

## Dead Pane Scenario

- `dead_pane_%648_scenario.txt`
  - Surface: minimal reproducible stale-pane evidence.
  - Shows state still points at `%648`, while `tmux list-panes` no longer contains `%648`.

## Worktree Note

The worktree was created from local `main` at `149d4e6`.
`git pull --ff-only` from `/Users/alauda/Documents/code/team-agent-public` failed because the SSH connection closed before the remote could be read.
