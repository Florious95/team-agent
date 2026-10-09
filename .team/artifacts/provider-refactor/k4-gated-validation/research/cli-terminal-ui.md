> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# Terminal UI

> A rich chat experience for Kiro CLI with syntax highlighting, interactive panels, and a polished terminal-native interface

The terminal UI is the default chat interface for Kiro CLI. It provides syntax-highlighted markdown rendering, interactive overlay panels, visual tool progress, and a full set of keyboard shortcuts for navigating your session.

## Quick start

Launch a chat session:

```bash
kiro-cli chat
```

To use the classic interface instead:

```bash
kiro-cli --classic
```

## Chat experience

Messages stream incrementally as the agent works. Responses render with full markdown support: syntax-highlighted code blocks, tables, lists, blockquotes, and nested formatting. Terminals that support synchronized output get flicker-free updates.

Use arrow keys to scroll through long responses.

While Kiro is working, the busy indicator shows elapsed active work time. The timer pauses whenever the turn is waiting for your approval or an answer to a question, then resumes after you respond.

### Tool display

Each tool type has a dedicated visual component (shell commands, file operations, grep, glob, code intelligence, and more). While a tool executes, you see:

- A descriptive title and spinner
- A status icon on completion: ✓ for success, ✗ for error, ⏸ when awaiting approval
- Collapsible output (press `Ctrl+O` to toggle between summary and full output)
- Progress bars for long-running MCP operations

MCP tool call summaries show all of the parameters passed to a tool, so you can see the full context of an invocation without expanding it.

### Large tool results in V3

For a large tool result, Kiro saves the complete result with the session and gives the model a short preview. This keeps the full output available without adding all of it to the model context.

### Thinking display

When the agent reasons through a complex request, its reasoning ("thinking") blocks stream inline above the response. This is on by default (`chat.showThinking`). Long traces collapse to a tail view that you can expand or collapse with `Ctrl+O`.

