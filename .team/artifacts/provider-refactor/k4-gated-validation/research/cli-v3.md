> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# What's new in CLI V3

> CLI V3 runs the unified agent harness in your terminal. What changed from 2.x, breaking changes, step-by-step migration, and new features.

**ℹ️ Info:** An early release of Kiro CLI V3 is now available. Use `kiro-cli --v3` for interactive CLI sessions. ACP clients use `kiro-cli acp --agent-engine=v3 --auth-method=cli`; see [Migrate an ACP client to CLI V3](https://kiro.dev/docs/cli/v3/acp-migration.md). V3 runs alongside your existing 2.x install — your current setup is unchanged until you opt in.

## A single harness for all Kiro surfaces

CLI V3 is built on the same [unified agent harness](https://kiro.dev/docs/how-kiro-works.md) that powers the Kiro IDE and Kiro Web. Every improvement to the harness (new tools, better planning, smarter tool selection) now ships to all clients simultaneously, and your `.kiro` configuration is portable across all surfaces.

## What's new in V3

**Spec-driven development** — the Spec agent brings structured development to the terminal. Define requirements, generate designs, execute task plans. Use `/spec new <name>` to start. [Learn more →](https://kiro.dev/docs/specs.md)

**Capability-based permissions** — declare structured policies in `permissions.yaml` for fine-grained, auditable control. One rule can allow or deny an entire category of operations across all tools. [Learn more →](https://kiro.dev/docs/permissions.md)

**Enhanced hooks** — standalone `.kiro/hooks/*.json` files with two action types (shell commands and agent prompts), new triggers, and a versioned schema. [Learn more →](https://kiro.dev/docs/hooks.md)

**Enhanced agent config** — tag-based tool selection, unified permissions block, Markdown format, inline MCP servers. [Learn more →](https://kiro.dev/docs/custom-agents.md)

**Tangent** — branch your conversation into side-conversations that inherit your context, explore freely, then jump back to exactly where you left off. Use `/tangent` to branch. [Learn more →](https://kiro.dev/docs/cli/v3/tangent.md)

## Feature overview

| Feature | Status | Change from 2.x | Action needed |
|---------|:------:|-----------------|:-------------:|
| **Chat (Default mode)** | ✅ Available | Renamed from "Vibe" | None |
| **Spec-driven development** | ✅ New | Built-in Spec agent | None |
| **Plan mode** | ✅ New | New built-in agent (Shift+Tab) | None |
| **Tangent** | ✅ New | Named, nestable side-conversations | None |
| **Custom agents** | ⬆️ Enhanced | Tags replace tool IDs; `toolsSettings` → `permissions` | ⚠️ Migrate |
| **Steering** | ✅ Available | Front matter metadata added | None |
| **Hooks** | ✅ Available | New JSON schema, standalone files | ⚠️ Migrate |
| **Permissions** | ✅ New | Replaces trust flags | ⚠️ Migrate |
| **MCP servers** | ✅ Available | OAuth, disabledTools, autoApprove added | None |
| **Skills** | ✅ Available | Unchanged | None |
| **Powers** | ✅ Available | Auto-pickup from IDE | None |
| **Sub-agents** | ✅ Available | Single-agent UI (was multi-monitor) | None |
| **Compaction** | ✅ Available | Manual `/compact` + improved auto | None |
| **Session export** | ✅ New | New capability | None |
| **Trusted workspaces** | ✅ New | New capability | None |
| **Code Intelligence** | ✅ Available | Client-vended LSP tool | None |
| **Knowledge** | ✅ Available | Semantic indexing | None |
| **Todo** | ⚠️ Gated | Available but visualization pending | None |
| **aws_tool** | ❌ Removed | Use MCP servers | ⚠️ Migrate |

## Breaking changes

| Change | Previous (2.x) | New (V3) | Impact |
|--------|----------------|-----------|--------|
| `aws_tool` removed | Built-in AWS tool | Use MCP servers for AWS access | High |
| Session format changed | v2 session format | v3 session format (not backward-compatible) | High — back up `~/.kiro/sessions/` before upgrading |
| Hook format changed | Hooks embedded in agent config | Standalone `.kiro/hooks/*.json` files | High |
| Hook trigger names | camelCase (`agentSpawn`) | PascalCase (`SessionStart`) | High |
| Tool IDs standardized | camelCase (`readFile`, `writeFile`) | snake_case with short aliases | Medium |
| Sub-agent UI simplified | Multi-agent monitor (Ctrl+G) | Single-agent UI — sub-agents run in background | Medium |
| Trust model replaced | `--trust-all-tools`, `/tools trust` | Capability-based `permissions.yaml` | High |
| "Vibe" concept removed | N/A for CLI | "Default" mode is the standard name | Low |

## Migration guide

Follow these steps in order. The most impactful changes are permissions and hooks — both require manual migration.

- [Full migration guide](https://kiro.dev/docs/cli/v3/migration-guide.md) — all 6 steps with examples
- [Permissions migration](https://kiro.dev/docs/cli/v3/permissions.md) — trust flags → `permissions.yaml`, behavioral changes
- [Hooks migration](https://kiro.dev/docs/cli/v3/hooks-migration.md) — embedded hooks → `.kiro/hooks/`, trigger name mapping
- [ACP client migration](https://kiro.dev/docs/cli/v3/acp-migration.md) — update ACP clients from the CLI v2 server to CLI V3
- [Agent config changes](https://kiro.dev/docs/cli/v3/agent-config.md) — new fields and Markdown format
- [New features](https://kiro.dev/docs/cli/v3/new-features.md) — plan mode, powers auto-pickup, skills, compaction, trusted workspaces, built-in tools

**ℹ️ Info:** To convert your existing V2 agent configurations to the universal V2+V3 format, run `/upgrade-agent` inside a V3 session. See [Upgrading agent configs](https://kiro.dev/docs/cli/v3/upgrade-agent.md) for details.

## Known gaps

| Gap | Detail |
|-----|--------|
| **Classic mode not supported** | The legacy non-TUI mode (`kiro-cli chat` without the TUI) does not support the v3 engine. Use the TUI. |
| **Session resume** | V3 sessions cannot be resumed in V2. If you switch back to the V2 engine, previously created V3 sessions will not be available. |

## Learn more

- `kiro-cli diagnostic` — diagnostics and environment validation
- `kiro-cli issue` — file a bug report
- [Hooks documentation](https://kiro.dev/docs/hooks.md) — full hooks reference
- [MCP configuration](https://kiro.dev/docs/mcp/configuration.md) — MCP server setup
- [Steering](https://kiro.dev/docs/steering.md) — steering document details
