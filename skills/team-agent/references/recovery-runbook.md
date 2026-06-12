# Team Agent Recovery Reference

This reference is for the Rust Team Agent runtime. This reference is a live document.

When a Team Agent framework problem appears, check this reference first. After the problem is resolved, update this reference before closing the work, whether or not the final fix used an existing entry here. New recovery advice may be added only after RS validation on a real Team Agent environment; unverified Python-only procedures stay out of this external reference.

## Validation Gate

Every prescription below must carry an RS verification record. A command copied from Python 0.2.11 or from an incident note is not valid until it has been verified against the RS runtime. If RS behavior differs, document the RS behavior, not the Python behavior.

Use placeholders only:

- `$WORKSPACE` for the team workspace
- `$TEAM` for the active team key when a command needs one
- `$AGENT_ID` for a worker id
- `$TASK_ID` for a task id

Do not write recipes that depend on a user's private absolute paths, private specification fields, or unverified local state.

## Recovery Levels

| Level | Who may run it | Scope | Misuse consequence |
| --- | --- | --- | --- |
| L1 | Any agent, including a worker | Read-only diagnosis, file report handoff, status/log/result inspection, current-payload E23 fallback only | Low risk, but must not mutate bindings or lifecycle state |
| L2 | Leader only | Resume-oriented lifecycle repair such as restart of an existing team or worker | A worker running L2 must be treated as `worker.privilege_escalation_attempt` |
| L3 | User/operator only | Claiming/taking over leader ownership, state surgery, coordinator respawn, fresh/reset | E19/#232 class risk: wrong workspace or pane can steal ownership or destroy context |

Spine for every incident:

1. Diagnose with L1 read-only commands.
2. Prefer resume or repair that preserves context.
3. Use L3 surgery only when the user/operator explicitly approves and the coordinator/write owner is controlled.
4. Use fresh/reset only as the last resort.

Fresh/reset red line:

> WARNING: context will be lost. Run this only after diagnostics, resume, and repair have all failed and the user has explicitly typed approval. Do not make it the default. Each use must emit `recovery.context_reset` with source and approval timestamp.

## Symptom Index

| Symptom | Root cause family | Prescription |
| --- | --- | --- |
| Status is confusing, messages/results may be pending, or a worker says it cannot report | Need a safe read-only picture first | §1 L1 Read-Only Triage |
| Leader pane exists but messages say `leader_not_attached`, `rebind_required`, `team_owner_mismatch`, or leader 被抢 | Leader ownership or receiver binding drift | §2 L3 Claim Or Rebind Leader |
| A live team must be recovered without losing worker context | Resume/restart path needed | §3 L2 Restart With Resume First |
| Shutdown/restart cleanup status is ambiguous | Need residue-based truth, not a single `ok` field | §4 L1/L2 Cleanup Verification |
| MCP tool call fails with `Transport closed` while the worker still has the payload | Provider-owned MCP stdio child died | §5 Superseded/Pending E23 Fallback Boundary |

## 1. L1 Read-Only Triage

[contextual: preserved] [level: L1] Any agent may run this. It reads state only and does not mutate the team.

Diagnosis:

```bash
team-agent status --workspace "$WORKSPACE" --json --detail
team-agent collect --workspace "$WORKSPACE" --json
```

What to look for:

- `leader_receiver.status`
- `coordinator.status`
- agent `status`
- uncollected results
- messages stuck in `accepted`, `failed`, or `rebind_required`

Resume/repair:

- If the answer identifies an L2/L3 condition, do not run the elevated command from a worker pane. Send the evidence to the leader/user.
- If MCP reporting is broken, write a handoff artifact under the team's artifacts directory and tell the leader the relative path.

Last resort:

- None. This entry is read-only. Do not turn triage into fresh/reset.

RS verification record:

