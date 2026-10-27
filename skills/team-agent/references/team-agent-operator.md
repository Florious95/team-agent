# Team Agent operator reference

Canonical source: `docs/reference/team-agent-operator.md`. The two packaged mirrors under `skills/team-agent/docs/reference/` and `skills/team-agent/references/` are byte-identical; update the source and synchronize both, never maintain divergent operating instructions.

<!-- command-coverage:normative-start -->
## Normative command inventory and compatibility accountability

The current daily forms below are coverage authorities, not permission to execute every command. Provider/permission choices, scope, caller, session and capability guards in this guide still apply. New forms remain honestly `declared_gap` until the strict source/assertion/negative-twin evidence exists; successful Help or a separate finite journey is not automatic promotion. Every compact-Skill command must belong to exactly one coverage bucket.

### Current documented forms
```text
team-agent --version
team-agent agent clone reviewer --as second-reviewer --workspace . --team project-team
team-agent agent fork reviewer --as continued-reviewer --workspace . --team project-team
team-agent agent remove reviewer --confirm --workspace . --team project-team
team-agent agent reset reviewer --discard-session --workspace . --team project-team
team-agent agent restart reviewer --session preserve --workspace . --team project-team
team-agent doctor --workspace . --json
team-agent inbox leader --workspace . --team project-team -n 3
team-agent models --provider NAME
team-agent profile init my-api --auth-mode compatible_api --workspace .
team-agent restart reviewer --workspace . --team project-team
team-agent route add pi -- --verbose
team-agent route clear pi
team-agent route disable
team-agent route enable
team-agent route set pi -- --mode rpc
team-agent route show pi --json
team-agent route status --json
team-agent send reviewer "Review the current change and report the main risk." --workspace . --team project-team
team-agent send reviewer "Review this change" --workspace . --team project-team
team-agent start --from ./my-team --workspace . --team project-team
team-agent start --from .team/current --workspace . --team project-team
team-agent start --help
team-agent start --workspace . --team project-team
team-agent start analyst --provider pi --model openai-codex/gpt-6-luna --bypass false --prompt "Analyze the proposed change"
team-agent start auditor --role-file /absolute/path/to/auditor.md
team-agent start reviewer --effort high --bypass false
team-agent start reviewer --effort high --bypass false --workspace . --team project-team
team-agent start reviewer --workspace . --team project-team
team-agent status --json
team-agent stop --workspace . --team project-team --json
team-agent stop reviewer
team-agent stop reviewer --workspace . --team project-team
team-agent team --help
team-agent team restart --session preserve --workspace . --team project-team
team-agent team restart --workspace . --team project-team
team-agent team restart --workspace . --team project-team --json
```

### Retained compatibility and negative-probe forms — NOT daily recipes
The following old argv remain in the closed-world catalog for existing scripts and historical tests. Some intentionally invalid or removed flag combinations are negative probes, not supported operational advice. Existing covered records keep their original evidence and are not renamed to imply new-command coverage.

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
team-agent status coder
```

The exact test binary's root Help complements this marked inventory; it does not authorize model launches or convert untested forms into covered claims.
<!-- command-coverage:normative-end -->

Only the authorized Leader operates team lifecycle. Check the chosen binary's `start --help` and `team --help`; an old installed CLI or a reused candidate version string is not proof of the intuitive surface. Do not silently install, switch binaries or copy credentials to make an example work.

## Configure once, use start/stop

A source directory contains `TEAM.md` and at least one role:

```text
my-team/
  TEAM.md
  agents/reviewer.md
