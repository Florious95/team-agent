---
name: team-agent
description: Use when the user asks to create, operate, inspect, or stop a Team Agent team. Treat the CLI as the public interface.
requires_team_agent: ">=0.5.0"
---
# Team Agent

Use the authorized **Leader's** native tool context for lifecycle operations. Workers report through their team-scoped MCP tools; loading this skill does not promote a worker to Leader. Operate in the workspace root, not inside another team's role directory.

## Check the actual CLI

Check `team-agent --version`, `team-agent start --help` and `team-agent team --help` before using this guide. It requires the intuitive `start`, `stop`, `team` and `agent` surface. A version string or this skill's compatibility floor is not proof those commands exist. If missing, ask for an authorized installation of a supporting build; do not silently switch binaries, rewrite bindings or fall back to an old creation recipe.

## User decisions come first

**The user decides each worker's provider and permission bypass. Do not silently choose Pi, true, or false.** Reuse an explicit user policy when it covers the worker; otherwise explain the choices and ask before starting.

Use existing resources rather than establishing accounts. `team-agent doctor --workspace . --json` reports discovered tools; installed is not authenticated, and an unknown probe is not proof of unavailability. Use `team-agent models --provider NAME` where supported for exact model IDs. Do not inspect credentials, automatically log in, or silently substitute providers.

| Choice | Explain |
|---|---|
| Pi | Several model catalogs; every Pi worker needs an exact qualified model ID such as `openai-codex/gpt-6-luna` from `team-agent models --provider pi`; Pi has no permission prompt to bypass, so keep bypass `false` |
| Claude | Every Claude worker needs an exact model ID from `team-agent models --provider claude`; existing subscription login only |
| Codex / Grok | Every Codex or Grok worker needs an exact model ID from `team-agent models --provider codex` or `team-agent models --provider grok`; existing subscription login only |
| Other native CLIs | The user's existing workflow, native permissions, model catalog and supported session capabilities |
| Bypass `true` | Fewer interruptions, broader automatic authority |
| Bypass `false` | Native permissions remain; human confirmation may interrupt unattended work |

False is **not** a read-only or sandbox guarantee. Different workers may use different providers; an existing worker cannot change provider. Do not invent a model or effort when the user left it unset.

## Prepare once, start every day

Keep the goal in `TEAM.md` and each worker's instructions in `agents/*.md`. The filename stem is the worker's in-team short name:

```text
.team/current/
  TEAM.md
  agents/reviewer.md
```

Record provider, model and boolean bypass in every new role. This example assumes the user explicitly chose Pi, a model copied from `team-agent models --provider pi`, and bypass false; it is not a default:

```markdown
---
provider: pi
model: openai-codex/gpt-6-luna
dangerously_skip_permissions: false
---
Review the change and report concrete regression risks.
```

From the authorized Leader context, select the intended workspace/team:

```sh
team-agent start --from .team/current --workspace . --team project-team
team-agent start --workspace . --team project-team
team-agent send reviewer "Review this change" --workspace . --team project-team
team-agent inbox leader --workspace . --team project-team -n 3
```

The first `start` creates a complete configured team through the original creation path. Later `start` keeps physically verified, drift-free live seats unchanged and starts stopped non-paused seats, including explicitly stopped seats. It is not a reset. Missing configuration, ambiguous scope or unknown physical identity is a refusal, not permission to overwrite state. `--dry-run` plans without lifecycle effects; final locked admission is still deferred.

Accepted/queued send is **not** the worker's answer. Wait for a natural reply/result. `ok: true` with pending readiness is not business readiness. A live no-op does not synchronize edited role bodies or apply changed launch settings.

## Stop, wake and restart

```sh
team-agent stop reviewer --workspace . --team project-team
team-agent start reviewer --workspace . --team project-team
team-agent restart reviewer --workspace . --team project-team
team-agent stop --workspace . --team project-team --json
team-agent start --workspace . --team project-team
team-agent team restart --workspace . --team project-team --json
```

| Intent | Rule |
|---|---|
| `start AGENT` | Wake a stopped registered seat; verified live/no-drift is a no-op. A RoleOnly seat can be added from its complete role file |
| `start` | Explicitly activate the selected non-paused roster; do not use it unless that activation is authorized |
| `stop AGENT` / `stop` | Retain saved session associations; whole-team stop also checks scoped residual resources |
| `restart AGENT` / `team restart` | Default `--session auto`: resume when proven, otherwise only the original legal fresh fallback. **Auto does not guarantee preserved memory** |
| `--session preserve` | Require available continuation evidence; refuse before stopping when continuation is known unavailable |

