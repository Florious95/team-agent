# Shutdown isolation live use cases (L1–L3)

This is the fixed black-box test book for the macOS candidate. It is a design artifact, not an authorization to run the vulnerable baseline against an existing user Team.

## Frozen test contract

- **Candidate:** the exact macOS `team-agent` binary supplied by the build owner; record absolute path, version, SHA-256, source/tree, and target before any run.
- **Runner:** Luna/Pi/max/bypass, with a clean context. The runner receives only the prepared `BIN`, `SCOPE`, isolated workspace paths, role/config, public steps, expected result, and observation window.
- **Resources:** preparation owner creates disposable outer tmux host `H`, workspaces `W_A`, `W_B`, and when needed `W_C`; all team names, session names, window names, pane IDs, and canary tokens are unique to the case.
- **Existing state:** no existing user Team, socket, coordinator, process, or credential is used as a fixture. Do not inject a fault into a real user session.
- **Receipt wrapper:** preparation owner supplies one wrapper that runs the fixed command, captures stdout/stderr/real exit code/start/end time, and writes a resource snapshot. The runner does not reconstruct paths, edit state, or print full argv/env.
- **Process observation:** only `pid,ppid,etime,stat,comm` may be shown. Coordinator identity includes its exact owned instance marker; do not print credentials or complete environment.
- **Message window:** for each unique token, wait once for the configured readiness/message window (maximum 120 seconds unless the preparation receipt fixes a shorter value). Do not resend a token because an early observation is empty.
- **First unexpected event:** stop that case immediately and retain the first return, rc, timestamp, before/after snapshot, and owned-resource list. A configuration/tool failure is not a product failure.

The public commands used below are limited to the verified CLI surface:

```text
team-agent quick-start TEAMDIR --workspace WORKSPACE --name NAME --yes --no-display --backend tmux --json --detail
team-agent send TO MESSAGE... --workspace WORKSPACE --team TEAM --json
team-agent status --workspace WORKSPACE --team TEAM --json
team-agent shutdown --workspace WORKSPACE --team TEAM --keep-logs --json
```

The preparation owner freezes the actual `TEAMDIR`, `WORKSPACE`, recipient, and `SCOPE` values in the receipt wrapper. The runner copies those values; it does not invent them.

## L1 — host-nested, cross-workspace Teams

**Goal:** a Team started while `$TMUX` exists must own a private worker endpoint. Shutting down A must not modify the host session H or independent Team B.

### Prepared fixture

- H: dedicated outer tmux session containing a normal shell canary and the leader pane.
- W_A: disposable Team A config, one Luna/Pi/max worker.
- W_B: disposable Team B config in a different workspace, one Luna/Pi/max worker.
- Canary tokens: `L1-A-<unique>` and `L1-B-<unique>`, generated once and recorded without secrets.
- Before snapshot: H session/window/pane IDs; B session/window/pane IDs; A and B coordinator `pid,ppid,etime,stat,comm` plus instance markers; A/B worker readiness.

### Fixed public steps

1. In H, keep the shell canary alive and start B normally from its prepared `TEAMDIR_B`/`W_B`. From the same outer tmux context, start A using the prepared `TEAMDIR_A`/`W_A`; do not unset `$TMUX` by hand. Record the returned JSON and the before snapshot.
2. Send the unique A token to the prepared A worker and the unique B token to the prepared B worker. Observe one real return for each and record the message IDs/timestamps and coordinator identities.
3. Run the prepared scoped `shutdown` for A once. Observe the raw JSON, real exit code, A worker disappearance, and the post-shutdown snapshot. H's canary and every B session/window/pane/coordinator identity must be unchanged. A's own residuals must be explicit; a refusal is a failure for this positive case.
4. Send a second never-before-used token to B and observe a real return. Then the cleanup owner shuts down only the recorded A/B/H resources by exact identity and records any residuals.

### PASS/FAIL

- **PASS:** A's owned worker exits; H and B retain the exact before identities and B completes the second token; no host socket/session/coordinator replacement.
- **FAIL:** any H/B session/window/pane disappears, B's coordinator changes, A only appears dead because B restarted it, or receipt/exit truth is missing.
- **STOP:** any unexpected product error, foreign resource change, or need for an out-of-step repair.

