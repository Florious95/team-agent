# Team Agent recovery reference

Recover the selected team through public CLI evidence, not guessed bindings or old creation habits. Check that the chosen binary exposes `start`, `stop`, `team` and `agent` help. A reused candidate version string alone does not establish feature support. If missing, ask for an authorized supporting installation; do not switch binaries or rewrite state automatically.

## Authority and non-destructive order

| Level | Who | Scope |
|---|---|---|
| L1 | Any agent | Read-only status/doctor/inbox, evidence and current-payload handoff |
| L2 | Authorized Leader | Context-preserving team/agent lifecycle repair |
| L3 | Explicitly authorized user/operator | Claim/takeover, intentional context discard, exceptional state/coordinator surgery |

A worker must not run L2/L3 because it can read this reference. Explicit child-team delegation still requires original caller admission and parent protection. Follow [nested teams](team-in-team.md).

1. Diagnose at L1 in the exact workspace/team.
2. Prefer proven resume with `--session preserve`.
3. Stop on unknown, partial or capability refusal; do not reset, retry or adopt a replacement blindly.
4. Only explicit user/operator authorization can permit context loss or binding surgery. Never broadly kill shared servers or edit another workspace.

Use placeholders, not private paths: `$WORKSPACE`, `$TEAM`, `$AGENT_ID`, `$TASK_ID`.

## 1. L1 read-only triage

```sh
team-agent team status --workspace "$WORKSPACE" --team "$TEAM" --json
team-agent doctor --workspace "$WORKSPACE" --team "$TEAM" --json
team-agent inbox "$AGENT_ID" --workspace "$WORKSPACE" --team "$TEAM" -n 3 --json
```

Status is the concise node projection: use `runtime_status`, `activity`, `health` and the reported attach target without inferring absent internals. `--detail` is not a full historical dump. Follow doctor's actual owner/receiver/coordinator findings and recovery actions. Installed is not authenticated; unknown is not proven dead, stopped or unavailable.

Accepted/queued messages are not replies. Keep task/result/receiver evidence separate from transport acceptance. If the worker already has a report but MCP is broken, use the payload handoff contingency below rather than guessing it was delivered.

## 2. L3 claim or rebind only when diagnosed

Use only when diagnosis identifies owner/receiver binding drift and the intended Leader pane/workspace/team are verified. Do not use this to bypass a failed creation, resume or caller check:

```sh
team-agent claim-leader --workspace "$WORKSPACE" --team "$TEAM" --json
```

If the previous owner is live, taking ownership additionally needs explicit user approval and the public confirmation flag:

```sh
team-agent claim-leader --workspace "$WORKSPACE" --team "$TEAM" --confirm --json
```

Recheck status/doctor and real Leader-bound delivery afterward. A command receipt alone is not actual client consumption. Do not clear CALLER/Leader identity or manually patch binding fields. State/coordinator surgery requires its own exact authorization and controlled write owner; it is not a normal recovery recipe.

## 3. L2 restart with strict continuation first

```sh
team-agent team restart --session preserve --workspace "$WORKSPACE" --team "$TEAM" --json
team-agent agent restart "$AGENT_ID" --session preserve --workspace "$WORKSPACE" --team "$TEAM" --json
```

Run the applicable operation, not both automatically. Single-seat restart preflights continuation/capabilities before stop and starts only after confirmed stop. Known missing capture/backing for existing context refuses before destruction. A genuinely never-used seat may still take its original first-fresh path; that is not evidence of remembered-context restoration.

Team restart leaves explicitly stopped and paused seats alone. To intentionally wake the selected stopped non-paused roster, use:

```sh
team-agent start --workspace "$WORKSPACE" --team "$TEAM" --json
```

This activation is not a repair fallback and must be authorized for the roster. It neither unpauses seats nor guesses around corrupt/spec-only/unknown state. A live no-op is not changed role-file synchronization or business readiness.

New restart defaults to **auto** when the option is omitted: proven resume or only the original legal fresh fallback. Auto can lose remembered context; it is not synonymous with preserve or reset. If strict continuation refuses, retain the receipt and escalate. Only if the user explicitly authorizes the possible loss may the Leader choose:

```sh
team-agent team restart --session auto --workspace "$WORKSPACE" --team "$TEAM" --json
```

Unsupported native fresh/resume remains a refusal; do not change provider, force legacy fallback or repeatedly retry a partial/unknown generation. Validate actual node identities/session receipts and a natural worker reply before claiming recovery.

## 4. L1 verification / L2 scoped stop

Only an authorized Leader stops the selected team:

```sh
team-agent stop --workspace "$WORKSPACE" --team "$TEAM" --json
```

Verify actual typed results, not just exit0:
- Intended team session/panes and owned native/helper/coordinator processes have the expected terminal outcome.
- Returned `residuals.sessions`, `residuals.processes` and `residuals.owned_files` are empty, with no unresolved degraded/unknown verification.
- Where zero-removable-resource acceptance is required, independently check the frozen owned inventory, including sockets, twice. Keep predeclared native archives/evidence separate from live residuals.
- Shared coordinator/Leader/sibling resources remain protected. A stopped last-team coordinator can be expected, not automatically a new defect.

Do not manually unlink unknown sockets or kill shared tmux servers to manufacture success. Preserve a failing scene and its ownership/birth evidence.

## 5. MCP transport-dead payload handoff

If a worker already owns the current report payload and its MCP stdio transport closes before normal send/report acceptance, preserve the exact payload in a team-scoped handoff artifact and inform the Leader/user through the remaining authorized visible channel. Do not copy credentials or substitute a database write/manual answer for natural Leader consumption.

This L1 contingency is not a general alternate messaging system or permission for worker lifecycle repair. After an authorized restart/resume, return to the three normal MCP tools. Queued notification, server write and actual Leader receipt remain different evidence layers.

## 6. Intentional clean context is not incident recovery

Only after explicit user approval to discard that worker's association:

```sh
team-agent agent reset "$AGENT_ID" --workspace "$WORKSPACE" --team "$TEAM" --discard-session --json
```

Warn that context will be lost. Each intentional fresh/reset must retain `recovery.context_reset` audit evidence with authorization source and timestamp; missing evidence is not permission to fabricate an event or claim approval. Preserve the actual result and do not claim every native archive was erased. Reset is never an automatic response to preserve refusal, permission prompts or partial replacement.

## Compatibility and validation scope

The old bare/explicit-path `restart` keeps its preserve default; `quick-start`, `shutdown` and suffixed agent commands remain compatibility bridges for existing scripts, not these daily recovery instructions. Do not mechanically rename scripts or rewrite historical receipts as if they used the new surface.

The short start/stop/restart journey has isolated two-Pi lifecycle verification, including live no-op, held-seat preservation, both-seat activation and scoped cleanup. Automated fixtures cover continuation/backing/partial/native/mixed refusals. This is not a fresh all-provider same-session memory or all-platform acceptance claim. Earlier Rust resume/cleanup records retain their original scope in history; Python-only repair, roster cloning, raw state injection and direct server/coordinator surgery are not promoted into validated public recipes.