Team restart preserves explicitly stopped and paused seats; use `start` when intentionally waking the roster. Paused seats are not implicitly unpaused. A same-named directory makes `restart ID` ambiguous: use `agent restart ID`. `reviewer.v2` remains an agent ID; choose a team source with `--from DIR`, `team start DIR` or an explicit path such as `./roles`.

For strict recovery, prefer `team-agent team restart --session preserve --workspace . --team project-team`. Do not convert a refusal or partial/unknown replacement into reset, force or blind retry. Inspect `status`/`doctor` in the same scope first. Native unsupported capabilities remain refusals, never a legacy fallback.

## Add or change one worker

With the user's provider/bypass choices, `start` unifies adding an absent seat and waking an existing one:

```sh
team-agent start auditor --role-file /absolute/path/to/auditor.md
team-agent start analyst --provider pi --model openai-codex/gpt-6-luna --bypass false --prompt "Analyze the proposed change"
team-agent stop reviewer
team-agent start reviewer --effort high --bypass false
```

The Pi/false example above still requires the user's explicit choices. File/CLI conflicts are rejected before writing or startup. Omitted settings/body are preserved; `--prompt` replaces the permanent role body, not a one-time task. Stop a running worker before changing settings or instructions. Provider changes are refused. Startup failure restores role/spec bytes and reports failure, not an applied update.

Use `agent add/remove/reset/clone/fork` only for those explicit operations. Reset needs intentional approval and `--discard-session`; fork requires `--as NEW_ID`, and `--in-place` is not supported. See [the operator reference](references/team-agent-operator.md) for capability, profile, force and import boundaries. Never put secrets in role text or copy authentication from another directory.

## Optional native CLI argv routing

Default OFF; no workspace or provider login is needed to manage routes:

```sh
team-agent route set pi -- --mode rpc
team-agent route enable
team-agent route status --json
team-agent route show pi --json
team-agent route add pi -- --verbose
team-agent route clear pi
team-agent route disable
```

`set` replaces one mapping, `add` appends without deduplication, `clear` removes it; none changes the switch. `enable`/`disable` preserve mappings in `~/.team-agent/argv-routing.json`. `TEAM_AGENT_CLI_ARGV_ROUTING` overrides the switch: `1/true/on`, `0/false/off` (case-insensitive, trimmed); invalid values disable routing. Check `persisted_enabled`, `effective_enabled` and `override_status`.

Keys: `claude`, `codex`, `copilot`, `gemini_cli`, `grok`, `pi`. Claude/Claude Code share `claude`; aliases `claude_code/claude-code` are accepted. Everything after the first `--`, including `--help`, `--json`, empty strings and `{workspace}`, is literal argv. No shell expansion. Do not route secrets or override framework-managed session/MCP/permission flags.

Mappings affect the next actual native spawn, not running/attached processes, dry-run or helper probes. Invalid enabled configuration refuses spawn; an emergency environment OFF avoids reading it, while `route disable` will not overwrite a broken file. Pi `--mode rpc` does not provide an RPC host or replace Team Agent's transport.

## Observe, communicate and clean up

Use `team-agent status --json` (or `team status`) for the concise node projection and `doctor` for diagnosis; `--detail` does not restore old internal status dumps. Use `<workspace>::<team>/<agent>` for an explicitly qualified logical recipient, or the in-team short name. Workers have exactly `send_message`, `report_result` and optional `get_team_status`; lifecycle stays on the authorized CLI. Read [nested teams](references/team-in-team.md) only when explicitly requested and [recovery](references/recovery-runbook.md) for incidents.

Finish with `team-agent stop --workspace . --team project-team --json` when authorized. Check the actual scoped residual report; exit0 alone is not proof of zero resources. Do not kill shared servers or touch sibling workspaces.

## Compatibility bridges, not the daily workflow

Old commands remain available for existing scripts; their output/defaults are not silently rewritten:

| Compatibility entry | Preferred daily surface |
|---|---|
| `quick-start` | `start --from TEAMDIR` for creation and later `start` for activation |
| `shutdown` | `stop` / `team stop` |
| `add-agent` / `start-agent` / `stop-agent` | `agent add` for strict add, or `start AGENT` / `stop AGENT` |
| `remove-agent` / `reset-agent` / `clone-agent` / `fork-agent` | The corresponding `agent` operation |
| Bare or explicit-path `restart` | Frozen preserve bridge; deliberate `team restart` defaults to auto |

These are not all byte-equivalent aliases: legacy single-seat start refuses a running seat, while new start can prove a no-op. Do not mechanically rename automation, assume unsupported provider capabilities, or infer deployment from package metadata.
