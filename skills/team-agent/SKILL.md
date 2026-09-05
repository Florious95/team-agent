---
name: team-agent
description: Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.
requires_team_agent: ">=0.5.0"
last_verified_against: "0.5.73"
---

# Team Agent

Use this skill only for Team Agent operation. The detailed operator handbook (permissions, models, routing, and recovery) is in [the operator reference](references/team-agent-operator.md). Nested teams have a separate [nested-team reference](references/team-in-team.md).

If `team-agent --version` does not match `last_verified_against`, inspect the live CLI before copying examples:

```bash
team-agent --version
team-agent --help
team-agent doctor --help
```

## Pi TeamMate startup

For a Pi worker, use this existing three-step flow:

1. Discover exact model ids: `team-agent models --provider pi` (add `--search TEXT` when useful).
2. Use the existing lifecycle command that matches the actual state: `team-agent quick-start` for a new team, `team-agent add-agent NAME --role-file FILE` for a new worker, or `team-agent start-agent NAME` for a missing worker.
3. Send work through the canonical route: `team-agent send AGENT MESSAGE`.

Pi roles preserve the direct Pi login, model defaults, extensions/plugins, skills, context, trust, and tools. Team Agent adds only per-seat identity/session isolation. `model` and `effort` are optional explicit pass-throughs. Use the shared role schema, a block-style `tools` list containing `mcp_team`, and the required boolean `dangerously_skip_permissions`.

## Operate

- Send: `team-agent send TO MESSAGE` using an in-team short name or fully qualified `<workspace>::<team>/<agent>`; these are co-equal positional TO forms. For example, `team-agent send reviewer "..."` uses an in-team short name. Do not poll after success.
- Inspect: `team-agent status` or `team-agent status --json`; `ok: true` with `ready: false` is not a crash.
- `quick-start` / `restart` / `claim-leader` JSON is compact by default (`ok`, `status`/`reason`, next action, attach/send, and `readiness.all_workers_spawned`); pass `--detail` only for internal diagnostics.
- On a structured failure with an `action`, run that action first, then stop. Never guess flags.
- Never rewrite a user-supplied model id or read `.env` files.

Workers report completion through `mcp_team` and call `report_result` exactly once.
