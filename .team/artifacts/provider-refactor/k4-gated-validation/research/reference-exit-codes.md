> ## Documentation Index
> Fetch the complete documentation index at: https://kiro.dev/llms.txt
> Use this file to discover all available pages before exploring further.

# Exit codes

> CLI exit codes for scripting and CI/CD integration

Kiro CLI returns specific exit codes to indicate operation status. Use these in scripts and CI/CD pipelines to detect success, failures, and specific error conditions.

## Exit code reference

| Code | Name | Description |
|------|------|-------------|
| 0 | Success | Command completed successfully |
| 1 | Failure | General failure (auth error, invalid args, operation failed) |
| 3 | MCP Startup Failure | A server required by `--require-mcp-startup` did not start |
| 4 | Requested Agent Not Found | The agent a non-interactive run named was unavailable on the selected agent engine |

## Requiring MCP servers

By default, MCP server failures are logged as warnings but don't affect the exit code. Use `--require-mcp-startup` to wait for configured MCP servers before a run begins:

```bash
kiro-cli chat --require-mcp-startup --no-interactive "Run task"
```

If a required server does not start, Kiro CLI exits with code 3. In a non-interactive V3 run, this includes a server failure, an undetermined startup state, or no reported status within 30 seconds.

**ℹ️ Info:** Use `--require-mcp-startup` in CI/CD pipelines where MCP tools are essential. This prevents the task from starting without the expected tools.

## Requested agent not found

A non-interactive run (`--no-interactive`, V2 or V3) exits with code 4 when its named agent isn't available on the selected agent engine:

- A run names an agent with `--agent` on either engine. If that agent isn't found, the run exits 4.
- On V3, a fresh session also names the `chat.defaultAgent` setting. If that configured default is unavailable, the run exits 4.
- A run with no named agent is not gated. This includes resumed sessions without `--agent`.

Exit code 4 applies regardless of `--trust-all-tools`.

**⚠️ Warning:** Kiro can detect the missing agent after the turn has started. In that case, the built-in default agent may already have done some work, including tool calls, before the run stops with exit code 4. Any output already written to stdout stays there.

Other `--agent` failures do not exit 4. They report the error and exit with code 1.

## Scripting examples

Handle different exit codes to take appropriate action in your automation:

### Bash script

```bash
#!/bin/bash
kiro-cli chat --require-mcp-startup --no-interactive --trust-all-tools "Run analysis"
exit_code=$?

case $exit_code in
    0) echo "Success" ;;
    3) echo "MCP servers failed to start"; exit 1 ;;
    4) echo "Requested agent not found"; exit 1 ;;
    *) echo "Failed with code $exit_code"; exit $exit_code ;;
esac
```

### CI/CD pipeline

```yaml
- name: Run Kiro task
  run: |
    kiro-cli chat --require-mcp-startup --no-interactive --trust-all-tools "Analyze code"
  continue-on-error: false
```

## Hook exit codes

[Hooks](https://kiro.dev/docs/hooks.md) use a separate set of exit codes to control tool execution:

| Code | Behavior |
|------|----------|
| 0 | Hook succeeded |
| 2 | (PreToolUse only) Block tool execution; STDERR returned to LLM |
| Other | Hook failed; STDERR shown as warning |

## Best practices

- **Use `--require-mcp-startup` in CI/CD** when your tasks depend on MCP tools
- **Add verbose logging** (`-v` or `-vv`) when debugging exit code issues
- **Check exit codes explicitly** rather than relying on implicit shell behavior
- **Separate MCP failures from general failures** to provide better error messages to users
