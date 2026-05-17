**English** | [中文](https://github.com/Florious95/team-agent/blob/main/README.zh.md)

# Team Agent

> Talk once. Ship a team.

A multi-agent runtime for Claude Code and Codex CLI where **the lead does the orchestration** — you describe a goal in plain language and it builds a team across providers, runs the work, and reports back.

No DAG. No YAML. No Kanban. Just a conversation.

```bash
npx @team-agent/installer@latest install
```

**Important:** the lead Claude/Codex conversation must run inside a
tmux-managed pane. The easiest path is `team-agent claude` or
`team-agent codex`; existing tmux layouts also work. A plain terminal without a
tmux pane is not enough, because teammates need a concrete pane target to send
verified messages back to the lead.

---

## Why this exists

You have a $20 Claude subscription. It's day 18 of the month. You're out of credits but you still have work to ship.

You could pay $200. You could switch to a cheaper model and lose Claude's taste. You could wait until the 1st.

Or you could let Claude stay the lead — designing tasks, coordinating, reviewing — while Codex or a third-party-API-routed Claude handles implementation. Same conversation, ~10x cheaper, no quality loss.

That's one of the things this is for. **The lead stays Claude. The hands can be anyone.**

---

## What it actually does

Install once. Then in any Claude Code or Codex CLI conversation, say something like:

> "Build a small SaaS for tracking client feedback — backend, frontend, tests, and acceptance criteria."

The lead figures out:

- What roles you need (and how rich each role's definition should be)
- Which provider each role should run on (Claude for taste, Codex for logic, third-party for cost)
- How teammates communicate (peer-to-peer, with shared task lists)
- When to escalate decisions back to you

It then spawns the team, runs the work, and reports back. You stay in the same conversation the whole time. Teammates show up in separate terminal windows so you can watch what each is doing without leaving the lead.

Close your terminal. Come back tomorrow. The team is still there. Open Claude Code again — pick up where you left off.

---

## What makes it different

There are good multi-agent tools today. Each picks a different tradeoff:

|                            | Form               | You configure                  | Lead                                  | Where it runs       |
| -------------------------- | ------------------ | ------------------------------ | ------------------------------------- | ------------------- |
| **agent-teams-ai** (871★)  | Electron app       | Roles + provisioning prompt in UI | "CTO" watches Kanban               | Desktop app         |
| **omo** (54.9k★)           | OpenCode plugin    | `ultrawork` command word       | Sisyphus, fixed roles                 | OpenCode TUI        |
| **CCB** (2.5k★)            | CLI + TOML         | `.ccb/ccb.config` per team     | None (you compose)                    | tmux                |
| **ClawTeam** (3.3k★)       | CLI + prompt inject | TOML team templates            | None                                  | tmux + Web UI       |
| **Team Agent** (this)      | MCP runtime        | Nothing                        | The native Claude/Codex you're already talking to | Your existing terminal |

The lead in this project isn't a special "orchestrator agent" with its own personality. **It's just Claude (or Codex)** — the same one you'd talk to about anything else, now with the ability to spawn and manage teammates.

This matters because:

- **Orchestration scales with model intelligence**, not with framework features. When Claude 5 ships, the lead gets smarter automatically. No update required on our side.
- **The lead can build any team for any task**, not just predefined coding roles. We've run academic paper revisions, multi-role brainstorming sessions, even adversarial games like Werewolf — none of them programmed, all of them composed by the lead in conversation.
- **You give up the orchestration UI**, you gain the ability to ship work whose specifics you couldn't have specified up front.

---

## How it works (briefly)

Three design choices make this possible:

**1. The orchestration layer is the lead.** No external workflow engine. The lead reasons about role definitions, dispatches via MCP tools, and adjusts the team in real time from your conversation. Add a teammate mid-flow, change someone's role, dissolve a team — all in plain language.

**2. Transport is infrastructure, identity is persistent.** Teammates run as long-lived `claude` or `codex` processes with stable session IDs. When a window dies, the runtime respawns it and restores state — without that event ever entering any agent's context. Identity is injected at the system-prompt level, not via fragile chat-history hacks.

**3. Standards over inventions.** MCP for tool calls, Skill files for role definitions. Anything the broader ecosystem ships, this picks up automatically.

---

## Quick start

### Install

```bash
npx @team-agent/installer@latest install
```

This sets up the MCP server, registers the Team Agent skill, and wires it into your Claude Code / Codex CLI config.

Source checkout install:

```bash
git clone https://github.com/Florious95/team-agent.git team-agent
cd team-agent
npm exec --yes --package . -- team-agent-installer install
```

### Use

Start the lead inside tmux. The shortcut commands create or attach a tmux
leader session when needed:

```bash
team-agent claude
team-agent codex
```

If you already use tmux/Ghostty/Finder layouts, keep using them; the hard
requirement is that the visible lead conversation has a tmux pane. Then talk to
the lead:

```
You:   I want to refactor this codebase, split it into a monorepo,
       and add proper test coverage. Help me plan and run this.

Lead:  [proposes a team — refactor architect (Claude), code mover (Codex),
        test author (Claude), reviewer (Codex). Surfaces the tradeoffs,
        waits for your confirmation.]

You:   Go.
```

That's it. Teammates appear in separate windows. The lead reports progress, raises decisions when needed, and shuts everything down when you say so.

### Stop / resume

```
You:   Close the team for now.
Lead:  [Saves state, closes panes. ~2 seconds.]

(next day)

You:   Continue yesterday's refactor team.
Lead:  [Restores teammates from saved sessions. ~2 seconds. Same context.]
```

---

## What works today

Verified across multiple real workflows:

- **Cross-vendor mixed teams** — Claude leading, Codex implementing, third-party-API Claude on tests
- **Web development teams** — 5 roles: frontend / backend / contract / requirements / testing
- **Academic collaboration** — 5-stage paper revision with adversarial reviewers and consensus mechanism
- **Game / experimental** — 4-round "Who's the Spy" experiments (1 lead + 4 players, fully autonomous, surfaced a real LLM theory-of-mind observation)
- **Emergent recovery** — leads automatically pressing enter on Codex permission prompts; teams surviving pane closures and resuming the next day
- **Dialog-driven team mutation** — adding teammates mid-flow, changing roles, dissolving a team, all without leaving the lead's conversation

---

## Supported leads and teammates

| Role     | Claude Code (subscription) | Claude Code (third-party API) | Codex CLI |
| -------- | -------------------------- | ----------------------------- | --------- |
| Lead     | ✓                          | ✓                             | ✓         |
| Teammate | ✓                          | ✓                             | ✓         |

Any teammate can be backed by a different provider/tier than the lead. The runtime handles auth, session lifecycle, and resume per teammate independently.

---

## Status

**Beta.** Working in real use, but expect rough edges in less common configurations. Issues and PRs welcome.

## License

AGPL-3.0-or-later. Commercial licensing available on request.