- `#249 Python 0.2.11 -> Rust 0.3.2 upgrade compatibility`: Rust `status`, `collect`, and `read` succeeded against an upgraded live team; restart resumed and post-restart MCP delivery worked. Evidence ledger: `dogfood-execution-ledger-T2.md`, case `#249`.
- Mac mini LOOP/G1-G4 evidence also captured status and command outputs after restart/shutdown flows; see `g1g4-macmini-confirm-report.md`.

Python 0.2.11 compatibility note:

- Python teams used equivalent `status`/`collect` verbs. Python-specific wrapper paths are deprecated and are intentionally not listed here.

## 2. L3 Claim Or Rebind Leader

[contextual: preserved] [level: L3] User/operator only. A worker must not run this. Misuse can steal the leader binding or recreate E19/#232 style ownership damage.

Use this when L1 triage shows the leader pane is live but leader-bound delivery is blocked by ownership or receiver drift.

Diagnosis:

```bash
team-agent status --workspace "$WORKSPACE" --json --detail
```

Confirm:

- The current pane is the intended leader/operator pane.
- The workspace is the intended team workspace.
- The target team is the intended `$TEAM` when multiple teams share a workspace.

Resume/repair:

```bash
team-agent claim-leader --workspace "$WORKSPACE" --team "$TEAM" --json
```

If the previous owner is live and the user explicitly approves stealing ownership, add the product's confirmation flag:

```bash
team-agent claim-leader --workspace "$WORKSPACE" --team "$TEAM" --confirm --json
```

Validate:

```bash
team-agent status --workspace "$WORKSPACE" --json --detail
```

The receiver should be attached and new leader-bound delivery should no longer report `leader_not_attached`.

Extreme surgery:

- Do not edit binding fields by hand unless a user/operator has approved the exact state repair and the coordinator/write owner is stopped or otherwise controlled.

Last resort:

- Fresh/reset is not a leader claim repair. Do not use it here.

RS verification record:

- Dogfood T2 R2: old live team restart/claim/takeover passed from the persistent leader pane. Evidence ledger: `dogfood-execution-ledger-T2.md`, case `R2`.
- RS #235 bug3 Mac mini run: `claim-leader` and `takeover` claimed live panes and persisted `leader_receiver.status="attached"` with `claimed_via="claim-leader"`. Evidence root id: `7b146ce-bug3-20260606T023011Z`.
- G1-G4 report: help path for `claim-leader` exited 0 and did not trigger action validation. Evidence: `g1g4-macmini-confirm-report.md`, CR-063.

Python 0.2.11 compatibility note:

- Python had analogous claim/takeover verbs. Use RS commands above for RS teams.

## 3. L2 Restart With Resume First

[contextual: partial] [level: L2] Leader only. A worker must not run this. The goal is to preserve session context; resume is always before fresh.

Use this when L1 triage shows a team or worker needs lifecycle repair but the state still carries enough runtime/session context to resume.

Diagnosis:

```bash
team-agent status --workspace "$WORKSPACE" --json --detail
```

Resume/repair:

```bash
team-agent restart "$WORKSPACE" --team "$TEAM" --json
```

For a single worker where the leader has explicitly selected that worker:

```bash
team-agent restart-agent "$AGENT_ID" --workspace "$WORKSPACE" --team "$TEAM" --json
```

Validate:

```bash
team-agent status --workspace "$WORKSPACE" --json --detail
team-agent collect --workspace "$WORKSPACE" --json
```

The expected result is `status="restarted"` or an equivalent running projection without duplicate-session errors. Do not treat a partial failure on one worker as permission to reset the whole team.

Extreme surgery:

- If restart reports a resume-integrity refusal, stop and escalate. Do not silently rerun with fresh flags.

Last resort:

```bash
# Last resort only; see red line at top of this reference.
team-agent restart "$WORKSPACE" --team "$TEAM" --allow-fresh --json
```

WARNING: context will be lost. Run this only after diagnostics, resume, and repair have all failed and the user has explicitly typed approval. Do not make it the default. Each use must emit `recovery.context_reset` with source and approval timestamp.

RS verification record:

