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

```sh
team-agent quick-start my-team
team-agent send reviewer "Review the current change and report the main risk."
team-agent inbox reviewer -n 3
# The role file already records the user's provider and bypass choices.
team-agent add-agent analyst --role-file /absolute/path/to/analyst.md
team-agent shutdown --workspace .
```

A successful `send` can mean only accepted or queued. Wait for the worker's actual reply/result; acceptance is not completion. Use `team-agent status --json` to inspect readiness. `ok: true` with `ready: false` means the command succeeded but the team is not ready; follow the reported action.

## User decisions and legacy compatibility

For a new team plan, explicitly record each user's provider and bypass decisions; do not silently select either. Existing file-based compilation remains compatible, including minimal documents. The following table describes legacy interpretation, not defaults to choose on the user's behalf:

| Setting | Default behavior |
|---|---|
| `provider` | `pi` |
| `model` | Unset by Team Agent; leave model selection to the provider |
| `effort` | Unset by Team Agent; leave effort selection to the provider |
| `dangerously_skip_permissions` | `false`; preserve native permissions |

Do not invent `model: null` or `effort: max`. Record the user's bypass decision explicitly in a new role, even when it is false. The absence of a model or effort override is not a promise about what the provider's own UI eventually displays. Team Agent does not fill role model from TEAM-level defaults. An unspecified role effort defaults to None (native); for non-Pi roles, a legacy explicit TEAM provider_effort remains supported for compatibility.

Optional supported role metadata:

- `agent_id` (or legacy `name`) and `role` for identity.
- `provider`, `model`, and `effort` to explicitly choose supported provider settings.
- `auth_mode` and `profile` to use a provider profile.
- `dangerously_skip_permissions: true` or `false` records the user's explicit permission choice. False preserves native permissions; it is not a read-only or sandbox guarantee.
- `communication_mode` to select a supported worker communication contract.

Unknown and historical role keys are ignored, not used as a way to restrict tools or permissions. In particular, `tools`, `permission_mode`, and `label` do not configure a worker through role frontmatter.

## Add and start

`add-agent ID --provider NAME --bypass true|false` creates and starts a new seat without a prepared file. A traditional `add-agent ID --role-file FILE` needs no repeated flags when the file already defines provider and boolean bypass. Missing decisions may be supplied by CLI; equal file/CLI values are accepted, but conflicting definitions (including model, effort, prompt and profile) are rejected before writing or startup. Model, effort, prompt and profile remain optional. External role-file inputs are imported into the selected team's `agents/ID.md`; the original stays untouched. A file already there is used in place. Occupied IDs are rejected, never force-recreated.

`start-agent ID` starts an existing stopped seat; it refuses a running seat, so use `stop-agent ID` first. Optional model, effort, bypass, prompt and profile update that same role document. Omitted fields and body are preserved; prompt replaces the body. A matching provider is allowed; any engine change, including manual file edits, is refused. Startup failure restores role and runtime spec bytes, and reports failure. File edits followed by start use the same compiler reload path.

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
dangerously_skip_permissions: false
auth_mode: compatible_api
profile: my-api
---
Use the configured provider to complete the assigned task.
```

The profile can supply its model when the role has no explicit `model`; an explicit role model remains an override. Use `team-agent models --provider <provider>` to discover provider model IDs when choosing one explicitly. Never print or paste profile secrets into chat or role files.

## Help and worker communication

Use `team-agent --help` or a command's `--help` for the installed CLI syntax. Worker MCP exposes `send_message`, `report_result`, and `get_team_status`; workers use `report_result` to return completion. Nested-team guidance is in [team-in-team.md](team-in-team.md).

Keep role instructions in Markdown. Record explicit provider/bypass decisions for new plans, and add other metadata only for intentional settings; do not copy internal state fields into roles.
