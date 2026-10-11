> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# CLI commands

> Complete reference for all Kiro CLI commands

This page provides a comprehensive reference for all Kiro CLI commands and their arguments.

## Global arguments

These arguments work with any Kiro CLI command:

| Argument | Short | Description |
|----------|-------|-------------|
| `--verbose` | `-v` | Increase logging verbosity (can be repeated: `-v`, `-vv`, `-vvv`) |
| `--agent` | | Start a conversation using a specific custom agent configuration |
| `--v3` | | Start with the V3 agent harness for this invocation |
| `--v2` | | Start with the V2 agent harness for this invocation |
| `--help` | `-h` | Show help information |
| `--version` | `-V` | Show version information |
| `--help-all` | | Print help for all subcommands |

`--v2` and `--v3` cannot be combined, and at the root `--v2` also conflicts with `--legacy-ui`. `--v2` takes precedence over a saved `chat.agentEngine` value for that invocation without changing it. Both flags are also accepted on `kiro-cli chat`; an explicit harness on `chat` wins over a root shorthand.

## Commands

### kiro-cli crew

Launch [Kiro Crew](https://github.com/kirodotdev/KiroCrew) or pass arguments to its command-line interface.

**Syntax:**
```bash
kiro-cli crew [--yes] [CREW_ARGUMENTS]...
```

On macOS and Linux, Kiro CLI prompts to install Crew when it isn't available. Kiro CLI consumes `--yes` or `-y` to install without the prompt. It forwards all other arguments after `crew` directly to Crew, without a `--` separator. On Windows, install Crew separately before using this command.

**Examples:**
```bash
# Start the Crew gateway to get started
kiro-cli crew gateway

# Install when needed, skip the prompt, and start the gateway
kiro-cli crew --yes gateway

# Forward a Crew option directly
kiro-cli crew --version
```

### kiro-cli agent

Manage agent configurations. Agent name is now a positional argument for create, edit, and other subcommands.

**Syntax:**
```bash
kiro-cli agent [SUBCOMMAND] [AGENT_NAME] [OPTIONS]
```

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `list` | List the available agents |
| `create <name>` | Create an agent config (name is positional argument, v1.26.0+) |
| `edit [name]` | Edit an existing agent config (defaults to current agent if no name provided, v1.26.0+) |
| `validate` | Validate a config with the given path |
| `migrate` | Migrate profiles to agents (potentially destructive to existing agents) |
| `set-default` | Define a default agent to use when starting a session |

**Examples:**
```bash
kiro-cli agent list

# v1.26.0+: Agent name as positional argument
kiro-cli agent create my-agent
kiro-cli agent edit my-agent
kiro-cli agent edit  # Defaults to current agent

# Previous syntax (still supported)
kiro-cli agent validate ./my-agent.json
kiro-cli agent set-default my-agent
```

**New in v1.26.0:**
- Agent name is now a positional argument (e.g., `kiro-cli agent create my-agent` instead of `--name my-agent`)
- `edit` command defaults to editing the current agent when no argument is provided

### kiro-cli chat

Start an interactive chat session with Kiro. When no subcommand is specified, `kiro` defaults to `kiro-cli chat`.

**Syntax:**
```bash
kiro-cli chat [OPTIONS] [INPUT]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--no-interactive` | Print first response to STDOUT without interactive mode |
| `--resume` / `-r` | Resume the previous conversation from this directory |
| `--resume-picker` | Open interactive session picker to choose which session to resume |
| `--resume-id <ID>` | Resume a specific session by its ID |
| `--list-sessions` | List all saved chat sessions for the current directory |
| `--list-models` | Display available models |
| `--delete-session <ID>` | Delete a saved chat session by ID |
| `--agent` | Specify which agent to use |
| `--v3` | Use the V3 agent harness for this session. Conflicts with `--v2` and `--agent-engine` |
| `--v2` | Use the V2 agent harness for this session. Takes precedence over a saved `chat.agentEngine` value without changing it. Conflicts with `--v3` and `--agent-engine` |
| `--trust-all-tools` | Allow the model to use any tool without confirmation |
| `--trust-tools` | Trust only specified tools (comma-separated list) |
| `--require-mcp-startup` | Exit with code 3 if any MCP server fails to start |
| `--wrap` | Line wrapping mode: `always`, `never`, or `auto` (default) |
| `--effort <LEVEL>` | Set the initial reasoning effort level for the session: `low`, `medium`, `high`, `xhigh`, or `max` |
| `INPUT` | The first question to ask (positional argument) |

**Examples:**
```bash
# Start interactive chat
kiro-cli 

# Ask a question directly
kiro-cli chat "How do I list files in Linux?"

# Non-interactive mode with trusted tools
kiro-cli chat --no-interactive --trust-all-tools "Show me the current directory"

# Resume previous conversation
kiro-cli chat --resume

# Resume a specific session by ID
kiro-cli chat --resume-id abc123-def456

# Open session picker to choose which session to resume
kiro-cli chat --resume-picker

# List all saved sessions
kiro-cli chat --list-sessions

# List available models (plain text)
kiro-cli chat --list-models

# List available models (JSON output for scripting)
kiro-cli chat --list-models --format json

# Use specific agent
kiro-cli chat --agent my-agent "Help me with AWS CLI"

# Use the V3 harness for this session
kiro-cli chat --v3

# Use the V2 harness for this session, leaving your saved default unchanged
kiro-cli chat --v2

# Set the initial reasoning effort level at launch
kiro-cli chat --effort high "Refactor this module for testability"
```

### kiro-cli translate

Translate natural language instructions to executable shell commands using AI.

**Syntax:**
```bash
kiro-cli translate [OPTIONS] [INPUT...]
```

**Arguments:**

| Argument | Short | Description |
|----------|-------|-------------|
| `--n` | `-n` | Number of completions to generate (max 5) |
| `INPUT` | | Natural language description (positional arguments) |

**Examples:**
```bash
kiro-cli translate "list all files in the current directory"
kiro-cli translate "find all Python files modified in the last week"
kiro-cli translate "compress all log files older than 30 days"
kiro-cli translate -n 3 "search for text in files"
```

### kiro-cli doctor

Diagnose and fix common installation and configuration issues.

**Syntax:**
```bash
kiro-cli doctor [OPTIONS]
```

**Arguments:**

| Argument | Short | Description |
|----------|-------|-------------|
| `--all` | `-a` | Run all diagnostic tests without fixes |
| `--strict` | `-s` | Error on warnings |
| `--format` | `-f` | Output format: `plain`, `json`, `json-pretty` |

**Examples:**
```bash
kiro-cli doctor
kiro-cli doctor --all
kiro-cli doctor --strict
```

### kiro-cli update

Update Kiro CLI to the latest version.

**Syntax:**
```bash
kiro-cli update [OPTIONS]
```

**Options:**

The available options depend on your platform.

    macOS
    Windows

| Option | Short | Description |
|--------|-------|-------------|
| `--non-interactive` | `-y` | Don't prompt for confirmation |
| `--relaunch-dashboard` | | Relaunch the dashboard after updating (default: false) |
| `--rollout` | | Honor staged rollout timing |

`--check` and `--force` are not available on macOS.

```bash
kiro-cli update
kiro-cli update --non-interactive
```

| Option | Description |
|--------|-------------|
| `--check` | Only check for updates without installing |
| `--force` | Install the advertised version even when the installed version is equal or newer (can reinstall or downgrade) |
| `--help` | Print help information |

```bash
kiro-cli update
kiro-cli update --check
```

### kiro-cli theme

Get or set the visual theme for the autocomplete dropdown menu.

**Syntax:**
```bash
kiro-cli theme [OPTIONS] [THEME]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--list` | List all available themes |
| `--folder` | Show the theme directory path |
| `THEME` | Theme name: `dark`, `light`, `system` |

**Examples:**
```bash
kiro-cli theme --list
kiro-cli theme dark
kiro-cli theme light
kiro-cli theme system
```

### kiro-cli integrations

Manage system integrations for Kiro.

**Syntax:**
```bash
kiro-cli integrations [SUBCOMMAND] [OPTIONS]
```

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `install [integration]` | Install an integration (e.g., kiro-command-router) |
| `uninstall [integration]` | Uninstall an integration |
| `reinstall [integration]` | Reinstall an integration |
| `status` | Check integration status |

**Options:**
- `--silent` / `-s`: Suppress status messages
- `--format` / `-f`: Output format (for status command)

**Examples:**
```bash
# Install kiro command router (v1.26.0+)
kiro-cli integrations install kiro-command-router

# Check integration status
kiro-cli integrations status

# Uninstall silently
kiro-cli integrations uninstall --silent
```

#### Kiro Command Router (v1.26.0+)

The kiro command router is a unified entry point that routes the `kiro` command between CLI and IDE based on your preference.

**Problem it solves:** By default, the `kiro` command launches Kiro IDE. Many users prefer it to launch the CLI since they use the app icon to open the IDE.

**Installation:**
```bash
# Install the router
kiro-cli integrations install kiro-command-router

# Set CLI as the default
kiro set-default cli

# Or set IDE as the default
kiro set-default ide
```

**After installation:**
- `kiro` - Launches your default (CLI or IDE)
- `kiro-cli` - Always launches CLI
- `kiro ide` - Always launches IDE

**Use cases:**
- CLI-focused workflows where you primarily use the terminal
- Quick access to CLI without typing `kiro-cli` every time
- Flexibility to switch defaults based on your current project or workflow

### kiro-cli inline

Manage inline suggestions (ghost text) that appear as you type.

**Syntax:**
```bash
kiro-cli inline [SUBCOMMAND] [OPTIONS]
```

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `enable` | Enable inline suggestions |
| `disable` | Disable inline suggestions |
| `status` | Show current status |
| `set-customization` | Select a customization model |
| `show-customizations` | Show available customizations |

**Examples:**
```bash
kiro-cli inline enable
kiro-cli inline disable
kiro-cli inline status
kiro-cli inline set-customization
kiro-cli inline show-customizations --format json
```

### kiro-cli login

Authenticate with Kiro CLI service using Builder ID, Identity Center, or social login (Google, GitHub).

**Syntax:**
```bash
kiro-cli login [OPTIONS]
```

**Options:**

| Option | Description |
|--------|-------------|
| `--license <TYPE>` | License type: `pro` (Identity Center) or `free` (Builder ID, Google, GitHub) |
| `--identity-provider <URL>` | Identity provider URL (for Identity Center) |
| `--region <REGION>` | AWS region (for Identity Center) |
| `--social <PROVIDER>` | Social provider: `google` or `github` |
| `--use-device-flow` | Force device flow (for remote/SSH environments) |
| `--verbose` | Increase logging verbosity (can be repeated) |
| `--help` | Print help information |

**Authentication Methods:**

**Local Environment:**
- Opens browser for unified auth portal
- Select authentication method interactively
- Login-selection flags (e.g., `--license`, `--social`) are generally ignored locally; the browser flow controls the method

**Remote Environment (SSH/Terminal):**
- Uses device flow automatically
- Shows device code and URL
- Complete authentication on another device
- CLI polls for completion

**Examples:**
```bash
# Basic login (opens browser locally, shows device code remotely)
kiro-cli login

# Identity Center login
kiro-cli login --license pro --identity-provider https://my-org.awsapps.com/start --region us-east-1

# Social login
kiro-cli login --social google

# Force device flow (useful for SSH sessions)
kiro-cli login --use-device-flow
```

**Troubleshooting:**

- **Already logged in error**: Logout first with `kiro-cli logout`
- **Browser doesn't open**: Use `--use-device-flow` flag
- **Authentication timeout**: Restart the login process
- **Identity Center fails**: Verify URL and region with your administrator

### kiro-cli logout

Sign out of Kiro CLI service and clear authentication credentials.

**Syntax:**
```bash
kiro-cli logout
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--verbose` | `-v` | Increase logging verbosity (can be repeated) |
| `--help` | `-h` | Print help information |

**What Gets Cleared:**
- Authentication tokens
- Session credentials
- User profile information

**What's Preserved:**
- Agent configurations
- Saved conversations
- Settings
- MCP server configurations

**Example:**
```bash
kiro-cli logout
```

**Output:**
```
You are now logged out
Run kiro-cli login to log back in to Kiro CLI
```

**Note:** Logout is user-wide and affects all workspaces.

### kiro-cli whoami

Display information about the current user and authentication status, including your email address for Builder ID, IAM Identity Center, and Social login types.

**Syntax:**
```bash
kiro-cli whoami [OPTIONS]
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--format` | `-f` | Output format: `plain`, `json`, `json-pretty` |
| `--verbose` | `-v` | Increase logging verbosity (can be repeated) |
| `--help` | `-h` | Print help information |

**Output Information:**
- Username/user ID
- Authentication method (Builder ID, Identity Center, Social)
- Session status
- Profile information

**Examples:**
```bash
# Check current user
kiro-cli whoami
# Logged in with Builder ID
# Email: user@example.com

kiro-cli whoami --format json
kiro-cli whoami --format json-pretty
```

**Example Output (Identity Center):**
```
Logged in with IAM Identity Center (https://my-org.awsapps.com/start)

Profile:
Q-Dev-Amazon-Profile
arn:aws:codewhisperer:us-east-1:...:profile/...
```

**Example Output (Builder ID):**
```
Logged in with Builder ID

Profile:
builder-id-username
```

**Troubleshooting:**

- **Not logged in error**: Login with `kiro-cli login`

### kiro-cli settings

Manage kiro-cli configuration settings.

**Syntax:**
```bash
kiro-cli settings [SUBCOMMAND] [OPTIONS] [KEY] [VALUE]
```

**Arguments:**

| Argument | Short | Description |
|----------|-------|-------------|
| `--delete` | `-d` | Delete a setting |
| `--format` | `-f` | Output format: `plain`, `json`, `json-pretty` |
| `KEY` | | Setting key (positional) |
| `VALUE` | | Setting value (positional) |

**Subcommands:**

| Subcommand | Description |
|------------|-------------|
| `open` | Open settings file in default editor |
| `list` | List configured settings |
| `list --all` | List all available settings with descriptions |

**Examples:**
```bash
# View all settings
kiro-cli settings list

# View all available settings
kiro-cli settings list --all

# Get a specific setting
kiro-cli settings telemetry.enabled

# Set a setting
kiro-cli settings telemetry.enabled true

# Delete a setting
kiro-cli settings --delete chat.defaultModel

# Open settings file
kiro-cli settings open

# JSON output
kiro-cli settings list --format json-pretty
```

### kiro-cli diagnostic

Run diagnostic tests and generate system information report for troubleshooting.

**Syntax:**
```bash
kiro-cli diagnostic [OPTIONS]
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--format` | `-f` | Output format: `plain`, `json`, `json-pretty` (default: `plain`) |
| `--force` | | Force limited diagnostic output (faster, works without app running) |
| `--verbose` | `-v` | Increase logging verbosity (can be repeated) |
| `--help` | `-h` | Print help information |

The `plain` format outputs Markdown-formatted text.

**Behavior:**

- **Without `--force`**: Requires Kiro CLI app to be running (use `kiro-cli launch` first). Generates comprehensive diagnostics by connecting to the running app.
- **With `--force`**: Standalone command that works without the app running. Generates limited but faster diagnostics.

**Output Information:**

The diagnostic report includes:
- System information (OS, architecture, memory)
- Kiro CLI version and build details
- Configuration status
- Environment variables
- Installed dependencies
- Potential issues

**Examples:**
```bash
# Generate full diagnostic report
kiro-cli diagnostic

# JSON output
kiro-cli diagnostic --format json-pretty

# Limited output (faster)
kiro-cli diagnostic --force
```

**Example Output (TOML format):**
```toml
[q-details]
version = "1.23.0"
hash = "97d58722cd90f6d3dda465f6462ee4c6dc104b22"
date = "2025-12-18T16:49:27.015389Z (4d ago)"
variant = "full"

[system-info]
os = "macOS 15.7.1 (24G231)"
chip = "Apple M1 Pro"
total-cores = 10
memory = "32.00 GB"

[environment]
cwd = "/Users/user/project"
cli-path = "/Users/user/.cargo/bin/kiro-cli"
os = "Mac"
shell-path = "/bin/bash"
shell-version = "5.1.16"
terminal = "iTerm2"
install-method = "cargo"

[env-vars]
PATH = "..."
SHELL = "/bin/zsh"
TERM = "xterm-256color"
```

**Troubleshooting:**

- **"Kiro CLI app is not running" error**: Launch the app with `kiro-cli launch` or use `--force` flag for standalone diagnostics
- **Diagnostic hangs**: Use `--force` for faster limited output
- **Permission errors**: Run with appropriate permissions or ignore errors

**Use Cases:**
- Troubleshooting installation issues
- Providing information to support
- Verifying environment configuration
- Checking for potential problems

### kiro-cli issue

Create a GitHub issue for feedback or bug reports.

**Syntax:**
```bash
kiro-cli issue [OPTIONS] [DESCRIPTION...]
```

**Arguments:**

| Argument | Short | Description |
|----------|-------|-------------|
| `--force` | `-f` | Force issue creation |
| `DESCRIPTION` | | Issue description (positional) |

**Examples:**
```bash
kiro-cli issue
kiro-cli issue "Autocomplete not working in zsh"
```

### kiro-cli version

Display version information and changelog.

**Syntax:**
```bash
kiro-cli version [OPTIONS]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--changelog` | Show changelog for current version |
| `--changelog=all` | Show changelog for all versions |
| `--changelog=x.x.x` | Show changelog for specific version |

**Examples:**
```bash
kiro-cli version
kiro-cli version --changelog
kiro-cli version --changelog=all
kiro-cli version --changelog=1.5.0
```

### kiro-cli mcp

Manage Model Context Protocol (MCP) servers.

**Syntax:**
```bash
kiro-cli mcp [SUBCOMMAND] [OPTIONS]
```

**Subcommands:**

#### kiro-cli mcp add

Add or replace a configured MCP server.

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--name` | Server name (required) |
| `--command` | Launch command (required) |
| `--scope` | Scope: `workspace` or `global` |
| `--env` | Environment variables: `key1=value1,key2=value2` |
| `--timeout` | Launch timeout in milliseconds |
| `--force` | Overwrite existing server |

**Example:**
```bash
kiro-cli mcp add --name my-server --command "node server.js" --scope workspace
```

#### kiro-cli mcp remove

Remove an MCP server.

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--name` | Server name (required) |
| `--scope` | Scope: `workspace` or `global` |

**Example:**
```bash
kiro-cli mcp remove --name my-server --scope workspace
```

#### kiro-cli mcp list

List configured MCP servers.

**Syntax:**
```bash
kiro-cli mcp list [SCOPE]
```

**Example:**
```bash
kiro-cli mcp list
kiro-cli mcp list workspace
kiro-cli mcp list global
```

#### kiro-cli mcp import

Import server configuration from a file.

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--file` | Configuration file (required) |
| `--force` | Overwrite existing servers |
| `SCOPE` | Scope: `workspace` or `global` |

**Example:**
```bash
kiro-cli mcp import --file config.json workspace
```

#### kiro-cli mcp status

Get the status of an MCP server.

**Arguments:**

| Argument | Description |
|----------|-------------|
| `--name` | Server name (required) |

**Example:**
```bash
kiro-cli mcp status --name my-server
```

## Session management

Kiro CLI automatically saves all chat sessions on every conversation turn. You can resume from any previous chat session at any time.

### From the command line

```bash
# Resume the most recent chat session
kiro-cli chat --resume

# Interactively pick a chat session to resume
kiro-cli chat --resume-picker

# List all saved chat sessions for the current directory
kiro-cli chat --list-sessions

# Delete a saved chat session
kiro-cli chat --delete-session <SESSION_ID>
```

### From within a chat session

Use the `/chat` command to manage sessions:

```bash
# Start a fresh conversation (saves current session automatically)
/chat new

# Start a fresh conversation with an initial prompt
/chat new <PROMPT>

# Resume a chat session (interactive selector)
/chat resume

# Save current session to a file
/chat save <FILE_PATH>

# Load a session from a file
/chat load <FILE_PATH>
```

The `.json` extension is optional when loading - Kiro will try both with and without the extension.

### Custom session storage

You can use custom scripts to control where chat sessions are saved to and loaded from. This allows you to store sessions in version control systems, cloud storage, databases, or any custom location.

```bash
# Save session via custom script (receives JSON via stdin)
/chat save-via-script <SCRIPT_PATH>

# Load session via custom script (outputs JSON to stdout)
/chat load-via-script <SCRIPT_PATH>
```

**Tips:**
- Session IDs are UUIDs that uniquely identify each chat session
- Sessions are stored per directory, so each project has its own set of sessions
- The most recently updated sessions appear first in the list

## Log files

Kiro CLI maintains log files for troubleshooting:

**Locations:**
- **macOS**: `$TMPDIR/kiro-log/`
- **Linux**: `$XDG_RUNTIME_DIR` or `/tmp/kiro-log/`

**Environment Variables:**

Control logging and Kiro home directory with these environment variables:

| Variable | Values | Description |
|----------|--------|-------------|
| `KIRO_HOME` | path | Override the `~/.kiro` directory used for global agents, prompts, skills, steering, settings, and sessions |
| `KIRO_LOG_LEVEL` | `error`, `warn`, `info`, `debug`, `trace` | Set logging verbosity (default: `error`) |
| `KIRO_LOG_NO_COLOR` | `1`, `true`, `yes` | Disable colored log output (v1.26.0+) |

**Log Levels:**

Set via `KIRO_LOG_LEVEL` environment variable:

- `error`: Only errors (default)
- `warn`: Warnings and errors
- `info`: Info, warnings, and errors
- `debug`: Debug info and above
- `trace`: All messages including detailed traces

**Examples:**
```bash
# Enable debug logging
export KIRO_LOG_LEVEL=debug
kiro-cli chat

# Disable colored output (useful for CI/CD, v1.26.0+)
export KIRO_LOG_NO_COLOR=1
kiro-cli chat

# Combined
export KIRO_LOG_LEVEL=debug
export KIRO_LOG_NO_COLOR=true
kiro-cli chat

# For fish shell
set -x KIRO_LOG_LEVEL debug
set -x KIRO_LOG_NO_COLOR 1
kiro-cli chat
```

**New in v1.26.0:** `KIRO_LOG_NO_COLOR` environment variable to disable colored log output, useful for CI/CD pipelines and log file processing.

**Warning:** Log files may contain sensitive information including file paths, code snippets, and command outputs. Be cautious when sharing logs.

## Next steps

- [Slash Commands Reference](https://kiro.dev/docs/reference/slash-commands.md)
- [Settings Configuration](https://kiro.dev/docs/reference/settings.md)
- [Setup and troubleshooting](https://kiro.dev/docs/cli/setup.md)
