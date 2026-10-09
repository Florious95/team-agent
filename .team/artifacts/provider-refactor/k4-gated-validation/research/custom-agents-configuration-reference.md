> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# Agent configuration reference

> Complete reference for agent configuration file format and advanced settings

This reference documents the agent configuration format for IDE 1.0 and CLI 3.0. If you have older agent configs, use the [`/upgrade-agent`](#upgrading-from-older-configs) command to migrate them.

## Version comparison

**New in IDE 1.0 / CLI 3.0:**

| Field | Description |
|-------|-------------|
| `permissions` | Capability-based access control rules (replaces `toolsSettings`) |
| `excludedTools` | Exclude specific tools even when `tools` allows them |
| `includeMcpJson` | Auto-include workspace MCP servers |
| `includePowers` | Auto-include IDE-installed powers |
| `welcomeMessage` | Custom greeting on session start |
| `resources` (expanded) | Now supports `skill://` and `knowledgeBase` in addition to `file://` |
| Markdown format (`.md`) | Frontmatter for config, body for system prompt |
| Tags in `tools` | Short names: `read`, `write`, `shell`, `web`, `@builtin`, `*` |

**Deprecated:**

| Field | Replacement |
|-------|-------------|
| `toolsSettings` (shell/write rules) | `permissions.rules` with capability-based patterns |

**Unchanged:** `name`, `description`, `prompt`, `model`, `mcpServers`, `toolAliases`, `allowedTools`, `keyboardShortcut`, `hooks` (CLI only - IDE ignores this field)

Use [`/upgrade-agent`](#upgrading-from-older-configs) to migrate supported fields in older configs.

## Field reference

Every agent configuration file can include the following sections:

- [`name`](#name-field) - The name of the agent (optional, derived from filename if not specified).
- [`description`](#description-field) - A description of the agent.
- [`prompt`](#prompt-field) - High-level context for the agent.
- [`mcpServers`](#mcpservers-field) - The MCP servers the agent has access to.
- [`tools`](#tools-field) - The tools available to the agent.
- [`toolAliases`](#toolaliases-field) - Tool name remapping for handling naming collisions.
- [`allowedTools`](#allowedtools-field) - Tools that can be used without prompting.
- [`permissions`](#permissions-field) - Inline capability-based access control rules.
- [`toolsSettings`](#toolssettings-field) - Per-tool configuration (deprecated for shell/fs rules).
- [`resources`](#resources-field) - Resources available to the agent.
- [`hooks`](#hooks-field) - Commands run at specific trigger points.
- [`includeMcpJson`](#includemcpjson-field) - Whether to include MCP servers from mcp.json files.
- [`model`](#model-field) - The model ID to use for this agent.
- [`keyboardShortcut`](#keyboardshortcut-field) - Keyboard shortcut for quickly switching to this agent.
- [`welcomeMessage`](#welcomemessage-field) - Message displayed when switching to this agent.

## Name field

The `name` field specifies the name of the agent. This is used for identification and display purposes.

```json
{
  "name": "aws-expert"
}
```

## Description field

The `description` field provides a description of what the agent does. This is primarily for human readability and helps users distinguish between different agents.

```json
{
  "description": "An agent specialized for AWS infrastructure tasks"
}
```

## Prompt field

The `prompt` field is intended to provide high-level context to the agent, similar to a system prompt. It supports both inline text and file:// URIs to reference external files.

### Inline prompt

```json
{
  "prompt": "You are an expert AWS infrastructure specialist"
}
```

### File URI prompt

You can reference external files using `file://` URIs. This allows you to maintain long, complex prompts in separate files for better organization and version control, while keeping your agent configuration clean and readable.

```json
{
  "prompt": "file://./my-agent-prompt.md"
}
```

#### File URI path resolution

- **Relative paths**: Resolved relative to the agent configuration file's directory
  - `"file://./prompt.md"` - `prompt.md` in the same directory as the agent config
  - `"file://../shared/prompt.md"` - `prompt.md` in a parent directory
- **Absolute paths**: Used as-is
  - `"file:///home/user/prompts/agent.md"` - Absolute path to the file

#### File URI examples

```json
{
  "prompt": "file://./prompts/aws-expert.md"
}
```


```json
{
  "prompt": "file:///Users/developer/shared-prompts/rust-specialist.md"
}
```

## McpServers field

The `mcpServers` field specifies which Model Context Protocol (MCP) servers the agent has access to. Each server is defined with a command and optional arguments.

```json
{
  "mcpServers": {
    "fetch": {
      "command": "fetch3.1",
      "args": []
    },
    "git": {
      "command": "git-mcp",
      "args": [],
      "env": {
        "GIT_CONFIG_GLOBAL": "/dev/null"
      },
      "timeout": 120000
    }
  }
}
```

Each MCP server configuration can include:
- `command` (required for local servers): The command to execute to start the MCP server
- `url` (for remote servers): The HTTP endpoint, with optional `headers` for authenticated endpoints
- `args` (optional): Arguments to pass to the command
- `env` (optional): Environment variables to set for the server. Values support `$` syntax and expand at runtime, keeping secrets out of the config file
- `timeout` (optional): Connection handshake timeout for stdio servers in milliseconds (default: 60000)
- `requestTimeout` (optional): Per-call request timeout in milliseconds (default: 120000)
- `oauth` (optional): OAuth configuration for HTTP-based MCP servers
  - `clientId` (optional): Pre-registered OAuth client ID used as a fallback when Dynamic Client Registration (DCR) fails. Required for services like Slack, GitHub, and Figma that don't support DCR and issue OAuth credentials through manual app registration.
  - `redirectUri` (optional): Custom redirect URI for OAuth flow (e.g., "127.0.0.1:7778")
- `oauthScopes` (optional): Array of OAuth scopes to request (e.g., `["read", "write"]`). This is a top-level field on the server entry — a sibling of `oauth`, not nested inside it.

### OAuth configuration

For HTTP-based MCP servers that require OAuth authentication, you can configure OAuth scopes:

```json
{
  "mcpServers": {
    "github": {
      "type": "http",
      "url": "https://api.github.com/mcp",
      "oauth": {
        "redirectUri": "127.0.0.1:8080"
      },
      "oauthScopes": ["repo", "user"]
    }
  }
}
```

If you encounter OAuth scope-related errors, you can configure an empty array to bypass scope requirements within the MCP server configuration:

```json
{
  "mcpServers": {
    "github": {
      "type": "http",
      "url": "https://api.github.com/mcp",
      "oauth": {
        "redirectUri": "127.0.0.1:8080"
      },
      "oauthScopes": []
    }
  }
}
```

For services that require a pre-registered OAuth app, set `oauth.clientId` to your app's ID:

```json
{
  "mcpServers": {
    "slack": {
      "type": "http",
      "url": "https://mcp.slack.com/mcp",
      "oauth": {
        "clientId": "your-slack-app-client-id"
      },
      "oauthScopes": ["search:read", "channels:read"]
    }
  }
}
```

## Tools field

The `tools` field lists all tools that the agent can potentially use. Tools include built-in tools and tools from MCP servers.

- Built-in tools are specified by their name (e.g., `read`, `shell`)
- MCP server tools are prefixed with `@` followed by the server name (e.g., `@git`)
- To specify a specific tool from an MCP server, use `@server_name/tool_name`
- Use `*` as a special wildcard to include all available tools (both built-in and from MCP servers)
- Use `@builtin` to include all built-in tools
- Use `@server_name` to include all tools from a specific MCP server

The field also accepts category tags. Each tag groups related capabilities so you don't need to enumerate individual tools:

| Tag | What it includes |
|-----|-----------------|
| `read` | File reading, directory listing, searching |
| `write` | File writing, editing, deleting |
| `shell` | Command execution and process management |
| `web` | Web fetching |
| `subagent` | Subagent delegation |
| `knowledge` | Knowledge base tools |
| `todo_list` | Task tracking in engines that provide it; V3 does not |
| `@mcp` | All MCP tools from mcp.json |
| `@builtin` | All built-in tools |
| `*` | Everything |

When new tools ship under a category, agents using that tag pick them up automatically.

```json
{
  "tools": [
    "read",
    "write",
    "shell",
    "@git",
    "@rust-analyzer/check_code"
  ]
}
```

To include all available tools, use:

```json
{
  "tools": ["*"]
}
```

## ToolAliases field

The `toolAliases` field is an advanced feature that allows you to remap tool names. This is primarily used to resolve naming collisions between tools from different MCP servers, or to create more intuitive names for specific tools.

For example, if both `@github-mcp` and `@gitlab-mcp` servers provide a tool called `get_issues`, you would have a naming collision. You can use `toolAliases` to disambiguate them:

```json
{
  "toolAliases": {
    "@github-mcp/get_issues": "github_issues",
    "@gitlab-mcp/get_issues": "gitlab_issues"
  }
}
```

With this configuration, the tools will be available to the agent as `github_issues` and `gitlab_issues` instead of having a collision on `get_issues`.

You can also use aliases to create shorter or more intuitive names for frequently used tools:

```json
{
  "toolAliases": {
    "@aws-cloud-formation/deploy_stack_with_parameters": "deploy_cf",
    "@kubernetes-tools/get_pod_logs_with_namespace": "pod_logs"
  }
}
```

The key is the original tool name (including server prefix for MCP tools), and the value is the new name to use.

## AllowedTools field

The `allowedTools` field specifies which tools can be used without prompting the user for permission. This is a security feature that helps prevent unauthorized tool usage.

```json
{
  "allowedTools": [
    "read",
    "write",
    "@git/git_status",
    "@server/read_*",
    "@fetch"
  ]
}
```

You can allow tools using several patterns:

### Exact matches
- **Built-in tools**: `"read"`, `"shell"`, `"knowledge"`
- **Specific MCP tools**: `"@server_name/tool_name"` (e.g., `"@git/git_status"`)
- **All tools from MCP server**: `"@server_name"` (e.g., `"@fetch"`)

### Wildcard patterns
The `allowedTools` field supports glob-style wildcard patterns using `*` and `?`:

#### MCP tool patterns
- **Tool prefix**: `"@server/read_*"` - matches `@server/read_file`, `@server/read_config`
- **Tool suffix**: `"@server/*_get"` - matches `@server/issue_get`, `@server/data_get`
- **Server pattern**: `"@*-mcp/read_*"` - matches `@git-mcp/read_file`, `@db-mcp/read_data`
- **Any tool from pattern servers**: `"@git-*/*"` - matches any tool from servers matching `git-*`

Optionally, you can also prefix native tools with the namespace `@builtin`.

### Examples

```json
{
  "allowedTools": [
    "read",
    "knowledge",
    "@server/specific_tool",

    "r*",
    "w*",
    "@builtin",

    "@server/api_*",
    "@server/read_*",
    "@git-server/get_*_info",
    "@*/status",

    "@fetch",
    "@git-*"
  ]
}
```

### Pattern matching rules
- **`*`** matches any sequence of characters (including none)
- **`?`** matches exactly one character
- **Exact matches** take precedence over patterns
- **Server-level permissions** (`@server_name`) allow all tools from that server
- **Case-sensitive** matching

Unlike the `tools` field, the `allowedTools` field does not support the `"*"` wildcard for allowing all tools. To allow tools, you must use specific patterns or server-level permissions.

**⚠️ Warning:** When you allow write tools (such as `write`, `shell`, or MCP tools with write access), the agent can modify any file the tools have access to - including all resources under `~/.kiro` such as skills, steering files, MCP configurations, and other agent configurations. All installed skills and resources operate with the same permissions as the agent. Review the [Security best practices](#security-best-practices) section before enabling write tools.

## ToolsSettings field

**⚠️ Warning:** `toolsSettings` is deprecated in CLI 3.0 and IDE 1.0. Use the [`permissions`](#permissions-field) field instead for capability-based access control. See [Permissions migration](https://kiro.dev/docs/cli/v3/permissions.md) for the full guide.

The `toolsSettings` field provides per-tool configuration. In older versions, this was used for shell command allow/deny lists and file path restrictions. These use cases are now handled by the `permissions` field.

The field is still supported for MCP tool-specific settings:

```json
{
  "toolsSettings": {
    "@git/git_status": {
      "git_user": "$GIT_USER"
    },
    "subagent": {
      "availableAgents": ["reviewer", "tester"],
      "trustedAgents": ["reviewer"]
    }
  }
}
```

## Permissions field

The `permissions` field provides inline capability-based policy rules embedded in the agent profile. This replaces the older `toolsSettings` approach for controlling what tools can do.

Rules use the same syntax as `permissions.yaml` files (see [Permissions](https://kiro.dev/docs/permissions.md)) but are scoped to this agent only.

```json
{
  "permissions": {
    "rules": [
      { "capability": "shell", "match": ["npm *", "git *"], "effect": "allow" },
      { "capability": "fs_write", "match": ["src/**", "tests/**"], "effect": "allow" },
      { "capability": "shell", "match": ["rm -rf *", "sudo *"], "effect": "deny" }
    ]
  }
}
```

Each rule has:

| Field | Description |
|-------|-------------|
| `capability` | The capability to control: `fs_read`, `fs_write`, `shell`, `web_fetch`, `web_search`, `mcp`, `subagent`, `all` |
| `match` | Glob patterns scoping the rule (file paths for fs, command prefixes for shell, server/tool names for MCP) |
| `effect` | `allow` (proceed silently), `ask` (prompt user), or `deny` (block always) |
| `exclude` | Optional glob patterns that must NOT match |

Agent-scoped permissions support all three effects (`allow`, `ask`, `deny`). The deny-overrides algorithm applies: a `deny` in any scope wins regardless of `allow` rules elsewhere.

## Resources field

The `resources` field gives an agent access to local resources. Resources can be files, skills, or knowledge bases.

```json
{
  "resources": [
    "file://README.md",
    "file://.kiro/steering/**/*.md",
    "skill://.kiro/skills/**/SKILL.md"
  ]
}
```

Resources support different types via URI schemes:

- `file://` - Files loaded directly into context at startup
- `skill://` - Skills with metadata loaded at startup, full content loaded on demand

Both support:
- Specific paths: `file://README.md` or `skill://my-skill.md`
- Glob patterns: `file://.kiro/**/*.md` or `skill://.kiro/skills/**/SKILL.md`
- Absolute or relative paths

### File resources

File resources are loaded directly into the agent's context when the agent starts. Use these for content the agent always needs.

```json
{
  "resources": [
    "file://README.md",
    "file://docs/**/*.md"
  ]
}
```

### Skill resources

Skills are progressively loaded - only metadata (name and description) is loaded at startup, with full content loaded on demand when the agent determines it's needed. This keeps context lean while giving agents access to extensive documentation.

Skill files must begin with YAML frontmatter containing `name` and `description`:

```markdown
---
name: dynamodb-data-modeling
description: Guide for DynamoDB data modeling best practices. Use when designing or analyzing DynamoDB schema.
---

# DynamoDB Data Modeling

... full content here ...
```


```json
{
  "resources": [
    "skill://.kiro/skills/**/SKILL.md"
  ]
}
```

Write specific descriptions so the agent can reliably determine when to load the full content.

### Knowledge base resources

Knowledge base resources allow agents to search indexed documentation and content. With support for millions of tokens of indexed content and incremental loading, agents can efficiently search large documentation sets.

```json
{
  "resources": [
    {
      "type": "knowledgeBase",
      "source": "file://./docs",
      "name": "ProjectDocs",
      "description": "Project documentation and guides",
      "indexType": "best",
      "autoUpdate": true
    }
  ]
}
```

**Fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | Must be `"knowledgeBase"` |
| `source` | Yes | Path to index. Use `file://` prefix for local paths |
| `name` | Yes | Display name for the knowledge base |
| `description` | No | Brief description of the content |
| `indexType` | No | Indexing strategy: `"best"` (default, higher quality) or `"fast"` (quicker indexing) |
| `autoUpdate` | No | Re-index when agent spawns. Default: `false` |

**Use cases:**
- Share team documentation across agents
- Give agents access to project-specific context (specs, decisions, meeting notes)
- Index large codebases and documentation
- Keep agent knowledge current with `autoUpdate: true`

## Hooks field

The `hooks` field defines commands to run at specific trigger points during agent lifecycle and tool execution.

Both the CLI and the IDE accept hooks written in this format, so an agent profile built for Kiro CLI loads in the IDE without rewriting its hooks.

For detailed information about hook behavior, input/output formats, and examples, see the [Hooks documentation](https://kiro.dev/docs/hooks.md).

```json
{
  "hooks": {
    "agentSpawn": [
      {
        "command": "git status"
      }
    ],
    "userPromptSubmit": [
      {
        "command": "ls -la"
      }
    ],
    "preToolUse": [
      {
        "matcher": "execute_bash",
        "command": "{ echo \"$(date) - Bash command:\"; cat; echo; } >> /tmp/bash_audit_log"
      },
      {
        "matcher": "use_aws",
        "command": "{ echo \"$(date) - AWS CLI call:\"; cat; echo; } >> /tmp/aws_audit_log"
      }
    ],
    "postToolUse": [
      {
        "matcher": "fs_write",
        "command": "cargo fmt --all"
      }
    ]
  }
}
```

Each hook is defined with:
- `command` (required): The command to execute
- `matcher` (optional): Pattern to match tool names for `preToolUse` and `postToolUse` hooks. Hook matchers use internal tool names (`fs_read`, `fs_write`, `execute_bash`, `use_aws`) rather than simplified names. See the [built-in tools documentation](https://kiro.dev/docs/reference/built-in-tools.md) for available tool names.

Available hook triggers:
- `agentSpawn`: Triggered when the agent is initialized.
- `userPromptSubmit`: Triggered when the user submits a message.
- `preToolUse`: Triggered before a tool is executed. Can block the tool use.
- `postToolUse`: Triggered after a tool is executed.
- `stop`: Triggered when the assistant finishes responding.

## includeMcpJson field

The `includeMcpJson` field determines whether to include MCP servers defined in the MCP configuration files (`~/.kiro/settings/mcp.json` for global and `<cwd>/.kiro/settings/mcp.json` for workspace).

```json
{
  "includeMcpJson": true
}
```

When set to `true`, the agent will have access to all MCP servers defined in the global and local configurations in addition to those defined in the agent's `mcpServers` field.

## Model field

The `model` field specifies the model ID to use for this agent. If not specified, the agent will use the default model.

```json
{
  "model": "claude-sonnet-4"
}
```

The model ID must match one of the available models returned by Kiro's model service. You can see available models by using the `/model` command in an active chat session.

If the specified model is not available, the agent will fall back to the default model and display a warning.

## KeyboardShortcut field

The `keyboardShortcut` field configures a keyboard shortcut for quickly switching to this agent during a chat session.

```json
{
  "keyboardShortcut": "ctrl+a"
}
```

Shortcuts consist of a modifier and a key, separated by `+`:

**Modifiers** (optional):
- `ctrl` - Control key
- `shift` - Shift key

**Keys**:
- Single letter: `a-z` (case insensitive)
- Single digit: `0-9`

**Examples**:

```json
"keyboardShortcut": "ctrl+a"
"keyboardShortcut": "shift+b"
```

**Toggle behavior:**

When you press a keyboard shortcut:
- If you're on a different agent: switches to this agent
- If you're already on this agent: switches back to your previous agent

**Conflict handling:**

If multiple agents have the same keyboard shortcut, a warning is logged and the shortcut is disabled. Use `/agent swap` to switch manually in this case.

## WelcomeMessage field

The `welcomeMessage` field specifies a message displayed when switching to this agent.

```json
{
  "welcomeMessage": "What would you like to build today?"
}
```

This message appears after the agent switch confirmation, helping orient users to the agent's purpose.

## Disabling default resource inheritance

By default, custom agents inherit default resources (steering files, skills, and `AGENTS.md`) alongside their own configured resources. You can disable this behavior with the `chat.disableInheritingDefaultResources` CLI setting.

| Property | Value |
|----------|-------|
| Setting key | `chat.disableInheritingDefaultResources` |
| Type | Boolean |
| Default | `false` (custom agents inherit default resources) |
| Scope | Global or workspace-overridable |

Set it via the CLI:

```bash
kiro-cli settings chat.disableInheritingDefaultResources true
```

Or scope it to a workspace:

```bash
kiro-cli settings --workspace chat.disableInheritingDefaultResources true
```

When set to `true`, custom (user-defined) agents will not receive default steering, skills, or `AGENTS.md` in their context. Built-in agents always inherit default resources regardless of this setting.

## Complete example

```json
{
  "name": "aws-rust-agent",
  "description": "A specialized agent for AWS and Rust development tasks",
  "mcpServers": {
    "fetch": {
      "command": "fetch3.1",
      "args": []
    },
    "git": {
      "command": "git-mcp",
      "args": []
    }
  },
  "tools": [
    "read",
    "write",
    "shell",
    "@git",
    "@fetch/fetch_url"
  ],
  "toolAliases": {
    "@git/git_status": "status",
    "@fetch/fetch_url": "get"
  },
  "allowedTools": [
    "read",
    "@git/git_status"
  ],
  "permissions": {
    "rules": [
      { "capability": "shell", "match": ["cargo *", "git *"], "effect": "allow" },
      { "capability": "fs_write", "match": ["src/**", "tests/**", "Cargo.toml"], "effect": "allow" },
      { "capability": "shell", "match": ["rm -rf *"], "effect": "deny" }
    ]
  },
  "resources": [
    "file://README.md",
    "file://docs/**/*.md"
  ],
  "hooks": {
    "agentSpawn": [
      {
        "command": "git status"
      }
    ],
    "postToolUse": [
      {
        "matcher": "fs_write",
        "command": "cargo fmt --all"
      }
    ]
  },
  "includeMcpJson": true,
  "model": "claude-sonnet-4",
  "keyboardShortcut": "ctrl+r",
  "welcomeMessage": "Ready to help with AWS and Rust development!"
}
```

## Best practices

1. **Start restrictive**: Begin with minimal tool access and expand as needed
2. **Name clearly**: Use descriptive names that indicate the agent's purpose
3. **Document usage**: Add clear descriptions to help team members understand the agent
4. **Version control**: Store agent configurations in your project repository
5. **Test thoroughly**: Verify tool permissions work as expected before sharing

### Local vs global agents

**Use local agents for:**
- Project-specific configurations
- Agents needing project files/tools
- Development environments with unique requirements
- Sharing with team via version control

**Use global agents for:**
- General-purpose agents across projects
- Personal productivity agents
- Agents without project-specific context
- Commonly used tools and workflows

### Security best practices

- Review `allowedTools` carefully
- Use specific patterns over wildcards
- Use `permissions.rules` to restrict sensitive operations
- Test agents in safe environments first

#### Write tool permissions

By default, the Kiro agent only has access to read-only tools. No write operations are permitted unless you explicitly enable them in `allowedTools` or approve them at runtime.

When you enable write tools (such as `write`, `shell`, or MCP tools with write capabilities), the agent operates with the same file-system permissions as your user account. This means:

- The agent can read and modify all files under `~/.kiro`, including skill context files, steering files, MCP server configurations (`mcp.json`), and other agent configurations.
- All installed skills and resources share the same permissions. There is no isolation between individual skills - any skill's context can be read or modified by the agent when write tools are enabled.
- A skill cannot execute code on its own. Skills are text files that provide instructions to the agent. However, if write tools like `shell` are allowed, the agent could execute commands referenced in any loaded skill.

To reduce risk when using write tools:

- Only enable the specific write tools you need (e.g., `"write"` but not `"shell"`).
- Define path and command restrictions with `permissions.rules` using narrow `match` patterns.
- Review skills and resources before installing them, especially from untrusted sources.
- Use `preToolUse` [hooks](https://kiro.dev/docs/hooks.md) to audit or block sensitive operations.

## Upgrading from older configs

If you have agent configs created before IDE 1.0 or CLI 3.0, `/upgrade-agent` identifies configs that need migration, backs up originals, and converts supported fields in place.

```bash
# Scan agents and open the selection menu
/upgrade-agent

# Review previously upgraded agents and any conversion warnings
/upgrade-agent diagnostics
```

The command scans `.kiro/agents/` in your workspace and `~/.kiro/agents/` globally, then shows a selection menu grouped by scope. Only agents that need upgrading appear.

Legacy CLI 2.x profiles remain visible and selectable in the V3 `/agent` picker. `/upgrade-agent` converts object-form hooks to the array format required by V3.

Before:

```json
{
  "hooks": {
    "agentSpawn": {
      "command": "git status"
    }
  }
}
```

After:

```json
{
  "hooks": {
    "agentSpawn": [
      {
        "command": "git status"
      }
    ]
  }
}
```

### What gets converted

| Old pattern | New equivalent |
|------------|----------------|
| `toolsSettings.shell.allowedCommands` | `permissions.rules` with `capability: shell`, `effect: allow` |
| `toolsSettings.shell.deniedCommands` | `permissions.rules` with `capability: shell`, `effect: deny` |
| `toolsSettings.write.allowedPaths` | `permissions.rules` with `capability: fs_write`, `effect: allow` |
| `allowedTools` entries | Capability-level allow rules |
| Tool names (`fs_read`, `execute_bash`) | Tags (`read`, `shell`) - both still work |
| `autoAllowReadonly` | Read-only shell policy rule |
| Object-form hooks | Array-form hooks required by the V3 schema |

Original files are backed up to `<filename>.json.bak`. To revert, rename the backup back.

For the full upgrade guide including diagnostics and troubleshooting, see [What's new in CLI 3.0 - Agent config](https://kiro.dev/docs/cli/v3/agent-config.md).

## Next steps

- [Creating custom agents](https://kiro.dev/docs/custom-agents/creating.md)
- [Examples](https://kiro.dev/docs/custom-agents/examples.md) - real-world agent configurations
- [Troubleshooting](https://kiro.dev/docs/custom-agents/troubleshooting.md) - resolve common issues
- [Hooks documentation](https://kiro.dev/docs/hooks.md)
