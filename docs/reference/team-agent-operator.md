# Team Agent operator reference

The assistant you are using is the team **leader**. Workers are described by Markdown files in a team directory; use the CLI for lifecycle operations.

## Plain-text team

Create a directory with `TEAM.md` and at least one `agents/*.md` file:

```text
my-team/
  TEAM.md
  agents/reviewer.md
```

`TEAM.md` contains the team goal. Each role file's body contains that worker's instructions. The filename stem (`reviewer`) is the default worker id. YAML frontmatter is optional; a role can be plain Markdown.

From a tmux-addressable terminal or supported agent pane, run commands in the workspace root (the parent of `my-team`):

<!-- command-coverage:normative-start -->
### Normative command inventory

This inventory is the handbook authority for command coverage. Only the command
lines between the two `command-coverage:normative-*` markers are canonical
argv forms. Examples elsewhere in this handbook may be diagnostics, negative
probes, historical observations, or prose and are not part of the inventory.

```text
team-agent add-agent <agent> --role-file <file>
team-agent add-agent reviewer --role-file .team/current/agents/reviewer.md --workspace .
team-agent approvals
team-agent approvals <agent_id>
team-agent approvals [coder]
team-agent attach-leader
team-agent claim-leader
team-agent claude
team-agent clone-agent <source> --as <new>
team-agent codex
team-agent codex --dangerously-bypass-approvals-and-sandbox
team-agent doctor
team-agent fork-agent <source> --as <new>
team-agent inbox <agent_id> -n 3
team-agent inbox coder -n 3
team-agent profile doctor <name> --workspace . --json
team-agent profile init <name> --auth-mode subscription --workspace .
team-agent profile init claude-default --auth-mode subscription --workspace .
team-agent profile init codex-default --auth-mode subscription --workspace .
team-agent profile init deepseek --auth-mode compatible_api --workspace .
team-agent profile show <name> --workspace . --json
team-agent profile show deepseek --workspace . --json
team-agent quick-start
team-agent quick-start ./roles --team-id alpha
team-agent quick-start .team/alpha
team-agent quick-start .team/current
team-agent quick-start <dir>
team-agent quick-start <plain-text-team-dir>
team-agent remove-agent <agent> --workspace . --confirm
team-agent reset-agent <agent> --discard-session
team-agent restart
team-agent restart .
team-agent restart . --allow-fresh
team-agent restart . --team <session_name_or_team_name>
team-agent send --task task_initial "Start"
team-agent send --watch-result
team-agent send --watch-result coder "Do the bounded task"
team-agent send TO MESSAGE
team-agent send reviewer "..."
team-agent send reviewer "Review this change"
team-agent shutdown --workspace . --keep-logs
team-agent start-agent <agent>
team-agent start-agent <agent_id> --workspace .
team-agent start-agent coder --workspace .
team-agent status
team-agent status --detail --json
team-agent status --json
team-agent status coder
```

The exact frozen CLI root help is a complementary public-surface authority:
`team-agent --help` must expose the root verb for every canonical command that
is not otherwise documented as a compatibility or provider form. The command
coverage gate parses this output from the exact test binary; it never invokes a
provider or treats a diagnostic example as a command approval.

<!-- command-coverage:normative-end -->

A successful `send` can mean only accepted or queued. Wait for the worker's actual reply/result; acceptance is not completion. Use `team-agent status --json` to inspect readiness. `ok: true` with `ready: false` means the command succeeded but the team is not ready; follow the reported action.

## Defaults and optional role fields

No role frontmatter is required. With no explicit overrides, Team Agent uses:

| Setting | Default behavior |
|---|---|
| `provider` | `pi` |
| `model` | Unset by Team Agent; leave model selection to the provider |
| `effort` | Unset by Team Agent; leave effort selection to the provider |
| `dangerously_skip_permissions` | `false`; preserve native permissions |

Do not write `model: null`, `effort: max`, or a bypass setting just to restate a default. The absence of a model or effort override is not a promise about what the provider's own UI eventually displays. Team Agent does not fill role model from TEAM-level defaults. An unspecified role effort defaults to None (native); for non-Pi roles, a legacy explicit TEAM provider_effort remains supported for compatibility.

Optional supported role metadata:

- `agent_id` (or legacy `name`) and `role` for identity.
- `provider`, `model`, and `effort` to explicitly choose supported provider settings.
- `auth_mode` and `profile` to use a provider profile.
- `dangerously_skip_permissions: true` only when the user explicitly wants the provider-supported bypass behavior. The default is false.
- `communication_mode` to select a supported worker communication contract.

Unknown and historical role keys are ignored, not used as a way to restrict tools or permissions. In particular, `tools`, `permission_mode`, and `label` do not configure a worker through role frontmatter.

## Profiles and custom models

A profile keeps provider credentials, endpoint settings, and optional model settings outside role text. This supports subscription profiles and custom/third-party API models, including Claude Code-compatible providers.

Create a local profile, then edit its generated file on the user's machine:

```sh
team-agent profile init my-api --auth-mode compatible_api --workspace .
```

Reference it without copying secrets into Markdown:

```yaml
---
provider: claude_code
auth_mode: compatible_api
profile: my-api
---
Use the configured provider to complete the assigned task.
```

The profile can supply its model when the role has no explicit `model`; an explicit role model remains an override. Use `team-agent models --provider <provider>` to discover provider model IDs when choosing one explicitly. Never print or paste profile secrets into chat or role files.

## Help and worker communication

Use `team-agent --help` or a command's `--help` for the installed CLI syntax. Worker MCP exposes `send_message`, `report_result`, and `get_team_status`; workers use `report_result` to return completion. Nested-team guidance is in [team-in-team.md](team-in-team.md).

Keep role instructions in Markdown. Add metadata only for an intentional override; do not copy internal state fields or old frontmatter templates into new roles.
