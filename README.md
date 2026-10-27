**English** | [中文](https://github.com/Florious95/team-agent/blob/main/README.zh.md)

# Team Agent

> Use Claude Code the way you always do — now lead a whole team.

![demo](assets/demo-en.gif)

## What is this

Right now, when you use Claude Code (or Codex, or Copilot CLI), you have one pair of hands: while it writes the frontend, the backend waits; while it runs tests, you wait.

With Team Agent installed, it's still the same conversation window, but you can say:

> "This is too slow for one person. Build a team: one for backend, one for frontend, one for tests."

Then:

- New windows pop up, one per teammate, **all working in parallel**
- Teammates message each other directly (frontend asks backend for the API schema — no need to go through you)
- You only talk to the lead; the lead reports progress and only escalates when there's a real decision

No new dashboard or orchestration language: the Leader prepares team files from your approved plan. If you can chat with Claude, you can run a team.

## Install

```bash
npx @team-agent/installer@latest install
```

Then start like this (instead of typing `claude` / `codex` / `copilot` directly):

```bash
team-agent claude
```

Two steps. Everything else happens in the conversation.

## Everyday CLI: start, stop, restart

Check the selected binary with `team-agent start --help` and `team-agent team --help`. If these are missing, obtain an authorized supporting build; a version string alone is not a feature guarantee. Lifecycle commands run in the authorized native **Leader's tool context**, not an ordinary worker's pane.

The Leader prepares `TEAM.md` and `agents/*.md` once, recording the user's provider and boolean permission-bypass choices. For example:

```text
.team/current/
  TEAM.md
  agents/reviewer.md
```

With that complete source, operate on an explicit workspace/team:

```sh
team-agent start --from .team/current --workspace . --team project-team
team-agent start --workspace . --team project-team
team-agent send reviewer "Review the change and report the main risk" --workspace . --team project-team
team-agent inbox leader --workspace . --team project-team -n 3
team-agent stop reviewer --workspace . --team project-team
team-agent start reviewer --workspace . --team project-team
team-agent restart reviewer --workspace . --team project-team
team-agent stop --workspace . --team project-team --json
team-agent start --workspace . --team project-team
team-agent team restart --workspace . --team project-team --json
```

| Command | Intent and boundary |
|---|---|
| `start` | Create once from complete configuration; later activate the selected non-paused roster. Verified live/drift-free instances stay unchanged |
| `start AGENT` | Wake a stopped seat, prove a live no-op, or add a RoleOnly seat from its complete file. A new ID needs explicit provider/bypass decisions |
| `stop [AGENT]` | Stop one agent or the selected team, retaining saved associations; team stop reports owned residuals |
| `restart AGENT` | Preflight, confirmed stop, then start that agent; no implicit role patches or reset |
| `team restart` | Deliberate team restart; preserve explicitly stopped/paused seats |
| `team status` / `status` | Read the concise node projection; diagnose with `doctor` |
| `agent add/remove/reset/clone/fork` | Explicit operations with their original confirmation/capability gates; reset requires `--discard-session`, fork requires `--as NEW_ID` |

New agent/team restart defaults to `--session auto`: resume when proven, otherwise only an existing legal fresh fallback. **Auto is not a promise of preserved memory.** For strict recovery use `team-agent team restart --session preserve --workspace . --team project-team`; known unavailable continuation refuses before stopping. Unknown or partial outcomes are not permission to reset or retry blindly. `--dry-run` on start/restart has no lifecycle effects, and no eligible targets is a nonzero refusal.

No-op does not synchronize changed role bodies/settings or prove business readiness. Stop before applying worker changes. A queued send is not the reply. Verify actual results and, after final `stop`, empty scoped residual resources; never clean up sibling teams or shared servers. Team sources use `--from DIR`/explicit paths; `reviewer.v2` is an agent ID, and `agent restart ID` disambiguates a same-named directory. Native unsupported capabilities remain refusals; `--in-place` fork is not implemented.

### Compatibility bridges

Existing automation may keep `quick-start`, `shutdown` and suffixed `*-agent` commands. They remain supported but are not the daily instructions above. Old bare/explicit-path `restart` retains preserve/default receipts; new `team restart` defaults to auto. Legacy single-seat start refuses a live seat, whereas new start can prove a no-op: do not mechanically rename scripts or assume byte-identical JSON. See the [operation skill](skills/team-agent/SKILL.md) and [operator reference](skills/team-agent/references/team-agent-operator.md).

## What you can say

Team building and management is all natural language. Some real examples:

```
"Refactor this codebase into a monorepo and add test coverage. Build a team, show me the plan first."

"Add another person just for code review — every merge goes through them."

"The frontend role isn't working out. Reset it and try a different approach."

"Wrap it up for today."              ← Team closes, state saved

(next day)
"Continue yesterday's refactor team." ← Same people, same memory, pick up where they left off
```

Teammates aren't limited to coding roles. People have used it for multi-round paper reviews (reviewers critique each other then reach consensus), multi-role brainstorming, even having 5 agents play four rounds of Werewolf autonomously. If you can describe the division of labor, the lead can build the team.

## Teammates can come from different CLIs

The lead and each teammate independently choose which CLI to use:

|          | Claude Code | Codex CLI | Copilot CLI |
| -------- | :--: | :--: | :--: |
| Lead     | ✓ | ✓ | ✓ |
| Teammate | ✓ | ✓ | ✓ |

In other words: let Claude be the lead for planning and review, let Codex teammates handle bulk implementation. Mix and match whatever subscriptions you have.

## Native CLI argv routing (opt-in)

Configure literal startup arguments globally, separately for each provider, from any directory:

```sh
team-agent route set pi -- --mode rpc
team-agent route enable
team-agent route status --json
team-agent route show pi --json
team-agent route add pi -- --verbose
team-agent route clear pi
team-agent route disable
```

The default is OFF. `~/.team-agent/argv-routing.json` stores the switch and mappings;
`TEAM_AGENT_CLI_ARGV_ROUTING=1|true|on` or `0|false|off` overrides the persisted switch
(case-insensitive, surrounding whitespace accepted). Invalid overrides safely disable routing.
`enable`/`disable` preserve mappings; `set` replaces, `add` appends, and `clear` removes one mapping.
Commands report both persisted and effective states, including an active environment override.

Provider keys: `claude`, `codex`, `copilot`, `gemini_cli`, `grok`, `pi`.
Claude/Claude Code share one mapping; `claude_code`/`claude-code` are accepted aliases.
Everything after the first `--` is literal argv data, including `--help`, `--json`, empty strings
and `{workspace}`; no shell expansion is performed. Arguments are inserted immediately after
the native executable, before its original arguments. Do not store secrets or override framework-managed
session/MCP/permission arguments; routing does not bypass existing identity or authentication checks.

Only the next actual native Leader/worker launch uses the current mapping, including restart,
start/reset/add/clone, Pi new-seat fork and remove rollback. Running/attached processes, dry runs,
model/version/auth probes and other helper processes are unchanged. Enabled invalid configuration
fails before native spawn; `TEAM_AGENT_CLI_ARGV_ROUTING=off` is an emergency override, not a file repair.
Pi `--mode rpc` is passed through as startup argv; it does **not** create an RPC host or change Team Agent's transport.

## FAQ

**What do I need?**
macOS or Linux (including WSL), at least one CLI from the table above, and tmux (`team-agent claude` handles tmux automatically — you don't need to know tmux).

**Do I have to watch the teammate windows?**
No. The windows are there if you want to glance at them. All reports come back to the lead's conversation.

**Will the team die if I close the terminal?**
No. The team keeps running in the background. Reopen your conversation and pick up where you left off.

**How is this different from Claude Code's built-in subagents?**
Subagents are one-shot, fire-and-forget, and can't talk to each other. Team Agent teammates are persistent roles: they have their own memory, can message each other, and are still there tomorrow.

## Status

`team-agent status` prints one concise row per registered node with nine fields:
`name`, `provider`, `model`, `effort`, `runtime_status`, `activity`, `health`, `session_name`, and `tmux_command`.
`model` and `effort` are the last accepted launch settings, or `null` when unspecified;
pending role-file edits and provider defaults are not inferred.
`--json` exposes the same projection as structured JSON. Missing or conflicting runtime evidence is
reported as `unknown`; a non-null `tmux_command` is safe to copy for the observed socket,
session, window, and pane. `--summary` and `--detail` remain accepted for compatibility and
do not add diagnostic or history fields. The accepted nodeprobe pairing and
operator-trusted capability receipt are documented in
[`docs/status-nodeprobe.md`](docs/status-nodeprobe.md).

**Beta.** Used daily in real projects. Rough edges possible in uncommon configurations. Issues and PRs welcome.

## License

AGPL-3.0-or-later. Commercial licensing available on request.
