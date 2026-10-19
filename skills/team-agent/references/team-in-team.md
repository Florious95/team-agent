# Team-in-team reference

Use only when the user/Leader explicitly authorizes a child Team Agent team. Ordinary workers stay in their parent team and report through their three team-scoped MCP tools. A provider's built-in team feature is not a substitute for Team Agent.

## Protect the parent

- Never create, reuse, edit or delete the parent team's `.team/current` as a child workspace.
- Choose an independent child workspace before writing TEAM/role files. The role directory is not the workspace.
- Never stop the parent because a name, session or directory already exists. Treat collisions as scope/refusal evidence.
- Loading a skill or receiving a task does not change caller authority. If original native caller admission refuses creation, preserve the receipt and ask the authorized Leader to bootstrap; do not spoof or clear CALLER/Leader identity, claim another pane, or copy authentication.
- MCP remains team-scoped: `send_message`, `report_result`, optional `get_team_status`. Lifecycle stays on the separately authorized public CLI.

## Create the child through start

The following paths are placeholders for a new, explicitly authorized workspace, not a command to reuse a live team:

```sh
CHILD_WORKSPACE=/absolute/new/child-workspace
CHILD_TEAM=child-review-team
mkdir -p "$CHILD_WORKSPACE/.team/current/agents"
```

Write a bounded goal in `$CHILD_WORKSPACE/.team/current/TEAM.md` and complete roles in its `agents/`. Record the user's provider and boolean bypass decisions; use the main skill's role format. Do not infer Pi/bypass or create roles from a parent's private configuration.

From an admitted native Leader context:

```sh
team-agent start --from "$CHILD_WORKSPACE/.team/current" \
  --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM"
team-agent team status --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM" --json
```

Start creates a genuinely uninitialized complete team once. Later start can wake stopped non-paused members or prove a live no-op; it is not repair/reset and does not prove business readiness. Resolve unknown scope, drift, conflicting state or caller refusal instead of switching to an old creation command or deleting runtime files.

## Parent-child messages are two-hop

The parent worker may act as child Leader only within explicitly delegated authority. Keep the bindings separate:
1. Parent Leader instructs the parent worker through the parent team's MCP.
2. The admitted child Leader sends into the child using explicit workspace/team:

```sh
team-agent send reviewer "Review the requested files and report findings" \
  --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM"
team-agent inbox leader --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM" -n 3
```

Accepted/queued is not the child's answer. A child worker reports using its own logical MCP tools:

```text
send_message(to="leader", content="short progress")
report_result(summary="short completion")
```

3. The parent worker relays the actual result upward with its **parent** MCP tools. Do not tell a child worker to address parent IDs directly or widen its MCP scope. Cross-team CLI scope selection is not a worker privilege escalation shortcut.

## Manage and finish only the child

```sh
team-agent stop reviewer --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM"
team-agent start reviewer --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM"
team-agent team restart --session preserve --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM"
team-agent stop --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM" --json
```

Use preserve when recovery must retain context; new restart otherwise defaults to auto, which can permit a legal fresh session and is not a memory guarantee. Team restart preserves explicitly stopped/paused seats; intentionally waking the roster uses team start. Unknown/partial/native unsupported outcomes are not permission to reset or fall back. Check child-owned residuals after stop; do not kill shared servers or parent resources.

For an intentionally clean-context worker, only after explicit approval:

```sh
team-agent agent reset "$AGENT_ID" --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM" --discard-session --json
```

This is not incident recovery; use [the recovery reference](recovery-runbook.md) for that. Legacy initial-creation, shutdown and suffixed agent commands remain compatibility bridges only; new child-team instructions use the forms above.
