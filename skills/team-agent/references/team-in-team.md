# Team-In-Team Reference

This reference is for explicitly requested nested Team Agent teams. Use it only when the user or leader asks a worker to create or operate a child team. For normal work, a worker should stay inside its parent team and report through `team_orchestrator`.

## Safety Rules

- Use the Team Agent framework skill and `team-agent` CLI. Do not use a provider's built-in agent-team feature when the user asked for Team Agent.
- Never create a child team in the parent team's `.team/current` directory.
- Never edit, delete, or reuse files under the parent team's `.team/current`.
- Never shut down a team just because `.team/current` is already occupied. In a nested setup, that is probably the parent team; shutting it down can kill the main node.
- A child team needs its own workspace directory. The role-doc directory is not the child workspace.
- MCP tools are team-scoped. A worker must not try to widen `team_orchestrator.send_message` to another team.

## Child Workspace

Pick an independent directory before writing `TEAM.md` or agent role docs. A safe default is a child workspace under the parent workspace, but outside the parent `.team/current`:

```bash
PARENT_WORKSPACE="$PWD"
case "$PARENT_WORKSPACE" in
  */.team/current|*/.team/current/*)
    echo "Refusing to create a child team from parent .team/current; cd to the parent workspace root first." >&2
    exit 1
    ;;
esac
CHILD_TEAM="child-review-team"
CHILD_WORKSPACE="$PARENT_WORKSPACE/.team/children/review-$(date +%Y%m%d%H%M%S)"
mkdir -p "$CHILD_WORKSPACE"
cd "$CHILD_WORKSPACE"
mkdir -p .team/current/agents
```

Then create the child team's files inside the child workspace:

```bash
cat > .team/current/TEAM.md <<'EOF'
---
name: child-review-team
objective: Run a bounded child-team task and report to the parent worker.
dangerous_auto_approve: false
fast: false
---

Child team config only.
EOF

cat > .team/current/agents/reviewer.md <<'EOF'
---
name: reviewer
role: Independent Reviewer
provider: codex
tools:
  - fs_read
  - fs_list
  - mcp_team
---

Review only the files and instructions provided by the child-team leader.
Report findings with team_orchestrator.report_result exactly once.
EOF

team-agent quick-start .team/current
```

If `quick-start` reports that `current` or a tmux session already exists, do not run `shutdown`. Move to a new child workspace or change the child team name.

## Parent Protection

The parent worker is also the child team's leader. Keep the two scopes separate:

- Parent team: the worker talks upward with its parent `team_orchestrator` MCP tools.
- Child team: the same worker talks downward with `team-agent send --workspace "$CHILD_WORKSPACE" --team "$CHILD_TEAM" ...`.

Do not run child lifecycle commands against the parent workspace. Always pass the child workspace and team when operating the child team from the parent worker.

## Parent-Child Messaging

Use explicit two-hop routing.

Parent leader to child worker:

1. Parent leader sends the child-task instruction to the parent worker that owns the child team.
2. Parent worker sends into the child team:

```bash
team-agent send reviewer "Review this patch and report findings" \
  --workspace "$CHILD_WORKSPACE" \
  --team "$CHILD_TEAM" \
  --watch-result
```

Child worker to parent leader:

1. Child worker reports to its child leader:

```text
team_orchestrator.send_message(to="leader", content="short progress")
team_orchestrator.report_result(summary="short completion", status="success")
```

2. The parent worker, acting as child leader, relays the relevant summary upward to the parent leader with its parent-team MCP tools.

Do not ask a child worker to send directly to a parent-team agent id. Current MCP scope is team-local and should refuse out-of-scope peers. Use CLI `--workspace` and `--team` only from the appropriate leader/operator side when crossing team boundaries.

## Clean-Context Roles

Asking for a clean-context role is a normal feature request when the user wants a blind reviewer, a fresh second opinion, or a role that forgets earlier conversation. It is not the same as failure recovery.

For an existing worker that should restart with a blank provider context, the leader can use:

```bash
team-agent reset-agent "$AGENT_ID" --workspace "$WORKSPACE" --team "$TEAM" --discard-session --json
```

or the equivalent restart form:

```bash
team-agent restart-agent "$AGENT_ID" --workspace "$WORKSPACE" --team "$TEAM" --discard-session --json
```

Use this only when the user intentionally wants a clean context. For incident recovery, follow `recovery-runbook.md`, where context-preserving repair comes first.

## Framework Gaps To Track

- Nested mode needs a way to pin the framework Team Agent skill by absolute path, so providers do not choose their built-in agent-team feature.
- A child-team worker should be prevented from shutting down the parent team by mistake.
- Child team creation should avoid defaulting to the fragile parent `current` directory.