## L2 — shared endpoint, cross shutdown

**Goal:** an explicitly prepared shared endpoint must still use positive Team ownership. A shutdown must not globally tear down B or the shell canary, and after A is rebuilt, B shutdown must not remove A's replacement.

### Prepared fixture

- One disposable shared endpoint fixture owned by the preparation owner; it contains owned Team A, foreign Team B, and a normal shell canary with multiple windows each.
- W_A and W_B are distinct workspace/team records. The fixture records the shared endpoint and the exact per-Team owner/generation evidence; runner does not hand-edit runtime state.
- Before snapshot: every A/B/canary session/window/pane, A/B coordinator instance identity, and the shared endpoint identity.
- Unique tokens: `L2-A-1`, `L2-B-1`, `L2-A-2`, `L2-B-2`.

### Fixed public steps

1. Start/attach the prepared shared fixture and verify both Teams are live with their multiple windows and the canary. Send `L2-A-1` and `L2-B-1`; observe both real returns and record the full before identity set.
2. Run one prepared scoped shutdown for A. Record raw stdout/stderr/exit and after identities. B's every original session/window/pane and coordinator must remain, and B must return `L2-B-2` without being restarted. The shell canary must remain usable.
3. Using a clean prepared owner identity, rebuild/start A as a new generation in the same fixture. Verify A's new token `L2-A-2`. Then shut down B once. A's replacement generation and the shell canary must remain and A must still complete its prepared return.
4. The cleanup owner shuts down the remaining A and canary resources by exact identity, then records shared-endpoint removal/residual status and repeats no destructive command.

### PASS/FAIL

- **PASS:** each scoped stop affects only its target Team; the other Team and canary retain exact session/window/pane and coordinator identities and complete post-stop messages; final cleanup removes only the last owned resources.
- **FAIL:** any `kill-server`-style global teardown, cross-Team disappearance, coordinator replacement, or “survival” that is only an automatic restart.
- **STOP:** first foreign resource loss or any need to repair the fixture during the steps.

## L3 — same workspace, scoped Teams, shared Coordinator

**Goal:** a same-workspace scoped Team stop preserves the sibling and shared Coordinator until the last owned Team is stopped; repeated scoped shutdown is idempotent and isolated.

### Prepared fixture

- One disposable W with prepared Team A and Team B records and a shared Coordinator instance. No user workspace is used.
- A/B have distinct sessions/windows/panes and exact owner/generation markers; the preparation receipt records the Coordinator instance before the run.
- Unique tokens: `L3-A-1`, `L3-B-1`, `L3-B-2`.

### Fixed public steps

1. Start both prepared Teams in W, verify both real message returns, and record the shared Coordinator identity plus all A/B resource identities.
2. Run `shutdown --team A` once. Record raw output and exit. B's original session/windows/panes and the shared Coordinator identity must be unchanged; send `L3-B-2` and observe a real return.
3. Run scoped shutdown for the final B Team. Confirm the owned Coordinator exits, the result is truthful, and no A/B resource remains. Repeat the scoped shutdown once to verify explicit idempotence, retaining the raw second receipt.

### PASS/FAIL

- **PASS:** A-only stop preserves B and the shared Coordinator; B returns after A stop; final B stop removes the Coordinator and the second stop is a bounded, explicit idempotent result.
- **FAIL:** A stop kills B or the shared Coordinator, final stop reports success while resources remain, or the repeated stop damages another case.
- **STOP:** first unexpected resource change, coordinator rotation, product error, or missing receipt.

## Receipt checklist

For each L case retain, under the preparation owner’s artifact directory:

1. candidate/source/tree/version/target/SHA-256;
2. fixed `BIN`, `SCOPE`, TEAMDIR/workspace identities and fixture manifest;
3. every command, start/end timestamp, stdout, stderr, and real exit code;
4. before/after session/window/pane identity snapshots;
5. coordinator instance and allowed process fields before/after;
6. message token and real return evidence for every required step;
7. first-error or PASS classification, cleanup owner, exact cleanup commands, and residual/UNKNOWN status.

A command being accepted or queued is not a message return. A process remaining alive is not proof of correct preservation unless its identity and a new token return are both observed.
