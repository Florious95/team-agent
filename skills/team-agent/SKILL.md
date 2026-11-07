---
name: team-agent
description: Use when the user asks to create, operate, inspect, or stop a Team Agent team. Treat the CLI as the public interface.
requires_team_agent: ">=0.5.0"
---
# Team Agent

Use the authorized **Leader's** native tool context for lifecycle operations. Workers report through their team-scoped MCP tools; loading this skill does not promote a worker to Leader. Operate in the workspace root, not inside another team's role directory.

## Do not poll

Do not run `inbox`, `status` or `doctor` repeatedly: no loops, sleep/retry loops or repeated tool calls. Team Agent pushes live messages directly to terminals. Finish the current work, stop issuing tool calls, and wait for the next incoming message. Check again only after a new event, relevant state-changing action or explicit user request; an empty inbox or busy status is not a reason. Mailbox-only messages require deliberate history review.

All three commands carry the guidance in a human `[Advisory: do not poll]` footer or a root JSON string `polling_advisory`, including handled errors. JSON remains one valid document; strict consumers must allow this additive key.

## Check the actual CLI

Check `team-agent --version`, `team-agent start --help` and `team-agent team --help` before using this guide. It requires the intuitive `start`, `stop`, `team` and `agent` surface. A version string or this skill's compatibility floor is not proof those commands exist. If missing, ask for an authorized installation of a supporting build; do not silently switch binaries, rewrite bindings or fall back to an old creation recipe.

## User decisions come first

**Honor the user's provider and permission-bypass choices.** Reuse an explicit user policy when it covers the worker. Omitted bypass retains the role or native default; it does not require another permission decision. Pi has no built-in tool approval gate, so `--bypass true` is accepted without inventing a native flag.

Use existing resources rather than establishing accounts. `team-agent doctor --workspace . --json` reports discovered tools; installed is not authenticated, and an unknown probe is not proof of unavailability. Use `team-agent models --provider NAME` where supported for exact model IDs. Do not inspect credentials, automatically log in, or silently substitute providers.

| Choice | Explain |
|---|---|
| Pi | Several model catalogs; an explicit model needs a qualified ID such as `openai-codex/gpt-6-luna` |
| Codex / Grok | Every Codex or Grok worker needs an exact model ID from `team-agent models --provider codex` or `team-agent models --provider grok`; existing subscription login only |
| Claude / other native CLIs | The user's existing workflow, native permissions, model catalog and supported session capabilities |
| Bypass `true` | Fewer interruptions, broader automatic authority |
| Bypass `false` | Native permissions remain; human confirmation may interrupt unattended work |

False is **not** a read-only or sandbox guarantee. Different workers may use different providers; a stopped worker can change provider with `start AGENT --provider PROVIDER --model MODEL --session fresh`, retaining its identity and history. Do not invent a model or effort when the user left it unset.

## Start directly, or prepare a reusable team

Open the intended workspace in a terminal and run `team-agent pi` (or another supported provider). With no live leader, launch automatically publishes the complete leader binding and discovery entry. A live leader on another terminal is preserved. Use `team-agent claim-leader --confirm` only for intentional recovery/preemption; repeated claims refresh the same binding and readiness. Provider `--attach-existing` is a claim alias, not another binding mechanism.

For an ad-hoc worker, run `team-agent start reviewer`; no source files are required. Receipts expose `declaration`, `defaulted`, and `session_policy`. New omitted settings default safely (supported leader provider or Pi, bypass=false, native model/effort where supported); unsupported native capabilities still refuse rather than inventing values.

For a reusable team, keep the goal in `TEAM.md` and instructions in `agents/*.md`. The filename stem is the worker's in-team short name:

```text
.team/current/
  TEAM.md
  agents/reviewer.md
```

Record explicit provider and permission choices when needed. This example pins Pi and bypass false:

```markdown
---
provider: pi
dangerously_skip_permissions: false
---
Review the change and report concrete regression risks.
```

From the authorized Leader context, select the intended workspace/team:

```sh
team-agent start --from .team/current --workspace . --team project-team
team-agent start --workspace . --team project-team
team-agent send reviewer "Review this change" --workspace . --team project-team
# Only for deliberate history review, not waiting for a reply:
team-agent inbox leader --workspace . --team project-team -n 3
```

The first `start` creates a complete configured team through the original creation path. Later team `start` keeps verified live seats unchanged and preserves explicitly stopped/paused seats; `start AGENT` reactivates the selected seat. It is not a reset. Missing configuration, ambiguous scope or unknown physical identity is a refusal, not permission to overwrite state. `--dry-run` plans without lifecycle effects; final locked admission is still deferred.

