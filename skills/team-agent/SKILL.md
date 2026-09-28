---
name: team-agent
description: Use when the user asks to create, operate, inspect, or stop a Team Agent team. Treat the CLI as the public interface.
requires_team_agent: ">=0.5.0"
---
# Team Agent

The current assistant is the **leader**. Workers use separate role files and report through Team Agent. Start from a tmux-addressable terminal or supported agent pane, in the workspace root (the parent of the team directory).

## Create and start

A team can be plain text; no YAML frontmatter is required:

```text
.team/current/
  TEAM.md                 # the team's goal
  agents/reviewer.md      # the worker's instructions
```

Put the goal in `TEAM.md` and the worker's instructions in each `agents/*.md` body. Use the filename stem as the worker id and in-team short name in commands such as `send` and `inbox`.

```sh
team-agent quick-start .team/current
team-agent send reviewer "Review this change"
team-agent inbox reviewer -n 3
team-agent add-agent analyst --role-file /absolute/path/to/analyst.md
team-agent shutdown --workspace .
```

The `reviewer` argument is an in-team short name; use `<workspace>::<team>/<agent>` when the recipient needs a fully qualified identity. A successful `send` may mean only queued; it is not the worker's reply. Wait for a natural response or result before treating the task as complete.

## Role defaults

All role metadata is optional. With no overrides:

| Setting | Default |
|---|---|
| Provider | `pi` |
| Model | unset; let the provider choose |
| Effort | unset; let the provider choose |
| Permission bypass | `false`; keep provider-native permissions |

Add frontmatter only when a role needs an override. Supported fields include `agent_id` (legacy alias: `name`), `role`, `provider`, `model`, `effort`, `auth_mode`, `profile`, `dangerously_skip_permissions`, and `communication_mode`. Old `tools`, `permission_mode`, and `label` role keys do not configure a worker.

Profiles keep provider authentication, endpoint, and custom-model settings local. Never put secrets in `TEAM.md` or a role file. See [the operator reference](references/team-agent-operator.md) for profile setup and field details.

## Inspect

Use `team-agent status --json` for readiness and `team-agent --help` for current command syntax. `ok: true` with `ready: false` means the command succeeded but the team is not ready; follow the reported action.

Worker MCP provides `send_message`, `report_result`, and `get_team_status`. Lifecycle actions remain on the authorized CLI. See [nested teams](references/team-in-team.md) only when the task needs them.

---
This skill is a concise public guide. Do not infer a tested version from package metadata; verify the installed CLI with `team-agent --version` when version-specific behavior matters.
