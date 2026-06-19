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

No config files. No new UI to learn. If you can chat with Claude, you can run a team.

## Install

```bash
npx @team-agent/installer@latest install
```

Then start like this (instead of typing `claude` / `codex` / `copilot` directly):

```bash
team-agent claude
```

Two steps. Everything else happens in the conversation.

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

**Beta.** Used daily in real projects. Rough edges possible in uncommon configurations. Issues and PRs welcome.

## License

AGPL-3.0-or-later. Commercial licensing available on request.