- Dogfood T2 R2: restart returned restarted and status after restart showed a running team. Evidence ledger: `dogfood-execution-ledger-T2.md`.
- Dogfood #249: Rust restart resumed session `019e9ffd-c99c-7b60-bff3-9612ab35bcf5`, post-restart MCP delivery worked, and cleanup residue was 0. Evidence ledger: `dogfood-execution-ledger-T2.md`, case `#249`.
- G1-G4 Mac mini reconfirmation: restart recovered the same fake team in CR-007, after acknowledge-idle context in CR-021, and in coordinator recovery context in CR-052. Evidence: `g1g4-macmini-confirm-report.md`.

Python 0.2.11 compatibility note:

- Python restart procedures are deprecated compatibility background only. For RS teams, use the RS commands above.

## 4. L1/L2 Cleanup Verification

[contextual: partial] [level: L1 for verification, L2 for shutdown command] A worker may inspect cleanup evidence. Only the leader should run lifecycle cleanup.

Diagnosis:

```bash
team-agent status --workspace "$WORKSPACE" --json --detail
```

Resume/repair:

- If a leader intentionally stops or shuts down a test team, verify by residue, not by a single top-level `ok` field.
- For cleanup verification, inspect whether the team session is gone and whether residual process/session lists are empty in the command output or status evidence.

Leader command:

```bash
team-agent shutdown --workspace "$WORKSPACE" --keep-logs --json
```

Validate:

- `session_killed=true` is decisive for a session that was expected to be killed.
- `residuals.sessions=[]` and `residuals.processes=[]` are decisive for cleanup.
- A missing coordinator after a successful cleanup can be expected; do not classify it alone as a product failure.

Extreme surgery:

- Do not manually kill shared tmux servers from a worker pane.

Last resort:

- None. Cleanup verification is not a context reset path.

RS verification record:

- Dogfood T2 R3: quick-start with true Codex worker, MCP server live, shutdown returned `ok=true`, `session_killed=true`, endpoint `has-session` rc=1, and post MCP/node/codex/process residue lines were all 0. Evidence ledger: `dogfood-execution-ledger-T2.md`.
- G1-G4 CR-005: stop returned `ok=true`, `status="stopped"`, `session_killed=true`; following shutdown reported missing without treating that alone as failure. Evidence: `g1g4-macmini-confirm-report.md`.

Python 0.2.11 compatibility note:

- Python cleanup notes may mention direct tmux commands. They are not listed here as RS recovery prescriptions.

## 5. Superseded/Pending E23 Fallback Boundary

[contextual: preserved] [level: L1] E23 fallback is the only narrow worker-scope exception to the normal tool boundary. It is for a current payload already in hand after a leader-bound MCP delivery failure.

Current status:

- Product implementation exists in the E23 branch for `fallback-send-leader` and `fallback-report-result`.
- Local RS verification has covered CLI help and focused E23 tests.
- This reference does not yet promote the fallback CLI to a general external prescription because the new documentation gate requires current-release RS validation in the Mac mini environment.

Until that validation is added:

- Workers should write a file handoff artifact for the current payload and ask for leader/user recovery.
- Leaders should prefer restart/resume of the affected worker after the current payload is preserved.

Do not use E23 fallback as a general message path. It does not permit arbitrary worker execution of L2/L3 recovery operations.

RS verification record:

- Local only at the time of this reference update: E23 focused tests and CLI help on the merged E23 documentation branch. Mac mini current-release validation is still required before adding exact fallback commands here.

Python 0.2.11 compatibility note:

- Python MCP stdio EOF had no durable reconnect. File handoff was the practical fallback. RS E23 adds product support, but this entry remains gated until real-machine validation is complete.

## Held Out Of This Reference

[contextual: unknown] Python-only procedures from older incident notes are intentionally not included as external RS prescriptions:

- legacy roster block cloning
- direct live runtime-state injection
- raw coordinator respawn commands
- direct tmux-server surgery
- hard-coded runtime paths

These may be useful forensic background for maintainers, but each must pass the RS validation gate before becoming a user-facing recipe.
