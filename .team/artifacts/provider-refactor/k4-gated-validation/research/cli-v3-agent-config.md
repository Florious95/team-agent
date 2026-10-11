> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# Agent config changes

> New optional fields and Markdown format for agent profiles in CLI 3.0

Agent profiles are backward-compatible — existing configs continue to work. The unified agent harness adds new optional fields and a Markdown format option.

## Old format

`.kiro/agents/my-agent.json`:

```json
{
  "name": "my-agent",
  "description": "A development agent",
  "prompt": "file://resources/MY_PROMPT.md",
  "model": "claude-sonnet-4.6",
  "tools": ["fs_read", "fs_write", "execute_bash", "grep", "glob", "code"],
  "toolsSettings": {
    "execute_bash": {
      "allowedCommands": ["^git status$", "^cargo build[^&;]*$"],
      "deniedCommands": ["^rm -rf"],
      "denyByDefault": false
    },
    "fs_read": {
      "allowedPaths": ["src/**", "docs/**"],
      "deniedPaths": [".env", "secrets/**"]
    },
    "fs_write": {
      "allowedPaths": ["src/**"],
      "deniedPaths": ["*.lock"]
    }
  },
  "resources": ["file://AGENTS.md"],
  "hooks": {
    "agentSpawn": [{ "command": "git status", "description": "Add git context" }]
  },
  "welcomeMessage": "Hello! How can I help?"
}
```

Both formats are supported. Use Markdown when your system prompt is long or benefits from human readability; JSON works well for programmatically generated configs. There's no functional difference — fields are identical across formats.

## New Markdown format

`.kiro/agents/backend-dev.md`:

```markdown
---
name: backend-dev
description: Backend development agent
model: claude-sonnet-4.6
tools: ["read", "write", "shell", "web"]
excludedTools: ["knowledge"]
includeMcpJson: true
includePowers: false
mcpServers:
  postgres:
    command: npx
    args: ["-y", "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: "${DATABASE_URL}"
resources:
  - file://./ARCHITECTURE.md
  - skill://backend-patterns
permissions:
  rules:
    - capability: shell
      match: ["npm *", "node *"]
      effect: allow
welcomeMessage: "Ready to work on backend code."
---

You are a backend developer focused on Node.js and TypeScript.
Always use async/await. All database queries must be parameterized.
```

## Tags vs Capabilities

V3 separates tool *visibility* from tool *authorization* into two distinct systems.

**Tags** (in the `tools` field) control which tools the agent can see and invoke. Use short category names — new tools added to a category become available automatically:

| Tag | Tools included |
|-----|---------------|
| `read` | read_file, read_files, list_directory, file_search, grep_search, code |
| `write` | fs_write, str_replace, delete_file |
| `shell` | execute_bash, control_bash_process |
| `web` | web_fetch, web_search |
| `subagent` | Subagent delegation tools |
| `knowledge` | Knowledge base tools |
| `todo_list` | Task tracking tools |
| `@mcp` | All MCP server tools |
| `@builtin` | All built-in tools |
| `*` | All tools (no filtering) |

**Capabilities** (in the `permissions` field) control what those tools can do at invocation time — auto-approved, blocked, or requiring confirmation. They use capability names (`fs_read`, `fs_write`, `shell`, etc.) which don't map 1:1 to tags.

> **Example:** `tools: ["web"]` gives the agent both `web_fetch` and `web_search` tools. But in permissions, these are separate capabilities — you can `allow` web_fetch for docs while `deny`-ing web_search.

## New fields reference

| Field | Type | Description |
|-------|------|-------------|
| `excludedTools` | string[] | Tools to exclude even if `tools` allows them |
| `includeMcpJson` | boolean | Include workspace `.kiro/settings/mcp.json` servers |
| `includePowers` | boolean | Include IDE-installed powers |
| `resources` | string[] | URIs to load into context: `file://./path`, `skill://name` |
| `permissions` | object | Inline policy rules (agent scope, supports all effects) |
| `welcomeMessage` | string | Custom greeting on session start |
| `hooks` | object | CLI only — inline hook definitions (same schema as `.kiro/hooks/`) |

> **Note:** Once the model list is available, Kiro checks whether an agent's `model:` value is available when a V3 terminal UI session starts. If that model is unavailable, Kiro keeps it selected and reports that requests will fail. Choose an available model with `/model`, or update the agent configuration. An explicit `--model` overrides the agent value. Without an explicit `--model`, Kiro uses `chat.defaultModel` only when the agent configuration has no model.

> **Note:** `toolsSettings` is removed in V3 — migrate per-tool rules to the unified `permissions` block. See [Permissions migration →](https://kiro.dev/docs/cli/v3/permissions.md).

## MCP server config

Supports stdio and HTTP transports. Stdio servers accept a `timeout` field (milliseconds). HTTP servers accept `headers`. Both expand environment variables at runtime using `$` syntax.

Agent config migration is complete. If your agents used embedded hooks, continue to [hooks migration →](https://kiro.dev/docs/cli/v3/hooks-migration.md).
