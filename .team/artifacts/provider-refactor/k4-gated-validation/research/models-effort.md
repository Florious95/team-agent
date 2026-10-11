> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# Reasoning effort

> Control how much reasoning the model applies to balance speed, depth, and cost

Reasoning effort controls how much thinking the model applies to your prompts. Lower effort levels produce faster, shorter responses and use fewer credits. Higher levels spend more tokens on deeper analysis, multi-step reasoning, and thorough code generation.

| Capability | IDE | CLI | Web | Mobile |
|------------|:---:|:---:|:---:|:------:|
| Reasoning effort selection | ✓ | ✓ | ✓ | — |

**Available levels:**

| Level | Behavior |
|-------|----------|
| `low` | Fast, concise responses. Good for simple questions and quick lookups. |
| `medium` | Balanced reasoning. Suitable for most development tasks. |
| `high` | Thorough analysis. Better for complex refactoring and architecture decisions. |
| `xhigh` | Extended reasoning. Useful for multi-file changes and nuanced problems. |
| `max` | Maximum depth. Best for difficult debugging, security analysis, and intricate logic. |

Not all models support every level. The picker only shows levels available for your current model. See [reasoning configuration examples](#reasoning-configuration-examples) for common model schemas.

A model's configuration schema may also declare an internal `default` level. It is never offered as a choice, and a model that declares only `default` shows no picker at all, because there is nothing to choose between.

## Setting effort level

    IDE
    CLI
    Web

Click the model button in the chat input bar to open the **Model** panel. Use **Filter models** when needed, then select a model or **Auto**.

When the selected model supports reasoning effort, an **Effort** slider appears below the model list. Choose a level with the slider; the change applies to this conversation beginning with your next message. To restore the model's default level, choose the `Default: <level>` reset action. This action is not another effort level.

The model button shows the selected model or **Auto**, without the effort level. Hover over it to see the current model and effort level in the tooltip.

In the IDE, Effort controls appear only for local sessions. New-session drafts in [Agent Focus Mode](https://kiro.dev/docs/ide/experimental/focus-mode.md) and [cloud sessions](https://kiro.dev/docs/cloud-sessions.md) do not show them.

Open `/model`, then choose **Thinking** or **Effort** for the selected model. The panel only shows settings and values that model supports.

- **Thinking** controls whether a supported model uses extended reasoning.
- **Effort** controls how much reasoning the model applies.

The **Thinking** setting in `/model` changes model behavior. It is separate from **Show thinking** in `/settings display`, which only controls whether reasoning blocks appear in the terminal, and from the experimental [Thinking tool](https://kiro.dev/docs/cli/experimental/thinking.md).

You can also use `/effort` directly:

```bash
# Open effort controls
/effort

# Set a level directly
/effort high
```

`/model` is the primary reasoning interface. In V2 and V3, bare `/effort` opens the `/model` picker focused on the Effort setting. `/effort <level>` remains supported in both.

To set the initial effort level when you start a session, use the `--effort` flag:

```bash
kiro-cli chat --effort high
```

The flag applies from your first prompt, so quick lookups stay fast and complex work gets deeper reasoning from the start.

In Kiro Web, select a model in the composer. When the model offers reasoning levels, a reasoning effort selector appears beside the model selector. Choose a level before sending your prompt.

Your selection applies to the current session only. It does not persist across sessions or change your IDE or CLI defaults. Only levels supported by the current model are shown.

## Reasoning configuration examples

Each effort-capable model defines the values it accepts for reasoning-related fields in its configuration schema. The tables below show common schemas and are not an exhaustive support list. Use your surface's model and effort pickers to see the models and levels currently available to you.

In these examples, `output_config.effort` lists the available effort levels; `thinking.type` and `thinking.display` control reasoning behavior and visibility; and `max_tokens` sets the output length limit.

**Claude examples** use `output_config.effort` with `thinking.type` and `thinking.display` controls:

| Model | `thinking.type` | `thinking.display` | `output_config.effort` | `max_tokens` |
|-------|-----------------|--------------------|------------------------|--------------|
| Claude Opus 5 | `adaptive`, `disabled` | `summarized`, `omitted` | `low`, `medium`, `high`, `xhigh`, `max` | 1024–128000 |
| Claude Opus 4.8 | `adaptive`, `disabled` | `summarized`, `omitted` | `low`, `medium`, `high`, `xhigh`, `max` | 1024–128000 |
| Claude Opus 4.7 | `adaptive`, `disabled` | `summarized`, `omitted` | `low`, `medium`, `high`, `xhigh`, `max` | 1024–64000 |
| Claude Opus 4.6 | `adaptive`, `disabled` | `summarized`, `omitted` | `low`, `medium`, `high`, `max` | 1024–64000 |
| Claude Sonnet 5.5 | `adaptive`, `between_tools` | `summarized`, `omitted` | `low`, `medium`, `high`, `xhigh`, `max` | 1024–64000 |
| Claude Sonnet 5 | `adaptive`, `disabled` | `summarized`, `omitted` | `low`, `medium`, `high`, `xhigh`, `max` | 1024–128000 |
| Claude Sonnet 4.6 | `adaptive`, `disabled` | `summarized`, `omitted` | `low`, `medium`, `high`, `max` | 1024–64000 |

**GPT-5.6 models** use `reasoning.effort`:

| Model | `reasoning.effort` |
|-------|-------------------|
| GPT-5.6 Terra | `none`, `low`, `medium`, `high`, `xhigh`, `max` |
| GPT-5.6 Sol | `none`, `low`, `medium`, `high`, `xhigh`, `max` |
| GPT-5.6 Luna | `none`, `low`, `medium`, `high`, `xhigh`, `max` |

GPT-5.6 models default to `effort: "high"`.

## Persisting your effort level (CLI)

Your effort choice in `/model` persists automatically. `/effort <level>` changes the current session only; run `/effort set-current-as-default` to save that level for the current model. In V3, `--effort` and `--model` apply only to the session Kiro starts with and aren't saved as defaults. Preferences are stored in `~/.kiro/settings/cli.json`. See [In-session settings](https://kiro.dev/docs/cli/chat/settings.md#persistence) for more on how preferences persist.

## Persistent defaults (CLI)

To set default model parameters per model - so you don't have to run `/effort` at the start of every session - add `chat.modelDefaults` to your settings file:

**Claude models** (use `output_config.effort`):

```json
{
  "chat.modelDefaults": {
    "claude-sonnet-4.6": {
      "output_config": {
        "effort": "high"
      }
    },
    "claude-opus-4.7": {
      "output_config": {
        "effort": "max"
      }
    }
  }
}
```

**GPT-5.6 models** (use `reasoning.effort`):

```json
{
  "chat.modelDefaults": {
    "gpt-5.6-sol": {
      "reasoning": {
        "effort": "max"
      }
    },
    "gpt-5.6-terra": {
      "reasoning": {
        "effort": "low"
      }
    }
  }
}
```

### Configuring thinking behavior

Control whether the model uses extended thinking and how it displays reasoning:

```json
{
  "chat.modelDefaults": {
    "claude-opus-4.8": {
      "thinking": {
        "type": "adaptive",
        "display": "summarized"
      }
    },
    "claude-sonnet-4.6": {
      "thinking": {
        "type": "disabled"
      }
    }
  }
}
```

- `thinking.type` - `adaptive` enables extended thinking when the model determines it's needed; `disabled` turns it off entirely on models that support that value. Claude Sonnet 5.5 uses `between_tools` instead of `disabled`; it applies the lowest thinking setting between tool calls and supports `low`, `medium`, and `high` effort.
- `thinking.display` - `summarized` shows a condensed version of reasoning; `omitted` hides it from output. Only applies when `type` is `adaptive`.

### Configuring max output tokens

Set the maximum number of tokens the model can generate per response:

```json
{
  "chat.modelDefaults": {
    "claude-opus-4.8": {
      "max_tokens": 128000
    },
    "claude-sonnet-4.6": {
      "max_tokens": 32000
    }
  }
}
```

**Example `max_tokens` limits:**

| Model | Minimum | Maximum |
|-------|---------|---------|
| Claude Opus 4.8 | 1024 | 128000 |
| Claude Opus 4.7 | 1024 | 64000 |
| Claude Opus 4.6 | 1024 | 64000 |
| Claude Sonnet 5.5 | 1024 | 64000 |
| Claude Sonnet 5 | 1024 | 128000 |
| Claude Sonnet 4.6 | 1024 | 64000 |
| GPT-5.6 Terra | 1024 | 128000 |
| GPT-5.6 Sol | 1024 | 128000 |
| GPT-5.6 Luna | 1024 | 128000 |

### Combining all options

You can combine `output_config`, `thinking`, and `max_tokens` in a single model entry:

**Claude example:**

```json
{
  "chat.modelDefaults": {
    "claude-opus-4.8": {
      "output_config": {
        "effort": "max"
      },
      "thinking": {
        "type": "adaptive",
        "display": "summarized"
      },
      "max_tokens": 128000
    }
  }
}
```

**GPT-5.6 example:**

```json
{
  "chat.modelDefaults": {
    "gpt-5.6-sol": {
      "reasoning": {
        "effort": "max"
      },
      "max_tokens": 128000
    }
  }
}
```

To open your settings file in your editor:

```bash
kiro-cli settings open
```

Or place a `.kiro/settings/cli.json` in your project root to set workspace-level defaults that apply to everyone working in that repository.

## Precedence

When determining the effort level for a session, Kiro applies this priority order:

1. **Session override** - value set via the Effort setting in `/model`, `/effort <level>`, or `--effort` during the current session. In V3, `/clear` keeps this value; `/chat new` starts from the saved defaults below
2. **Workspace defaults** - `chat.modelDefaults` in `.kiro/settings/cli.json`
3. **User defaults** - `chat.modelDefaults` in `~/.kiro/settings/cli.json`
4. **Built-in defaults** - the model's standard effort level

## When to adjust effort

- **Bump up** when the agent is giving shallow answers, missing edge cases, or producing incomplete implementations
- **Bump down** when you need quick answers and don't want to wait for extended reasoning
- **Use `max`** for security reviews, complex debugging sessions, or when you need the agent to consider many interacting constraints

## Related

- [Models](https://kiro.dev/docs/models.md) - available models and their capabilities
- [Slash commands reference](https://kiro.dev/docs/reference/slash-commands.md#effort) - quick command reference
- [Settings](https://kiro.dev/docs/reference/settings.md) - all configurable settings