```

The body of `TEAM.md` is the team goal. Each role body contains instructions; the filename stem is the default agent ID. Record the user's provider and boolean bypass decisions for a new plan. From the native Leader tool context in the workspace:

```sh
team-agent start --from ./my-team --workspace . --team project-team
team-agent start --workspace . --team project-team
team-agent send reviewer "Review the current change and report the main risk." --workspace . --team project-team
team-agent inbox leader --workspace . --team project-team -n 3
team-agent stop --workspace . --team project-team --json
```

Creation needs complete configuration; start is not automatic template generation or state repair. Later start starts stopped non-paused members, including explicitly stopped members, and leaves verified live/drift-free instances unchanged. A no-op does not synchronize role bodies, apply new launch settings or prove tool/business readiness. A queued send is not completion; wait for a natural reply/result.

## Scope and deterministic targets

- Pin `--workspace PATH` and `--team KEY` when operating outside the selected team, across workspaces or when several teams exist.
- `start reviewer.v2` is an agent, even if that directory exists. Team sources use `--from DIR`, `team start DIR` or explicit path syntax such as `./my-team`.
- `restart ID` selects an agent; a same-named directory causes conservative refusal. `agent restart ID` removes the ambiguity.
- Explicit missing paths, unknown flags, conflicts and extra arguments refuse; do not try a guessed CWD or takeover instead.
- `--dry-run` on start/restart has no lifecycle effects. A proposed plan is not final lock-held admission. No eligible targets returns nonzero with the selected scope and skipped seats, not a successful start.

## User decisions and compatibility

Do not silently pick provider, model, effort or bypass. False preserves native permissions; it is not a read-only or sandbox guarantee. Use supported `models --provider NAME` discovery and retain exact model IDs.

Traditional minimal Markdown remains accepted by the original compiler. Its compatibility interpretation is **not a default for a new user plan**: provider defaults to Pi, bypass to false, and unset effort is left to the provider. Claude and Pi workers run only through their contract leaves, which require an exact model ID from `team-agent models --provider claude|pi`; an unset model is refused, never guessed. Team Agent does not infer a role model from TEAM defaults. Legacy explicit TEAM provider_effort remains supported for non-Pi roles; do not invent an effort for unspecified roles.

Supported role metadata:
- `agent_id` (legacy `name`) and `role` identify the seat.
- `provider`, `model`, `effort`, and boolean `dangerously_skip_permissions` select intentional launch settings.
- `auth_mode` and `profile` refer to local provider configuration.
- `communication_mode` selects an admitted communication contract.
- Historical `tools`, `permission_mode` and `label` do not restrict worker tools or permissions.

## Add or wake with start

A RoleOnly seat can be registered from its complete role file. A genuinely absent ID needs a complete file or explicit provider/bypass choices. These Pi/false values are examples of user decisions, not defaults:

```sh
team-agent start auditor --role-file /absolute/path/to/auditor.md
team-agent start analyst --provider pi --model openai-codex/gpt-6-luna --bypass false --prompt "Analyze the proposed change"
```

An imported external role goes into this team's `agents/ID.md`; the original stays untouched. A file already at that destination is used in place. Equal file/CLI definitions are accepted; conflicting provider/model/effort/bypass/prompt/profile values refuse before writes/startup. Omitted model, effort, profile and role body remain unchanged.

Use `agent add` when explicitly requesting strict add-only behavior. It creates and starts a new seat but refuses an occupied ID, including stopped seats. Its disaster-recovery `--force` still requires a positively dead registered old pane and no live target cohort; a live or unknown target refuses. Force is not a role-update or routine restart shortcut.

## Change a registered seat

```sh
team-agent stop reviewer --workspace . --team project-team
team-agent start reviewer --effort high --bypass false --workspace . --team project-team
```

Stop first before applying edited instructions or launch settings. A running seat with requested changes or known drift refuses rather than claiming a successful no-op. `--prompt` replaces permanent role instructions, not a one-time task; send one-time work with `send`. `--bypass` requires explicit true/false. Matching provider is allowed, but changing engines, even by file edits, is refused; use a new ID.

The original role/spec transaction and provider admission remain authoritative. A failure is not an applied update; never assume a partial/unknown native replacement was rolled back or is safe to retry. Inspect the exact scoped receipt, status and doctor first.

## Restart and session policy

```sh
team-agent restart reviewer --workspace . --team project-team
team-agent team restart --workspace . --team project-team
# Strict continuation: refuse before stop if known continuation evidence is absent.
team-agent agent restart reviewer --session preserve --workspace . --team project-team
team-agent team restart --session preserve --workspace . --team project-team
```

New agent/team restart defaults to **auto**: proven resume when possible, otherwise only the provider's existing legal fresh path. Auto is not a guarantee of saved memory, reset permission or a fallback around native capability refusal. Preserve requests strict continuation. The old bare/explicit-path restart bridge retains its original preserve default and output.

Single-agent restart checks scope, role/session backing and capabilities before stop; only confirmed stop allows start. On partial failure, report actual completed steps and inspect; do not blindly reset or replay. Team restart does not reactivate explicitly stopped or paused seats. Whole-team start intentionally activates the selected non-paused roster; neither command silently unpauses.

## Explicit agent operations

```sh
team-agent agent remove reviewer --confirm --workspace . --team project-team
# Only after explicit authorization to discard this worker's context:
team-agent agent reset reviewer --discard-session --workspace . --team project-team
team-agent agent clone reviewer --as second-reviewer --workspace . --team project-team
team-agent agent fork reviewer --as continued-reviewer --workspace . --team project-team
```

Remove/reset/clone/fork retain original confirmation, ownership and provider-capability gates. Clone copies configuration; fork is a new-seat continuation only where admitted. `--in-place` is unsupported. Unsupported native lifecycle is a refusal, never permission to silently use a legacy engine. Reset does not erase every native provider archive and must not be incident recovery's first response.

## Profiles and custom models

Profiles keep authentication/endpoints outside role text. Imported roles use this team's/workspace's profiles, not the external role's original directory. Prepare a named profile locally through public commands; never discover, copy or print external credentials:

```sh
team-agent profile init my-api --auth-mode compatible_api --workspace .
```

A role may explicitly reference `auth_mode: compatible_api` and `profile: my-api`, with the user's chosen provider/bypass. Profile model applies only when the role has no explicit override. Subscription and custom-API capabilities differ; follow actual provider admission, not a generic promise.

## Observe and finish

`status` / `team status` expose the concise node projection; `--detail` does not restore historical internal dumps. Use `doctor` for diagnosis. Check actual readiness and returned recovery actions; unknown evidence is not a stopped/live proof. Stop with `stop --workspace PATH --team KEY --json` and verify scoped residual sessions, processes and owned files. Protect parent/sibling/shared resources.

Workers have only `send_message`, `report_result` and optional `get_team_status`. Report completion through `report_result`; do not promote a worker to lifecycle operator. See [nested teams](https://github.com/Florious95/team-agent/blob/main/skills/team-agent/references/team-in-team.md) and [recovery](https://github.com/Florious95/team-agent/blob/main/skills/team-agent/references/recovery-runbook.md).

## Legacy command bridges

Existing scripts may retain `quick-start`, `shutdown`, and `add-agent/start-agent/stop-agent/remove-agent/reset-agent/clone-agent/fork-agent`. These are compatibility interfaces, not the daily workflow. Use the short/team/agent forms above for new instructions. Do not mechanically rename scripts: legacy start refuses a live seat, new start can prove a no-op; bare/explicit-path legacy restart preserves its default. Old normal command output and failure evidence remain authoritative.