Toggle the display from `/settings display` (select **Show thinking**) or from the command line with `kiro-cli settings chat.showThinking false`. The setting is startup-only, so changes apply to your next session. See [Thinking display](https://kiro.dev/docs/cli/chat/settings.md#thinking-display) for details.

### Sessions

The `/chat` command opens a fuzzy picker showing previous sessions with timestamps. Sessions are scoped to the current working directory, and sessions from the classic interface load in the terminal UI without issues.

You can also resume a specific session by ID:

```bash
kiro-cli chat --resume-id <SESSION_ID>
```

For full details on session management, see [Session management](https://kiro.dev/docs/cli/chat/session-management.md).

## Interacting with the agent

### Approval UX

When a tool requires permission, a notification bar appears above your input with Yes, Trust, and No options. If multiple tools need permission at the same time, approvals queue and appear one at a time. You can drill into an approval to provide feedback instead of a binary yes/no.

For shell commands, trust can be scoped at different levels: exact command, command prefix, or base command. See [Permissions](https://kiro.dev/docs/permissions.md) for the full trust model.

**ℹ️ Info:** Using `--trust-all-tools` in the terminal UI shows a confirmation warning before granting access. You must acknowledge the risk before proceeding.

### Model fallback notices

In a V2 terminal UI session, turn-level model fallback is on by default. If the current model refuses a turn, Kiro can try your configured fallback or a service-provided fallback. When the backend marks the current model unavailable, Kiro can try only your configured fallback. V3 does not use this turn-level fallback.

Kiro explains why it switched models. After a refusal, the answering model may become the conversation's model. A switch caused by model unavailability applies only to that turn. After the fallback chain, Kiro lists the models tried and the outcome. Use `/model fallback` to view or change the configured fallback.

### Model availability notices

Once the model list is known, a new V3 terminal UI session checks its saved default. If that model is unavailable, Kiro uses the service-provided default and points you to `/model`. It does not rewrite `chat.defaultModel`.

If the model list is not available yet, Kiro keeps the current selection without showing an availability notice.

An unavailable model from the agent configuration or a resumed session stays selected. Kiro reports that requests will fail. Choose another model with `/model`, or update the agent configuration when it owns the selection.

If an explicit `--model` value is unavailable, Kiro keeps it selected and reports that requests will fail. Choose another model with `/model`.

The same availability check runs after `/chat new` and `/clear`. V3 headless runs do not show these session-start notices.

V2 does not show session-start availability notices. It keeps an unlisted model ID, and each request fails until you choose another model. Turn-level fallback does not cover an unlisted model ID.

### Input

The terminal UI supports several input methods beyond standard text entry.

| Feature | How |
|---------|-----|
| Newline | `Shift+Enter`, `Ctrl+J`, or `Alt+Enter` |
| Configure multi-line keys | `/settings terminal` auto-configures your terminal app |
| File/directory reference | `@path` with tab-complete picker |
| Paste image | `/paste` or paste from clipboard |
| Paste long text | Long pastes collapse into a chip. Press `Tab` on a chip to expand it into inline editable text |
| Input queuing | Type your next message while the agent is processing, and edit queued messages before they send |
| Command history | `Up`/`Down` arrows. Per-session by default; switch to a shared history with `/settings history` |
| Reverse history search | `Ctrl+R` |
| Multi-line editor | `/editor` opens `$EDITOR` (defaults to `vi`) |

### Shell escape

Run shell commands without going through the agent by prefixing with `!`:

```bash
!npm run build
```

Output streams in real time. Long output collapses to a head + tail view. Press `Ctrl+O` to expand.

### Real-time shell output

Shell command output from the agent streams to the terminal line by line as it runs, so you can follow build progress, test output, and deployments as they happen. You'll see output appear incrementally instead of all at once when the command completes.

**⚠️ Warning:** Interactive commands that require user input (`rm -i`, `npm init`, `sudo`, `ssh` host key prompts) are not supported and will exit immediately. Use non-interactive alternatives like `npm init -y` or `rm` without `-i`.

## UI components

### Overlay panels

Commands like `/help`, `/context`, `/tools`, `/mcp`, `/knowledge`, and `/code` open as overlay panels. Each panel is searchable, scrollable, and dismissible with `Esc`.

[Video](https://kiro.dev/videos/tui-overlay-panel.mp4)

### Activity tray

Press `Ctrl+X` to see task progress and queued messages at a glance without scrolling through conversation history.

[Video](https://kiro.dev/videos/tui-tasks.mp4)

### Crew monitor

Press `Ctrl+G` to monitor subagent activity and spawned sessions in real time during multi-agent workflows. Navigate between sessions with `Ctrl+D`/`Ctrl+U` and press `q` to close. Use `/spawn <task>` to launch a parallel session from the command line, or set up subagents to see them in action with the crew monitor. See [Subagents](https://kiro.dev/docs/custom-agents/subagents.md) for the subagent side, and `/spawn` in the [Slash commands reference](https://kiro.dev/docs/reference/slash-commands.md#spawn) for user-driven sessions.

### Workflow monitor

Run `/workflow` to open workflow history, select a run, and enter its full-screen monitor. The monitor shows the step tree, active-step output, run status, and context-sensitive controls.

| Shortcut | Action |
|---|---|
| Arrow keys | Select a run or step |
| `p` | Request a cooperative pause |
| `r` | Resume a paused run or retry failed/aborted work when offered |
| `s` | Send a text reply to the selected paused step |
| `Ctrl+X` | Arm a stop; press `Ctrl+X` again to confirm, or Esc to keep the run going |
| Esc | Close the current form or return to the previous view |

Workflow commands also accept an explicit run ID. See [Run and manage workflows](https://kiro.dev/docs/workflows/manage.md) for command syntax, recovery, and the difference between Pause and Stop.

### Spec review

When a spec run reaches a phase checkpoint, the checkpoint offers to open the phase document for review. Press `Ctrl+X` to read the document in place and stage comments against the lines you want changed. Returning to the checkpoint keeps your comments staged, and answering the checkpoint question sends them all to the agent as one revision request.

The review screen also supports the mouse: scroll to navigate the document and click to position the cursor on a line. Press `m` to toggle mouse support on or off.

| Shortcut | Action |
|----------|--------|
| `↑` `↓` / `j` `k` | Move a line |
| `PgUp` `PgDn` / `Ctrl+U` `Ctrl+D` | Move half a page |
| `n` `N` | Next / previous section |
| `]` `[` | Next / previous comment |
| `Home` `End` / `g` `G` | Jump to top / bottom |
| `Enter` / `e` | Comment on this line, or edit the comment under the cursor |
| `Del` | Delete the comment under the cursor |
| `m` | Toggle mouse support (scroll to navigate, click to position) |
| `?` | List every shortcut |
| `Esc` / `q` | Return to the checkpoint |

Because this surface owns the keyboard while it's open, `Ctrl+X`, `Ctrl+U`, and `Ctrl+D` act on the document here rather than toggling the activity tray or moving between subagents.

See [Specs](https://kiro.dev/docs/specs.md) for the full spec workflow.

### Themes

Three built-in themes ship with the terminal UI: dark, light, and safe (an ANSI fallback for SSH or constrained terminals). The UI auto-detects your terminal background and selects the appropriate theme. Use `/theme` to customize colors with a live preview or switch between base themes.

Themes use named ANSI colors instead of hardcoded hex values, so they render correctly on terminals with remapped color palettes. The `NO_COLOR` environment variable is respected — set it to disable all color output.

[Video](https://kiro.dev/videos/tui-theme.mp4)

## Slash commands

Slash commands open interactive panels or trigger actions. All panels support fuzzy search and close with `Esc`.

| Command | Description |
|---------|-------------|
| `/help` | All available commands |
| `/context` | Context breakdown with per-file token percentages. Supports `add`, `remove`, `show`, and `clear` subcommands |
| `/usage` | Usage limits with progress bar and credit balance |
| `/knowledge` | Knowledge base management |
| `/prompts` | MCP and file-based prompts via a selection menu with detail drill-in |
| `/editor` | Open `$EDITOR` to compose multi-line prompts |
| `/feedback` | Submit feedback about Kiro CLI |
| `/paste` | Paste an image from the clipboard |
| `/chat` | Switch between previous sessions via a fuzzy picker |
| [`/fullscreen`](https://kiro.dev/docs/cli/fullscreen.md) | Toggle a dedicated full-terminal conversation view with mouse scrolling |
| `/plan` | Enter plan mode (also `Shift+Tab`) |
| `/agent` | Switch between agents |
| `/model` | Switch the active model and configure thinking and effort |
| `/mcp` | View MCP servers and registry status |
| `/tools` | View and reset tool permissions. `/tools reset` clears all runtime permissions including shell trust patterns, filesystem paths, and denied tools |
| `/code` | Code intelligence panel |
| `/hooks` | View configured hooks |
| `/guide` | Switch to the Kiro guide agent for help and onboarding |
| `/transcript` | Open the conversation transcript in `$PAGER` (also `Ctrl+T`) |
| `/theme` | Override theme colors for prompt and response text |
| `/settings` | Configure theme, keybindings, terminal input, display preferences, and account-available features |
| `/effort` | Open the `/model` picker focused on **Effort** in V2 and V3; `/effort <level>` changes the current session directly. Use `/model` as the primary reasoning interface |
| `/rewind` | Fork conversation at an earlier turn to explore a different path |
| `/workflow` | Open workflow history or run, inspect, pause, resume, cancel, and retry declarative workflows |
| `/goal` | Start a goal-driven iterative loop |
| `/copy` | Copy the last assistant response to clipboard (works over SSH) |
| `/spawn` | Run a parallel agent session with a task |
| `/clear` | Erase this session's conversation history and reset its context |
| `/compact` | Summarize the conversation to free context space |
| `/reply` | Reply to the last message |
| `/exit` | Exit the session (alias for `/quit`) |
| `/skill-name` | Invoke a skill directly by name (e.g., `/pr-review`). See [Agent Skills](https://kiro.dev/docs/skills.md) |

For the full command reference, see [Slash commands](https://kiro.dev/docs/reference/slash-commands.md).

## Terminal features

The terminal UI detects your terminal's capabilities and adapts automatically.

| Feature | Details |
|---------|---------|
| Progress indicator | Terminal tab/title bar animates and reflects agent state (streaming, pending approval, error) while Kiro works. Supported in iTerm2, WezTerm, Windows Terminal, and ConEmu (with ANSI processing enabled) |
| Clickable hyperlinks | Markdown links are clickable in iTerm2, WezTerm, kitty, and Ghostty |
| Theme detection | Automatically detects dark/light mode from your terminal background |
| 256-color fallback | Graceful degradation for terminals without truecolor |
| Non-ASCII support | CJK characters, emoji, and accented letters render correctly |

**ℹ️ Info:** The title bar can also show a custom name you set and animate while Kiro works. Enable terminal-title updates from `/settings display` → **Terminal title**; use [`/title`](https://kiro.dev/docs/reference/slash-commands.md) to set a custom name.

## Configuration

### UI engine precedence

The UI engine is determined in this order (highest to lowest priority):

1. CLI flag: `--tui` or `--classic`
2. Environment variable: `KIRO_CHAT_UI`
3. Setting: `chat.ui`
4. Default: `tui`

### Disabling individual features

Opt out of specific terminal features with environment variables:

```bash
KIRO_NO_HYPERLINKS=1 kiro-cli chat    # Disable clickable links
KIRO_NO_PROGRESS=1 kiro-cli chat      # Disable progress indicator
KIRO_NO_SYNCHRONIZED=1 kiro-cli chat  # Disable synchronized output
```

### Using the classic interface

The classic interface is deprecated. Starting with Kiro CLI 2.26.0, a classic session shows a notice that names the flag, environment variable, or setting that selected it. To return to the terminal UI, remove `--classic` from your command, unset `KIRO_CHAT_UI`, or run `kiro-cli settings chat.ui "tui"`, then restart Kiro CLI.

Switch to the classic interface permanently:

```bash
kiro-cli settings chat.ui "classic"
```

Or for a single session:

```bash
kiro-cli --classic
```

### Customizing keybindings

Override the default cancel, close-menu, and quit shortcuts through settings. Values accept `ctrl+`, `shift+`, and `alt+`/`meta+` modifiers with a single key. Invalid values fall back to the built-in default.

```bash
kiro-cli settings chat.keybindings.cancelStream "ctrl+x"
kiro-cli settings chat.keybindings.closeMenu "ctrl+["
kiro-cli settings chat.keybindings.quit "ctrl+shift+q"
```

See [Key bindings (terminal UI)](https://kiro.dev/docs/reference/settings.md#key-bindings-terminal-ui) for the full setting reference.

## Keyboard shortcuts

### Text editing

Core editing shortcuts, including a full Emacs-style kill ring.

| Shortcut | Action |
|----------|--------|
| `Shift+Enter` | Insert newline |
| `Ctrl+J` | Insert newline |
| `Alt+Enter` | Insert newline |
| `Alt+Backspace` | Delete previous word |
| `Ctrl+W` | Delete previous word (kill ring) |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+U` | Kill to start of line |
| `Ctrl+L` | Clear the screen. Earlier output stays in terminal scrollback (inline) or reachable with `Page Up` (fullscreen) |
| `Ctrl+Y` | Yank (paste from kill ring) |
| `Ctrl+_` | Undo last edit |

### Navigation and history

Move through your session and search previous inputs.

| Shortcut | Action |
|----------|--------|
| `Up` / `Down` | Navigate prompt history |
| Arrow keys | Scroll line-by-line |
| `Ctrl+R` | Reverse incremental history search |

By default, prompt history is per-session: `Up`/`Down` recall only the prompts you entered in the current session. Run `/settings history` to switch between `session` (default) and `global` (shared across all sessions) modes. The change applies to your next session.

### Panels and views

Open and navigate the overlay interfaces.

| Shortcut | Action |
|----------|--------|
| `Ctrl+G` | Open crew monitor |
| `Ctrl+D` / `Ctrl+U` | Navigate between subagents (crew monitor) |
| `q` | Close crew monitor |
| `Ctrl+X` | Toggle activity tray. In `/verbosity`, cycles the preview. At a spec phase checkpoint, opens [spec review](#spec-review) instead |
| `Ctrl+T` | Open conversation transcript in `$PAGER` |
| `Ctrl+O` | Expand/collapse tool output and thinking traces; reveal every change in the What's New popup |
| `Esc` | Close panels, cancel agent, clear prompt queue |

### Agent interaction

Control the agent and manage your session.

| Shortcut | Action |
|----------|--------|
| `Tab` | Drill into approval options / autocomplete file references |
| `Shift+Tab` | Enter plan mode |
| `Ctrl+C` | Exit session |
| `Ctrl+D` | Exit session (from chat input) |
| `Ctrl+Z` (twice) | Suspend the session. A double-press is required to avoid accidental suspension while the agent is streaming |

## Platform support

The terminal UI is supported on macOS, Linux (including Red Hat Enterprise Linux), and Windows.

## Limitations

- Requires embedded assets in the build. If unavailable, Kiro falls back to the classic interface.
- Agent shell tool does not support interactive commands that require user input. See the [shell escape](#shell-escape) section for details and workarounds.
- External diff tools (`chat.diffTool` setting) are not yet supported. All diffs use the built-in viewer.
- Some terminal emulators may not support all features (hyperlinks, progress indicators, synchronized output). Use the environment variables in [Configuration](#disabling-individual-features) to disable specific features.

## Troubleshooting

### Terminal UI not loading

Verify your setting:

```bash
kiro-cli settings list | grep chat.ui
```

If the setting is correct but the terminal UI still doesn't load, update to the latest version of Kiro CLI.

### Rendering issues

If you see visual artifacts or broken rendering:

1. Try a different terminal. iTerm2, WezTerm, kitty, and Ghostty provide the best experience.
2. Disable synchronized output: `KIRO_NO_SYNCHRONIZED=1 kiro-cli chat`
3. Check that your terminal supports truecolor (most modern terminals do).

## Related

- [Terminal UI vs classic](https://kiro.dev/docs/cli/terminal-ui/comparison.md) — Detailed comparison for users choosing between the two interfaces
- [Chat](https://kiro.dev/docs/cli/chat.md) — Chat usage and options
- [Settings reference](https://kiro.dev/docs/reference/settings.md) — Full settings reference
- [Custom agents](https://kiro.dev/docs/custom-agents/creating.md) — Create custom agents with welcome messages
