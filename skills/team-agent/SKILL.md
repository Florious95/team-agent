---
name: team-agent
description: Use when the user asks to create, operate, inspect, or stop a Team Agent team. Treat the CLI as the public interface.
requires_team_agent: ">=0.5.0"
---
# Team Agent

The current assistant is the **leader**. Workers use separate role files and report through Team Agent. Start from a tmux-addressable terminal or supported agent pane, in the workspace root (the parent of the team directory).

## Decide before creating a team

**The user decides each worker's provider and permission bypass. Do not silently choose Pi, true, or false.** Reuse an explicit user policy when it covers the worker; otherwise explain the choices and ask before starting.

Use existing resources rather than establishing new accounts: `team-agent doctor --workspace . --json` reports discovered tools, but installed does not mean authenticated and an absent entry or unknown auth is not proof of unavailability. For providers supported by `models`, use `team-agent models --provider NAME` to check exact model IDs. Do not inspect credentials, automatically log in, or silently substitute a provider after a failed probe.

| Choice | What to explain |
|---|---|
| Pi | One Agent tool can use several model catalogs; explicit models require a qualified ID such as `openai-codex/gpt-6-luna` |
| Codex | The user's existing Codex workflow, native tools, permissions and resumable sessions |
| Claude / Claude Code | The user's existing Claude workflow, models, native tools and session behavior |
| Bypass `true` | Fewer permission interruptions, but broader automatic execution authority |
| Bypass `false` | Keep the selected tool's native permissions; human confirmation may interrupt unattended work |

False is **not** a read-only or sandbox guarantee. Native permission capabilities differ between tools. Suggest combinations based on the user's installed tools, subscriptions and task: for example, an explicitly chosen Codex implementer and Claude reviewer, or two Pi workers when those are the available resources. Different workers may use different providers; an existing worker cannot change provider.

## Create and start

Keep the goal in `TEAM.md` and each worker's instructions in `agents/*.md`. The filename stem is the worker's in-team short name.

```text
.team/current/
  TEAM.md
  agents/reviewer.md
```

For a **new plan**, record the user's two decisions in every role. This example assumes the user selected Pi and bypass false; it is not a default:

```markdown
---
provider: pi
dangerously_skip_permissions: false
---
Review the change and report concrete regression risks.
```

```sh
team-agent quick-start .team/current
team-agent send reviewer "Review this change"
team-agent inbox reviewer -n 3
```

Traditional file-based quick-start/compile behavior remains supported, including legacy minimal role documents; no conversion to CLI-created roles is required. Legacy interpretation is not permission to silently choose settings for a new user plan. Model and effort may remain unset; preserve the existing file compiler's native/default interpretation rather than inventing a model or effort.

To add a **new** seat to an existing team, provider and bypass must be explicitly defined in the CLI or role file. A complete traditional role file needs no repeated flags. If both sources define a field, equal values are accepted and **conflicting values are rejected before any write or startup**:

```sh
# These values must be the user's choices, not inferred defaults.
team-agent add-agent analyst --provider pi --bypass true --prompt "Analyze the proposed change"
# auditor.md already contains the user's provider and bypass decisions.
team-agent add-agent auditor --role-file /absolute/path/to/auditor.md
```

Add creates and immediately starts the seat. An occupied ID is rejected, including stopped seats; add never overwrites an existing worker. External role files are imported into the team's `agents/ID.md` and left untouched. A file already at that path is used in place, without copying itself.

## Change and start an existing worker

**Stop first if it is running.** Start never implicitly kills or restarts a running worker:

```sh
team-agent stop-agent reviewer
team-agent start-agent reviewer --effort high --bypass true
team-agent stop-agent reviewer
team-agent start-agent reviewer --prompt "Review only regression risks; do not change product code"
```

Start updates the same role file and recompiles it before normal startup. Omitted options preserve the file's values and body; `--prompt` replaces the permanent role body, not a one-time task message. `--bypass true` and `--bypass false` are explicit values, not bare switches. Model, effort and an existing profile can also be set. Direct file edits followed by start use the same reload path.

A different `--provider`, including a manually edited role provider, is rejected before launch. A matching provider is allowed. Create a separate ID to use a different engine. If startup fails, the role file and runtime spec are restored to their pre-command contents; the command reports failure, not an applied update.

Profiles keep provider authentication, endpoints and custom-model settings local. Imported roles use this team's/workspace's `profiles`, never the original external directory. Prepare a named profile locally through the public profile commands; do not search for or copy external credentials. Never put secrets in `TEAM.md` or a role file. See [the operator reference](references/team-agent-operator.md) for existing profile and field details. Supported metadata also includes `agent_id` (legacy `name`), `role`, `auth_mode`, and `communication_mode`; old `tools`, `permission_mode`, and `label` keys do not configure a worker.

## Inspect and stop

Use `team-agent status --json` for readiness and the installed command's `--help` for syntax. `ok: true` with `ready: false` means the team is not ready; follow the reported action. Accepted/queued send is not the worker's reply: wait for a natural response or result. Use `<workspace>::<team>/<agent>` when a fully qualified recipient is needed.

Worker MCP provides `send_message`, `report_result`, and `get_team_status`. Lifecycle actions remain on the authorized CLI. See [nested teams](references/team-in-team.md) only when needed. Stop the selected team with `team-agent shutdown --workspace .` when authorized.

---
This skill is a concise public guide. Do not infer a tested version from package metadata; verify the installed CLI with `team-agent --version` when version-specific behavior matters.
