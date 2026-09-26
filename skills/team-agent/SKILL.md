---
name: team-agent
description: Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.
requires_team_agent: ">=0.5.0"
last_verified_against: "0.5.66"
---
# Team Agent
Sealed appliance for someone who just got the CLI. Operator handbook (permissions, models, routing, recovery): `references/team-agent-operator.md`.
If `team-agent --version` differs from `last_verified_against`, learn from the live CLI before copying examples:
```bash
team-agent --version
team-agent --help
team-agent doctor --help
```
**Launch** from a tmux-addressable pane: `team-agent claude` or `team-agent codex`, then `team-agent quick-start .team/current`. Do not start a real team from a naked terminal; workers use independent background windows on the workspace-scoped tmux server.
**Operate**
- Dispatch: `team-agent send TO MESSAGE` (example: `team-agent send reviewer MESSAGE`; `--watch-result` is deprecated). After success, do not poll with `sleep` / `status` / `inbox` / `collect`. TO may be an in-team short name or fully qualified `<workspace>::<team>/<agent>`; use qualified across workspaces.
- Inspect: `team-agent status` / `status --json`; `ok: true` plus `ready: false` is not a crash.
- Lifecycle: `restart .` resumes a stopped team; `add-agent NAME --role-file FILE` adds or `--force` recreates one worker; `shutdown --workspace .` stops. Do not shutdown the team to add a worker.
- Roles: every `agents/*.md` must declare boolean `dangerously_skip_permissions`. Never rewrite user-supplied model ids or read `.env` files.
- On a structured failure `action`, run that action first, then stop. Do not guess flags. `coordinator.session_missing` is self-healing; re-check `status --json`, do not shutdown.
The current user-facing agent is the leader (orchestrate only). Workers call `report_result` exactly once. Nested teams: `references/team-in-team.md`.
## Provider Capability Matrix
See `references/team-agent-operator.md` for Claude / Codex / Copilot / Gemini / fake. `quick-start` / `restart` / `claim-leader` JSON is compact by default; pass `--detail` only for internal diagnostics.
| Provider | Resume | Turn-state detection | Per-worker model override | Team Agent fork |
|---|---|---|---|---|
| `grok` | yes (`--resume <id>`, archive-gated) | no | yes (role `model` required) | yes (`--fork-session` + new `--session-id`) |
| `cursor_agent` | yes (`--resume <chatId>`, archive-gated) | no | required on role; same-family catalog id may take effect; unknown id silent-fallback; pane chrome ≠ proof | **no — `CapabilityUnsupported`** |
| `pi` | yes (exact `--session <backing-path>`, not selector/continue) | Unknown (no JSONL turn-state reader) | yes (role `model` / `effort`, catalog-validated) | yes (full snapshot into separate seat/session; exact backing resume) |
Grok / `cursor_agent` have no JSONL turn-state reader (classify → Unknown). Pi `fork-agent SOURCE --as TARGET` requires a captured Pi subscription source and reports both session ids plus the target backing path; it does not mutate the source session.
## Provider Prep
### Cursor provider notes
Frontmatter: `provider: cursor_agent` (not `cursor`; launcher verb `team-agent cursor`), `auth_mode: subscription`, `name:` required, plus `role:`, `tools:`, and boolean `dangerously_skip_permissions:`. Subscription needs no `profile`.
`model:` is required. The flag stays on argv; same-family catalog ids can change pane chrome, while unknown ids silent-fallback without warning. Pick a catalog id; after spawn, `capture-pane` once for chrome. The role field and pane chrome do not prove the live model.
One `cursor_agent` per workspace; a second seat fail-closes because `.cursor/mcp.json` is directory-scoped and overwrites `TEAM_AGENT_ID`. Several Cursor seats need separate workspace directories. Same seat, fresh context: `reset-agent --discard-session`.
`clone-agent` copies the source role but starts fresh. Runtime role replacement: `clone-agent` → `stop-agent` → `remove-agent --confirm` (deletes `.team/dynamic-role-files/`) → write role file → `add-agent --role-file` → dispatch.
- Restart emits `--resume <chatId>` when `store.db` / `meta.json` exist; the gate does not read chat text. Persist anything that must survive restart.
- Delivery sends one Enter; a second Enter interrupts the turn.
- After spawn, the pane footer should show `Cursor Agent v<version>`. Do not use `strings` to probe the binary.
