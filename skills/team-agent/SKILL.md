---
name: team-agent
description: Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.
requires_team_agent: ">=0.5.0"
last_verified_against: "0.5.69"
---

# Team Agent

Sealed appliance for someone who just got the CLI. Operator handbook (permissions, models, routing, recovery): `docs/reference/team-agent-operator.md`.

If `team-agent --version` does not match `last_verified_against`, do not copy examples as current truth. Learn from the live CLI:

```bash
team-agent --version
team-agent --help
team-agent doctor --help
```

**Operate**

- Dispatch: `team-agent send TO MESSAGE` (positional TO; `--watch-result` is deprecated). After success, do not poll with `sleep` / `status` / `inbox` / `collect`.
  TO has two co-equal logical forms: an **in-team short name** (`team-agent send reviewer "..."`) and a fully qualified `<workspace>::<team>/<agent>`. Use the qualified form across workspaces.
- Inspect: `team-agent status` / `status --json`. `ok: true` plus `ready: false` is not a crash.
- Lifecycle: `restart .` resumes a stopped team; `add-agent NAME --role-file FILE` adds or `--force` recreates one worker; `shutdown --workspace .` stops. Do not shutdown the whole team to add a worker.
- Roles: every `agents/*.md` must declare boolean `dangerously_skip_permissions`. Never rewrite a user-supplied model id. Never read `.env` files.

**On failure:** if the CLI prints a structured `action`, run that `action` first, then stop. Do not guess flags. `coordinator.session_missing` is a self-healing transient — re-check `status --json`; do not shutdown because of it.

The current user-facing agent is the leader (orchestrate only). Workers call `report_result` exactly once. Nested teams: `skills/team-agent/references/team-in-team.md`.

## Provider Capability Matrix

Claude / Codex / Copilot / Gemini / fake: `docs/reference/team-agent-operator.md`. These two are in the runtime but were missing from that table:

| Provider | Resume | Turn-state detection | Per-worker model override | Native session fork |
|---|---|---|---|---|
| `grok` | yes (`--resume <id>`, archive-gated) | no | yes (role `model` required) | yes (`--fork-session` + new `--session-id`) |
| `cursor_agent` | yes (argv `--resume <chatId>`, archive-gated) | no | required on role; same-family catalog id can take effect; unknown id silent-fallback; pane chrome ≠ proven live | **no — `CapabilityUnsupported`** |

Grok / `cursor_agent` have no JSONL turn-state reader (classify → Unknown).

## Provider Prep

### Pi TeamMate roles

Use the standard role schema with `provider: pi`, a block-style `tools` list containing `mcp_team`, and the required boolean `dangerously_skip_permissions`. That role boolean is the only bypass setting; `TEAM.md` `dangerous_auto_approve` is compatibility input, not a second user-facing switch.

Pi TeamMates keep the same login, model, extensions/plugins, skills, context files, project trust, and tools as a direct `pi` run on that machine. Team Agent only appends the role contract, a per-seat Team MCP registration, and per-seat session storage; do not duplicate those direct Pi settings in the role. `model` and `effort` remain optional compatibility overrides, and explicit values are passed through.

### Cursor provider notes

Frontmatter: `provider: cursor_agent` (not `cursor`; launcher verb is `team-agent cursor`), `auth_mode: subscription`, `name:` required (omit → `missing front matter field name`). Also required: `role:`, `tools:`, `dangerously_skip_permissions:` (bool). Subscription needs no `profile`.

`model:` is required (omit compile-fails; blocks a silent builtin `sonnet-4-thinking`). The flag stays on argv. Same-family catalog ids can change pane chrome. An id not in that provider's catalog silent-falls back (landing not stable; no events/stderr/pane warning). Pick names from the catalog; after spawn, `capture-pane` once for chrome. Do not treat the role field or pane chrome as proof of the live model.

One `cursor_agent` per **provider-config overlay** under `<workspace>/.team/runtime/provider-config/<id>/cursor`. Same workspace may run multiple cursor seats; each writes `TEAM_AGENT_ID` into that project's `.cursor/mcp.json`. Spawn points `--workspace` there and `--add-dir` at the real workspace. `HOME` stays the user home. If isolation is off or cannot be materialized, adding a second seat still fail-closes:

```
error: cursor_agent seat already occupies this workspace
reason: <workspace>/.cursor/mcp.json is directory-scoped; a second seat overwrites TEAM_AGENT_ID (last-writer)
action: do not add another CursorAgent in this workspace until per-seat MCP identity is isolated
```

Do not share `<workspace>/.cursor/mcp.json` for team_orchestrator identity. Same seat, fresh context → `reset-agent --discard-session`.

`clone-agent` copies the source role (provider unchanged). Runtime add: `clone-agent` → `stop-agent` → `remove-agent --confirm` (deletes `.team/dynamic-role-files/`) → write the role file → `add-agent --role-file` → dispatch.

- Restart emits `--resume <chatId>` when `store.db`/`meta.json` exist; the gate does not read chat text. Persist anything that must survive restart.
- Delivery sends one Enter; a second Enter interrupts the turn.
- After spawn, the pane footer should show `Cursor Agent v<version>`. Do not use `strings` to probe the binary.