Accepted/queued send is **not** the worker's answer. Wait for a natural reply/result. `ok: true` with pending readiness is not business readiness. A live no-op does not synchronize edited role bodies or apply changed launch settings.

## Native REPL maintenance: --bare

Only a trusted leader/operator may send one live non-leader worker a native input line:

```sh
team-agent send worker '/compact' --bare --workspace . --team project-team
```

Claude, Codex and Pi are supported. `--bare` preserves admitted content without message headers, tokens or added newlines and sends exactly one native Enter. Pi uses bracketed paste to avoid autocomplete consuming that Enter; Claude/Codex do not. It requires an empty idle composer, one nonblank line of at most 512 UTF-8 bytes, and no control characters. It refuses mailbox/fanout/raw endpoint/leader targets and task/presentation flags. `/exit` and `/quit` are refused: use `team-agent stop <agent>`. Ordinary tasks remain framed; MCP exposes no bare option.

There is no `--raw` alias. Unmanaged raw addressing is separate from native-input framing.

Audit stays in the inbox JSONL with `framing: "bare"`; no `events.jsonl` is created. `--message-id` conflicts across framing use `request_id_conflict`; anonymous invocations remain separate sends. After any post-paste uncertainty, do not replay or clear input automatically. `accepted: null` and `session_binding: "not_reobserved"` mean submission is not native completion or session reconciliation. Bare never marks `first_send_at` or task attribution; session-changing commands may leave the previous observed session id stale. See the [operator reference](references/team-agent-operator.md) for byte, grammar and lifecycle boundaries.

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
| `start AGENT` | Create, adopt a role, resume a stopped seat, or prove an identical live no-op through one contract |
| `start` | Explicitly activate the selected non-paused roster; do not use it unless that activation is authorized |
| `stop AGENT` / `stop` | Retain saved session associations; whole-team stop also checks scoped residual resources |
| `restart AGENT` / `team restart` | Default `--session auto`: resume when proven, otherwise only the original legal fresh fallback. **Auto does not guarantee preserved memory** |
| `--session preserve` | Require available continuation evidence; refuse before stopping when continuation is known unavailable |

Team start/restart preserves explicitly stopped and paused seats; use `start AGENT` when intentionally waking one seat. Paused seats are not implicitly unpaused. A same-named directory makes `restart ID` ambiguous: use `agent restart ID`. `reviewer.v2` remains an agent ID; choose a team source with `--from DIR`, `team start DIR` or an explicit path such as `./roles`.

For strict recovery, prefer `team-agent team restart --session preserve --workspace . --team project-team`. Do not convert a refusal or partial/unknown replacement into reset, force or blind retry. Inspect `status`/`doctor` in the same scope first. Native unsupported capabilities remain refusals, never a legacy fallback.

## Add or change one worker

`start` resolves creation, adoption, wake-up and configuration through the same declaration resolver:

```sh
team-agent start auditor --role-file /absolute/path/to/auditor.md
team-agent start analyst --provider pi --bypass false --prompt "Analyze the proposed change"
team-agent stop reviewer
team-agent start reviewer --effort high --bypass false
```

Explicit CLI fields override the selected declaration; omitted values preserve existing settings or appear in `defaulted`. `--prompt` replaces permanent instructions, not a one-time task. Stop a running worker before changing configuration or requesting a fresh session. A stopped provider/model migration uses `team-agent start AGENT --provider PROVIDER --model MODEL --session fresh`; identity and old history are retained. `preserve` refuses provider changes. `fresh` and `--allow-fresh` cannot be combined. Unknown/partial startup does not promise rollback: inspect scoped evidence before retrying.

Use `agent remove/reset/clone/fork` only for those distinct operations; never remove a seat merely to change provider/model. Reset needs intentional approval and `--discard-session`. Remove and reset refuse a seat that is still working: wait for it or `stop` it first (remove also needs `--force` for a running idle seat). Fork requires `--as NEW_ID`, and `--in-place` is not supported. See [the operator reference](references/team-agent-operator.md) for capability, profile, force and import boundaries. Never put secrets in role text or copy authentication from another directory.

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

Use `team-agent start` for creation, adoption and activation; use `stop` and the corresponding `agent` operations for deliberate lifecycle changes. Older spellings remain hidden compatibility aliases, not separate creation contracts or operational recipes.

The unified start aliases share defaults, session policy, no-op/refusal rules and receipts. Bare or explicit-path `restart` retains its preserve default; deliberate `team restart` defaults to auto. Do not assume unsupported provider capabilities or infer deployment from package metadata.
