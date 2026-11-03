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
team-agent models --provider claude
team-agent models --provider codex
team-agent models --provider grok
team-agent models --provider pi
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
team-agent start analyst --provider pi --bypass false --prompt "Analyze the proposed change"
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

### Supplemental canonical forms and negative probes — NOT blanket execution authorization
The following forms supplement the daily recipes. Intentionally invalid flag combinations remain negative probes, not supported operational advice. Historical compatibility evidence is not renamed to claim coverage for canonical spellings; new forms require their own evidence.

```text
team-agent start <agent> --role-file <file>
team-agent start reviewer --role-file .team/current/agents/reviewer.md --workspace .
team-agent approvals
team-agent approvals <agent_id>
team-agent approvals [coder]
team-agent claim-leader
team-agent claude
team-agent agent clone <source> --as <new>
team-agent codex
team-agent codex --dangerously-bypass-approvals-and-sandbox
team-agent doctor
team-agent agent fork <source> --as <new>
team-agent inbox <agent_id> -n 3
team-agent inbox coder -n 3
team-agent profile doctor <name> --workspace . --json
team-agent profile init <name> --auth-mode subscription --workspace .
team-agent profile init claude-default --auth-mode subscription --workspace .
team-agent profile init codex-default --auth-mode subscription --workspace .
team-agent profile init deepseek --auth-mode compatible_api --workspace .
team-agent profile show <name> --workspace . --json
team-agent profile show deepseek --workspace . --json
team-agent start --from .
team-agent start --from ./roles --team alpha
team-agent start --from .team/alpha
team-agent start --from .team/current
team-agent start --from <dir>
team-agent start --from <plain-text-team-dir>
team-agent agent remove <agent> --workspace . --confirm
team-agent agent reset <agent> --discard-session
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
team-agent stop --workspace .
team-agent start <agent>
team-agent start <agent_id> --workspace .
team-agent start coder --workspace .
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

`start AGENT` can create an ad-hoc seat without pre-existing files, or adopt its existing role document. Team creation from `--from` uses a complete source. Later team start preserves explicitly stopped/paused members and leaves verified live seats unchanged; `start AGENT` explicitly reactivates one seat. Start is not permission to repair corrupt ownership. A no-op does not synchronize role bodies, apply new launch settings or prove tool/business readiness. A queued send is not completion; wait for a natural reply/result.

## Scope and deterministic targets

- Pin `--workspace PATH` and `--team KEY` when operating outside the selected team, across workspaces or when several teams exist.
- `start reviewer.v2` is an agent, even if that directory exists. Team sources use `--from DIR`, `team start DIR` or explicit path syntax such as `./my-team`.
- `restart ID` selects an agent; a same-named directory causes conservative refusal. `agent restart ID` removes the ambiguity.
- Explicit missing paths, unknown flags, conflicts and extra arguments refuse; do not try a guessed CWD or takeover instead.
- `--dry-run` on start/restart has no lifecycle effects. A proposed plan is not final lock-held admission. No eligible targets returns nonzero with the selected scope and skipped seats, not a successful start.

## User decisions and compatibility

Honor explicit provider, model, effort and bypass choices. The unified resolver reports omitted decisions in `defaulted`; provider defaults to a supported leader provider or Pi, bypass to false, and native model/effort defaults are used only where the provider admits them. False preserves native permissions; it is not a read-only or sandbox guarantee. Use supported `models --provider NAME` discovery and retain exact model IDs.

Traditional minimal Markdown remains accepted by the original compiler. Omitted values use the same safe declaration defaults: provider falls back to Pi, bypass to false, and unset model/effort are left to the provider where admitted. Codex and Grok workers run only through their contract leaves, which require an exact model ID from `team-agent models --provider codex|grok`; an unset model is refused, never guessed. Team Agent does not infer a role model from TEAM defaults. Contract leaves use only an explicit role effort; legacy explicit TEAM provider_effort remains supported for non-Pi legacy roles (Claude, Copilot, Gemini); do not invent an effort for unspecified roles.

Supported role metadata:
- `agent_id` (legacy `name`) and `role` identify the seat.
- `provider`, `model`, `effort`, and boolean `dangerously_skip_permissions` select intentional launch settings.
- `auth_mode` and `profile` refer to local provider configuration.
- `communication_mode` selects an admitted communication contract.
- Historical `tools`, `permission_mode` and `label` do not restrict worker tools or permissions.

## Add or wake with start

One `start AGENT` contract creates absent seats, adopts existing role files, and resumes stopped seats. No prewritten file or repeated provider/bypass decision is required for an ad-hoc start. Receipts include `session_policy`, an effective `declaration`, and the `defaulted` field list:

```sh
team-agent start auditor --role-file /absolute/path/to/auditor.md
team-agent start analyst --provider pi --bypass false --prompt "Analyze the proposed change"
```

An imported external role goes into this team's `agents/ID.md`; the original stays untouched. A file already at that destination is used in place. Explicit CLI fields override the selected declaration. Omitted existing fields and role instructions remain unchanged; genuinely omitted defaults are identified in receipts.

Hidden spelling aliases delegate directly to `start AGENT`. There is no second strict-add contract and no start `--force` option. Unknown ownership or liveness refuses rather than granting permission to replace a process.

## Change a registered seat

```sh
team-agent stop reviewer --workspace . --team project-team
team-agent start reviewer --effort high --bypass false --workspace . --team project-team
```

Stop first before applying edited instructions or launch settings. A running seat with requested changes or known drift refuses rather than claiming a successful no-op. `--prompt` replaces permanent role instructions, not a one-time task; send one-time work with `send`. `--bypass` requires explicit true/false. A stopped seat may change provider/model with `start AGENT --provider PROVIDER --model MODEL --session fresh`. Its logical ID and history are retained. Provider changes under preserve or auto refuse and instruct fresh. A live seat with identical configuration is a no-op; any changed configuration or fresh-session request requires `stop AGENT` first.

The original role/spec transaction and provider admission remain authoritative. A failure can follow a committed declaration or uncertain launch; never assume a partial/unknown native replacement was rolled back or is safe to retry. Inspect the exact scoped receipt, status and doctor first.

## Restart and session policy

```sh
team-agent restart reviewer --workspace . --team project-team
team-agent team restart --workspace . --team project-team
# Strict continuation: refuse before stop if known continuation evidence is absent.
team-agent agent restart reviewer --session preserve --workspace . --team project-team
team-agent team restart --session preserve --workspace . --team project-team
```

New agent/team restart defaults to **auto**: proven resume when possible, otherwise only the provider's existing legal fresh path. Auto is not a guarantee of saved memory, reset permission or a fallback around native capability refusal. Preserve requests strict continuation. Start defaults to preserve for existing resumable context. Contextless seats and legacy providers without resume support start fresh and report `op=fresh`; launch timestamps and pending IDs alone do not require continuation. `start AGENT --session fresh` explicitly creates a new native session while retaining old history. Fresh and `--allow-fresh` cannot be combined. The old bare/explicit-path restart bridge retains its original preserve default and output.

Single-agent restart checks scope, role/session backing and capabilities before stop; only confirmed stop allows start. On partial failure, report actual completed steps and inspect; do not blindly reset or replay. Team restart does not reactivate explicitly stopped or paused seats. Whole-team start also preserves explicit stops; use `start AGENT` for an intentional wake-up. Neither command silently unpauses.

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

Existing scripts may retain hidden compatibility spellings, but new instructions use the canonical start/stop/team/agent forms above. The start aliases share one resolver, defaults, session policy, no-op/refusal rules and receipts. Bare/explicit-path legacy restart preserves its separate documented default. Startup evidence remains authoritative.
