---
name: team-agent
description: Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.
requires_team_agent: ">=0.5.0"
last_verified_against: "0.5.66"
---

# Team Agent

Sealed appliance for someone who just got the CLI. Operator handbook (permissions, models, routing, recovery): `docs/reference/team-agent-operator.md`.

If `team-agent --version` does not match `last_verified_against`, do not copy examples as current truth. Learn from the live CLI:

```bash
team-agent --version
team-agent --help
team-agent doctor --help
```

**Launch** from a tmux-addressable pane: `team-agent claude` or `team-agent codex`, then `team-agent quick-start .team/current`. Do not start a real team from a naked terminal. Existing tmux/Ghostty layouts are valid if `quick-start` runs from the leader pane.

**Operate**

- Dispatch: `team-agent send TO MESSAGE` (positional TO; `--watch-result` is deprecated). After success, do not poll with `sleep` / `status` / `inbox` / `collect`.
- Inspect: `team-agent status` / `status --json`. `ok: true` plus `ready: false` is not a crash.
- Lifecycle: `restart .` resumes a stopped team; `add-agent NAME --role-file FILE` adds or `--force` recreates one worker; `shutdown --workspace .` stops. Do not shutdown the whole team to add a worker.
- Roles: every `agents/*.md` must declare boolean `dangerously_skip_permissions`. Never rewrite a user-supplied model id. Never read `.env` files.

**On failure:** if the CLI prints a structured `action`, run that `action` first, then stop. Do not guess flags. `coordinator.session_missing` is a self-healing transient — re-check `status --json`; do not shutdown because of it.

The current user-facing agent is the leader (orchestrate only). Workers call `report_result` exactly once. Nested teams: `skills/team-agent/references/team-in-team.md`.
